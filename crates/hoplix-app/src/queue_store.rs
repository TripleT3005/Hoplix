use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hoplix_core::{
    ExportLabels, FileSystem, History, HistoryEntry, HistoryResult, JobId, JobOptions, JobSnapshot,
    JobStatus, Location, RecentConnection, SavedJob, SavedQueue, SiteLimit, TransferDirection,
    TransferRequest, Verification, to_csv, to_plain_text,
};
use iced::{Subscription, Task};

use crate::app::{Hoplix, Message, Page, Tab, TabId};
use crate::i18n::Strings;
use crate::session::{Offline, TabOrigin};
use crate::site_exchange::{clean_path, documents_folder};
use crate::transfers::{Direction, TransferTarget};
use crate::{local_time, ui};

// Constants

const SAVE_INTERVAL: Duration = Duration::from_secs(1);
pub const HISTORY_EXPORT_NAME: &str = "hoplix-history.csv";

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelTab {
    #[default]
    Queue,
    History,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub source: Location,
    pub target: Location,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waiting {
    pub source: bool,
    pub target: bool,
}

#[derive(Debug, Default)]
pub struct QueueStore {
    pub(crate) endpoints: HashMap<JobId, Endpoints>,
    pub(crate) waiting: HashMap<JobId, Waiting>,
    pub(crate) connections: HashMap<TabId, RecentConnection>,
    pub(crate) resume_after: Option<Location>,
    saved: Option<String>,
    history_dirty: bool,
    pending: bool,
    last_save: Option<Instant>,
}

#[derive(Debug)]
pub struct HistoryView {
    pub history: History,
    pub filter: Option<HistoryResult>,
    pub tab: PanelTab,
    pub export_path: String,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryMessage {
    ShowTab(PanelTab),
    Filter(Option<HistoryResult>),
    Clear,
    Copy,
    ExportPath(String),
    ExportCsv,
    ExportText,
    Flush,
}

impl Default for HistoryView {
    fn default() -> Self {
        Self {
            history: History::default(),
            filter: None,
            tab: PanelTab::default(),
            export_path: documents_folder()
                .join(HISTORY_EXPORT_NAME)
                .to_string_lossy()
                .into_owned(),
            notice: None,
        }
    }
}

impl Waiting {
    fn any(self) -> bool {
        self.source || self.target
    }
}

// Locations

impl Hoplix {
    pub(crate) fn location_of(&self, tab: &Tab) -> Option<Location> {
        match &tab.origin {
            TabOrigin::Local => Some(Location::Local),
            TabOrigin::Site(id) => Some(Location::Site(id.clone())),
            TabOrigin::Direct => self
                .store
                .connections
                .get(&tab.id)
                .cloned()
                .map(|connection| Location::Direct(Box::new(connection))),
        }
    }

    pub(crate) fn direct_connection(&self) -> Option<RecentConnection> {
        if self.connect_form.site.is_some() {
            return None;
        }
        let params = self.connect_form.to_params().ok()?;
        Some(self.connect_form.recent_connection(&params))
    }

    pub(crate) fn remember_endpoints(&mut self, id: JobId, endpoints: Option<Endpoints>) {
        if let Some(endpoints) = endpoints {
            self.store.endpoints.insert(id, endpoints);
            self.persist(false);
        }
    }

    pub fn location_name(&self, location: &Location) -> String {
        match location {
            Location::Local => self.strings.get("transfer.local").to_string(),
            Location::Site(id) => self
                .sites
                .get(id)
                .map_or_else(|| id.clone(), |site| site.name.clone()),
            Location::Direct(connection) => connection.label(),
        }
    }

    pub fn waiting_location(&self, job: JobId) -> Option<&Location> {
        let waiting = self.store.waiting.get(&job)?;
        let endpoints = self.store.endpoints.get(&job)?;
        if waiting.source {
            Some(&endpoints.source)
        } else if waiting.target {
            Some(&endpoints.target)
        } else {
            None
        }
    }

    pub fn is_waiting(&self, job: JobId) -> bool {
        self.store.waiting.contains_key(&job)
    }

    // Restoring

    pub(crate) fn restore_queue(&mut self) {
        let Some(directory) = self.config_dir.clone() else {
            return;
        };
        if !self.settings.restore_transfers {
            return;
        }
        for job in SavedQueue::load(&directory).jobs {
            self.restore_job(job);
        }
        self.transfer_jobs = self.transfers.snapshots();
        if !self.transfer_jobs.is_empty() {
            self.transfer_panel_open = true;
        }
        self.store.saved = Some(self.saved_queue().to_text());
    }

    fn restore_job(&mut self, job: SavedJob) {
        let (Some(source), Some(target)) =
            (self.placeholder(&job.source), self.placeholder(&job.target))
        else {
            return;
        };
        let site_limits: Vec<SiteLimit> = [&job.source, &job.target]
            .into_iter()
            .filter_map(|location| match location {
                Location::Site(id) => self.sites.get(id),
                _ => None,
            })
            .filter_map(|site| site.max_transfers.map(|max| SiteLimit::new(&site.id, max)))
            .fold(Vec::new(), |mut limits, limit| {
                if !limits.contains(&limit) {
                    limits.push(limit);
                }
                limits
            });
        let direction = match (job.source.is_local(), job.target.is_local()) {
            (false, true) => Direction::Download,
            (true, false) => Direction::Upload,
            _ => Direction::Copy,
        };
        let id = self.transfers.add_paused(
            TransferRequest {
                source,
                source_path: job.source_path,
                target: Arc::clone(&target),
                target_directory: job.target_directory.clone(),
                target_name: job.target_name,
            },
            JobOptions {
                policy: job.policy,
                keep_modified: job.keep_modified,
                site_limits: site_limits.clone(),
                ..JobOptions::default()
            },
            job.resume,
        );
        self.transfer_targets.insert(
            id,
            TransferTarget {
                file_system: target,
                directory: job.target_directory,
                direction,
                site_limits,
            },
        );
        let waiting = Waiting {
            source: !job.source.is_local(),
            target: !job.target.is_local(),
        };
        if waiting.any() {
            self.store.waiting.insert(id, waiting);
        }
        self.store.endpoints.insert(
            id,
            Endpoints {
                source: job.source,
                target: job.target,
            },
        );
    }

    fn placeholder(&self, location: &Location) -> Option<Arc<dyn FileSystem>> {
        match location {
            Location::Local => Some(Arc::clone(&self.services.local)),
            Location::Site(id) => self.sites.get(id).map(|site| Offline::shared(&site.name)),
            Location::Direct(connection) => Some(Offline::shared(&connection.label())),
        }
    }

    // Connections

    pub(crate) fn bind_connection(
        &mut self,
        location: &Location,
        file_system: &Arc<dyn FileSystem>,
    ) {
        let resume = self.store.resume_after.as_ref() == Some(location);
        let mut ids: Vec<JobId> = self.store.waiting.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(endpoints) = self.store.endpoints.get(&id).cloned() else {
                continue;
            };
            let Some(mut waiting) = self.store.waiting.get(&id).copied() else {
                continue;
            };
            let source =
                (waiting.source && endpoints.source == *location).then(|| Arc::clone(file_system));
            let target =
                (waiting.target && endpoints.target == *location).then(|| Arc::clone(file_system));
            if source.is_none() && target.is_none() {
                continue;
            }
            let rebound_target = target.is_some();
            if !self.transfers.rebind(id, source.clone(), target) {
                continue;
            }
            waiting.source &= source.is_none();
            waiting.target &= !rebound_target;
            if rebound_target && let Some(target) = self.transfer_targets.get_mut(&id) {
                target.file_system = Arc::clone(file_system);
            }
            if waiting.any() {
                self.store.waiting.insert(id, waiting);
            } else {
                self.store.waiting.remove(&id);
                if resume {
                    self.transfers.resume(id);
                }
            }
        }
        if resume {
            self.store.resume_after = None;
        }
    }

    pub(crate) fn connect_waiting(&mut self, job: JobId) -> Task<Message> {
        let Some(location) = self.waiting_location(job).cloned() else {
            return Task::none();
        };
        self.store.resume_after = Some(location.clone());
        match location {
            Location::Site(id) => self.connect_site(&id),
            Location::Direct(connection) => {
                self.connect_form.fill_from_recent(&connection);
                self.page = Page::Connect;
                Task::none()
            }
            Location::Local => Task::none(),
        }
    }

    pub(crate) fn forget_job(&mut self, id: JobId) {
        self.store.endpoints.remove(&id);
        self.store.waiting.remove(&id);
    }

    // Saving

    fn saved_queue(&self) -> SavedQueue {
        if !self.settings.restore_transfers {
            return SavedQueue::default();
        }
        let jobs = self
            .transfers
            .job_states()
            .into_iter()
            .filter(|state| !matches!(state.status, JobStatus::Completed | JobStatus::Cancelled))
            .filter_map(|state| {
                let endpoints = self.store.endpoints.get(&state.id)?;
                Some(SavedJob {
                    source: endpoints.source.clone(),
                    source_path: state.source_path,
                    target: endpoints.target.clone(),
                    target_directory: state.target_directory,
                    target_name: state.target_name,
                    policy: state.policy,
                    keep_modified: state.keep_modified,
                    resume: state.resume,
                })
            })
            .collect();
        SavedQueue { jobs }
    }

    pub(crate) fn persist(&mut self, force: bool) {
        let Some(directory) = self.config_dir.clone() else {
            return;
        };
        if !force
            && self
                .store
                .last_save
                .is_some_and(|last| last.elapsed() < SAVE_INTERVAL)
        {
            self.store.pending = true;
            return;
        }
        self.store.pending = false;
        self.store.last_save = Some(Instant::now());
        let queue = self.saved_queue();
        let text = queue.to_text();
        if self.store.saved.as_deref() != Some(text.as_str()) {
            match queue.save(&directory) {
                Ok(()) => self.store.saved = Some(text),
                Err(error) => self.report_store_error(&error),
            }
        }
        if self.store.history_dirty {
            match self.history.history.save(&directory) {
                Ok(()) => self.store.history_dirty = false,
                Err(error) => self.report_store_error(&error),
            }
        }
    }

    fn report_store_error(&mut self, error: &std::io::Error) {
        self.notice = Some(
            self.strings
                .format("transfer.save_failed", &[("detail", &error.to_string())]),
        );
    }

    pub(crate) fn apply_store_settings(&mut self) {
        self.history.history.set_limit(self.settings.history_limit);
        self.store.history_dirty = true;
        self.persist(true);
    }

    // History

    pub(crate) fn load_history(&mut self) {
        self.history.history = match &self.config_dir {
            Some(directory) => History::load(directory, self.settings.history_limit),
            None => History::new(self.settings.history_limit),
        };
    }

    pub(crate) fn record_finished(&mut self, snapshot: &JobSnapshot) {
        if !self.settings.history_enabled {
            return;
        }
        let Some(request) = self.transfers.request(snapshot.id) else {
            return;
        };
        let (result, error) = match &snapshot.status {
            JobStatus::Completed
                if snapshot.skipped > 0 && snapshot.skipped == snapshot.files_done =>
            {
                (HistoryResult::Skipped, String::new())
            }
            JobStatus::Completed => (HistoryResult::Done, String::new()),
            JobStatus::Failed(error) => {
                (HistoryResult::Failed, ui::error_text(&self.strings, error))
            }
            JobStatus::Cancelled => (HistoryResult::Cancelled, String::new()),
            _ => return,
        };
        let describe = |file_system: &Arc<dyn FileSystem>, path: &str| match file_system.label() {
            Some(label) => format!("{label}:{path}"),
            None => path.to_string(),
        };
        let finished = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();
        self.history.history.push(HistoryEntry {
            finished,
            direction: TransferDirection::of(request.source.as_ref(), request.target.as_ref()),
            source: describe(&request.source, &request.source_path),
            target: describe(&request.target, &snapshot.target_path),
            size: snapshot.transferred,
            duration_ms: u64::try_from(snapshot.elapsed.as_millis()).unwrap_or(u64::MAX),
            result,
            error,
            checksum: snapshot.checksum,
        });
        self.store.history_dirty = true;
        self.persist(false);
    }

    pub(crate) fn update_history(&mut self, message: HistoryMessage) -> Task<Message> {
        match message {
            HistoryMessage::ShowTab(tab) => self.history.tab = tab,
            HistoryMessage::Filter(filter) => self.history.filter = filter,
            HistoryMessage::Clear => {
                self.history.history.clear();
                self.history.notice = None;
                self.store.history_dirty = true;
                self.persist(true);
            }
            HistoryMessage::Copy => {
                let text = self.history_export(false);
                self.history.notice = Some(self.strings.get("history.copied").to_string());
                return iced::clipboard::write(text);
            }
            HistoryMessage::ExportPath(path) => {
                self.history.export_path = path;
                self.history.notice = None;
            }
            HistoryMessage::ExportCsv => self.export_history(true),
            HistoryMessage::ExportText => self.export_history(false),
            HistoryMessage::Flush => self.persist(true),
        }
        Task::none()
    }

    pub fn visible_history(&self) -> Vec<&HistoryEntry> {
        self.history.history.filtered(self.history.filter)
    }

    pub(crate) fn history_export(&self, csv: bool) -> String {
        let strings = &self.strings;
        let result = |result: HistoryResult| result_text(strings, result);
        let direction = |direction: TransferDirection| direction_text(strings, direction);
        let checksum = |checksum: Option<Verification>| {
            checksum
                .map(|value| checksum_text(strings, value))
                .unwrap_or_default()
        };
        let time = |seconds: u64| time_text(strings, seconds);
        let labels = ExportLabels {
            header: [
                strings.get("history.column.time"),
                strings.get("history.column.direction"),
                strings.get("history.column.source"),
                strings.get("history.column.target"),
                strings.get("history.column.size"),
                strings.get("history.column.duration"),
                strings.get("history.column.speed"),
                strings.get("history.column.result"),
                strings.get("history.column.checksum"),
            ],
            result: &result,
            direction: &direction,
            checksum: &checksum,
            time: &time,
        };
        let entries = self.visible_history();
        if csv {
            format!("\u{feff}{}", to_csv(&entries, &labels))
        } else {
            to_plain_text(&entries, &labels)
        }
    }

    fn export_history(&mut self, csv: bool) {
        let path = PathBuf::from(clean_path(&self.history.export_path));
        if path.as_os_str().is_empty() {
            self.history.notice = Some(self.strings.get("history.export.no_path").to_string());
            return;
        }
        let text = self.history_export(csv);
        let count = self.visible_history().len();
        self.history.notice = Some(match std::fs::write(&path, text) {
            Ok(()) => self.strings.format(
                "history.export.done",
                &[
                    ("count", &count.to_string()),
                    ("path", &path.to_string_lossy()),
                ],
            ),
            Err(error) => self
                .strings
                .format("history.export.failed", &[("detail", &error.to_string())]),
        });
    }
}

// Texts

pub fn result_text(strings: &Strings, result: HistoryResult) -> String {
    strings
        .get(match result {
            HistoryResult::Done => "history.result.done",
            HistoryResult::Skipped => "history.result.skipped",
            HistoryResult::Failed => "history.result.failed",
            HistoryResult::Cancelled => "history.result.cancelled",
        })
        .to_string()
}

pub fn direction_text(strings: &Strings, direction: TransferDirection) -> String {
    strings
        .get(match direction {
            TransferDirection::Local => "history.direction.local",
            TransferDirection::Download => "history.direction.download",
            TransferDirection::Upload => "history.direction.upload",
            TransferDirection::ServerToServer => "history.direction.server",
        })
        .to_string()
}

pub fn checksum_text(strings: &Strings, verification: Verification) -> String {
    match verification {
        Verification::Matched(algorithm) => {
            strings.format("checksum.matched", &[("algorithm", algorithm.name())])
        }
        Verification::SizeChecked => strings.get("checksum.size").to_string(),
        Verification::Mismatch(Some(algorithm)) => {
            strings.format("checksum.mismatch", &[("algorithm", algorithm.name())])
        }
        Verification::Mismatch(None) => strings.get("checksum.mismatch_size").to_string(),
    }
}

pub fn time_text(strings: &Strings, seconds: u64) -> String {
    let time = UNIX_EPOCH + Duration::from_secs(seconds);
    local_time::local(time).map_or_else(String::new, |local| {
        format!(
            "{}:{:02}",
            local_time::format(&local, strings.get("date.format")),
            seconds % 60
        )
    })
}

// Subscriptions

pub fn subscription(app: &Hoplix) -> Subscription<Message> {
    if app.store.pending {
        iced::time::every(SAVE_INTERVAL).map(|_| Message::History(HistoryMessage::Flush))
    } else {
        Subscription::none()
    }
}
