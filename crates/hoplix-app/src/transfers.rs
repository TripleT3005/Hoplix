use std::sync::Arc;

use hoplix_core::{
    ConflictDecision, ConflictPolicy, FileSystem, JobId, JobOptions, JobStatus, SiteLimit,
    TransferEvent, TransferQueue, TransferRequest,
};
use iced::futures::SinkExt;
use iced::keyboard::Modifiers;
use iced::{Point, Subscription, Task, event, keyboard, mouse};

use crate::app::{Dialog, Hoplix, Message, Tab, TabId};
use crate::queue_store::Endpoints;
use crate::session::TabOrigin;

// Constants

const DRAG_THRESHOLD: f32 = 6.0;
const EVENT_BUFFER: usize = 256;

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropTarget {
    Pane(usize),
    Tab(TabId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drag {
    pub source: TabId,
    pub moved: bool,
    origin: Option<Point>,
    collapse: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferAction {
    Pause(JobId),
    Resume(JobId),
    Cancel(JobId),
    Retry(JobId),
    Remove(JobId),
    Connect(JobId),
    PauseAll,
    ResumeAll,
    CancelAll,
    ClearFinished,
    TogglePanel,
    PolicySelected(ConflictPolicy),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
    Copy,
}

#[derive(Debug, Clone)]
pub struct TransferTarget {
    pub file_system: Arc<dyn FileSystem>,
    pub directory: String,
    pub direction: Direction,
    pub site_limits: Vec<SiteLimit>,
}

impl Drag {
    pub fn new(source: TabId, collapse: Option<usize>) -> Self {
        Self {
            source,
            moved: false,
            origin: None,
            collapse,
        }
    }
}

// Selection

pub(crate) fn select(tab: &mut Tab, index: usize, modifiers: Modifiers) -> Option<usize> {
    if modifiers.shift() {
        let anchor = tab.anchor.unwrap_or(index);
        tab.selection = (anchor.min(index)..=anchor.max(index)).collect();
        tab.anchor = Some(anchor);
        return None;
    }
    if modifiers.command() {
        if let Some(position) = tab.selection.iter().position(|&item| item == index) {
            tab.selection.remove(position);
        } else {
            tab.selection.push(index);
            tab.selection.sort_unstable();
        }
        tab.anchor = Some(index);
        return None;
    }
    if tab.is_selected(index) && tab.selection.len() > 1 {
        return Some(index);
    }
    tab.selection = vec![index];
    tab.anchor = Some(index);
    None
}

// Messages

impl Hoplix {
    pub(crate) fn update_transfers(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TransferEvent(event) => return self.apply_transfer_event(event),
            Message::Transfer(action) => return self.run_transfer_action(action),
            Message::TransferTo { from, to } => return self.start_transfers(from, to),
            Message::ConflictAnswered(decision) => return self.answer_conflict(decision),
            Message::ConflictApplyToAll(value) => {
                if let Some(Dialog::Conflict { apply_to_all, .. }) = &mut self.dialog {
                    *apply_to_all = value;
                }
            }
            Message::ModifiersChanged(modifiers) => self.modifiers = modifiers,
            Message::DragEntered(target) => self.drop_target = Some(target),
            Message::DragLeft(left, fallback) => {
                if self.drop_target == Some(left) {
                    self.drop_target = fallback;
                }
            }
            Message::CursorMoved(position) => {
                if let Some(drag) = &mut self.drag {
                    match drag.origin {
                        None => drag.origin = Some(position),
                        Some(origin) if origin.distance(position) > DRAG_THRESHOLD => {
                            drag.moved = true;
                        }
                        Some(_) => {}
                    }
                }
            }
            Message::MouseReleased => return self.finish_drag(),
            _ => {}
        }
        Task::none()
    }

    fn apply_transfer_event(&mut self, event: TransferEvent) -> Task<Message> {
        match event {
            TransferEvent::Updated(snapshot) => {
                let id = snapshot.id;
                let status = snapshot.status.clone();
                let finished = status.is_finished();
                let was_finished = match self.transfer_jobs.iter_mut().find(|job| job.id == id) {
                    Some(job) => {
                        let was = job.status.is_finished();
                        *job = snapshot;
                        was
                    }
                    None => {
                        self.transfer_jobs.push(snapshot);
                        self.transfer_jobs.sort_by_key(|job| job.id);
                        false
                    }
                };
                if finished
                    && !was_finished
                    && let Some(snapshot) = self.transfer_jobs.iter().find(|job| job.id == id)
                {
                    let snapshot = snapshot.clone();
                    self.record_finished(&snapshot);
                }
                self.persist(false);
                if finished && !was_finished {
                    return Task::batch([
                        self.refresh_target(id),
                        self.finish_edit_transfer(id, &status),
                        self.sync_progressed(),
                    ]);
                }
            }
            TransferEvent::Removed(id) => {
                self.transfer_jobs.retain(|job| job.id != id);
                self.transfer_targets.remove(&id);
                self.forget_job(id);
                self.persist(false);
            }
            TransferEvent::Conflict(conflict) => {
                self.conflicts.push_back(conflict);
                self.show_next_conflict();
            }
            TransferEvent::Verified { .. } => {}
        }
        Task::none()
    }

    fn run_transfer_action(&mut self, action: TransferAction) -> Task<Message> {
        let queue = &self.transfers;
        match action {
            TransferAction::Pause(id) => queue.pause(id),
            TransferAction::Resume(id) if self.is_waiting(id) => return self.connect_waiting(id),
            TransferAction::Resume(id) => queue.resume(id),
            TransferAction::Connect(id) => return self.connect_waiting(id),
            TransferAction::Cancel(id) => queue.cancel(id),
            TransferAction::Retry(id) => queue.retry(id),
            TransferAction::Remove(id) => queue.remove(id),
            TransferAction::PauseAll => queue.pause_all(),
            TransferAction::ResumeAll => {
                for job in &self.transfer_jobs {
                    if !self.store.waiting.contains_key(&job.id) {
                        queue.resume(job.id);
                    }
                }
            }
            TransferAction::CancelAll => queue.cancel_all(),
            TransferAction::ClearFinished => queue.clear_finished(),
            TransferAction::TogglePanel => self.transfer_panel_open = !self.transfer_panel_open,
            TransferAction::PolicySelected(policy) => queue.set_policy(policy),
        }
        Task::none()
    }

    // Starting

    pub(crate) fn start_transfers(&mut self, from: TabId, to: TabId) -> Task<Message> {
        if from == to {
            return Task::none();
        }
        let Some(source) = self.find_tab(from) else {
            return Task::none();
        };
        let Some(target) = self.find_tab(to) else {
            return Task::none();
        };
        if source.path.is_empty() || target.path.is_empty() {
            return Task::none();
        }
        let names: Vec<String> = source
            .selected_entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        if names.is_empty() {
            return Task::none();
        }
        if same_file_system(&source.file_system, &target.file_system) && source.path == target.path
        {
            self.notice = Some(self.strings.get("transfer.same_folder").to_string());
            return Task::none();
        }
        let direction = match (
            source.file_system.label().is_some(),
            target.file_system.label().is_some(),
        ) {
            (true, false) => Direction::Download,
            (false, true) => Direction::Upload,
            _ => Direction::Copy,
        };
        let site_limits = self.site_limits(&[source, target]);
        let endpoints = self
            .location_of(source)
            .zip(self.location_of(target))
            .map(|(source, target)| Endpoints { source, target });
        let source_file_system = Arc::clone(&source.file_system);
        let source_path = source.path.clone();
        let target_file_system = Arc::clone(&target.file_system);
        let target_path = target.path.clone();
        for name in names {
            let request = TransferRequest {
                source: Arc::clone(&source_file_system),
                source_path: source_file_system.join(&source_path, &name),
                target: Arc::clone(&target_file_system),
                target_directory: target_path.clone(),
                target_name: None,
            };
            let id = self.transfers.add_with(
                request,
                JobOptions {
                    site_limits: site_limits.clone(),
                    ..JobOptions::default()
                },
            );
            self.remember_endpoints(id, endpoints.clone());
            self.transfer_targets.insert(
                id,
                TransferTarget {
                    file_system: Arc::clone(&target_file_system),
                    directory: target_path.clone(),
                    direction,
                    site_limits: site_limits.clone(),
                },
            );
        }
        self.notice = None;
        self.transfer_panel_open = true;
        Task::none()
    }

    pub(crate) fn site_limits(&self, tabs: &[&Tab]) -> Vec<SiteLimit> {
        let mut limits: Vec<SiteLimit> = Vec::new();
        for tab in tabs {
            let TabOrigin::Site(id) = &tab.origin else {
                continue;
            };
            if let Some(site) = self.sites.get(id)
                && let Some(max) = site.max_transfers
                && !limits.iter().any(|limit| limit.site == site.id)
            {
                limits.push(SiteLimit::new(&site.id, max));
            }
        }
        limits
    }

    pub fn held_by_site_limit(&self, job: JobId) -> Option<usize> {
        let limits = &self.transfer_targets.get(&job)?.site_limits;
        let position = self
            .transfer_jobs
            .iter()
            .position(|other| other.id == job)?;
        limits
            .iter()
            .find(|limit| {
                let ahead = self
                    .transfer_jobs
                    .iter()
                    .enumerate()
                    .filter(|(index, other)| {
                        other.status.is_active()
                            || (*index < position && other.status == JobStatus::Queued)
                    })
                    .filter(|(_, other)| other.id != job)
                    .filter_map(|(_, other)| self.transfer_targets.get(&other.id))
                    .filter(|other| other.site_limits.iter().any(|item| item.site == limit.site))
                    .count();
                ahead >= limit.max
            })
            .map(|limit| limit.max)
    }

    pub(crate) fn other_pane_tab(&self, id: TabId) -> Option<TabId> {
        let (pane, _) = self.locate(id)?;
        self.panes
            .iter()
            .enumerate()
            .find(|(index, _)| *index != pane)
            .and_then(|(_, other)| other.active_tab())
            .map(|tab| tab.id)
    }

    pub fn transfer_destinations(&self, id: TabId) -> Vec<(TabId, String)> {
        self.panes
            .iter()
            .flat_map(|pane| pane.tabs.iter())
            .filter(|tab| tab.id != id && !tab.path.is_empty())
            .map(|tab| (tab.id, crate::ui::tab_title(tab)))
            .collect()
    }

    // Dragging

    fn finish_drag(&mut self) -> Task<Message> {
        let Some(drag) = self.drag.take() else {
            return Task::none();
        };
        if !drag.moved {
            if let Some(index) = drag.collapse
                && let Some(tab) = self.tab_mut(drag.source)
            {
                tab.selection = vec![index];
                tab.anchor = Some(index);
            }
            return Task::none();
        }
        let target = match self.drop_target {
            Some(DropTarget::Tab(id)) => Some(id),
            Some(DropTarget::Pane(pane)) => self
                .panes
                .get(pane)
                .and_then(|pane| pane.active_tab())
                .map(|tab| tab.id),
            None => None,
        };
        match target {
            Some(target) if target != drag.source => self.start_transfers(drag.source, target),
            _ => Task::none(),
        }
    }

    // Conflicts

    pub(crate) fn answer_conflict(&mut self, decision: ConflictDecision) -> Task<Message> {
        let Some(Dialog::Conflict {
            conflict,
            apply_to_all,
        }) = self.dialog.take()
        else {
            return Task::none();
        };
        self.transfers.decide(conflict.job, decision, apply_to_all);
        if apply_to_all {
            for waiting in self.conflicts.drain(..) {
                self.transfers.decide(waiting.job, decision, false);
            }
        }
        self.show_next_conflict();
        Task::none()
    }

    pub(crate) fn show_next_conflict(&mut self) {
        if self.dialog.is_some() {
            return;
        }
        while let Some(conflict) = self.conflicts.pop_front() {
            let waiting = self
                .transfer_jobs
                .iter()
                .any(|job| job.id == conflict.job && job.status == JobStatus::WaitingForDecision);
            if waiting {
                self.dialog = Some(Dialog::Conflict {
                    conflict,
                    apply_to_all: false,
                });
                return;
            }
        }
    }

    // Refreshing

    fn refresh_target(&mut self, id: JobId) -> Task<Message> {
        let Some(target) = self.transfer_targets.get(&id).cloned() else {
            return Task::none();
        };
        let tabs: Vec<TabId> = self
            .panes
            .iter()
            .flat_map(|pane| pane.tabs.iter())
            .filter(|tab| {
                tab.path == target.directory
                    && same_file_system(&tab.file_system, &target.file_system)
            })
            .map(|tab| tab.id)
            .collect();
        Task::batch(tabs.into_iter().map(|tab| self.refresh(tab)))
    }

    fn find_tab(&self, id: TabId) -> Option<&Tab> {
        self.panes
            .iter()
            .flat_map(|pane| pane.tabs.iter())
            .find(|tab| tab.id == id)
    }
}

// Helpers

pub(crate) fn same_file_system(a: &Arc<dyn FileSystem>, b: &Arc<dyn FileSystem>) -> bool {
    (a.label().is_none() && b.label().is_none())
        || std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
}

// Subscriptions

pub fn subscription(app: &Hoplix) -> Subscription<Message> {
    let mut subscriptions = vec![
        Subscription::run_with(app.transfers.clone(), transfer_events),
        event::listen_with(input_event),
        iced::window::events().map(|(id, event)| Message::Window(id, event)),
    ];
    if app.drag.is_some() {
        subscriptions.push(event::listen_with(cursor_event));
    }
    Subscription::batch(subscriptions)
}

fn transfer_events(queue: &TransferQueue) -> impl iced::futures::Stream<Item = Message> + use<> {
    let queue = queue.clone();
    iced::stream::channel(EVENT_BUFFER, async move |mut output| {
        let mut events = queue.subscribe();
        let forward = async move {
            while let Some(event) = events.recv().await {
                if output.send(Message::TransferEvent(event)).await.is_err() {
                    break;
                }
            }
        };
        let driver = queue.drive();
        iced::futures::future::select(Box::pin(driver), Box::pin(forward)).await;
    })
}

pub(crate) fn input_event(
    event: iced::Event,
    status: event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
            Some(Message::ModifiersChanged(modifiers))
        }
        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
            Some(Message::MouseReleased)
        }
        iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
            Some(Message::KeyPressed {
                key,
                modifiers,
                captured: status == event::Status::Captured,
            })
        }
        _ => None,
    }
}

pub(crate) fn cursor_event(
    event: iced::Event,
    _status: event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
            Some(Message::CursorMoved(position))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Pane;
    use crate::settings::{Layout, Settings};
    use hoplix_core::{Conflict, Entry, EntryKind, LocalFileSystem};

    fn app(layout: Layout) -> Hoplix {
        let settings = Settings {
            layout,
            ..Settings::default()
        };
        let mut app = Hoplix::with_settings(settings, None);
        for _ in 0..layout.pane_count() {
            app.panes.push(Pane::default());
            let _ = app.open_local_tab(app.panes.len() - 1, None);
        }
        for (index, tab) in app
            .panes
            .iter_mut()
            .flat_map(|pane| pane.tabs.iter_mut())
            .enumerate()
        {
            tab.path = format!("/folder{index}");
            tab.entries = ["a", "b", "c", "d"]
                .iter()
                .map(|name| Entry {
                    name: (*name).to_string(),
                    kind: EntryKind::File,
                    size: Some(1),
                    modified: None,
                    is_link: false,
                })
                .collect();
        }
        app
    }

    fn press(app: &mut Hoplix, index: usize, modifiers: Modifiers) {
        app.modifiers = modifiers;
        let _ = app.update(Message::Tab(1, crate::app::TabMessage::Select(index)));
    }

    // Selection

    #[test]
    fn clicks_select_toggle_and_extend() {
        let mut app = app(Layout::Modern);
        press(&mut app, 1, Modifiers::default());
        assert_eq!(app.panes[0].tabs[0].selection, [1]);
        press(&mut app, 3, Modifiers::SHIFT);
        assert_eq!(app.panes[0].tabs[0].selection, [1, 2, 3]);
        press(&mut app, 2, Modifiers::CTRL);
        assert_eq!(app.panes[0].tabs[0].selection, [1, 3]);
        press(&mut app, 0, Modifiers::CTRL);
        assert_eq!(app.panes[0].tabs[0].selection, [0, 1, 3]);

        press(&mut app, 1, Modifiers::default());
        assert_eq!(app.panes[0].tabs[0].selection, [0, 1, 3]);
        let _ = app.update(Message::MouseReleased);
        assert_eq!(app.panes[0].tabs[0].selection, [1]);
        assert!(app.drag.is_none());
    }

    // Transfers

    #[test]
    fn dragging_onto_the_other_pane_queues_transfers() {
        let mut app = app(Layout::Classic);
        press(&mut app, 0, Modifiers::default());
        press(&mut app, 2, Modifiers::CTRL);
        let _ = app.update(Message::CursorMoved(Point::new(10.0, 10.0)));
        let _ = app.update(Message::CursorMoved(Point::new(80.0, 12.0)));
        let _ = app.update(Message::DragEntered(DropTarget::Pane(1)));
        let _ = app.update(Message::MouseReleased);

        let jobs = app.transfers.snapshots();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].name, "a");
        assert_eq!(jobs[1].name, "c");
        assert!(app.transfer_panel_open);
        let target = app
            .transfer_targets
            .get(&jobs[0].id)
            .expect("target missing");
        assert_eq!(target.directory, "/folder1");
        assert_eq!(target.direction, Direction::Copy);
    }

    #[test]
    fn short_clicks_and_same_folders_do_not_transfer() {
        let mut app = app(Layout::Classic);
        press(&mut app, 0, Modifiers::default());
        let _ = app.update(Message::CursorMoved(Point::new(10.0, 10.0)));
        let _ = app.update(Message::CursorMoved(Point::new(12.0, 11.0)));
        let _ = app.update(Message::DragEntered(DropTarget::Pane(1)));
        let _ = app.update(Message::MouseReleased);
        assert!(app.transfers.snapshots().is_empty());

        app.panes[1].tabs[0].path = "/folder0".to_string();
        let _ = app.update(Message::Tab(1, crate::app::TabMessage::CopyToOtherPane));
        assert!(app.transfers.snapshots().is_empty());
        assert!(app.notice.is_some());

        app.panes[1].tabs[0].path = "/elsewhere".to_string();
        let _ = app.update(Message::TransferTo { from: 1, to: 2 });
        assert_eq!(app.transfers.snapshots().len(), 1);
    }

    #[test]
    fn destinations_list_other_tabs() {
        let mut app = app(Layout::Modern);
        let _ = app.open_tab(
            0,
            Arc::new(LocalFileSystem),
            Some("Server".to_string()),
            None,
        );
        app.panes[0].tabs[1].path = "/srv".to_string();
        let destinations = app.transfer_destinations(1);
        assert_eq!(destinations, [(2, "Server: srv".to_string())]);
    }

    // Conflicts

    #[test]
    fn conflicts_show_one_after_another() {
        let mut app = app(Layout::Modern);
        for id in [1, 2] {
            app.transfer_jobs.push(hoplix_core::JobSnapshot {
                id,
                name: "x".to_string(),
                source_path: "x".to_string(),
                target_path: "y".to_string(),
                is_directory: false,
                status: JobStatus::WaitingForDecision,
                transferred: 0,
                total: Some(1),
                files_done: 0,
                files_total: Some(1),
                skipped: 0,
                bytes_per_second: 0.0,
                current_file: None,
                checksum: None,
                elapsed: std::time::Duration::ZERO,
            });
            let _ = app.update(Message::TransferEvent(TransferEvent::Conflict(Conflict {
                job: id,
                target_path: format!("/t/{id}"),
                source_size: 2,
                target_size: 1,
                can_resume: true,
            })));
        }
        let Some(Dialog::Conflict { conflict, .. }) = &app.dialog else {
            panic!("no conflict dialog");
        };
        assert_eq!(conflict.job, 1);
        let _ = app.update(Message::ConflictAnswered(ConflictDecision::Skip));
        let Some(Dialog::Conflict { conflict, .. }) = &app.dialog else {
            panic!("no second conflict dialog");
        };
        assert_eq!(conflict.job, 2);
        let _ = app.update(Message::ConflictApplyToAll(true));
        let _ = app.update(Message::ConflictAnswered(ConflictDecision::Rename));
        assert!(app.dialog.is_none());
        assert_eq!(app.transfers.policy(), ConflictPolicy::Rename);
    }

    #[test]
    fn finished_events_update_the_list() {
        let mut app = app(Layout::Modern);
        let snapshot = hoplix_core::JobSnapshot {
            id: 7,
            name: "x".to_string(),
            source_path: "x".to_string(),
            target_path: "/folder0/x".to_string(),
            is_directory: false,
            status: JobStatus::Running,
            transferred: 0,
            total: Some(1),
            files_done: 0,
            files_total: Some(1),
            skipped: 0,
            bytes_per_second: 0.0,
            current_file: None,
            checksum: None,
            elapsed: std::time::Duration::ZERO,
        };
        let _ = app.update(Message::TransferEvent(TransferEvent::Updated(
            snapshot.clone(),
        )));
        assert_eq!(app.transfer_jobs.len(), 1);
        let done = hoplix_core::JobSnapshot {
            status: JobStatus::Completed,
            ..snapshot
        };
        let _ = app.update(Message::TransferEvent(TransferEvent::Updated(done)));
        assert_eq!(app.transfer_jobs[0].status, JobStatus::Completed);
        let _ = app.update(Message::TransferEvent(TransferEvent::Removed(7)));
        assert!(app.transfer_jobs.is_empty());
    }
}
