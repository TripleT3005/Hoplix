use hoplix_core::{Bookmark, Bookmarks, Entry, NameFilter};
use iced::widget::operation::{self, RelativeOffset};
use iced::{Subscription, Task, event, mouse};

use crate::app::{Dialog, Hoplix, Message, Tab, TabId};
use crate::session::TabOrigin;
use crate::ui;

// Constants

pub const MAX_HISTORY: usize = 50;

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryStep {
    Back,
    Forward,
}

#[derive(Debug, Default)]
pub struct ListState {
    pub filter: String,
    name_filter: NameFilter,
    pub back: Vec<String>,
    pub forward: Vec<String>,
    pub(crate) step: Option<HistoryStep>,
    pub(crate) reveal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkScope {
    Local,
    Site(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkChoice {
    Open(String),
    Add,
    Edit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarksDialog {
    pub scope: BookmarkScope,
    pub items: Vec<Bookmark>,
}

#[derive(Debug, Clone)]
pub enum ListMessage {
    Filter(TabId, String),
    ClearFilter(TabId),
    Back(TabId),
    Forward(TabId),
    MouseButton(mouse::Button),
    Bookmark(TabId, BookmarkChoice),
    BookmarkRenamed(usize, String),
    BookmarkRemoved(usize),
    ColumnToggled(OptionalColumn, bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionalColumn {
    Permissions,
    Owner,
}

// Tabs

impl Tab {
    pub fn shows(&self, entry: &Entry) -> bool {
        self.list.name_filter.matches(&entry.name)
    }

    pub fn is_filtered(&self) -> bool {
        !self.list.name_filter.is_all()
    }

    pub fn visible_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| self.shows(entry))
            .count()
    }

    pub(crate) fn set_filter(&mut self, text: String) {
        self.list.name_filter = NameFilter::new(&text);
        self.list.filter = text;
        let entries = &self.entries;
        let filter = &self.list.name_filter;
        self.selection.retain(|index| {
            entries
                .get(*index)
                .is_some_and(|entry| filter.matches(&entry.name))
        });
        if self
            .cursor
            .is_some_and(|cursor| !self.selection.contains(&cursor))
        {
            self.cursor = self.selection.last().copied();
        }
        if self
            .anchor
            .is_some_and(|anchor| !self.selection.contains(&anchor))
        {
            self.anchor = self.selection.first().copied();
        }
    }

    pub(crate) fn arrive(&mut self) {
        let step = self.list.step.take();
        if self.path.is_empty() {
            return;
        }
        let previous = self.path.clone();
        let list = &mut self.list;
        match step {
            Some(HistoryStep::Back) => {
                list.back.pop();
                push_limited(&mut list.forward, previous);
            }
            Some(HistoryStep::Forward) => {
                list.forward.pop();
                push_limited(&mut list.back, previous);
            }
            None => {
                push_limited(&mut list.back, previous);
                list.forward.clear();
            }
        }
    }

    pub(crate) fn reveal_pending(&mut self) -> Option<(usize, usize)> {
        let name = self.list.reveal.take()?;
        let index = self.entries.iter().position(|entry| entry.name == name)?;
        self.selection = vec![index];
        self.anchor = Some(index);
        self.cursor = Some(index);
        Some((index, self.entries.len()))
    }

    pub(crate) fn forget_pending(&mut self) {
        self.list.step = None;
        self.list.reveal = None;
    }
}

// Messages

impl Hoplix {
    pub(crate) fn update_list(&mut self, message: ListMessage) -> Task<Message> {
        match message {
            ListMessage::Filter(id, text) => {
                if let Some(tab) = self.tab_mut(id) {
                    tab.set_filter(text);
                }
            }
            ListMessage::ClearFilter(id) => {
                if let Some(tab) = self.tab_mut(id) {
                    tab.set_filter(String::new());
                }
            }
            ListMessage::ColumnToggled(column, shown) => {
                match column {
                    OptionalColumn::Permissions => self.settings.show_permissions = shown,
                    OptionalColumn::Owner => self.settings.show_owner = shown,
                }
                self.save_settings();
            }
            ListMessage::Back(id) => return self.go_back(id),
            ListMessage::Forward(id) => return self.go_forward(id),
            ListMessage::MouseButton(button) => {
                let Some(id) = self.focused_tab().map(|tab| tab.id) else {
                    return Task::none();
                };
                return match button {
                    mouse::Button::Back => self.go_back(id),
                    mouse::Button::Forward => self.go_forward(id),
                    _ => Task::none(),
                };
            }
            ListMessage::Bookmark(id, choice) => return self.choose_bookmark(id, choice),
            ListMessage::BookmarkRenamed(index, name) => {
                if let Some(Dialog::Bookmarks(dialog)) = &mut self.dialog
                    && let Some(item) = dialog.items.get_mut(index)
                {
                    item.name = name;
                }
            }
            ListMessage::BookmarkRemoved(index) => {
                if let Some(Dialog::Bookmarks(dialog)) = &mut self.dialog
                    && index < dialog.items.len()
                {
                    dialog.items.remove(index);
                }
            }
        }
        Task::none()
    }

    // Filter

    pub(crate) fn focus_filter(&mut self) -> Task<Message> {
        match self.focused_tab() {
            Some(tab) if !tab.path.is_empty() => operation::focus(ui::filter_input_id(tab.id)),
            _ => Task::none(),
        }
    }

    pub(crate) fn clear_focused_filter(&mut self) -> bool {
        let Some(id) = self
            .focused_tab()
            .filter(|tab| !tab.list.filter.is_empty())
            .map(|tab| tab.id)
        else {
            return false;
        };
        if let Some(tab) = self.tab_mut(id) {
            tab.set_filter(String::new());
        }
        true
    }

    // History

    pub(crate) fn go_back(&mut self, id: TabId) -> Task<Message> {
        let Some(path) = self
            .tab_by_id(id)
            .and_then(|tab| tab.list.back.last().cloned())
        else {
            return Task::none();
        };
        let task = self.navigate(id, path);
        if let Some(tab) = self.tab_mut(id) {
            tab.list.step = Some(HistoryStep::Back);
        }
        task
    }

    pub(crate) fn go_forward(&mut self, id: TabId) -> Task<Message> {
        let Some(path) = self
            .tab_by_id(id)
            .and_then(|tab| tab.list.forward.last().cloned())
        else {
            return Task::none();
        };
        let task = self.navigate(id, path);
        if let Some(tab) = self.tab_mut(id) {
            tab.list.step = Some(HistoryStep::Forward);
        }
        task
    }

    pub(crate) fn reveal_in(
        &mut self,
        id: TabId,
        directory: String,
        name: String,
    ) -> Task<Message> {
        let same = self.tab_by_id(id).is_some_and(|tab| tab.path == directory);
        let task = self.navigate(id, directory);
        if let Some(tab) = self.tab_mut(id) {
            tab.list.reveal = Some(name);
            if same {
                tab.list.step = None;
            }
        }
        task
    }

    pub(crate) fn scroll_to_revealed(id: TabId, position: (usize, usize)) -> Task<Message> {
        let (index, count) = position;
        let fraction = if count > 1 {
            index as f32 / (count - 1) as f32
        } else {
            0.0
        };
        operation::snap_to(
            ui::entry_list_id(id),
            RelativeOffset {
                x: None,
                y: Some(fraction),
            },
        )
    }

    // Bookmarks

    pub fn bookmark_scope(&self, tab: &Tab) -> Option<BookmarkScope> {
        match &tab.origin {
            TabOrigin::Local => Some(BookmarkScope::Local),
            TabOrigin::Site(id) => self.sites.get(id).map(|_| BookmarkScope::Site(id.clone())),
            TabOrigin::Direct => None,
        }
    }

    pub fn bookmarks_for(&self, scope: &BookmarkScope) -> Option<&Bookmarks> {
        match scope {
            BookmarkScope::Local => Some(&self.bookmarks),
            BookmarkScope::Site(id) => self.sites.get(id).map(|site| &site.bookmarks),
        }
    }

    pub fn tab_bookmarks(&self, tab: &Tab) -> Option<&Bookmarks> {
        self.bookmark_scope(tab)
            .and_then(|scope| self.bookmarks_for(&scope))
    }

    fn bookmarks_mut(&mut self, scope: &BookmarkScope) -> Option<&mut Bookmarks> {
        match scope {
            BookmarkScope::Local => Some(&mut self.bookmarks),
            BookmarkScope::Site(id) => self
                .sites
                .sites
                .iter_mut()
                .find(|site| site.id == *id)
                .map(|site| &mut site.bookmarks),
        }
    }

    fn save_bookmarks(&mut self, scope: &BookmarkScope) {
        match scope {
            BookmarkScope::Local => {
                let Some(directory) = &self.config_dir else {
                    return;
                };
                if let Err(error) = self.bookmarks.save(directory) {
                    self.notice = Some(
                        self.strings
                            .format("bookmarks.save_failed", &[("detail", &error.to_string())]),
                    );
                }
            }
            BookmarkScope::Site(_) => self.save_sites(),
        }
    }

    pub(crate) fn add_bookmark(&mut self, id: TabId) {
        let Some((scope, name, path)) = self.tab_by_id(id).and_then(|tab| {
            let scope = self.bookmark_scope(tab)?;
            (!tab.path.is_empty()).then(|| {
                (
                    scope,
                    tab.file_system.file_name(&tab.path).to_string(),
                    tab.path.clone(),
                )
            })
        }) else {
            return;
        };
        if self
            .bookmarks_mut(&scope)
            .is_some_and(|bookmarks| bookmarks.add(&name, &path))
        {
            self.save_bookmarks(&scope);
        }
    }

    pub(crate) fn edit_bookmarks(&mut self, id: TabId) -> Task<Message> {
        let Some(scope) = self.tab_by_id(id).and_then(|tab| self.bookmark_scope(tab)) else {
            return Task::none();
        };
        let items = self
            .bookmarks_for(&scope)
            .map(|bookmarks| bookmarks.items.clone())
            .unwrap_or_default();
        self.dialog = Some(Dialog::Bookmarks(Box::new(BookmarksDialog {
            scope,
            items,
        })));
        Task::none()
    }

    pub(crate) fn confirm_bookmarks(&mut self, dialog: &BookmarksDialog) {
        let Some(bookmarks) = self.bookmarks_mut(&dialog.scope) else {
            return;
        };
        let renamed: Vec<Bookmark> = dialog
            .items
            .iter()
            .map(|item| Bookmark::new(&item.name, &item.path))
            .collect();
        bookmarks.items = renamed;
        self.save_bookmarks(&dialog.scope);
    }

    fn choose_bookmark(&mut self, id: TabId, choice: BookmarkChoice) -> Task<Message> {
        match choice {
            BookmarkChoice::Open(path) => {
                if let Some((pane, index)) = self.locate(id) {
                    self.panes[pane].active = index;
                    self.focused_pane = pane;
                }
                self.navigate(id, path)
            }
            BookmarkChoice::Add => {
                self.add_bookmark(id);
                Task::none()
            }
            BookmarkChoice::Edit => self.edit_bookmarks(id),
        }
    }
}

// Mouse buttons

pub fn subscription() -> Subscription<Message> {
    event::listen_with(mouse_event)
}

pub(crate) fn mouse_event(
    event: iced::Event,
    _status: event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Mouse(mouse::Event::ButtonPressed(
            button @ (mouse::Button::Back | mouse::Button::Forward),
        )) => Some(Message::List(ListMessage::MouseButton(button))),
        _ => None,
    }
}

// Helpers

fn push_limited(stack: &mut Vec<String>, path: String) {
    if stack.last() == Some(&path) {
        return;
    }
    stack.push(path);
    if stack.len() > MAX_HISTORY {
        stack.remove(0);
    }
}
