use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hoplix_core::{
    CompareOptions, CompareStatus, ComparedEntry, Entry, Error, JobId, Pattern, SyncDirection,
    SyncEndpoint, SyncMode, SyncOptions, SyncPlan, build_plan, compare_entries, refine,
};
use iced::Task;

use crate::app::{Dialog, Hoplix, LoadState, Message, Tab, TabId};
use crate::transfers::same_file_system;
use crate::ui;

// Constants

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mark {
    OnlyHere,
    NewerHere,
    OlderHere,
    SizeDiffers,
    KindDiffers,
    ContentDiffers,
    Equal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub left: TabId,
    pub right: TabId,
    pub recursive: bool,
    pub only_differences: bool,
    pub loading: bool,
    pub error: Option<Error>,
    pub statuses: HashMap<String, CompareStatus>,
    pub changed_folders: HashSet<String>,
    request: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub only_left: usize,
    pub only_right: usize,
    pub newer_left: usize,
    pub newer_right: usize,
    pub different: usize,
    pub equal: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncDialog {
    pub left: TabId,
    pub right: TabId,
    pub left_title: String,
    pub right_title: String,
    pub direction: SyncDirection,
    pub mode: SyncMode,
    pub delete_extra: bool,
    pub recursive: bool,
    pub size_only: bool,
    pub skip_hidden: bool,
    pub exclude: String,
    pub confirm_deletions: bool,
    pub entries: Option<Vec<ComparedEntry>>,
    pub error: Option<Error>,
    recursive_entries: bool,
    request: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunningSync {
    tabs: [TabId; 2],
    jobs: Option<Vec<JobId>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompareState {
    pub comparison: Option<Comparison>,
    running: Option<RunningSync>,
}

#[derive(Debug, Clone)]
pub enum CompareMessage {
    Toggle,
    With(TabId),
    Stop,
    OnlyDifferences(bool),
    Recursive(bool),
    Compared(u64, Result<Vec<ComparedEntry>, Error>),
    OpenSync,
    Direction(SyncDirection),
    Mode(SyncMode),
    DeleteExtra(bool),
    SyncRecursive(bool),
    SizeOnly(bool),
    SkipHidden(bool),
    Exclude(String),
    ConfirmDeletions(bool),
    SyncCompared(u64, Result<Vec<ComparedEntry>, Error>),
    SyncStarted(Result<Vec<JobId>, Error>),
}

// Marks

impl Mark {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::OnlyHere => "+",
            Self::NewerHere => "»",
            Self::OlderHere => "«",
            Self::SizeDiffers => "≠",
            Self::KindDiffers => "!",
            Self::ContentDiffers => "•",
            Self::Equal => "",
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Self::OnlyHere => "compare.mark.only_here",
            Self::NewerHere => "compare.mark.newer",
            Self::OlderHere => "compare.mark.older",
            Self::SizeDiffers => "compare.mark.size",
            Self::KindDiffers => "compare.mark.kind",
            Self::ContentDiffers => "compare.mark.content",
            Self::Equal => "compare.mark.equal",
        }
    }
}

impl Comparison {
    fn new(left: TabId, right: TabId, recursive: bool, only_differences: bool) -> Self {
        Self {
            left,
            right,
            recursive,
            only_differences,
            loading: true,
            error: None,
            statuses: HashMap::new(),
            changed_folders: HashSet::new(),
            request: next_request(),
        }
    }

    pub fn involves(&self, tab: TabId) -> bool {
        self.left == tab || self.right == tab
    }

    pub fn mark(&self, tab: TabId, entry: &Entry) -> Option<Mark> {
        let status = *self.statuses.get(&entry.name)?;
        let status = if tab == self.left {
            status
        } else if tab == self.right {
            status.mirrored()
        } else {
            return None;
        };
        Some(match status {
            CompareStatus::OnlyLeft | CompareStatus::OnlyRight => Mark::OnlyHere,
            CompareStatus::NewerLeft => Mark::NewerHere,
            CompareStatus::NewerRight => Mark::OlderHere,
            CompareStatus::SizeDiffers => Mark::SizeDiffers,
            CompareStatus::KindDiffers => Mark::KindDiffers,
            CompareStatus::Equal if self.changed_folders.contains(&entry.name) => {
                Mark::ContentDiffers
            }
            CompareStatus::Equal => Mark::Equal,
        })
    }

    pub fn hides(&self, tab: TabId, entry: &Entry) -> bool {
        self.only_differences
            && self.mark(tab, entry) == Some(Mark::Equal)
            && (self.recursive || !entry.is_directory())
    }

    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for (name, status) in &self.statuses {
            match status {
                CompareStatus::OnlyLeft => counts.only_left += 1,
                CompareStatus::OnlyRight => counts.only_right += 1,
                CompareStatus::NewerLeft => counts.newer_left += 1,
                CompareStatus::NewerRight => counts.newer_right += 1,
                CompareStatus::SizeDiffers | CompareStatus::KindDiffers => counts.different += 1,
                CompareStatus::Equal if self.changed_folders.contains(name) => {
                    counts.different += 1;
                }
                CompareStatus::Equal => counts.equal += 1,
            }
        }
        counts
    }

    fn apply(&mut self, entries: &[ComparedEntry]) {
        self.statuses = entries
            .iter()
            .filter(|entry| !entry.path.contains('/'))
            .map(|entry| (entry.name.clone(), entry.status))
            .collect();
        self.changed_folders = entries
            .iter()
            .filter(|entry| entry.status.is_difference())
            .filter_map(|entry| entry.path.split_once('/'))
            .map(|(top, _)| top.to_string())
            .collect();
        self.loading = false;
        self.error = None;
    }
}

// Sync dialog

impl SyncDialog {
    pub fn compare_options(&self) -> CompareOptions {
        CompareOptions {
            recursive: self.recursive,
            size_only: self.size_only,
            skip_hidden: self.skip_hidden,
            exclude: Pattern::parse_list(&self.exclude),
            ..CompareOptions::default()
        }
    }

    pub fn can_delete(&self) -> bool {
        self.mode == SyncMode::Mirror && self.direction != SyncDirection::Both
    }

    pub fn options(&self) -> SyncOptions {
        SyncOptions {
            compare: self.compare_options(),
            direction: self.direction,
            mode: self.mode,
            delete_extra: self.delete_extra && self.can_delete(),
        }
    }

    pub fn plan(&self) -> Option<SyncPlan> {
        let options = self.options();
        self.entries
            .as_ref()
            .map(|entries| build_plan(&refine(entries, &options.compare), &options))
    }

    pub fn can_start(&self) -> bool {
        self.plan().is_some_and(|plan| {
            !plan.is_empty() && (!plan.has_deletions() || self.confirm_deletions)
        })
    }

    fn needs_listing(&self) -> bool {
        self.entries.is_none() || (self.recursive && !self.recursive_entries)
    }
}

// Messages

impl Hoplix {
    pub(crate) fn update_compare(&mut self, message: CompareMessage) -> Task<Message> {
        match message {
            CompareMessage::Toggle => {
                if self.compare.comparison.is_some() {
                    self.compare.comparison = None;
                    return Task::none();
                }
                return match self.compare_pair() {
                    Some((left, right)) => self.start_comparison(left, right),
                    None => {
                        self.notice = Some(self.strings.get("compare.choose").to_string());
                        Task::none()
                    }
                };
            }
            CompareMessage::With(other) => {
                let Some(focused) = self.focused_tab().map(|tab| tab.id) else {
                    return Task::none();
                };
                return self.start_comparison(focused, other);
            }
            CompareMessage::Stop => self.compare.comparison = None,
            CompareMessage::OnlyDifferences(value) => {
                if let Some(comparison) = &mut self.compare.comparison {
                    comparison.only_differences = value;
                }
            }
            CompareMessage::Recursive(value) => {
                if let Some(comparison) = &mut self.compare.comparison {
                    comparison.recursive = value;
                    return self.recompare();
                }
            }
            CompareMessage::Compared(request, result) => {
                if let Some(comparison) = &mut self.compare.comparison
                    && comparison.request == request
                {
                    match result {
                        Ok(entries) => comparison.apply(&entries),
                        Err(error) => {
                            comparison.loading = false;
                            comparison.error = Some(error);
                        }
                    }
                }
            }
            CompareMessage::OpenSync => return self.open_sync(),
            CompareMessage::SyncCompared(request, result) => {
                if let Some(dialog) = self.sync_dialog_mut()
                    && dialog.request == request
                {
                    match result {
                        Ok(entries) => {
                            dialog.recursive_entries = dialog.recursive;
                            dialog.entries = Some(entries);
                            dialog.error = None;
                        }
                        Err(error) => dialog.error = Some(error),
                    }
                }
            }
            CompareMessage::SyncStarted(result) => return self.sync_started(result),
            message => return self.edit_sync(message),
        }
        Task::none()
    }

    fn edit_sync(&mut self, message: CompareMessage) -> Task<Message> {
        let Some(dialog) = self.sync_dialog_mut() else {
            return Task::none();
        };
        dialog.confirm_deletions = false;
        match message {
            CompareMessage::Direction(direction) => dialog.direction = direction,
            CompareMessage::Mode(mode) => dialog.mode = mode,
            CompareMessage::DeleteExtra(value) => dialog.delete_extra = value,
            CompareMessage::SizeOnly(value) => dialog.size_only = value,
            CompareMessage::SkipHidden(value) => dialog.skip_hidden = value,
            CompareMessage::Exclude(text) => dialog.exclude = text,
            CompareMessage::ConfirmDeletions(value) => dialog.confirm_deletions = value,
            CompareMessage::SyncRecursive(value) => {
                dialog.recursive = value;
                return self.list_for_sync();
            }
            _ => {}
        }
        Task::none()
    }

    // Comparing

    pub(crate) fn compare_pair(&self) -> Option<(TabId, TabId)> {
        if let Some(comparison) = &self.compare.comparison {
            return Some((comparison.left, comparison.right));
        }
        if self.panes.len() > 1 {
            let left = self.panes[0].active_tab()?.id;
            let right = self.panes[1].active_tab()?.id;
            return Some((left, right));
        }
        let focused = self.focused_tab()?.id;
        match self.transfer_destinations(focused).as_slice() {
            [(other, _)] => Some((focused, *other)),
            _ => None,
        }
    }

    fn start_comparison(&mut self, left: TabId, right: TabId) -> Task<Message> {
        if left == right {
            return Task::none();
        }
        let (recursive, only_differences) = self
            .compare
            .comparison
            .as_ref()
            .map_or((false, false), |old| (old.recursive, old.only_differences));
        self.compare.comparison = Some(Comparison::new(left, right, recursive, only_differences));
        self.notice = None;
        self.recompare()
    }

    pub(crate) fn tab_listed(&mut self, id: TabId) -> Task<Message> {
        if self
            .compare
            .comparison
            .as_ref()
            .is_some_and(|comparison| comparison.involves(id))
        {
            return self.recompare();
        }
        Task::none()
    }

    fn recompare(&mut self) -> Task<Message> {
        let Some(comparison) = &self.compare.comparison else {
            return Task::none();
        };
        let (left_id, right_id, recursive) =
            (comparison.left, comparison.right, comparison.recursive);
        let (Some(left), Some(right)) = (self.tab_by_id(left_id), self.tab_by_id(right_id)) else {
            self.compare.comparison = None;
            return Task::none();
        };
        if !is_ready(left) || !is_ready(right) {
            return Task::none();
        }
        if !recursive {
            let entries = compare_entries(
                "",
                &left.entries,
                &right.entries,
                &CompareOptions::default(),
            );
            if let Some(comparison) = &mut self.compare.comparison {
                comparison.apply(&entries);
            }
            return Task::none();
        }
        let (left, right) = (endpoint(left), endpoint(right));
        let request = next_request();
        if let Some(comparison) = &mut self.compare.comparison {
            comparison.request = request;
            comparison.loading = true;
        }
        let options = CompareOptions {
            recursive: true,
            ..CompareOptions::default()
        };
        Task::perform(
            async move { hoplix_core::compare(&left, &right, &options).await },
            move |result| Message::Compare(CompareMessage::Compared(request, result)),
        )
    }

    // Synchronising

    fn open_sync(&mut self) -> Task<Message> {
        let Some((left, right)) = self.compare_pair() else {
            self.notice = Some(self.strings.get("compare.choose").to_string());
            return Task::none();
        };
        let (Some(left_tab), Some(right_tab)) = (self.tab_by_id(left), self.tab_by_id(right))
        else {
            return Task::none();
        };
        if !is_ready(left_tab) || !is_ready(right_tab) {
            return Task::none();
        }
        if same_file_system(&left_tab.file_system, &right_tab.file_system)
            && left_tab.path == right_tab.path
        {
            self.notice = Some(self.strings.get("transfer.same_folder").to_string());
            return Task::none();
        }
        let dialog = SyncDialog {
            left,
            right,
            left_title: ui::tab_title(left_tab),
            right_title: ui::tab_title(right_tab),
            direction: SyncDirection::LeftToRight,
            mode: SyncMode::Update,
            delete_extra: false,
            recursive: self
                .compare
                .comparison
                .as_ref()
                .is_some_and(|comparison| comparison.recursive),
            size_only: false,
            skip_hidden: false,
            exclude: String::new(),
            confirm_deletions: false,
            entries: None,
            error: None,
            recursive_entries: false,
            request: 0,
        };
        self.palette = None;
        self.dialog = Some(Dialog::Sync(Box::new(dialog)));
        self.list_for_sync()
    }

    fn list_for_sync(&mut self) -> Task<Message> {
        let Some(dialog) = self.sync_dialog() else {
            return Task::none();
        };
        if !dialog.needs_listing() {
            return Task::none();
        }
        let recursive = dialog.recursive;
        let (Some(left), Some(right)) = (self.tab_by_id(dialog.left), self.tab_by_id(dialog.right))
        else {
            return Task::none();
        };
        let (left, right) = (endpoint(left), endpoint(right));
        let request = next_request();
        if let Some(dialog) = self.sync_dialog_mut() {
            dialog.request = request;
            dialog.entries = None;
            dialog.error = None;
        }
        let options = CompareOptions {
            recursive,
            ..CompareOptions::default()
        };
        Task::perform(
            async move { hoplix_core::compare(&left, &right, &options).await },
            move |result| Message::Compare(CompareMessage::SyncCompared(request, result)),
        )
    }

    pub(crate) fn start_sync(&mut self) -> Task<Message> {
        let Some(dialog) = self.sync_dialog().cloned() else {
            return Task::none();
        };
        let Some(plan) = dialog.plan().filter(|_| dialog.can_start()) else {
            return Task::none();
        };
        let (Some(left), Some(right)) = (self.tab_by_id(dialog.left), self.tab_by_id(dialog.right))
        else {
            self.dialog = None;
            return Task::none();
        };
        let (left, right) = (endpoint(left), endpoint(right));
        self.dialog = None;
        self.compare.running = Some(RunningSync {
            tabs: [dialog.left, dialog.right],
            jobs: None,
        });
        self.transfer_panel_open = true;
        self.notice = Some(self.strings.get("sync.running").to_string());
        let queue = self.transfers.clone();
        Task::perform(
            async move { hoplix_core::execute_plan(&plan, &left, &right, &queue).await },
            |result| Message::Compare(CompareMessage::SyncStarted(result)),
        )
    }

    fn sync_started(&mut self, result: Result<Vec<JobId>, Error>) -> Task<Message> {
        match result {
            Ok(jobs) => {
                if let Some(running) = &mut self.compare.running {
                    running.jobs = Some(jobs);
                }
                self.sync_progressed()
            }
            Err(error) => {
                self.notice = Some(self.strings.format(
                    "sync.failed",
                    &[("detail", &ui::error_text(&self.strings, &error))],
                ));
                let tabs = self.compare.running.take().map(|running| running.tabs);
                self.refresh_tabs(tabs.unwrap_or_default())
            }
        }
    }

    pub(crate) fn sync_progressed(&mut self) -> Task<Message> {
        let Some(running) = &self.compare.running else {
            return Task::none();
        };
        let Some(jobs) = &running.jobs else {
            return Task::none();
        };
        let finished = jobs.iter().all(|id| {
            self.transfer_jobs
                .iter()
                .any(|job| job.id == *id && job.status.is_finished())
        });
        if !finished {
            return Task::none();
        }
        let failed = jobs.iter().any(|id| {
            self.transfer_jobs
                .iter()
                .any(|job| job.id == *id && job.status != hoplix_core::JobStatus::Completed)
        });
        let tabs = running.tabs;
        self.compare.running = None;
        self.notice = Some(
            self.strings
                .get(if failed {
                    "sync.done_failed"
                } else {
                    "sync.done"
                })
                .to_string(),
        );
        self.refresh_tabs(tabs)
    }

    fn refresh_tabs(&mut self, tabs: [TabId; 2]) -> Task<Message> {
        Task::batch(tabs.into_iter().map(|tab| self.refresh(tab)))
    }

    // Lookup

    pub fn sync_dialog(&self) -> Option<&SyncDialog> {
        match &self.dialog {
            Some(Dialog::Sync(dialog)) => Some(dialog),
            _ => None,
        }
    }

    fn sync_dialog_mut(&mut self) -> Option<&mut SyncDialog> {
        match &mut self.dialog {
            Some(Dialog::Sync(dialog)) => Some(dialog),
            _ => None,
        }
    }

    pub(crate) fn tab_by_id(&self, id: TabId) -> Option<&Tab> {
        self.panes
            .iter()
            .flat_map(|pane| pane.tabs.iter())
            .find(|tab| tab.id == id)
    }

    pub fn visible_indices(&self, tab: &Tab) -> Option<Vec<usize>> {
        let comparison = self
            .compare
            .comparison
            .as_ref()
            .filter(|comparison| comparison.only_differences && comparison.involves(tab.id))?;
        Some(
            tab.entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| !comparison.hides(tab.id, entry))
                .map(|(index, _)| index)
                .collect(),
        )
    }
}

// Helpers

fn next_request() -> u64 {
    NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
}

fn is_ready(tab: &Tab) -> bool {
    !tab.path.is_empty() && tab.state == LoadState::Ready
}

fn endpoint(tab: &Tab) -> SyncEndpoint {
    SyncEndpoint::new(Arc::clone(&tab.file_system), tab.path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hoplix_core::EntryKind;

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.to_string(),
            kind,
            size: Some(1),
            modified: None,
            is_link: false,
        }
    }

    fn compared(path: &str, status: CompareStatus) -> ComparedEntry {
        ComparedEntry {
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            left: None,
            right: None,
            status,
        }
    }

    #[test]
    fn marks_follow_the_side_and_folders_collect_changes() {
        let mut comparison = Comparison::new(1, 2, true, true);
        comparison.apply(&[
            compared("docs", CompareStatus::Equal),
            compared("docs/a.txt", CompareStatus::NewerRight),
            compared("same", CompareStatus::Equal),
            compared("new.txt", CompareStatus::OnlyLeft),
            compared("old.txt", CompareStatus::NewerLeft),
        ]);
        let docs = entry("docs", EntryKind::Directory);
        let old = entry("old.txt", EntryKind::File);
        assert_eq!(comparison.mark(1, &docs), Some(Mark::ContentDiffers));
        assert_eq!(comparison.mark(1, &old), Some(Mark::NewerHere));
        assert_eq!(comparison.mark(2, &old), Some(Mark::OlderHere));
        assert_eq!(
            comparison.mark(1, &entry("new.txt", EntryKind::File)),
            Some(Mark::OnlyHere)
        );
        assert_eq!(comparison.mark(3, &old), None);
        let same = entry("same", EntryKind::Directory);
        assert!(comparison.hides(1, &same));
        comparison.recursive = false;
        assert!(!comparison.hides(1, &same));
        let counts = comparison.counts();
        assert_eq!(
            (
                counts.only_left,
                counts.newer_left,
                counts.different,
                counts.equal
            ),
            (1, 1, 1, 1)
        );
    }
}
