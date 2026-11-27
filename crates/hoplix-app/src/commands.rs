use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use iced::widget::operation;
use iced::widget::scrollable::Viewport;
use iced::{Task, widget};

use crate::app::{Hoplix, LoadState, Message, Page, Tab, TabMessage};
use crate::compare::CompareMessage;
use crate::file_list::{BookmarkChoice, ListMessage};
use crate::preview::PreviewMessage;
use crate::properties::PropertiesMessage;
use crate::search::SearchMessage;
use crate::settings::{Layout, ThemePreference};
use crate::transfers::TransferAction;
use crate::ui;

// Constants

pub const PALETTE_INPUT: &str = "command-palette";
pub const MAX_PALETTE_ITEMS: usize = 12;
const PAGE_STEP: isize = 10;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    CommandPalette,
    Connect,
    Sites,
    Settings,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    Refresh,
    Up,
    Open,
    Edit,
    NewFolder,
    Rename,
    Delete,
    SelectAll,
    ToggleTheme,
    ToggleLayout,
    ToggleTransfers,
    Compare,
    Synchronise,
    Filter,
    SearchSubfolders,
    Back,
    Forward,
    AddBookmark,
    EditBookmarks,
    OpenBookmark(String),
    Properties,
    TogglePreview,
    CopyPath,
    ConnectSite(String),
    ConnectRecent(usize),
    MoveSelection { delta: isize, extend: bool },
    SelectEdge { last: bool, extend: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    pub action: Action,
    pub label: String,
    pub shortcut: Option<&'static str>,
}

// Keys

pub fn action_for_key(key: &Key, modifiers: Modifiers) -> Option<Action> {
    let command = modifiers.command();
    let shift = modifiers.shift();
    match key.as_ref() {
        Key::Character(character) if command => match character.to_lowercase().as_str() {
            "k" => Some(Action::CommandPalette),
            "a" => Some(Action::SelectAll),
            "f" if shift => Some(Action::SearchSubfolders),
            "f" => Some(Action::Filter),
            "b" => Some(Action::AddBookmark),
            "c" if shift => Some(Action::CopyPath),
            "t" => Some(Action::NewTab),
            "w" => Some(Action::CloseTab),
            "d" if shift => Some(Action::Synchronise),
            "d" => Some(Action::Compare),
            _ => None,
        },
        Key::Named(Named::Tab) if command => Some(if shift {
            Action::PreviousTab
        } else {
            Action::NextTab
        }),
        Key::Named(Named::ArrowUp) if modifiers.alt() => Some(Action::Up),
        Key::Named(Named::ArrowLeft) if modifiers.alt() => Some(Action::Back),
        Key::Named(Named::ArrowRight) if modifiers.alt() => Some(Action::Forward),
        Key::Named(Named::Enter) if modifiers.alt() => Some(Action::Properties),
        Key::Named(named) if !command && !modifiers.alt() => match named {
            Named::Enter => Some(Action::Open),
            Named::Backspace => Some(Action::Up),
            Named::F5 => Some(Action::Refresh),
            Named::F2 => Some(Action::Rename),
            Named::F3 => Some(Action::TogglePreview),
            Named::F4 => Some(Action::Edit),
            Named::Delete => Some(Action::Delete),
            Named::ArrowUp => Some(Action::MoveSelection {
                delta: -1,
                extend: shift,
            }),
            Named::ArrowDown => Some(Action::MoveSelection {
                delta: 1,
                extend: shift,
            }),
            Named::PageUp => Some(Action::MoveSelection {
                delta: -PAGE_STEP,
                extend: shift,
            }),
            Named::PageDown => Some(Action::MoveSelection {
                delta: PAGE_STEP,
                extend: shift,
            }),
            Named::Home => Some(Action::SelectEdge {
                last: false,
                extend: shift,
            }),
            Named::End => Some(Action::SelectEdge {
                last: true,
                extend: shift,
            }),
            _ => None,
        },
        _ => None,
    }
}

pub fn shortcut(action: &Action) -> Option<&'static str> {
    match action {
        Action::CommandPalette => Some("Ctrl+K"),
        Action::NewTab => Some("Ctrl+T"),
        Action::CloseTab => Some("Ctrl+W"),
        Action::NextTab => Some("Ctrl+Tab"),
        Action::PreviousTab => Some("Ctrl+Shift+Tab"),
        Action::Refresh => Some("F5"),
        Action::Up => Some("Backspace"),
        Action::Open => Some("Enter"),
        Action::Rename => Some("F2"),
        Action::Edit => Some("F4"),
        Action::Delete => Some("Del"),
        Action::SelectAll => Some("Ctrl+A"),
        Action::Compare => Some("Ctrl+D"),
        Action::Synchronise => Some("Ctrl+Shift+D"),
        Action::Filter => Some("Ctrl+F"),
        Action::SearchSubfolders => Some("Ctrl+Shift+F"),
        Action::Back => Some("Alt+←"),
        Action::Forward => Some("Alt+→"),
        Action::AddBookmark => Some("Ctrl+B"),
        Action::Properties => Some("Alt+Enter"),
        Action::TogglePreview => Some("F3"),
        Action::CopyPath => Some("Ctrl+Shift+C"),
        _ => None,
    }
}

// Matching

pub fn fuzzy_score(query: &str, candidate: &str) -> Option<u32> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let candidate: Vec<char> = candidate.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0u32;
    let mut position = 0usize;
    let mut previous: Option<usize> = None;
    for wanted in query {
        let found = (position..candidate.len()).find(|&index| candidate[index] == wanted)?;
        let at_word_start = found == 0 || !candidate[found - 1].is_alphanumeric();
        score += 1;
        if at_word_start {
            score += 8;
        }
        if previous.is_some_and(|last| last + 1 == found) {
            score += 5;
        }
        score = score.saturating_sub(u32::try_from(found - position).unwrap_or(u32::MAX).min(3));
        previous = Some(found);
        position = found + 1;
    }
    Some(score)
}

// Palette

impl Hoplix {
    pub fn palette_items(&self) -> Vec<PaletteItem> {
        let query = self
            .palette
            .as_ref()
            .map(|palette| palette.query.as_str())
            .unwrap_or_default();
        let strings = &self.strings;
        let mut actions: Vec<(Action, String)> = [
            (Action::Connect, "command.connect"),
            (Action::Sites, "command.sites"),
            (Action::Settings, "command.settings"),
            (Action::NewTab, "command.new_tab"),
            (Action::CloseTab, "command.close_tab"),
            (Action::NextTab, "command.next_tab"),
            (Action::PreviousTab, "command.previous_tab"),
            (Action::Refresh, "command.refresh"),
            (Action::Up, "command.up"),
            (Action::NewFolder, "command.new_folder"),
            (Action::Rename, "command.rename"),
            (Action::Edit, "command.edit"),
            (Action::Delete, "command.delete"),
            (Action::SelectAll, "command.select_all"),
            (Action::ToggleTheme, "command.toggle_theme"),
            (Action::ToggleLayout, "command.toggle_layout"),
            (Action::ToggleTransfers, "command.toggle_transfers"),
            (Action::Compare, "command.compare"),
            (Action::Synchronise, "command.synchronise"),
            (Action::Filter, "command.filter"),
            (Action::SearchSubfolders, "command.search"),
            (Action::Back, "command.back"),
            (Action::Forward, "command.forward"),
            (Action::AddBookmark, "command.add_bookmark"),
            (Action::EditBookmarks, "command.edit_bookmarks"),
            (Action::Properties, "command.properties"),
            (Action::TogglePreview, "command.toggle_preview"),
            (Action::CopyPath, "command.copy_path"),
        ]
        .into_iter()
        .map(|(action, key)| (action, strings.get(key).to_string()))
        .collect();
        if let Some(bookmarks) = self.focused_tab().and_then(|tab| self.tab_bookmarks(tab)) {
            actions.extend(bookmarks.items.iter().map(|bookmark| {
                (
                    Action::OpenBookmark(bookmark.path.clone()),
                    strings.format("command.open_bookmark", &[("name", &bookmark.name)]),
                )
            }));
        }
        actions.extend(self.sites.sorted().into_iter().map(|site| {
            (
                Action::ConnectSite(site.id.clone()),
                strings.format("command.connect_site", &[("name", &site.display_path())]),
            )
        }));
        actions.extend(
            self.recent
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    (
                        Action::ConnectRecent(index),
                        strings.format("command.connect_recent", &[("name", &entry.label())]),
                    )
                }),
        );
        let mut scored: Vec<(u32, usize, PaletteItem)> = actions
            .into_iter()
            .enumerate()
            .filter_map(|(order, (action, label))| {
                let score = fuzzy_score(query, &label)?;
                Some((
                    score,
                    order,
                    PaletteItem {
                        shortcut: shortcut(&action),
                        action,
                        label,
                    },
                ))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(MAX_PALETTE_ITEMS)
            .map(|(_, _, item)| item)
            .collect()
    }

    pub(crate) fn open_palette(&mut self) -> Task<Message> {
        self.palette = Some(Palette::default());
        operation::focus(PALETTE_INPUT)
    }

    pub(crate) fn update_palette(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PaletteQuery(query) => {
                if let Some(palette) = &mut self.palette {
                    palette.query = query;
                    palette.selected = 0;
                }
            }
            Message::PaletteMove(delta) => {
                let count = self.palette_items().len();
                if let Some(palette) = &mut self.palette
                    && count > 0
                {
                    palette.selected = palette.selected.saturating_add_signed(delta).min(count - 1);
                }
            }
            Message::PaletteSubmitted => {
                let selected = self.palette.as_ref().map_or(0, |palette| palette.selected);
                let item = self.palette_items().into_iter().nth(selected);
                self.palette = None;
                if let Some(item) = item {
                    return self.run_action(item.action);
                }
            }
            Message::PaletteClosed => self.palette = None,
            _ => {}
        }
        Task::none()
    }

    // Keys

    pub(crate) fn handle_key(
        &mut self,
        key: &Key,
        modifiers: Modifiers,
        captured: bool,
    ) -> Task<Message> {
        let escape = matches!(key, Key::Named(Named::Escape));
        if self.palette.is_some() {
            return match key {
                Key::Named(Named::Escape) => {
                    self.palette = None;
                    Task::none()
                }
                Key::Named(Named::ArrowUp) => self.update_palette(Message::PaletteMove(-1)),
                Key::Named(Named::ArrowDown) => self.update_palette(Message::PaletteMove(1)),
                _ => Task::none(),
            };
        }
        if matches!(key, Key::Named(Named::Tab)) && !modifiers.command() && !modifiers.alt() {
            return if modifiers.shift() {
                operation::focus_previous()
            } else {
                operation::focus_next()
            };
        }
        if self.dialog.is_some() {
            if escape {
                return self.update(Message::DialogCancelled);
            }
            if !captured && matches!(key, Key::Named(Named::Enter)) {
                return self.update(Message::DialogConfirmed);
            }
            return Task::none();
        }
        let Some(action) = action_for_key(key, modifiers) else {
            if escape && !captured && self.page != Page::Browser {
                self.page = Page::Browser;
            } else if escape && self.page == Page::Browser {
                self.clear_focused_filter();
            }
            return Task::none();
        };
        if action == Action::CommandPalette {
            return self.open_palette();
        }
        if matches!(action, Action::Filter | Action::SearchSubfolders) && self.page == Page::Browser
        {
            return self.run_action(action);
        }
        if captured || self.page != Page::Browser {
            return Task::none();
        }
        let inputs: Vec<String> = self
            .panes
            .iter()
            .filter_map(|pane| pane.active_tab())
            .map(|tab| ui::path_input_id(tab.id))
            .collect();
        unless_focused(inputs, action)
    }

    // Actions

    pub(crate) fn run_action(&mut self, action: Action) -> Task<Message> {
        let focused = self.focused_tab().map(|tab| tab.id);
        let on_tab = |message: TabMessage| match focused {
            Some(id) => Message::Tab(id, message),
            None => Message::PaletteClosed,
        };
        match action {
            Action::CommandPalette => return self.open_palette(),
            Action::Connect => self.page = Page::Connect,
            Action::Sites => self.page = Page::Sites,
            Action::Settings => self.page = Page::Settings,
            Action::NewTab => return self.update(Message::NewTab(self.focused_pane)),
            Action::CloseTab => {
                if let Some(id) = focused {
                    return self.update(Message::CloseTab(id));
                }
            }
            Action::NextTab => self.cycle_tab(1),
            Action::PreviousTab => self.cycle_tab(-1),
            Action::Refresh => return self.update(on_tab(TabMessage::Refresh)),
            Action::Up => return self.update(on_tab(TabMessage::Up)),
            Action::NewFolder => return self.update(on_tab(TabMessage::NewFolder)),
            Action::Rename => return self.update(on_tab(TabMessage::Rename)),
            Action::Edit => return self.update(on_tab(TabMessage::Edit)),
            Action::Delete => return self.update(on_tab(TabMessage::Delete)),
            Action::Open => {
                if let Some(index) = self
                    .focused_tab()
                    .and_then(|tab| tab.selection.last().copied())
                {
                    return self.update(on_tab(TabMessage::Open(index)));
                }
            }
            Action::SelectAll => {
                let visible = self.focused_tab().and_then(|tab| self.visible_indices(tab));
                if let Some(tab) = self.focused_tab_mut() {
                    tab.selection = visible.unwrap_or_else(|| (0..tab.entries.len()).collect());
                    tab.anchor = tab.selection.first().copied();
                    tab.cursor = tab.selection.last().copied();
                }
            }
            Action::ToggleTheme => {
                let theme = if self.theme().extended_palette().is_dark {
                    ThemePreference::Light
                } else {
                    ThemePreference::Dark
                };
                return self.update(Message::ThemeSelected(theme));
            }
            Action::ToggleLayout => {
                let layout = match self.settings.layout {
                    Layout::Modern => Layout::Classic,
                    Layout::Classic => Layout::Modern,
                };
                return self.update(Message::LayoutSelected(layout));
            }
            Action::ToggleTransfers => {
                return self.update(Message::Transfer(TransferAction::TogglePanel));
            }
            Action::Compare => return self.update(Message::Compare(CompareMessage::Toggle)),
            Action::Synchronise => return self.update(Message::Compare(CompareMessage::OpenSync)),
            Action::Filter => return self.focus_filter(),
            Action::SearchSubfolders => return self.update(Message::Search(SearchMessage::Open)),
            Action::Back => {
                if let Some(id) = focused {
                    return self.update(Message::List(ListMessage::Back(id)));
                }
            }
            Action::Forward => {
                if let Some(id) = focused {
                    return self.update(Message::List(ListMessage::Forward(id)));
                }
            }
            Action::AddBookmark => {
                if let Some(id) = focused {
                    return self.update(Message::List(ListMessage::Bookmark(
                        id,
                        BookmarkChoice::Add,
                    )));
                }
            }
            Action::EditBookmarks => {
                if let Some(id) = focused {
                    return self.update(Message::List(ListMessage::Bookmark(
                        id,
                        BookmarkChoice::Edit,
                    )));
                }
            }
            Action::OpenBookmark(path) => {
                if let Some(id) = focused {
                    return self.update(Message::List(ListMessage::Bookmark(
                        id,
                        BookmarkChoice::Open(path),
                    )));
                }
            }
            Action::Properties => return self.update(Message::Properties(PropertiesMessage::Open)),
            Action::TogglePreview => return self.update(Message::Preview(PreviewMessage::Toggle)),
            Action::CopyPath => return self.copy_paths(),
            Action::ConnectSite(id) => return self.update(Message::SiteConnect(id)),
            Action::ConnectRecent(index) => return self.use_recent(index, true),
            Action::MoveSelection { delta, extend } => return self.move_selection(delta, extend),
            Action::SelectEdge { last, extend } => {
                let delta = if last { isize::MAX } else { isize::MIN };
                return self.move_selection(delta, extend);
            }
        }
        Task::none()
    }

    fn cycle_tab(&mut self, step: isize) {
        let Some(pane) = self.panes.get_mut(self.focused_pane) else {
            return;
        };
        let count = pane.tabs.len();
        if count > 0 {
            let next = (pane.active.cast_signed() + step).rem_euclid(count.cast_signed());
            pane.active = next.cast_unsigned();
        }
    }

    fn move_selection(&mut self, delta: isize, extend: bool) -> Task<Message> {
        let visible = self.focused_tab().and_then(|tab| self.visible_indices(tab));
        let Some(tab) = self.focused_tab_mut() else {
            return Task::none();
        };
        let Some(index) = next_index(tab, delta, visible.as_deref()) else {
            return Task::none();
        };
        if extend {
            let anchor = tab.anchor.or(tab.cursor).unwrap_or(index);
            tab.selection = (anchor.min(index)..=anchor.max(index))
                .filter(|item| {
                    visible
                        .as_ref()
                        .is_none_or(|visible| visible.contains(item))
                })
                .collect();
            tab.anchor = Some(anchor);
        } else {
            tab.selection = vec![index];
            tab.anchor = Some(index);
        }
        tab.cursor = Some(index);
        let (row, count) = match &visible {
            Some(visible) => (
                visible.iter().position(|item| *item == index).unwrap_or(0),
                visible.len(),
            ),
            None => (index, tab.entries.len()),
        };
        match tab
            .viewport
            .and_then(|viewport| reveal_offset(&viewport, row, count))
        {
            Some(offset) => operation::scroll_to(
                ui::entry_list_id(tab.id),
                operation::AbsoluteOffset {
                    x: None,
                    y: Some(offset),
                },
            ),
            None => Task::none(),
        }
    }

    fn focused_tab_mut(&mut self) -> Option<&mut Tab> {
        let pane = self.panes.get_mut(self.focused_pane)?;
        pane.tabs.get_mut(pane.active)
    }

    #[cfg(test)]
    pub(crate) fn effective_mode(&self) -> iced::theme::Mode {
        if self.theme().extended_palette().is_dark {
            iced::theme::Mode::Dark
        } else {
            iced::theme::Mode::Light
        }
    }
}

// Helpers

pub(crate) fn reveal_offset(viewport: &Viewport, index: usize, count: usize) -> Option<f32> {
    if count == 0 {
        return None;
    }
    let row_height = viewport.content_bounds().height / count as f32;
    let top = index as f32 * row_height;
    let bottom = top + row_height;
    let offset = viewport.absolute_offset().y;
    let height = viewport.bounds().height;
    if top < offset {
        Some(top)
    } else if bottom > offset + height {
        Some(bottom - height)
    } else {
        None
    }
}

fn next_index(tab: &Tab, delta: isize, visible: Option<&[usize]>) -> Option<usize> {
    if tab.entries.is_empty() || tab.state == LoadState::Disconnected {
        return None;
    }
    let Some(visible) = visible else {
        let last = tab.entries.len() - 1;
        let current = tab.cursor.or_else(|| tab.selection.last().copied());
        return Some(match current {
            None if delta < 0 => last,
            None => 0,
            Some(current) => current.saturating_add_signed(delta).min(last),
        });
    };
    let last = visible.len().checked_sub(1)?;
    let position = tab
        .cursor
        .or_else(|| tab.selection.last().copied())
        .and_then(|current| visible.iter().position(|&index| index >= current));
    let position = match position {
        None if delta < 0 => last,
        None => 0,
        Some(position) => position.saturating_add_signed(delta).min(last),
    };
    visible.get(position).copied()
}

fn unless_focused(inputs: Vec<String>, action: Action) -> Task<Message> {
    let mut task: Task<bool> = Task::done(false);
    for input in inputs {
        task = task.then(move |focused| {
            if focused {
                Task::done(true)
            } else {
                operation::is_focused(widget::Id::from(input.clone()))
            }
        });
    }
    task.then(move |focused| {
        if focused {
            Task::none()
        } else {
            Task::done(Message::RunAction(action.clone()))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Pane;
    use crate::settings::Settings;
    use hoplix_core::{Entry, EntryKind};
    use iced::theme::Mode;

    fn app() -> Hoplix {
        let mut app = Hoplix::with_settings(Settings::default(), None);
        app.panes.push(Pane::default());
        let _ = app.open_local_tab(0, None);
        let tab = &mut app.panes[0].tabs[0];
        tab.path = "/data".to_string();
        tab.state = LoadState::Ready;
        tab.entries = (0..5)
            .map(|index| Entry {
                name: format!("file{index}"),
                kind: EntryKind::File,
                size: Some(1),
                modified: None,
                is_link: false,
                permissions: None,
                owner: None,
            })
            .collect();
        app
    }

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    // Keys

    #[test]
    fn keys_map_to_actions() {
        let none = Modifiers::default();
        assert_eq!(
            action_for_key(&character("k"), Modifiers::CTRL),
            Some(Action::CommandPalette)
        );
        assert_eq!(
            action_for_key(&character("T"), Modifiers::CTRL),
            Some(Action::NewTab)
        );
        assert_eq!(action_for_key(&character("t"), none), None);
        assert_eq!(
            action_for_key(&Key::Named(Named::Tab), Modifiers::CTRL | Modifiers::SHIFT),
            Some(Action::PreviousTab)
        );
        assert_eq!(action_for_key(&Key::Named(Named::Tab), none), None);
        assert_eq!(
            action_for_key(&Key::Named(Named::ArrowUp), Modifiers::ALT),
            Some(Action::Up)
        );
        assert_eq!(
            action_for_key(&Key::Named(Named::ArrowDown), Modifiers::SHIFT),
            Some(Action::MoveSelection {
                delta: 1,
                extend: true
            })
        );
        assert_eq!(
            action_for_key(&Key::Named(Named::F5), none),
            Some(Action::Refresh)
        );
        assert_eq!(
            action_for_key(&Key::Named(Named::Delete), none),
            Some(Action::Delete)
        );
        assert_eq!(
            action_for_key(&Key::Named(Named::F3), none),
            Some(Action::TogglePreview)
        );
        assert_eq!(
            action_for_key(&character("C"), Modifiers::CTRL | Modifiers::SHIFT),
            Some(Action::CopyPath)
        );
        assert_eq!(action_for_key(&character("c"), Modifiers::CTRL), None);
    }

    #[test]
    fn dialogs_and_palettes_take_escape_first() {
        let mut app = app();
        let _ = app.handle_key(&character("k"), Modifiers::CTRL, false);
        assert!(app.palette.is_some());
        let _ = app.handle_key(&Key::Named(Named::Escape), Modifiers::default(), true);
        assert!(app.palette.is_none());

        let _ = app.update(Message::Tab(1, TabMessage::NewFolder));
        assert!(app.dialog.is_some());
        let _ = app.handle_key(&Key::Named(Named::Escape), Modifiers::default(), true);
        assert!(app.dialog.is_none());

        app.page = Page::Settings;
        let _ = app.handle_key(&Key::Named(Named::Escape), Modifiers::default(), false);
        assert_eq!(app.page, Page::Browser);
    }

    // Selection

    #[test]
    fn arrows_move_and_extend_the_selection() {
        let mut app = app();
        let _ = app.run_action(Action::MoveSelection {
            delta: 1,
            extend: false,
        });
        assert_eq!(app.panes[0].tabs[0].selection, [0]);
        let _ = app.run_action(Action::MoveSelection {
            delta: 1,
            extend: false,
        });
        let _ = app.run_action(Action::MoveSelection {
            delta: 2,
            extend: true,
        });
        assert_eq!(app.panes[0].tabs[0].selection, [1, 2, 3]);
        let _ = app.run_action(Action::MoveSelection {
            delta: -3,
            extend: true,
        });
        assert_eq!(app.panes[0].tabs[0].selection, [0, 1]);
        let _ = app.run_action(Action::SelectEdge {
            last: true,
            extend: false,
        });
        assert_eq!(app.panes[0].tabs[0].selection, [4]);
        let _ = app.run_action(Action::SelectAll);
        assert_eq!(app.panes[0].tabs[0].selection, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn tabs_cycle_in_both_directions() {
        let mut app = app();
        let _ = app.open_local_tab(0, None);
        let _ = app.open_local_tab(0, None);
        assert_eq!(app.panes[0].active, 2);
        let _ = app.run_action(Action::NextTab);
        assert_eq!(app.panes[0].active, 0);
        let _ = app.run_action(Action::PreviousTab);
        assert_eq!(app.panes[0].active, 2);
    }

    // Palette

    #[test]
    fn fuzzy_matching_prefers_word_starts_and_runs() {
        assert!(fuzzy_score("nt", "New tab").is_some());
        assert!(fuzzy_score("xyz", "New tab").is_none());
        assert_eq!(fuzzy_score("", "Anything"), Some(0));
        let word_start = fuzzy_score("ren", "Rename").expect("no match");
        let scattered = fuzzy_score("ren", "Refresh listing now").expect("no match");
        assert!(word_start > scattered, "{word_start} {scattered}");
        assert!(fuzzy_score("NEW", "new folder").is_some());
    }

    #[test]
    fn palette_filters_runs_and_lists_sites() {
        let mut app = app();
        app.strings = crate::i18n::load("en");
        let mut site = hoplix_core::Site::new("Backup server");
        site.host = "backup.example".to_string();
        app.sites.upsert(site);
        let _ = app.open_palette();
        let all = app.palette_items();
        assert!(all.len() <= MAX_PALETTE_ITEMS);
        assert_eq!(all[0].shortcut, None);

        let _ = app.update(Message::PaletteQuery("backup".to_string()));
        let items = app.palette_items();
        assert!(matches!(items[0].action, Action::ConnectSite(_)));

        let _ = app.update(Message::PaletteQuery("settings".to_string()));
        let _ = app.update(Message::PaletteMove(5));
        let _ = app.update(Message::PaletteMove(-9));
        let _ = app.update(Message::PaletteSubmitted);
        assert!(app.palette.is_none());
        assert_eq!(app.page, Page::Settings);
    }

    #[test]
    fn toggles_switch_theme_and_layout() {
        let mut app = app();
        app.system_mode = Mode::Light;
        let _ = app.run_action(Action::ToggleTheme);
        assert_eq!(app.settings.theme, ThemePreference::Dark);
        assert_eq!(app.effective_mode(), Mode::Dark);
    }
}
