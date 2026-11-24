use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, mpsc, oneshot, watch};

use crate::{Error, ErrorKind, FileSystem, Result};

// Constants

pub const DEFAULT_PARALLELISM: usize = 2;
pub const MAX_RETRIES: u32 = 3;
const BUFFER_SIZE: usize = 128 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
const SPEED_WINDOW: Duration = Duration::from_secs(3);
const RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_DEPTH: usize = 64;

// Types

pub type JobId = u64;

#[derive(Debug, Clone)]
pub struct TransferRequest {
    pub source: Arc<dyn FileSystem>,
    pub source_path: String,
    pub target: Arc<dyn FileSystem>,
    pub target_directory: String,
    pub target_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    #[default]
    Ask,
    Overwrite,
    Skip,
    Resume,
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JobOptions {
    pub policy: Option<ConflictPolicy>,
    pub keep_modified: bool,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictDecision {
    Overwrite,
    Skip,
    Resume,
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub job: JobId,
    pub target_path: String,
    pub source_size: u64,
    pub target_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Scanning,
    Running,
    Retrying(u32),
    WaitingForDecision,
    Paused,
    Completed,
    Failed(Error),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobSnapshot {
    pub id: JobId,
    pub name: String,
    pub source_path: String,
    pub target_path: String,
    pub is_directory: bool,
    pub status: JobStatus,
    pub transferred: u64,
    pub total: Option<u64>,
    pub files_done: usize,
    pub files_total: Option<usize>,
    pub skipped: usize,
    pub bytes_per_second: f64,
    pub current_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransferEvent {
    Updated(JobSnapshot),
    Removed(JobId),
    Conflict(Conflict),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Run,
    Pause,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Directory {
        target: String,
    },
    File {
        source: String,
        target: String,
        size: u64,
        modified: Option<SystemTime>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Completed,
    Interrupted(Control),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolution {
    Skip,
    Copy { path: String, offset: u64 },
    Interrupted(Control),
}

struct Job {
    snapshot: JobSnapshot,
    request: TransferRequest,
    options: JobOptions,
    control: watch::Sender<Control>,
    plan: Option<Vec<Item>>,
    next_item: usize,
    completed_bytes: u64,
    partial: HashMap<String, String>,
    pending: Option<oneshot::Sender<ConflictDecision>>,
    active: bool,
}

struct State {
    jobs: Vec<Job>,
    next_id: JobId,
    parallelism: usize,
    policy: ConflictPolicy,
    retry_delay: Duration,
    events: Option<mpsc::UnboundedSender<TransferEvent>>,
}

struct Inner {
    state: Mutex<State>,
    wake: Notify,
}

#[derive(Clone)]
pub struct TransferQueue {
    inner: Arc<Inner>,
}

#[derive(Clone, Copy)]
struct FileItem<'a> {
    source: &'a str,
    target: &'a str,
    size: u64,
    modified: Option<SystemTime>,
}

struct Progress {
    samples: VecDeque<(Instant, u64)>,
    last_emit: Option<Instant>,
}

// Statuses

impl JobStatus {
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed(_) | Self::Cancelled)
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Scanning | Self::Running | Self::Retrying(_) | Self::WaitingForDecision
        )
    }
}

impl JobSnapshot {
    pub fn remaining_seconds(&self) -> Option<u64> {
        let total = self.total?;
        if self.bytes_per_second < 1.0 {
            return None;
        }
        let remaining = total.saturating_sub(self.transferred) as f64;
        Some((remaining / self.bytes_per_second).ceil() as u64)
    }

    pub fn fraction(&self) -> f32 {
        match self.total {
            Some(0) if self.status == JobStatus::Completed => 1.0,
            Some(0) | None => 0.0,
            Some(total) => (self.transferred as f64 / total as f64).min(1.0) as f32,
        }
    }
}

impl From<ConflictDecision> for ConflictPolicy {
    fn from(decision: ConflictDecision) -> Self {
        match decision {
            ConflictDecision::Overwrite => Self::Overwrite,
            ConflictDecision::Skip => Self::Skip,
            ConflictDecision::Resume => Self::Resume,
            ConflictDecision::Rename => Self::Rename,
        }
    }
}

// Queue

impl Default for TransferQueue {
    fn default() -> Self {
        Self::new(DEFAULT_PARALLELISM)
    }
}

impl std::fmt::Debug for TransferQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TransferQueue")
            .finish_non_exhaustive()
    }
}

impl Hash for TransferQueue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.inner).hash(state);
    }
}

impl PartialEq for TransferQueue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl TransferQueue {
    pub fn new(parallelism: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    jobs: Vec::new(),
                    next_id: 1,
                    parallelism: parallelism.max(1),
                    policy: ConflictPolicy::default(),
                    retry_delay: RETRY_DELAY,
                    events: None,
                }),
                wake: Notify::new(),
            }),
        }
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<TransferEvent> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.state().events = Some(sender);
        receiver
    }

    pub async fn drive(self) {
        loop {
            self.start_ready();
            self.inner.wake.notified().await;
        }
    }

    // Jobs

    pub fn add(&self, request: TransferRequest) -> JobId {
        self.add_with(request, JobOptions::default())
    }

    pub fn add_with_policy(&self, request: TransferRequest, policy: ConflictPolicy) -> JobId {
        self.add_with(
            request,
            JobOptions {
                policy: Some(policy),
                ..JobOptions::default()
            },
        )
    }

    pub fn add_with(&self, request: TransferRequest, options: JobOptions) -> JobId {
        let name = request
            .target_name
            .clone()
            .unwrap_or_else(|| request.source.file_name(&request.source_path).to_string());
        let target_path = request.target.join(&request.target_directory, &name);
        let mut state = self.state();
        let id = state.next_id;
        state.next_id += 1;
        let snapshot = JobSnapshot {
            id,
            name,
            source_path: request.source_path.clone(),
            target_path,
            is_directory: false,
            status: JobStatus::Queued,
            transferred: 0,
            total: None,
            files_done: 0,
            files_total: None,
            skipped: 0,
            bytes_per_second: 0.0,
            current_file: None,
        };
        emit(&state, TransferEvent::Updated(snapshot.clone()));
        state.jobs.push(Job {
            snapshot,
            request,
            options,
            control: watch::Sender::new(Control::Run),
            plan: None,
            next_item: 0,
            completed_bytes: 0,
            partial: HashMap::new(),
            pending: None,
            active: false,
        });
        drop(state);
        self.inner.wake.notify_one();
        id
    }

    pub fn snapshots(&self) -> Vec<JobSnapshot> {
        self.state()
            .jobs
            .iter()
            .map(|job| job.snapshot.clone())
            .collect()
    }

    pub fn pause(&self, id: JobId) {
        self.change(id, |job| {
            if job.active {
                job.control.send_replace(Control::Pause);
            } else if job.snapshot.status == JobStatus::Queued {
                job.snapshot.status = JobStatus::Paused;
            }
        });
    }

    pub fn resume(&self, id: JobId) {
        self.change(id, |job| {
            job.control.send_replace(Control::Run);
            if !job.active && job.snapshot.status == JobStatus::Paused {
                job.snapshot.status = JobStatus::Queued;
            }
        });
        self.inner.wake.notify_one();
    }

    pub fn cancel(&self, id: JobId) {
        self.change(id, |job| {
            job.pending = None;
            if job.active {
                job.control.send_replace(Control::Cancel);
            } else if !job.snapshot.status.is_finished() {
                job.snapshot.status = JobStatus::Cancelled;
            }
        });
    }

    pub fn retry(&self, id: JobId) {
        self.change(id, |job| {
            if !job.active
                && matches!(
                    job.snapshot.status,
                    JobStatus::Failed(_) | JobStatus::Cancelled
                )
            {
                job.control.send_replace(Control::Run);
                job.snapshot.status = JobStatus::Queued;
            }
        });
        self.inner.wake.notify_one();
    }

    pub fn remove(&self, id: JobId) {
        let mut state = self.state();
        if let Some(index) = state
            .jobs
            .iter()
            .position(|job| job.snapshot.id == id && !job.active)
        {
            state.jobs.remove(index);
            emit(&state, TransferEvent::Removed(id));
        }
    }

    pub fn clear_finished(&self) {
        let mut state = self.state();
        let finished: Vec<JobId> = state
            .jobs
            .iter()
            .filter(|job| {
                !job.active
                    && matches!(
                        job.snapshot.status,
                        JobStatus::Completed | JobStatus::Cancelled
                    )
            })
            .map(|job| job.snapshot.id)
            .collect();
        state
            .jobs
            .retain(|job| !finished.contains(&job.snapshot.id));
        for id in finished {
            emit(&state, TransferEvent::Removed(id));
        }
    }

    pub fn pause_all(&self) {
        for id in self.ids() {
            self.pause(id);
        }
    }

    pub fn resume_all(&self) {
        for id in self.ids() {
            self.resume(id);
        }
    }

    pub fn cancel_all(&self) {
        for id in self.ids() {
            self.cancel(id);
        }
    }

    pub fn decide(&self, id: JobId, decision: ConflictDecision, apply_to_all: bool) {
        let mut state = self.state();
        if apply_to_all {
            state.policy = decision.into();
        }
        if let Some(sender) = state
            .jobs
            .iter_mut()
            .find(|job| job.snapshot.id == id)
            .and_then(|job| job.pending.take())
        {
            let _ = sender.send(decision);
        }
    }

    // Settings

    pub fn policy(&self) -> ConflictPolicy {
        self.state().policy
    }

    pub fn set_policy(&self, policy: ConflictPolicy) {
        self.state().policy = policy;
    }

    pub fn set_parallelism(&self, parallelism: usize) {
        self.state().parallelism = parallelism.max(1);
        self.inner.wake.notify_one();
    }

    pub fn set_retry_delay(&self, delay: Duration) {
        self.state().retry_delay = delay;
    }

    // Scheduling

    fn start_ready(&self) {
        let mut state = self.state();
        let mut running = state.jobs.iter().filter(|job| job.active).count();
        let mut started = Vec::new();
        let parallelism = state.parallelism;
        for job in &mut state.jobs {
            if running >= parallelism {
                break;
            }
            if job.active || job.snapshot.status != JobStatus::Queued {
                continue;
            }
            job.active = true;
            job.snapshot.status = if job.plan.is_some() {
                JobStatus::Running
            } else {
                JobStatus::Scanning
            };
            running += 1;
            started.push(job.snapshot.clone());
        }
        for snapshot in started {
            emit(&state, TransferEvent::Updated(snapshot.clone()));
            let queue = self.clone();
            tokio::spawn(async move {
                let result = queue.execute(snapshot.id).await;
                queue.finish(snapshot.id, result);
            });
        }
    }

    fn finish(&self, id: JobId, result: Result<Outcome>) {
        self.change(id, |job| {
            job.active = false;
            job.pending = None;
            job.snapshot.bytes_per_second = 0.0;
            job.snapshot.current_file = None;
            let control = *job.control.borrow();
            job.snapshot.status = match result {
                Ok(Outcome::Completed) => JobStatus::Completed,
                Ok(Outcome::Interrupted(Control::Pause)) if control == Control::Run => {
                    JobStatus::Queued
                }
                Ok(Outcome::Interrupted(Control::Pause | Control::Run)) => JobStatus::Paused,
                Ok(Outcome::Interrupted(Control::Cancel)) => JobStatus::Cancelled,
                Err(error) => JobStatus::Failed(error),
            };
        });
        self.inner.wake.notify_one();
    }

    // Execution

    async fn execute(&self, id: JobId) -> Result<Outcome> {
        let Some((request, mut control, target_root, planned)) = self.with_job(id, |job| {
            (
                job.request.clone(),
                job.control.subscribe(),
                job.snapshot.target_path.clone(),
                job.plan.is_some(),
            )
        }) else {
            return Ok(Outcome::Interrupted(Control::Cancel));
        };
        if !planned {
            let Some((plan, is_directory)) = scan(&request, &target_root, &control).await? else {
                return Ok(Outcome::Interrupted(*control.borrow()));
            };
            let total = plan
                .iter()
                .map(|item| match item {
                    Item::File { size, .. } => *size,
                    Item::Directory { .. } => 0,
                })
                .sum();
            let files = plan
                .iter()
                .filter(|item| matches!(item, Item::File { .. }))
                .count();
            self.change(id, |job| {
                job.plan = Some(plan);
                job.snapshot.is_directory = is_directory;
                job.snapshot.total = Some(total);
                job.snapshot.files_total = Some(files);
                job.snapshot.status = JobStatus::Running;
            });
        }
        loop {
            let current = *control.borrow();
            if current != Control::Run {
                return Ok(Outcome::Interrupted(current));
            }
            let Some(item) = self
                .with_job(id, |job| {
                    job.plan
                        .as_ref()
                        .and_then(|plan| plan.get(job.next_item).cloned())
                })
                .flatten()
            else {
                return Ok(Outcome::Completed);
            };
            let mut skipped = false;
            match &item {
                Item::Directory { target } => {
                    ensure_directory(request.target.as_ref(), target).await?;
                }
                Item::File {
                    source,
                    target,
                    size,
                    modified,
                } => {
                    let file = request.source.file_name(source).to_string();
                    self.change(id, |job| job.snapshot.current_file = Some(file));
                    let file = FileItem {
                        source,
                        target,
                        size: *size,
                        modified: *modified,
                    };
                    match self
                        .transfer_file(id, &request, &file, &mut control)
                        .await?
                    {
                        Some(Outcome::Interrupted(signal)) => {
                            return Ok(Outcome::Interrupted(signal));
                        }
                        Some(Outcome::Completed) => {}
                        None => skipped = true,
                    }
                }
            }
            self.change(id, |job| {
                job.next_item += 1;
                if let Item::File { target, size, .. } = &item {
                    job.partial.remove(target);
                    job.completed_bytes += size;
                    job.snapshot.transferred = job.completed_bytes;
                    job.snapshot.files_done += 1;
                    if skipped {
                        job.snapshot.skipped += 1;
                    }
                }
            });
        }
    }

    async fn transfer_file(
        &self,
        id: JobId,
        request: &TransferRequest,
        file: &FileItem<'_>,
        control: &mut watch::Receiver<Control>,
    ) -> Result<Option<Outcome>> {
        let FileItem {
            source,
            target,
            size,
            modified,
        } = *file;
        let (mut path, mut offset) = match self
            .resolve(id, request.target.as_ref(), target, size, control)
            .await?
        {
            Resolution::Skip => return Ok(None),
            Resolution::Interrupted(signal) => return Ok(Some(Outcome::Interrupted(signal))),
            Resolution::Copy { path, offset } => (path, offset),
        };
        self.change(id, |job| {
            job.partial.insert(target.to_string(), path.clone());
            if !job.snapshot.is_directory {
                job.snapshot.target_path.clone_from(&path);
            }
        });
        let base = self.with_job(id, |job| job.completed_bytes).unwrap_or(0);
        let mut progress = Progress::new();
        let mut attempt = 0;
        loop {
            let result = {
                let mut report = |position: u64| {
                    let now = Instant::now();
                    let speed = progress.record(now, position);
                    if progress.should_emit(now) {
                        self.change(id, |job| {
                            job.snapshot.transferred = base + position;
                            job.snapshot.bytes_per_second = speed;
                            job.snapshot.status = JobStatus::Running;
                        });
                    }
                };
                copy_stream(request, source, &path, offset, control, &mut report).await
            };
            match result {
                Ok(Outcome::Completed) => {
                    let options = self.with_job(id, |job| job.options).unwrap_or_default();
                    if let Some(time) = modified
                        .or(options.modified)
                        .filter(|_| options.keep_modified)
                    {
                        let _ = request.target.set_modified(&path, time).await;
                    }
                    return Ok(Some(Outcome::Completed));
                }
                Ok(outcome) => return Ok(Some(outcome)),
                Err(error) if is_retryable(&error) && attempt < MAX_RETRIES => {
                    attempt += 1;
                    let delay = self.state().retry_delay * 2u32.pow(attempt - 1);
                    self.change(id, |job| {
                        job.snapshot.status = JobStatus::Retrying(attempt);
                        job.snapshot.bytes_per_second = 0.0;
                    });
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        signal = wait_for_stop(control) => {
                            return Ok(Some(Outcome::Interrupted(signal)));
                        }
                    }
                    offset = match request.target.metadata(&path).await {
                        Ok(entry) => entry.size.unwrap_or(0).min(size),
                        Err(error) if error.kind() == ErrorKind::NotFound => 0,
                        Err(_) => offset,
                    };
                    if offset > size {
                        offset = 0;
                        path = target.to_string();
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn resolve(
        &self,
        id: JobId,
        file_system: &dyn FileSystem,
        target: &str,
        size: u64,
        control: &watch::Receiver<Control>,
    ) -> Result<Resolution> {
        let partial = self
            .with_job(id, |job| job.partial.get(target).cloned())
            .flatten();
        if let Some(path) = partial {
            let offset = match file_system.metadata(&path).await {
                Ok(entry) => entry.size.unwrap_or(0),
                Err(error) if error.kind() == ErrorKind::NotFound => 0,
                Err(error) => return Err(error),
            };
            let offset = if offset > size { 0 } else { offset };
            return Ok(Resolution::Copy { path, offset });
        }
        let existing = match file_system.metadata(target).await {
            Ok(entry) => entry,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Resolution::Copy {
                    path: target.to_string(),
                    offset: 0,
                });
            }
            Err(error) => return Err(error),
        };
        if existing.is_directory() {
            return Err(Error::new(ErrorKind::AlreadyExists, target));
        }
        let target_size = existing.size.unwrap_or(0);
        let policy = self
            .with_job(id, |job| job.options.policy)
            .flatten()
            .unwrap_or_else(|| self.state().policy);
        let decision = match policy {
            ConflictPolicy::Ask => {
                let (sender, receiver) = oneshot::channel();
                let conflict = Conflict {
                    job: id,
                    target_path: target.to_string(),
                    source_size: size,
                    target_size,
                };
                self.change(id, |job| {
                    job.pending = Some(sender);
                    job.snapshot.status = JobStatus::WaitingForDecision;
                });
                emit(&self.state(), TransferEvent::Conflict(conflict));
                let mut stop = control.clone();
                let decision = tokio::select! {
                    decision = receiver => decision.ok(),
                    signal = wait_for_cancel(&mut stop) => {
                        return Ok(Resolution::Interrupted(signal));
                    }
                };
                self.change(id, |job| job.snapshot.status = JobStatus::Running);
                match decision {
                    Some(decision) => decision,
                    None => return Ok(Resolution::Interrupted(Control::Cancel)),
                }
            }
            ConflictPolicy::Overwrite => ConflictDecision::Overwrite,
            ConflictPolicy::Skip => ConflictDecision::Skip,
            ConflictPolicy::Resume => ConflictDecision::Resume,
            ConflictPolicy::Rename => ConflictDecision::Rename,
        };
        Ok(match decision {
            ConflictDecision::Overwrite => Resolution::Copy {
                path: target.to_string(),
                offset: 0,
            },
            ConflictDecision::Skip => Resolution::Skip,
            ConflictDecision::Resume if target_size == size => Resolution::Skip,
            ConflictDecision::Resume => Resolution::Copy {
                path: target.to_string(),
                offset: if target_size < size { target_size } else { 0 },
            },
            ConflictDecision::Rename => Resolution::Copy {
                path: free_name(file_system, target).await?,
                offset: 0,
            },
        })
    }

    // State

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn ids(&self) -> Vec<JobId> {
        self.state()
            .jobs
            .iter()
            .map(|job| job.snapshot.id)
            .collect()
    }

    fn with_job<R>(&self, id: JobId, action: impl FnOnce(&mut Job) -> R) -> Option<R> {
        let mut state = self.state();
        state
            .jobs
            .iter_mut()
            .find(|job| job.snapshot.id == id)
            .map(action)
    }

    fn change(&self, id: JobId, action: impl FnOnce(&mut Job)) {
        let mut state = self.state();
        let Some(job) = state.jobs.iter_mut().find(|job| job.snapshot.id == id) else {
            return;
        };
        action(job);
        let snapshot = job.snapshot.clone();
        emit(&state, TransferEvent::Updated(snapshot));
    }
}

// Progress

impl Progress {
    fn new() -> Self {
        Self {
            samples: VecDeque::new(),
            last_emit: None,
        }
    }

    fn record(&mut self, now: Instant, position: u64) -> f64 {
        self.samples.push_back((now, position));
        while self
            .samples
            .front()
            .is_some_and(|(time, _)| now.duration_since(*time) > SPEED_WINDOW)
        {
            self.samples.pop_front();
        }
        let Some((first_time, first_position)) = self.samples.front().copied() else {
            return 0.0;
        };
        let elapsed = now.duration_since(first_time).as_secs_f64();
        if elapsed < 0.05 {
            return 0.0;
        }
        let bytes = position.saturating_sub(first_position) as f64;
        bytes / elapsed
    }

    fn should_emit(&mut self, now: Instant) -> bool {
        if self
            .last_emit
            .is_some_and(|last| now.duration_since(last) < PROGRESS_INTERVAL)
        {
            return false;
        }
        self.last_emit = Some(now);
        true
    }
}

// Helpers

fn emit(state: &State, event: TransferEvent) {
    if let Some(events) = &state.events {
        let _ = events.send(event);
    }
}

fn is_retryable(error: &Error) -> bool {
    matches!(error.kind(), ErrorKind::ConnectionFailed | ErrorKind::Other)
}

async fn wait_for_stop(control: &mut watch::Receiver<Control>) -> Control {
    loop {
        let current = *control.borrow_and_update();
        if current != Control::Run {
            return current;
        }
        if control.changed().await.is_err() {
            return Control::Cancel;
        }
    }
}

async fn wait_for_cancel(control: &mut watch::Receiver<Control>) -> Control {
    loop {
        if *control.borrow_and_update() == Control::Cancel {
            return Control::Cancel;
        }
        if control.changed().await.is_err() {
            return Control::Cancel;
        }
    }
}

async fn scan(
    request: &TransferRequest,
    target_root: &str,
    control: &watch::Receiver<Control>,
) -> Result<Option<(Vec<Item>, bool)>> {
    let source = request.source.as_ref();
    let target = request.target.as_ref();
    let root = source.metadata(&request.source_path).await?;
    if !root.is_directory() {
        let item = Item::File {
            source: request.source_path.clone(),
            target: target_root.to_string(),
            size: root.size.unwrap_or(0),
            modified: root.modified,
        };
        return Ok(Some((vec![item], false)));
    }
    let mut plan = vec![Item::Directory {
        target: target_root.to_string(),
    }];
    let mut pending = VecDeque::from([(request.source_path.clone(), target_root.to_string(), 0)]);
    while let Some((source_directory, target_directory, depth)) = pending.pop_front() {
        if *control.borrow() != Control::Run {
            return Ok(None);
        }
        let mut entries = source.list(&source_directory).await?;
        crate::sort_entries(
            &mut entries,
            crate::SortKey::Name,
            crate::SortOrder::Ascending,
        );
        for entry in entries {
            let source_path = source.join(&source_directory, &entry.name);
            let target_path = target.join(&target_directory, &entry.name);
            if entry.is_directory() {
                if depth >= MAX_DEPTH || entry.is_link {
                    continue;
                }
                plan.push(Item::Directory {
                    target: target_path.clone(),
                });
                pending.push_back((source_path, target_path, depth + 1));
            } else {
                plan.push(Item::File {
                    source: source_path,
                    target: target_path,
                    size: entry.size.unwrap_or(0),
                    modified: entry.modified,
                });
            }
        }
    }
    Ok(Some((plan, true)))
}

async fn ensure_directory(file_system: &dyn FileSystem, path: &str) -> Result<()> {
    match file_system.create_directory(path).await {
        Ok(()) => Ok(()),
        Err(error) => match file_system.metadata(path).await {
            Ok(entry) if entry.is_directory() => Ok(()),
            _ => Err(error),
        },
    }
}

async fn copy_stream(
    request: &TransferRequest,
    source: &str,
    target: &str,
    offset: u64,
    control: &mut watch::Receiver<Control>,
    report: &mut impl FnMut(u64),
) -> Result<Outcome> {
    let mut reader = request.source.open_read(source, offset).await?;
    let mut writer = request.target.open_write(target, offset).await?;
    let mut buffer = vec![0; BUFFER_SIZE];
    let mut position = offset;
    report(position);
    loop {
        let read = tokio::select! {
            biased;
            signal = wait_for_stop(control) => {
                let _ = writer.shutdown().await;
                return Ok(Outcome::Interrupted(signal));
            }
            read = reader.read(&mut buffer) => read.map_err(|error| Error::from_io(&error, source))?,
        };
        if read == 0 {
            break;
        }
        writer
            .write_all(&buffer[..read])
            .await
            .map_err(|error| Error::from_io(&error, target))?;
        position += read as u64;
        report(position);
    }
    writer
        .shutdown()
        .await
        .map_err(|error| Error::from_io(&error, target))?;
    report(position);
    Ok(Outcome::Completed)
}

async fn free_name(file_system: &dyn FileSystem, path: &str) -> Result<String> {
    let name = file_system.file_name(path).to_string();
    let parent = file_system.parent(path).unwrap_or_default();
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_string(), format!(".{extension}")),
        _ => (name.clone(), String::new()),
    };
    for number in 1..10_000 {
        let candidate = file_system.join(&parent, &format!("{stem} ({number}){extension}"));
        match file_system.metadata(&candidate).await {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(candidate),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
    }
    Err(Error::new(ErrorKind::AlreadyExists, path))
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, ReadBuf};
    use tokio::time::{Sleep, timeout};

    use super::*;
    use crate::{BoxFuture, Entry, LocalFileSystem, Reader, Writer};

    // Fakes

    #[derive(Debug, Default)]
    struct TestFileSystem {
        delay: Option<Duration>,
        failures: AtomicU32,
        fail_after: u64,
    }

    struct TestReader {
        inner: Reader,
        delay: Option<Duration>,
        sleep: Option<Pin<Box<Sleep>>>,
        fail_after: Option<u64>,
        read: u64,
    }

    impl AsyncRead for TestReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if let Some(sleep) = self.sleep.as_mut() {
                if sleep.as_mut().poll(context).is_pending() {
                    return Poll::Pending;
                }
                self.sleep = None;
            }
            if self.fail_after.is_some_and(|limit| self.read >= limit) {
                return Poll::Ready(Err(std::io::Error::other("connection reset")));
            }
            let mut chunk = vec![0; 1024];
            let mut limited = ReadBuf::new(&mut chunk);
            let this = &mut *self;
            match Pin::new(&mut this.inner).poll_read(context, &mut limited) {
                Poll::Ready(Ok(())) => {
                    let filled = limited.filled();
                    buffer.put_slice(filled);
                    this.read += filled.len() as u64;
                    if let Some(delay) = this.delay {
                        this.sleep = Some(Box::pin(tokio::time::sleep(delay)));
                    }
                    Poll::Ready(Ok(()))
                }
                other => other,
            }
        }
    }

    impl FileSystem for TestFileSystem {
        fn home(&self) -> BoxFuture<'_, Result<String>> {
            LocalFileSystem.home()
        }

        fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
            LocalFileSystem.list(path)
        }

        fn metadata(&self, path: &str) -> BoxFuture<'_, Result<Entry>> {
            LocalFileSystem.metadata(path)
        }

        fn create_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
            LocalFileSystem.create_directory(path)
        }

        fn remove_file(&self, path: &str) -> BoxFuture<'_, Result<()>> {
            LocalFileSystem.remove_file(path)
        }

        fn remove_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
            LocalFileSystem.remove_directory(path)
        }

        fn rename(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
            LocalFileSystem.rename(from, to)
        }

        fn open_read(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Reader>> {
            let path = path.to_string();
            Box::pin(async move {
                let inner = LocalFileSystem.open_read(&path, offset).await?;
                let mut left = self.failures.load(Ordering::SeqCst);
                let failing = loop {
                    if left == 0 {
                        break false;
                    }
                    match self.failures.compare_exchange_weak(
                        left,
                        left - 1,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(_) => break true,
                        Err(current) => left = current,
                    }
                };
                let reader: Reader = Box::new(TestReader {
                    inner,
                    delay: self.delay,
                    sleep: None,
                    fail_after: failing.then_some(self.fail_after),
                    read: 0,
                });
                Ok(reader)
            })
        }

        fn open_write(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Writer>> {
            LocalFileSystem.open_write(path, offset)
        }

        fn join(&self, base: &str, name: &str) -> String {
            LocalFileSystem.join(base, name)
        }

        fn parent(&self, path: &str) -> Option<String> {
            LocalFileSystem.parent(path)
        }
    }

    // Helpers

    fn temporary_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-transfer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("creating the test directory failed");
        directory
    }

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn content(size: usize) -> Vec<u8> {
        (0..size)
            .map(|index| u8::try_from(index % 251).unwrap_or(0))
            .collect()
    }

    fn request(
        source: Arc<dyn FileSystem>,
        path: &Path,
        target: Arc<dyn FileSystem>,
        directory: &Path,
    ) -> TransferRequest {
        TransferRequest {
            source,
            source_path: text(path),
            target,
            target_directory: text(directory),
            target_name: None,
        }
    }

    fn local() -> Arc<dyn FileSystem> {
        Arc::new(LocalFileSystem)
    }

    fn start(queue: &TransferQueue) -> mpsc::UnboundedReceiver<TransferEvent> {
        let events = queue.subscribe();
        tokio::spawn(queue.clone().drive());
        events
    }

    async fn wait_until(
        events: &mut mpsc::UnboundedReceiver<TransferEvent>,
        mut condition: impl FnMut(&TransferEvent) -> bool,
    ) -> TransferEvent {
        timeout(Duration::from_secs(20), async {
            loop {
                let event = events.recv().await.expect("the queue stopped");
                if condition(&event) {
                    return event;
                }
            }
        })
        .await
        .expect("timed out")
    }

    async fn wait_for_status(
        events: &mut mpsc::UnboundedReceiver<TransferEvent>,
        id: JobId,
        wanted: impl Fn(&JobStatus) -> bool,
    ) -> JobSnapshot {
        match wait_until(events, |event| {
            matches!(event, TransferEvent::Updated(snapshot) if snapshot.id == id && wanted(&snapshot.status))
        })
        .await
        {
            TransferEvent::Updated(snapshot) => snapshot,
            _ => unreachable!(),
        }
    }

    // Copying

    #[tokio::test(flavor = "multi_thread")]
    async fn folders_copy_recursively() {
        let directory = temporary_directory("tree");
        let source = directory.join("source");
        std::fs::create_dir_all(source.join("sub/deeper")).expect("creating failed");
        std::fs::create_dir_all(source.join("empty")).expect("creating failed");
        std::fs::write(source.join("a.bin"), content(5000)).expect("writing failed");
        std::fs::write(source.join("sub/b.txt"), b"hello").expect("writing failed");
        std::fs::write(source.join("sub/deeper/c.bin"), content(300_000)).expect("writing failed");
        let target = directory.join("target");
        std::fs::create_dir_all(&target).expect("creating failed");

        let queue = TransferQueue::default();
        let mut events = start(&queue);
        let id = queue.add(request(local(), &source, local(), &target));
        let done = wait_for_status(&mut events, id, |status| status.is_finished()).await;

        assert_eq!(done.status, JobStatus::Completed);
        assert!(done.is_directory);
        assert_eq!(done.files_total, Some(3));
        assert_eq!(done.files_done, 3);
        assert_eq!(done.total, Some(305_005));
        assert_eq!(done.transferred, 305_005);
        assert!((done.fraction() - 1.0).abs() < f32::EPSILON);
        let copied = target.join("source");
        assert_eq!(
            std::fs::read(copied.join("sub/deeper/c.bin")).expect("reading failed"),
            content(300_000)
        );
        assert_eq!(
            std::fs::read(copied.join("sub/b.txt")).expect("reading failed"),
            b"hello"
        );
        assert!(copied.join("empty").is_dir());
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conflict_policies_apply() {
        let directory = temporary_directory("policies");
        let source = directory.join("data.txt");
        std::fs::write(&source, b"0123456789").expect("writing failed");
        let target = directory.join("target");
        std::fs::create_dir_all(&target).expect("creating failed");
        let existing = target.join("data.txt");

        let queue = TransferQueue::default();
        let mut events = start(&queue);

        for (policy, before, after) in [
            (ConflictPolicy::Skip, &b"old"[..], &b"old"[..]),
            (ConflictPolicy::Overwrite, b"old", b"0123456789"),
            (ConflictPolicy::Resume, b"0123", b"0123456789"),
            (ConflictPolicy::Resume, b"ABCDEFGHIJKL", b"0123456789"),
        ] {
            std::fs::write(&existing, before).expect("writing failed");
            queue.set_policy(policy);
            let id = queue.add(request(local(), &source, local(), &target));
            let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
            assert_eq!(done.status, JobStatus::Completed, "{policy:?}");
            assert_eq!(
                std::fs::read(&existing).expect("reading failed"),
                after,
                "{policy:?}"
            );
            assert_eq!(done.skipped, usize::from(policy == ConflictPolicy::Skip));
        }

        queue.set_policy(ConflictPolicy::Rename);
        std::fs::write(target.join("data (1).txt"), b"taken").expect("writing failed");
        let id = queue.add(request(local(), &source, local(), &target));
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(
            std::fs::read(target.join("data (2).txt")).expect("reading failed"),
            b"0123456789"
        );
        assert!(
            done.target_path.ends_with("data (2).txt"),
            "{}",
            done.target_path
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn job_policies_override_the_queue_policy() {
        let directory = temporary_directory("job-policy");
        let source = directory.join("data.txt");
        std::fs::write(&source, b"new").expect("writing failed");
        let target = directory.join("target");
        std::fs::create_dir_all(&target).expect("creating failed");
        std::fs::write(target.join("data.txt"), b"old").expect("writing failed");

        let queue = TransferQueue::default();
        let mut events = start(&queue);
        let id = queue.add_with_policy(
            request(local(), &source, local(), &target),
            ConflictPolicy::Overwrite,
        );
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(
            std::fs::read(target.join("data.txt")).expect("reading failed"),
            b"new"
        );
        assert_eq!(queue.policy(), ConflictPolicy::Ask);
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conflicts_wait_for_a_decision() {
        let directory = temporary_directory("ask");
        let source = directory.join("source");
        std::fs::create_dir_all(&source).expect("creating failed");
        std::fs::write(source.join("one.txt"), b"new one").expect("writing failed");
        std::fs::write(source.join("two.txt"), b"new two").expect("writing failed");
        let target = directory.join("target/source");
        std::fs::create_dir_all(&target).expect("creating failed");
        std::fs::write(target.join("one.txt"), b"old").expect("writing failed");
        std::fs::write(target.join("two.txt"), b"old").expect("writing failed");

        let queue = TransferQueue::default();
        let mut events = start(&queue);
        let id = queue.add(request(
            local(),
            &source,
            local(),
            &directory.join("target"),
        ));
        let conflict = match wait_until(&mut events, |event| {
            matches!(event, TransferEvent::Conflict(_))
        })
        .await
        {
            TransferEvent::Conflict(conflict) => conflict,
            _ => unreachable!(),
        };
        assert_eq!(conflict.job, id);
        assert_eq!(conflict.source_size, 7);
        assert_eq!(conflict.target_size, 3);
        assert!(conflict.target_path.ends_with("one.txt"));
        assert_eq!(queue.snapshots()[0].status, JobStatus::WaitingForDecision);

        queue.decide(id, ConflictDecision::Overwrite, true);
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(queue.policy(), ConflictPolicy::Overwrite);
        for name in ["one.txt", "two.txt"] {
            assert!(
                std::fs::read(target.join(name))
                    .expect("reading failed")
                    .starts_with(b"new")
            );
        }
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Control

    #[tokio::test(flavor = "multi_thread")]
    async fn jobs_pause_resume_and_cancel() {
        let directory = temporary_directory("pause");
        let source = directory.join("slow.bin");
        std::fs::write(&source, content(200 * 1024)).expect("writing failed");
        let target = directory.join("target");
        std::fs::create_dir_all(&target).expect("creating failed");
        let slow: Arc<dyn FileSystem> = Arc::new(TestFileSystem {
            delay: Some(Duration::from_millis(2)),
            ..TestFileSystem::default()
        });

        let queue = TransferQueue::default();
        let mut events = start(&queue);
        let id = queue.add(request(Arc::clone(&slow), &source, local(), &target));
        wait_until(&mut events, |event| {
            matches!(event, TransferEvent::Updated(snapshot) if snapshot.transferred > 10 * 1024)
        })
        .await;
        queue.pause(id);
        let paused = wait_for_status(&mut events, id, |status| *status == JobStatus::Paused).await;
        assert!(paused.transferred < 200 * 1024);
        let partial = std::fs::metadata(target.join("slow.bin"))
            .expect("partial file missing")
            .len();
        assert!(partial > 0 && partial < 200 * 1024);

        queue.resume(id);
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(
            std::fs::read(target.join("slow.bin")).expect("reading failed"),
            content(200 * 1024)
        );

        let other_target = directory.join("other");
        std::fs::create_dir_all(&other_target).expect("creating failed");
        let other = queue.add(request(slow, &source, local(), &other_target));
        wait_for_status(&mut events, other, |status| *status == JobStatus::Running).await;
        queue.cancel(other);
        let cancelled = wait_for_status(&mut events, other, JobStatus::is_finished).await;
        assert_eq!(cancelled.status, JobStatus::Cancelled);

        queue.clear_finished();
        assert!(queue.snapshots().is_empty());
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connection_errors_are_retried_with_resume() {
        let directory = temporary_directory("retry");
        let source = directory.join("flaky.bin");
        std::fs::write(&source, content(100 * 1024)).expect("writing failed");
        let target = directory.join("target");
        std::fs::create_dir_all(&target).expect("creating failed");

        let queue = TransferQueue::default();
        queue.set_retry_delay(Duration::from_millis(10));
        let mut events = start(&queue);
        let flaky: Arc<dyn FileSystem> = Arc::new(TestFileSystem {
            failures: AtomicU32::new(2),
            fail_after: 30 * 1024,
            ..TestFileSystem::default()
        });
        let id = queue.add(request(flaky, &source, local(), &target));
        wait_for_status(&mut events, id, |status| *status == JobStatus::Retrying(1)).await;
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(
            std::fs::read(target.join("flaky.bin")).expect("reading failed"),
            content(100 * 1024)
        );

        let broken: Arc<dyn FileSystem> = Arc::new(TestFileSystem {
            failures: AtomicU32::new(MAX_RETRIES + 1),
            fail_after: 10 * 1024,
            ..TestFileSystem::default()
        });
        std::fs::remove_file(target.join("flaky.bin")).expect("removing failed");
        let id = queue.add(request(broken, &source, local(), &target));
        let failed = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert!(matches!(failed.status, JobStatus::Failed(_)));
        queue.retry(id);
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(
            std::fs::read(target.join("flaky.bin")).expect("reading failed"),
            content(100 * 1024)
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn parallelism_limits_running_jobs() {
        let directory = temporary_directory("parallel");
        let target = directory.join("target");
        std::fs::create_dir_all(&target).expect("creating failed");
        let slow: Arc<dyn FileSystem> = Arc::new(TestFileSystem {
            delay: Some(Duration::from_millis(1)),
            ..TestFileSystem::default()
        });
        let queue = TransferQueue::new(2);
        let mut events = start(&queue);
        let mut ids = Vec::new();
        for index in 0..4 {
            let source = directory.join(format!("file{index}.bin"));
            std::fs::write(&source, content(40 * 1024)).expect("writing failed");
            ids.push(queue.add(request(Arc::clone(&slow), &source, local(), &target)));
        }
        let mut statuses: HashMap<JobId, JobStatus> = HashMap::new();
        let mut most = 0;
        while statuses.len() < 4 || statuses.values().any(|status| !status.is_finished()) {
            if let TransferEvent::Updated(snapshot) = wait_until(&mut events, |_| true).await {
                statuses.insert(snapshot.id, snapshot.status);
                most = most.max(
                    statuses
                        .values()
                        .filter(|status| status.is_active())
                        .count(),
                );
            }
        }
        assert_eq!(most, 2);
        assert!(
            statuses
                .values()
                .all(|status| *status == JobStatus::Completed)
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Progress

    #[test]
    fn remaining_time_uses_the_speed() {
        let snapshot = JobSnapshot {
            id: 1,
            name: "a".to_string(),
            source_path: "a".to_string(),
            target_path: "b".to_string(),
            is_directory: false,
            status: JobStatus::Running,
            transferred: 1000,
            total: Some(5000),
            files_done: 0,
            files_total: Some(1),
            skipped: 0,
            bytes_per_second: 400.0,
            current_file: None,
        };
        assert_eq!(snapshot.remaining_seconds(), Some(10));
        assert!((snapshot.fraction() - 0.2).abs() < 0.001);
        let stalled = JobSnapshot {
            bytes_per_second: 0.0,
            ..snapshot
        };
        assert_eq!(stalled.remaining_seconds(), None);
    }

    #[test]
    fn decisions_become_policies() {
        assert_eq!(
            ConflictPolicy::from(ConflictDecision::Rename),
            ConflictPolicy::Rename
        );
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::Ask);
    }
}
