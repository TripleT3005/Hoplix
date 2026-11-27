use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, mpsc, oneshot, watch};

use crate::rate::RateLimiter;
use crate::{ChecksumMode, Error, ErrorKind, FileSystem, Result, Verification, verify_copy};

// Constants

pub const DEFAULT_PARALLELISM: usize = 2;
pub const DEFAULT_RETRIES: u32 = 3;
pub const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(1);
const BUFFER_SIZE: usize = 128 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
const SPEED_WINDOW: Duration = Duration::from_secs(3);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Local,
    Download,
    Upload,
    ServerToServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parallelism {
    pub total: usize,
    pub downloads: Option<usize>,
    pub uploads: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub delay: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpeedLimits {
    pub download: Option<u64>,
    pub upload: Option<u64>,
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JobOptions {
    pub policy: Option<ConflictPolicy>,
    pub keep_modified: bool,
    pub modified: Option<SystemTime>,
    pub site_limits: Vec<SiteLimit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteLimit {
    pub site: String,
    pub max: usize,
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
    Verifying,
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
    pub checksum: Option<Verification>,
    pub elapsed: Duration,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum TransferEvent {
    Updated(JobSnapshot),
    Removed(JobId),
    Conflict(Conflict),
    Verified {
        job: JobId,
        path: String,
        verification: Verification,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResumePoint {
    pub next_item: usize,
    pub checkpoint: Option<String>,
    pub partial: Vec<(String, String)>,
    pub fresh: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobState {
    pub id: JobId,
    pub source_path: String,
    pub target_directory: String,
    pub target_name: Option<String>,
    pub policy: Option<ConflictPolicy>,
    pub keep_modified: bool,
    pub status: JobStatus,
    pub resume: ResumePoint,
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
    direction: TransferDirection,
    control: watch::Sender<Control>,
    plan: Option<Vec<Item>>,
    next_item: usize,
    completed_bytes: u64,
    partial: HashMap<String, String>,
    fresh: HashSet<String>,
    restore: Option<ResumePoint>,
    started: Option<Instant>,
    pending: Option<oneshot::Sender<ConflictDecision>>,
    active: bool,
}

struct State {
    jobs: Vec<Job>,
    next_id: JobId,
    parallelism: Parallelism,
    policy: ConflictPolicy,
    retry: RetryPolicy,
    checksum: ChecksumMode,
    events: Option<mpsc::UnboundedSender<TransferEvent>>,
}

struct Inner {
    state: Mutex<State>,
    wake: Notify,
    download_limit: RateLimiter,
    upload_limit: RateLimiter,
}

#[derive(Clone)]
pub struct TransferQueue {
    inner: Arc<Inner>,
}

struct Stream<'a> {
    source: &'a str,
    target: &'a str,
    offset: u64,
    limiters: &'a [&'a RateLimiter],
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

// Directions and limits

impl TransferDirection {
    pub fn of(source: &dyn FileSystem, target: &dyn FileSystem) -> Self {
        match (source.is_remote(), target.is_remote()) {
            (false, false) => Self::Local,
            (true, false) => Self::Download,
            (false, true) => Self::Upload,
            (true, true) => Self::ServerToServer,
        }
    }

    pub fn downloads(self) -> bool {
        matches!(self, Self::Download | Self::ServerToServer)
    }

    pub fn uploads(self) -> bool {
        matches!(self, Self::Upload | Self::ServerToServer)
    }
}

impl Parallelism {
    pub fn new(total: usize) -> Self {
        Self {
            total: total.max(1),
            downloads: None,
            uploads: None,
        }
    }

    pub fn download_limit(&self) -> usize {
        self.downloads
            .unwrap_or(self.total)
            .clamp(1, self.total.max(1))
    }

    pub fn upload_limit(&self) -> usize {
        self.uploads
            .unwrap_or(self.total)
            .clamp(1, self.total.max(1))
    }
}

impl SiteLimit {
    pub fn new(site: &str, max: usize) -> Self {
        Self {
            site: site.to_string(),
            max: max.max(1),
        }
    }
}

impl Default for Parallelism {
    fn default() -> Self {
        Self::new(DEFAULT_PARALLELISM)
    }
}

impl RetryPolicy {
    pub fn delay_before(&self, attempt: u32) -> Duration {
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1));
        self.delay.saturating_mul(factor)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: DEFAULT_RETRIES,
            delay: DEFAULT_RETRY_DELAY,
        }
    }
}

// Jobs

impl Job {
    fn state(&self) -> JobState {
        let checkpoint = self
            .next_item
            .checked_sub(1)
            .and_then(|index| self.plan.as_ref()?.get(index))
            .map(|item| item.target().to_string());
        let resume = match (&self.restore, checkpoint) {
            (Some(point), _) if self.plan.is_none() => ResumePoint {
                partial: sorted_pairs(&self.partial),
                fresh: sorted(&self.fresh),
                ..point.clone()
            },
            (_, checkpoint) => ResumePoint {
                next_item: if checkpoint.is_some() {
                    self.next_item
                } else {
                    0
                },
                checkpoint,
                partial: sorted_pairs(&self.partial),
                fresh: sorted(&self.fresh),
            },
        };
        JobState {
            id: self.snapshot.id,
            source_path: self.request.source_path.clone(),
            target_directory: self.request.target_directory.clone(),
            target_name: self.request.target_name.clone(),
            policy: self.options.policy,
            keep_modified: self.options.keep_modified,
            status: self.snapshot.status.clone(),
            resume,
        }
    }

    fn sites(&self) -> Vec<&str> {
        let mut sites: Vec<&str> = self
            .options
            .site_limits
            .iter()
            .map(|limit| limit.site.as_str())
            .collect();
        sites.sort_unstable();
        sites.dedup();
        sites
    }
}

// Statuses

impl JobStatus {
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed(_) | Self::Cancelled)
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Scanning
                | Self::Running
                | Self::Retrying(_)
                | Self::WaitingForDecision
                | Self::Verifying
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
                    parallelism: Parallelism::new(parallelism),
                    policy: ConflictPolicy::default(),
                    retry: RetryPolicy::default(),
                    checksum: ChecksumMode::Off,
                    events: None,
                }),
                wake: Notify::new(),
                download_limit: RateLimiter::default(),
                upload_limit: RateLimiter::default(),
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
        self.insert(request, options, None)
    }

    pub fn add_paused(
        &self,
        request: TransferRequest,
        options: JobOptions,
        resume: ResumePoint,
    ) -> JobId {
        self.insert(request, options, Some(resume))
    }

    fn insert(
        &self,
        request: TransferRequest,
        options: JobOptions,
        restore: Option<ResumePoint>,
    ) -> JobId {
        let name = request
            .target_name
            .clone()
            .unwrap_or_else(|| request.source.file_name(&request.source_path).to_string());
        let target_path = request.target.join(&request.target_directory, &name);
        let direction = TransferDirection::of(request.source.as_ref(), request.target.as_ref());
        let mut state = self.state();
        let id = state.next_id;
        state.next_id += 1;
        let snapshot = JobSnapshot {
            id,
            name,
            source_path: request.source_path.clone(),
            target_path,
            is_directory: false,
            status: if restore.is_some() {
                JobStatus::Paused
            } else {
                JobStatus::Queued
            },
            transferred: 0,
            total: None,
            files_done: 0,
            files_total: None,
            skipped: 0,
            bytes_per_second: 0.0,
            current_file: None,
            checksum: None,
            elapsed: Duration::ZERO,
        };
        emit(&state, TransferEvent::Updated(snapshot.clone()));
        let (partial, fresh) = restore
            .as_ref()
            .map(|point| {
                (
                    point.partial.iter().cloned().collect(),
                    point.fresh.iter().cloned().collect(),
                )
            })
            .unwrap_or_default();
        state.jobs.push(Job {
            snapshot,
            request,
            options,
            direction,
            control: watch::Sender::new(Control::Run),
            plan: None,
            next_item: 0,
            completed_bytes: 0,
            partial,
            fresh,
            restore: restore.filter(|point| point.next_item > 0),
            started: None,
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

    pub fn job_states(&self) -> Vec<JobState> {
        self.state().jobs.iter().map(Job::state).collect()
    }

    pub fn request(&self, id: JobId) -> Option<TransferRequest> {
        self.with_job(id, |job| job.request.clone())
    }

    pub fn rebind(
        &self,
        id: JobId,
        source: Option<Arc<dyn FileSystem>>,
        target: Option<Arc<dyn FileSystem>>,
    ) -> bool {
        let mut rebound = false;
        self.change(id, |job| {
            if job.active {
                return;
            }
            if let Some(source) = source {
                job.request.source = source;
            }
            if let Some(target) = target {
                job.request.target = target;
            }
            job.direction =
                TransferDirection::of(job.request.source.as_ref(), job.request.target.as_ref());
            if job.plan.is_none() {
                let request = &job.request;
                job.snapshot.name = request
                    .target_name
                    .clone()
                    .unwrap_or_else(|| request.source.file_name(&request.source_path).to_string());
                job.snapshot.target_path = request
                    .target
                    .join(&request.target_directory, &job.snapshot.name);
            }
            rebound = true;
        });
        rebound
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

    pub fn parallelism(&self) -> Parallelism {
        self.state().parallelism
    }

    pub fn set_parallelism(&self, total: usize) {
        self.state().parallelism.total = total.max(1);
        self.inner.wake.notify_one();
    }

    pub fn set_direction_limits(&self, downloads: Option<usize>, uploads: Option<usize>) {
        {
            let mut state = self.state();
            state.parallelism.downloads = downloads.map(|limit| limit.max(1));
            state.parallelism.uploads = uploads.map(|limit| limit.max(1));
        }
        self.inner.wake.notify_one();
    }

    pub fn retry_policy(&self) -> RetryPolicy {
        self.state().retry
    }

    pub fn set_retry_policy(&self, retry: RetryPolicy) {
        self.state().retry = retry;
    }

    pub fn checksum_mode(&self) -> ChecksumMode {
        self.state().checksum
    }

    pub fn set_checksum_mode(&self, mode: ChecksumMode) {
        self.state().checksum = mode;
    }

    pub fn speed_limits(&self) -> SpeedLimits {
        SpeedLimits {
            download: self.inner.download_limit.rate(),
            upload: self.inner.upload_limit.rate(),
        }
    }

    pub fn set_speed_limits(&self, limits: SpeedLimits) {
        self.inner.download_limit.set_rate(limits.download);
        self.inner.upload_limit.set_rate(limits.upload);
    }

    // Scheduling

    fn start_ready(&self) {
        let mut state = self.state();
        let parallelism = state.parallelism;
        let mut running = 0;
        let mut downloads = 0;
        let mut uploads = 0;
        for job in state.jobs.iter().filter(|job| job.active) {
            running += 1;
            downloads += usize::from(job.direction.downloads());
            uploads += usize::from(job.direction.uploads());
        }
        let mut per_site: HashMap<String, usize> = HashMap::new();
        for job in state.jobs.iter().filter(|job| job.active) {
            for site in job.sites() {
                *per_site.entry(site.to_string()).or_default() += 1;
            }
        }
        let mut started = Vec::new();
        for job in &mut state.jobs {
            if running >= parallelism.total {
                break;
            }
            if job.active || job.snapshot.status != JobStatus::Queued {
                continue;
            }
            let direction = job.direction;
            if (direction.downloads() && downloads >= parallelism.download_limit())
                || (direction.uploads() && uploads >= parallelism.upload_limit())
            {
                continue;
            }
            if job.options.site_limits.iter().any(|limit| {
                per_site.get(&limit.site).copied().unwrap_or_default() >= limit.max.max(1)
            }) {
                continue;
            }
            for site in job.sites() {
                *per_site.entry(site.to_string()).or_default() += 1;
            }
            downloads += usize::from(direction.downloads());
            uploads += usize::from(direction.uploads());
            job.active = true;
            job.started = Some(Instant::now());
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
            if let Some(started) = job.started.take() {
                job.snapshot.elapsed += started.elapsed();
            }
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
        let Some((request, mut control, target_root, planned, direction)) =
            self.with_job(id, |job| {
                (
                    job.request.clone(),
                    job.control.subscribe(),
                    job.snapshot.target_path.clone(),
                    job.plan.is_some(),
                    job.direction,
                )
            })
        else {
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
                if let Some(point) = job.restore.take() {
                    let reached = point.next_item.min(plan.len());
                    let matches = reached > 0
                        && point.checkpoint.as_deref() == plan.get(reached - 1).map(Item::target);
                    if matches {
                        let done = plan[..reached].iter().filter_map(|item| match item {
                            Item::File { size, .. } => Some(*size),
                            Item::Directory { .. } => None,
                        });
                        job.next_item = reached;
                        job.snapshot.files_done = done.clone().count();
                        job.completed_bytes = done.sum();
                        job.snapshot.transferred = job.completed_bytes;
                    }
                }
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
                        .transfer_file(id, &request, &file, direction, &mut control)
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
        direction: TransferDirection,
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
        if !request.target.can_append() {
            offset = 0;
        }
        self.change(id, |job| {
            job.partial.insert(target.to_string(), path.clone());
            if !job.snapshot.is_directory {
                job.snapshot.target_path.clone_from(&path);
            }
        });
        let base = self.with_job(id, |job| job.completed_bytes).unwrap_or(0);
        let limiters = self.limiters(direction);
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
                let stream = Stream {
                    source,
                    target: &path,
                    offset,
                    limiters: &limiters,
                };
                copy_stream(request, &stream, control, &mut report).await
            };
            match result {
                Ok(Outcome::Completed) => {
                    let options = self
                        .with_job(id, |job| job.options.clone())
                        .unwrap_or_default();
                    if let Some(time) = modified
                        .or(options.modified)
                        .filter(|_| options.keep_modified)
                    {
                        let _ = request.target.set_modified(&path, time).await;
                    }
                    if self.state().checksum != ChecksumMode::Off {
                        self.verify(id, request, file, &path).await?;
                    }
                    return Ok(Some(Outcome::Completed));
                }
                Ok(outcome) => return Ok(Some(outcome)),
                Err(error) if is_retryable(&error) && attempt < self.state().retry.attempts => {
                    attempt += 1;
                    let delay = self.state().retry.delay_before(attempt);
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
                    if !request.target.can_append() {
                        offset = 0;
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn verify(
        &self,
        id: JobId,
        request: &TransferRequest,
        file: &FileItem<'_>,
        path: &str,
    ) -> Result<()> {
        self.change(id, |job| {
            job.snapshot.status = JobStatus::Verifying;
            job.snapshot.bytes_per_second = 0.0;
        });
        let verification = verify_copy(
            request.source.as_ref(),
            file.source,
            request.target.as_ref(),
            path,
            file.size,
        )
        .await?;
        self.change(id, |job| {
            job.snapshot.checksum = Some(
                job.snapshot
                    .checksum
                    .map_or(verification, |previous| previous.combine(verification)),
            );
            if verification.is_mismatch() {
                job.fresh.insert(file.target.to_string());
            } else {
                job.snapshot.status = JobStatus::Running;
            }
        });
        emit(
            &self.state(),
            TransferEvent::Verified {
                job: id,
                path: path.to_string(),
                verification,
            },
        );
        if verification.is_mismatch() {
            return Err(Error::new(ErrorKind::ChecksumMismatch, path));
        }
        Ok(())
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
            if self
                .with_job(id, |job| job.fresh.remove(target))
                .unwrap_or(false)
            {
                return Ok(Resolution::Copy { path, offset: 0 });
            }
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

    fn limiters(&self, direction: TransferDirection) -> Vec<&RateLimiter> {
        let mut limiters = Vec::new();
        if direction.downloads() {
            limiters.push(&self.inner.download_limit);
        }
        if direction.uploads() {
            limiters.push(&self.inner.upload_limit);
        }
        limiters
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

impl Item {
    fn target(&self) -> &str {
        match self {
            Self::Directory { target } | Self::File { target, .. } => target,
        }
    }
}

fn sorted(values: &HashSet<String>) -> Vec<String> {
    let mut values: Vec<String> = values.iter().cloned().collect();
    values.sort();
    values
}

fn sorted_pairs(values: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut values: Vec<(String, String)> = values
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    values.sort();
    values
}

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
    stream: &Stream<'_>,
    control: &mut watch::Receiver<Control>,
    report: &mut impl FnMut(u64),
) -> Result<Outcome> {
    let Stream {
        source,
        target,
        offset,
        limiters,
    } = *stream;
    let mut reader = request.source.open_read(source, offset).await?;
    let mut writer = request.target.open_write(target, offset).await?;
    let mut buffer = vec![0; BUFFER_SIZE];
    let mut position = offset;
    report(position);
    loop {
        let chunk = limiters
            .iter()
            .fold(BUFFER_SIZE, |size, limiter| limiter.chunk_size(size));
        let read = tokio::select! {
            biased;
            signal = wait_for_stop(control) => {
                let _ = writer.shutdown().await;
                return Ok(Outcome::Interrupted(signal));
            }
            read = reader.read(&mut buffer[..chunk]) => read.map_err(|error| Error::from_io(&error, source))?,
        };
        if read == 0 {
            break;
        }
        for limiter in limiters {
            tokio::select! {
                biased;
                signal = wait_for_stop(control) => {
                    let _ = writer.shutdown().await;
                    return Ok(Outcome::Interrupted(signal));
                }
                () = limiter.acquire(read as u64) => {}
            }
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

    type Pair = (Arc<dyn FileSystem>, Arc<dyn FileSystem>);

    #[derive(Debug, Default)]
    struct TestFileSystem {
        delay: Option<Duration>,
        failures: AtomicU32,
        fail_after: u64,
        remote: bool,
        hashes: bool,
        wrong_hashes: Mutex<u32>,
        write_offsets: Mutex<Vec<u64>>,
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
        fn is_remote(&self) -> bool {
            self.remote
        }

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
            if let Ok(mut offsets) = self.write_offsets.lock() {
                offsets.push(offset);
            }
            LocalFileSystem.open_write(path, offset)
        }

        fn checksum_algorithms(&self) -> BoxFuture<'_, Vec<crate::HashAlgorithm>> {
            let algorithms = if self.hashes {
                vec![crate::HashAlgorithm::Sha256]
            } else {
                Vec::new()
            };
            Box::pin(async move { algorithms })
        }

        fn checksum(
            &self,
            path: &str,
            algorithm: crate::HashAlgorithm,
        ) -> BoxFuture<'_, Result<Vec<u8>>> {
            let path = path.to_string();
            Box::pin(async move {
                let wrong = self.wrong_hashes.lock().is_ok_and(|mut left| {
                    let wrong = *left > 0;
                    *left = left.saturating_sub(1);
                    wrong
                });
                if wrong {
                    return Ok(vec![0; 32]);
                }
                crate::hash_file(&LocalFileSystem, &path, algorithm).await
            })
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
        queue.set_retry_policy(RetryPolicy {
            delay: Duration::from_millis(10),
            ..RetryPolicy::default()
        });
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
            failures: AtomicU32::new(DEFAULT_RETRIES + 1),
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

    #[derive(Default)]
    struct Peaks {
        statuses: HashMap<JobId, JobStatus>,
        total: usize,
        downloads: usize,
        uploads: usize,
    }

    impl Peaks {
        fn record(
            &mut self,
            snapshot: JobSnapshot,
            directions: &HashMap<JobId, TransferDirection>,
        ) {
            self.statuses.insert(snapshot.id, snapshot.status);
            let active: Vec<TransferDirection> = self
                .statuses
                .iter()
                .filter(|(_, status)| status.is_active())
                .filter_map(|(id, _)| directions.get(id).copied())
                .collect();
            self.total = self.total.max(active.len());
            self.downloads = self
                .downloads
                .max(active.iter().filter(|d| d.downloads()).count());
            self.uploads = self
                .uploads
                .max(active.iter().filter(|d| d.uploads()).count());
        }

        fn all_finished(&self, count: usize) -> bool {
            self.statuses.len() == count && self.statuses.values().all(JobStatus::is_finished)
        }
    }

    fn slow(remote: bool, delay: u64) -> Arc<dyn FileSystem> {
        Arc::new(TestFileSystem {
            delay: Some(Duration::from_millis(delay)),
            remote,
            ..TestFileSystem::default()
        })
    }

    fn remote() -> Arc<dyn FileSystem> {
        Arc::new(TestFileSystem {
            remote: true,
            ..TestFileSystem::default()
        })
    }

    fn add_jobs(
        queue: &TransferQueue,
        directory: &Path,
        jobs: &[Pair],
        size: usize,
    ) -> HashMap<JobId, TransferDirection> {
        let mut directions = HashMap::new();
        for (index, (source, target)) in jobs.iter().enumerate() {
            let file = directory.join(format!("file{index}.bin"));
            std::fs::write(&file, content(size)).expect("writing failed");
            let folder = directory.join(format!("target{index}"));
            std::fs::create_dir_all(&folder).expect("creating failed");
            let id = queue.add(request(
                Arc::clone(source),
                &file,
                Arc::clone(target),
                &folder,
            ));
            directions.insert(id, TransferDirection::of(source.as_ref(), target.as_ref()));
        }
        directions
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn direction_limits_cap_running_jobs() {
        let directory = temporary_directory("directions");
        let queue = TransferQueue::new(3);
        queue.set_direction_limits(Some(1), Some(2));
        assert_eq!(queue.parallelism().download_limit(), 1);
        assert_eq!(queue.parallelism().upload_limit(), 2);
        let mut events = start(&queue);
        let jobs: Vec<_> = (0..6)
            .map(|index| {
                if index % 2 == 0 {
                    (slow(true, 1), local())
                } else {
                    (slow(false, 1), remote())
                }
            })
            .collect();
        let directions = add_jobs(&queue, &directory, &jobs, 40 * 1024);
        let mut peaks = Peaks::default();
        while !peaks.all_finished(jobs.len()) {
            if let TransferEvent::Updated(snapshot) = wait_until(&mut events, |_| true).await {
                peaks.record(snapshot, &directions);
            }
        }
        assert_eq!((peaks.downloads, peaks.uploads, peaks.total), (1, 2, 3));
        assert!(
            peaks
                .statuses
                .values()
                .all(|status| *status == JobStatus::Completed)
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn site_limits_cap_running_jobs_per_site() {
        let directory = temporary_directory("site-limits");
        let queue = TransferQueue::new(6);
        let mut events = start(&queue);
        let alpha = SiteLimit::new("alpha", 1);
        let beta = SiteLimit::new("beta", 2);
        let plans: Vec<(Pair, Vec<SiteLimit>)> = vec![
            ((slow(true, 1), local()), vec![alpha.clone()]),
            ((slow(true, 1), local()), vec![alpha.clone()]),
            ((slow(true, 1), local()), vec![beta.clone()]),
            ((slow(true, 1), local()), vec![beta.clone()]),
            ((slow(true, 1), local()), vec![beta.clone()]),
            ((slow(true, 1), remote()), vec![alpha.clone(), beta.clone()]),
            ((slow(true, 1), local()), Vec::new()),
        ];
        let mut sites: HashMap<JobId, Vec<String>> = HashMap::new();
        for (index, ((source, target), limits)) in plans.iter().enumerate() {
            let file = directory.join(format!("file{index}.bin"));
            std::fs::write(&file, content(40 * 1024)).expect("writing failed");
            let folder = directory.join(format!("target{index}"));
            std::fs::create_dir_all(&folder).expect("creating failed");
            let id = queue.add_with(
                request(Arc::clone(source), &file, Arc::clone(target), &folder),
                JobOptions {
                    site_limits: limits.clone(),
                    ..JobOptions::default()
                },
            );
            sites.insert(id, limits.iter().map(|limit| limit.site.clone()).collect());
        }
        let mut statuses: HashMap<JobId, JobStatus> = HashMap::new();
        let mut peaks: HashMap<String, usize> = HashMap::new();
        let mut total = 0;
        while statuses.len() < plans.len() || !statuses.values().all(JobStatus::is_finished) {
            if let TransferEvent::Updated(snapshot) = wait_until(&mut events, |_| true).await {
                statuses.insert(snapshot.id, snapshot.status);
                let active: Vec<JobId> = statuses
                    .iter()
                    .filter(|(_, status)| status.is_active())
                    .map(|(id, _)| *id)
                    .collect();
                total = total.max(active.len());
                for site in ["alpha", "beta"] {
                    let count = active
                        .iter()
                        .filter(|id| sites[*id].iter().any(|name| name == site))
                        .count();
                    let peak = peaks.entry(site.to_string()).or_default();
                    *peak = (*peak).max(count);
                }
            }
        }
        assert_eq!(peaks["alpha"], 1);
        assert_eq!(peaks["beta"], 2);
        assert!(total >= 3, "unlimited jobs should run next to limited ones");
        assert!(
            statuses
                .values()
                .all(|status| *status == JobStatus::Completed)
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn site_limits_are_at_least_one() {
        assert_eq!(SiteLimit::new("a", 0).max, 1);
        assert_eq!(SiteLimit::new("a", 3).max, 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn server_to_server_copies_count_as_download_and_upload() {
        assert_eq!(
            TransferDirection::of(remote().as_ref(), remote().as_ref()),
            TransferDirection::ServerToServer
        );
        assert_eq!(
            TransferDirection::of(local().as_ref(), local().as_ref()),
            TransferDirection::Local
        );
        let directory = temporary_directory("server-to-server");
        let queue = TransferQueue::new(4);
        queue.set_direction_limits(None, Some(1));
        let mut events = start(&queue);
        let jobs = [
            (slow(true, 1), remote()),
            (slow(false, 1), remote()),
            (slow(true, 1), local()),
        ];
        let directions = add_jobs(&queue, &directory, &jobs, 40 * 1024);
        let mut peaks = Peaks::default();
        while !peaks.all_finished(jobs.len()) {
            if let TransferEvent::Updated(snapshot) = wait_until(&mut events, |_| true).await {
                peaks.record(snapshot, &directions);
            }
        }
        assert_eq!(peaks.uploads, 1);
        assert_eq!(peaks.total, 2);
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn parallelism_changes_apply_live() {
        let directory = temporary_directory("live-parallel");
        let queue = TransferQueue::new(1);
        let mut events = start(&queue);
        let jobs: Vec<_> = (0..3).map(|_| (slow(true, 2), local())).collect();
        let directions = add_jobs(&queue, &directory, &jobs, 200 * 1024);
        let mut peaks = Peaks::default();
        let mut raised = false;
        while !peaks.all_finished(jobs.len()) {
            if let TransferEvent::Updated(snapshot) = wait_until(&mut events, |_| true).await {
                let running = snapshot.status == JobStatus::Running;
                peaks.record(snapshot, &directions);
                if running && !raised {
                    assert_eq!(peaks.total, 1);
                    queue.set_parallelism(3);
                    raised = true;
                }
            }
        }
        assert_eq!(peaks.total, 3);
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Retries

    #[tokio::test(flavor = "multi_thread")]
    async fn retries_follow_the_policy() {
        let directory = temporary_directory("retry-policy");
        let source = directory.join("flaky.bin");
        std::fs::write(&source, content(20 * 1024)).expect("writing failed");
        let queue = TransferQueue::default();
        let mut events = start(&queue);
        let flaky = |failures: u32| -> Arc<dyn FileSystem> {
            Arc::new(TestFileSystem {
                failures: AtomicU32::new(failures),
                fail_after: 4 * 1024,
                ..TestFileSystem::default()
            })
        };

        for (attempts, failures, completes) in [(0, 1, false), (1, 2, false), (2, 2, true)] {
            queue.set_retry_policy(RetryPolicy {
                attempts,
                delay: Duration::from_millis(100),
            });
            let target = directory.join(format!("target-{attempts}-{failures}"));
            std::fs::create_dir_all(&target).expect("creating failed");
            let started = Instant::now();
            let id = queue.add(request(flaky(failures), &source, local(), &target));
            let mut retries = Vec::new();
            let done = loop {
                let snapshot = wait_for_status(&mut events, id, |status| {
                    status.is_finished() || matches!(status, JobStatus::Retrying(_))
                })
                .await;
                match snapshot.status {
                    JobStatus::Retrying(attempt) => {
                        if retries.last() != Some(&attempt) {
                            retries.push(attempt);
                        }
                    }
                    _ => break snapshot,
                }
            };
            let expected: Vec<u32> = (1..=attempts.min(failures)).collect();
            assert_eq!(retries, expected, "{attempts} attempts");
            assert_eq!(
                done.status == JobStatus::Completed,
                completes,
                "{attempts} attempts"
            );
            let waited: u64 = (1..=attempts.min(failures))
                .map(|attempt| 100 * 2u64.pow(attempt - 1))
                .sum();
            assert!(
                started.elapsed() >= Duration::from_millis(waited),
                "{attempts} attempts took {:?}",
                started.elapsed()
            );
        }
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn retry_delays_double() {
        let policy = RetryPolicy {
            attempts: 10,
            delay: Duration::from_secs(3),
        };
        let delays: Vec<u64> = (1..=4)
            .map(|attempt| policy.delay_before(attempt).as_secs())
            .collect();
        assert_eq!(delays, [3, 6, 12, 24]);
        assert_eq!(RetryPolicy::default().attempts, 3);
        assert_eq!(RetryPolicy::default().delay, Duration::from_secs(1));
    }

    // Speed limits

    async fn timed_copy(
        queue: &TransferQueue,
        events: &mut mpsc::UnboundedReceiver<TransferEvent>,
        directory: &Path,
        source: Arc<dyn FileSystem>,
        target: Arc<dyn FileSystem>,
        size: usize,
    ) -> Duration {
        let name = format!("limited-{}.bin", queue.snapshots().len());
        let file = directory.join(&name);
        std::fs::write(&file, content(size)).expect("writing failed");
        let folder = directory.join(format!("{name}-target"));
        std::fs::create_dir_all(&folder).expect("creating failed");
        let started = Instant::now();
        let id = queue.add(request(source, &file, target, &folder));
        let done = wait_for_status(events, id, JobStatus::is_finished).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(
            std::fs::read(folder.join(&name)).expect("reading failed"),
            content(size)
        );
        started.elapsed()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn speed_limits_hold_the_rate_per_direction() {
        let directory = temporary_directory("speed");
        let queue = TransferQueue::default();
        let mut events = start(&queue);
        queue.set_speed_limits(SpeedLimits {
            download: Some(256 * 1024),
            upload: Some(128 * 1024),
        });
        assert_eq!(queue.speed_limits().download, Some(256 * 1024));

        let size = 320 * 1024;
        let download = timed_copy(&queue, &mut events, &directory, remote(), local(), size).await;
        assert!(
            (Duration::from_millis(900)..Duration::from_secs(4)).contains(&download),
            "{download:?}"
        );
        let upload = timed_copy(&queue, &mut events, &directory, local(), remote(), size).await;
        assert!(
            (Duration::from_millis(2_000)..Duration::from_secs(6)).contains(&upload),
            "{upload:?}"
        );
        let copy = timed_copy(&queue, &mut events, &directory, remote(), remote(), size).await;
        assert!(copy >= Duration::from_millis(2_000), "{copy:?}");
        let local_copy = timed_copy(&queue, &mut events, &directory, local(), local(), size).await;
        assert!(local_copy < Duration::from_millis(900), "{local_copy:?}");

        queue.set_speed_limits(SpeedLimits::default());
        let unlimited = timed_copy(&queue, &mut events, &directory, remote(), local(), size).await;
        assert!(unlimited < Duration::from_millis(900), "{unlimited:?}");
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn speed_limits_are_shared_by_running_jobs() {
        let directory = temporary_directory("shared-speed");
        let queue = TransferQueue::new(4);
        queue.set_speed_limits(SpeedLimits {
            download: Some(512 * 1024),
            upload: None,
        });
        let mut events = start(&queue);
        let jobs: Vec<_> = (0..4).map(|_| (remote(), local())).collect();
        let started = Instant::now();
        let directions = add_jobs(&queue, &directory, &jobs, 256 * 1024);
        let mut peaks = Peaks::default();
        while !peaks.all_finished(jobs.len()) {
            if let TransferEvent::Updated(snapshot) = wait_until(&mut events, |_| true).await {
                peaks.record(snapshot, &directions);
            }
        }
        let elapsed = started.elapsed();
        assert!(
            (Duration::from_millis(1_600)..Duration::from_secs(6)).contains(&elapsed),
            "{elapsed:?}"
        );
        assert_eq!(peaks.downloads, 4);
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
            checksum: None,
            elapsed: Duration::ZERO,
        };
        assert_eq!(snapshot.remaining_seconds(), Some(10));
        assert!((snapshot.fraction() - 0.2).abs() < 0.001);
        let stalled = JobSnapshot {
            bytes_per_second: 0.0,
            ..snapshot
        };
        assert_eq!(stalled.remaining_seconds(), None);
    }

    // Verification

    #[tokio::test(flavor = "multi_thread")]
    async fn verification_hashes_local_copies_and_checks_sizes_otherwise() {
        let directory = temporary_directory("verify");
        let source = directory.join("data.bin");
        std::fs::write(&source, content(70_000)).expect("writing failed");
        let queue = TransferQueue::default();
        queue.set_checksum_mode(ChecksumMode::WhenSupported);
        let mut events = start(&queue);

        let target = directory.join("local");
        std::fs::create_dir_all(&target).expect("creating failed");
        let id = queue.add(request(local(), &source, local(), &target));
        let verified = wait_until(
            &mut events,
            |event| matches!(event, TransferEvent::Verified { job, .. } if *job == id),
        )
        .await;
        assert!(matches!(
            verified,
            TransferEvent::Verified {
                verification: Verification::Matched(crate::HashAlgorithm::Sha256),
                ..
            }
        ));
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(
            done.checksum,
            Some(Verification::Matched(crate::HashAlgorithm::Sha256))
        );

        let target = directory.join("remote");
        std::fs::create_dir_all(&target).expect("creating failed");
        let id = queue.add(request(local(), &source, remote(), &target));
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(done.checksum, Some(Verification::SizeChecked));

        queue.set_checksum_mode(ChecksumMode::Off);
        let target = directory.join("off");
        std::fs::create_dir_all(&target).expect("creating failed");
        let id = queue.add(request(local(), &source, local(), &target));
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(done.checksum, None);
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn checksum_mismatches_fail_and_retry_from_scratch() {
        let directory = temporary_directory("mismatch");
        let source = directory.join("data.bin");
        std::fs::write(&source, content(50_000)).expect("writing failed");
        let target = directory.join("target");
        std::fs::create_dir_all(&target).expect("creating failed");
        let server = Arc::new(TestFileSystem {
            remote: true,
            hashes: true,
            wrong_hashes: Mutex::new(1),
            ..TestFileSystem::default()
        });
        let queue = TransferQueue::default();
        queue.set_checksum_mode(ChecksumMode::WhenSupported);
        let mut events = start(&queue);
        let server_file_system: Arc<dyn FileSystem> = Arc::clone(&server) as Arc<dyn FileSystem>;
        let id = queue.add(request(local(), &source, server_file_system, &target));
        let failed = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        let JobStatus::Failed(error) = &failed.status else {
            panic!("the mismatch was not reported: {:?}", failed.status);
        };
        assert_eq!(error.kind(), ErrorKind::ChecksumMismatch);
        assert_eq!(
            failed.checksum,
            Some(Verification::Mismatch(Some(crate::HashAlgorithm::Sha256)))
        );

        queue.retry(id);
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(
            *server.write_offsets.lock().expect("poisoned"),
            [0, 0],
            "the retry did not start from scratch"
        );
        assert_eq!(
            std::fs::read(target.join("data.bin")).expect("reading failed"),
            content(50_000)
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Restoring

    #[tokio::test(flavor = "multi_thread")]
    async fn restored_jobs_stay_paused_and_resume_partial_files() {
        let directory = temporary_directory("restore");
        let source = directory.join("big.bin");
        std::fs::write(&source, content(100_000)).expect("writing failed");
        let target = directory.join("target");
        std::fs::create_dir_all(&target).expect("creating failed");
        let partial = target.join("big.bin");
        std::fs::write(&partial, &content(100_000)[..30_000]).expect("writing failed");
        let writer = Arc::new(TestFileSystem::default());

        let queue = TransferQueue::default();
        let mut events = start(&queue);
        let point = ResumePoint {
            partial: vec![(text(&partial), text(&partial))],
            ..ResumePoint::default()
        };
        let writer_file_system: Arc<dyn FileSystem> = Arc::clone(&writer) as Arc<dyn FileSystem>;
        let id = queue.add_paused(
            request(local(), &source, writer_file_system, &target),
            JobOptions::default(),
            point.clone(),
        );
        assert_eq!(queue.snapshots()[0].status, JobStatus::Paused);
        let saved = queue.job_states();
        assert_eq!(saved[0].resume, point);
        assert_eq!(saved[0].target_directory, text(&target));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(queue.snapshots()[0].status, JobStatus::Paused);

        assert!(queue.rebind(id, Some(local()), None));
        queue.resume(id);
        let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(*writer.write_offsets.lock().expect("poisoned"), [30_000]);
        assert_eq!(
            std::fs::read(&partial).expect("reading failed"),
            content(100_000)
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restored_folders_continue_after_the_checkpoint() {
        let directory = temporary_directory("restore-folder");
        let source = directory.join("src");
        std::fs::create_dir_all(&source).expect("creating failed");
        std::fs::write(source.join("a.txt"), b"new a").expect("writing failed");
        std::fs::write(source.join("b.txt"), b"new b").expect("writing failed");
        let queue = TransferQueue::default();
        let mut events = start(&queue);

        for (checkpoint, expected) in [("a.txt", &b"old a"[..]), ("gone.txt", b"new a")] {
            let target = directory.join(format!("target-{checkpoint}"));
            let copied = target.join("src");
            std::fs::create_dir_all(&copied).expect("creating failed");
            std::fs::write(copied.join("a.txt"), b"old a").expect("writing failed");
            let point = ResumePoint {
                next_item: 2,
                checkpoint: Some(text(&copied.join(checkpoint))),
                ..ResumePoint::default()
            };
            let id = queue.add_paused(
                request(local(), &source, local(), &target),
                JobOptions {
                    policy: Some(ConflictPolicy::Overwrite),
                    ..JobOptions::default()
                },
                point.clone(),
            );
            let states = queue.job_states();
            let state = states.iter().find(|state| state.id == id).expect("state");
            assert_eq!(state.resume, point);
            queue.resume(id);
            let done = wait_for_status(&mut events, id, JobStatus::is_finished).await;
            assert_eq!(done.status, JobStatus::Completed);
            assert_eq!(done.files_done, 2);
            assert_eq!(
                std::fs::read(copied.join("a.txt")).expect("reading failed"),
                expected,
                "{checkpoint}"
            );
            assert_eq!(
                std::fs::read(copied.join("b.txt")).expect("reading failed"),
                b"new b"
            );
        }
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn paused_folder_jobs_report_their_checkpoint() {
        let directory = temporary_directory("checkpoint");
        let source = directory.join("src");
        std::fs::create_dir_all(&source).expect("creating failed");
        for index in 0..4 {
            std::fs::write(source.join(format!("{index}.bin")), content(60 * 1024))
                .expect("writing failed");
        }
        let target = directory.join("target");
        std::fs::create_dir_all(&target).expect("creating failed");
        let queue = TransferQueue::default();
        let mut events = start(&queue);
        let id = queue.add(request(slow(false, 2), &source, local(), &target));
        wait_until(
            &mut events,
            |event| matches!(event, TransferEvent::Updated(snapshot) if snapshot.files_done >= 2),
        )
        .await;
        queue.pause(id);
        wait_for_status(&mut events, id, |status| *status == JobStatus::Paused).await;
        let state = queue.job_states().remove(0);
        assert!(state.resume.next_item >= 3, "{:?}", state.resume);
        let checkpoint = state.resume.checkpoint.expect("no checkpoint");
        assert!(checkpoint.starts_with(&text(&target.join("src"))));
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
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
