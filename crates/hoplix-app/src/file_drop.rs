use std::path::Path;
use std::sync::Arc;

use hoplix_core::{JobOptions, TransferRequest};
use iced::window;

use crate::app::{Hoplix, LoadState, Message, Pane, TabId};
use crate::transfers::{Direction, DropTarget, TransferTarget, same_file_system};

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDrop {
    pub target: DropTarget,
    hovered: usize,
    dropped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Unavailable,
    SameFolder,
    Unreadable,
}

// Targets

pub fn choose_target(pointer: Option<DropTarget>, panes: &[Pane], focused: usize) -> DropTarget {
    let valid = pointer.filter(|target| match target {
        DropTarget::Tab(id) => panes
            .iter()
            .any(|pane| pane.tabs.iter().any(|tab| tab.id == *id)),
        DropTarget::Pane(index) => *index < panes.len(),
    });
    valid.unwrap_or(DropTarget::Pane(focused.min(panes.len().saturating_sub(1))))
}

pub fn target_tab(target: DropTarget, panes: &[Pane]) -> Option<TabId> {
    match target {
        DropTarget::Tab(id) => Some(id),
        DropTarget::Pane(index) => panes
            .get(index)
            .and_then(Pane::active_tab)
            .map(|tab| tab.id),
    }
}

// Events

impl Hoplix {
    pub(crate) fn track_pointer(&mut self, message: &Message) {
        match message {
            Message::DragEntered(target) => self.pointer = Some(*target),
            Message::DragLeft(left, Some(fallback)) if self.pointer == Some(*left) => {
                self.pointer = Some(*fallback);
            }
            _ => {}
        }
    }

    pub fn external_target(&self) -> DropTarget {
        choose_target(self.pointer, &self.panes, self.focused_pane)
    }

    pub(crate) fn file_event(&mut self, event: &window::Event) {
        match event {
            window::Event::FileHovered(_) => {
                let target = self.external_target();
                let drop = self.file_drop.get_or_insert(FileDrop {
                    target,
                    hovered: 0,
                    dropped: 0,
                });
                if drop.dropped == 0 {
                    drop.hovered += 1;
                }
            }
            window::Event::FileDropped(path) => {
                let target = self.external_target();
                let drop = self.file_drop.get_or_insert(FileDrop {
                    target,
                    hovered: 0,
                    dropped: 0,
                });
                drop.dropped += 1;
                let target = drop.target;
                if drop.dropped >= drop.hovered {
                    self.file_drop = None;
                }
                let result = target_tab(target, &self.panes)
                    .ok_or(Refusal::Unavailable)
                    .and_then(|tab| self.queue_dropped(path, tab));
                self.notice = match result {
                    Ok(()) => None,
                    Err(refusal) => Some(self.strings.get(refusal.key()).to_string()),
                };
            }
            window::Event::FilesHoveredLeft => self.file_drop = None,
            _ => {}
        }
    }

    fn queue_dropped(&mut self, path: &Path, id: TabId) -> Result<(), Refusal> {
        let tab = self.tab_by_id(id).ok_or(Refusal::Unavailable)?;
        if tab.path.is_empty() || tab.state == LoadState::Disconnected {
            return Err(Refusal::Unavailable);
        }
        let source_path = path.to_str().ok_or(Refusal::Unreadable)?.to_string();
        let source = Arc::clone(&self.services.local);
        if same_file_system(&source, &tab.file_system)
            && source.parent(&source_path).as_deref() == Some(tab.path.as_str())
        {
            return Err(Refusal::SameFolder);
        }
        let direction = if tab.file_system.is_remote() {
            Direction::Upload
        } else {
            Direction::Copy
        };
        let site_limits = self.site_limits(&[tab]);
        let target = Arc::clone(&tab.file_system);
        let directory = tab.path.clone();
        let job = self.transfers.add_with(
            TransferRequest {
                source,
                source_path,
                target: Arc::clone(&target),
                target_directory: directory.clone(),
                target_name: None,
            },
            JobOptions {
                site_limits: site_limits.clone(),
                ..JobOptions::default()
            },
        );
        self.transfer_targets.insert(
            job,
            TransferTarget {
                file_system: target,
                directory,
                direction,
                site_limits,
            },
        );
        self.transfer_panel_open = true;
        Ok(())
    }

    pub fn file_drop_target(&self) -> Option<DropTarget> {
        self.file_drop.map(|drop| drop.target)
    }
}

impl Refusal {
    fn key(&self) -> &'static str {
        match self {
            Self::Unavailable => "drop.unavailable",
            Self::SameFolder => "transfer.same_folder",
            Self::Unreadable => "drop.unreadable",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Layout, Settings};

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
        app
    }

    #[test]
    fn drops_go_to_the_last_hovered_place_or_the_focused_pane() {
        let mut app = app(Layout::Classic);
        let _ = app.open_local_tab(1, None);
        assert_eq!(app.external_target(), DropTarget::Pane(1));
        app.focused_pane = 0;
        assert_eq!(app.external_target(), DropTarget::Pane(0));

        app.track_pointer(&Message::DragEntered(DropTarget::Pane(1)));
        app.track_pointer(&Message::DragEntered(DropTarget::Tab(2)));
        assert_eq!(app.external_target(), DropTarget::Tab(2));
        app.track_pointer(&Message::DragLeft(
            DropTarget::Tab(2),
            Some(DropTarget::Pane(1)),
        ));
        app.track_pointer(&Message::DragLeft(DropTarget::Pane(1), None));
        assert_eq!(app.external_target(), DropTarget::Pane(1));
        assert_eq!(target_tab(DropTarget::Pane(1), &app.panes), Some(3));
        assert_eq!(target_tab(DropTarget::Tab(2), &app.panes), Some(2));

        app.pointer = Some(DropTarget::Tab(99));
        assert_eq!(app.external_target(), DropTarget::Pane(0));
        app.pointer = Some(DropTarget::Pane(5));
        assert_eq!(app.external_target(), DropTarget::Pane(0));
        assert_eq!(choose_target(None, &[], 3), DropTarget::Pane(0));
        assert_eq!(target_tab(DropTarget::Pane(0), &[]), None);
    }

    #[test]
    fn hovering_several_files_keeps_one_target_until_all_are_dropped() {
        let mut app = app(Layout::Classic);
        app.track_pointer(&Message::DragEntered(DropTarget::Pane(1)));
        for name in ["a", "b"] {
            app.file_event(&window::Event::FileHovered(name.into()));
        }
        assert_eq!(app.file_drop_target(), Some(DropTarget::Pane(1)));
        app.track_pointer(&Message::DragEntered(DropTarget::Pane(0)));
        app.file_event(&window::Event::FileDropped("a".into()));
        assert_eq!(app.file_drop_target(), Some(DropTarget::Pane(1)));
        app.file_event(&window::Event::FileDropped("b".into()));
        assert_eq!(app.file_drop_target(), None);

        app.file_event(&window::Event::FileHovered("c".into()));
        assert_eq!(app.file_drop_target(), Some(DropTarget::Pane(0)));
        app.file_event(&window::Event::FilesHoveredLeft);
        assert_eq!(app.file_drop_target(), None);
    }

    #[test]
    fn folders_that_are_not_ready_refuse_drops() {
        let mut app = app(Layout::Modern);
        app.file_event(&window::Event::FileDropped("a".into()));
        assert!(app.transfers.snapshots().is_empty());
        assert_eq!(app.notice.as_deref(), Some("drop.unavailable"));
    }
}
