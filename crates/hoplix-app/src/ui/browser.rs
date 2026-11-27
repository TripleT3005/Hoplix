use hoplix_core::{Entry, SortKey, SortOrder};
use iced::widget::{
    Column, Row, Space, button, column, container, mouse_area, pick_list, responsive, row,
    scrollable, text, text_input,
};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, LoadState, Message, Pane, Tab, TabId, TabMessage};
use crate::compare::{Comparison, Mark};
use crate::i18n::Strings;
use crate::preview::PreviewMessage;
use crate::settings::Layout;
use crate::transfers::DropTarget;
use crate::ui::Choice;
use crate::{format, local_time, theme, ui};

// Constants

const KIND_WIDTH: f32 = 110.0;
const SIZE_WIDTH: f32 = 110.0;
const MODIFIED_WIDTH: f32 = 130.0;
const PERMISSIONS_WIDTH: f32 = 90.0;
const OWNER_WIDTH: f32 = 90.0;
const ROW_PADDING: [u16; 2] = [3, 8];
const DESTINATION_WIDTH: f32 = 220.0;
const MARK_WIDTH: f32 = 18.0;
const NAME_MIN_WIDTH: f32 = 160.0;
const NAME_GAP: f32 = 8.0;
const MODIFIED_GAP: f32 = 16.0;
const SCROLLBAR_GAP: f32 = 12.0;
const PREVIEW_GAP: f32 = 2.0;
const PATH_MIN_WIDTH: f32 = 220.0;
const TOOLBAR_SPACING: f32 = 6.0;
const BUTTON_PADDING: f32 = 20.0;
const BUTTON_CHAR_WIDTH: f32 = 16.0 * 0.6;

// Columns

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Columns {
    pub kind: bool,
    pub modified: bool,
    pub permissions: bool,
    pub owner: bool,
}

impl Columns {
    const ALL: Self = Self {
        kind: true,
        modified: true,
        permissions: false,
        owner: false,
    };
    const WITHOUT_KIND: Self = Self {
        kind: false,
        modified: true,
        permissions: false,
        owner: false,
    };
    const NAME_AND_SIZE: Self = Self {
        kind: false,
        modified: false,
        permissions: false,
        owner: false,
    };

    fn with_optional(self, permissions: bool, owner: bool) -> Self {
        Self {
            permissions,
            owner,
            ..self
        }
    }

    fn fixed_width(self) -> f32 {
        let mut width = 2.0 * f32::from(ROW_PADDING[1]) + SCROLLBAR_GAP + NAME_GAP + SIZE_WIDTH;
        if self.kind {
            width += KIND_WIDTH;
        }
        if self.modified {
            width += MODIFIED_GAP + MODIFIED_WIDTH;
        }
        if self.permissions {
            width += MODIFIED_GAP + PERMISSIONS_WIDTH;
        }
        if self.owner {
            width += MODIFIED_GAP + OWNER_WIDTH;
        }
        width
    }

    fn fit(width: f32, permissions: bool, owner: bool) -> Self {
        [
            Self::ALL.with_optional(permissions, owner),
            Self::WITHOUT_KIND.with_optional(permissions, owner),
            Self::WITHOUT_KIND.with_optional(permissions, false),
            Self::ALL,
            Self::WITHOUT_KIND,
        ]
        .into_iter()
        .find(|columns| width - columns.fixed_width() >= NAME_MIN_WIDTH)
        .unwrap_or(Self::NAME_AND_SIZE)
    }

    fn min_list_width() -> f32 {
        Self::WITHOUT_KIND.fixed_width() + NAME_MIN_WIDTH
    }
}

// Browser

pub fn view(app: &Hoplix) -> Element<'_, Message> {
    let show_focus = app.settings.layout == Layout::Classic;
    let highlighted = highlighted_target(app);
    let panes = app.panes.iter().enumerate().map(|(index, pane)| {
        let focused = show_focus && index == app.focused_pane;
        let dropping = match highlighted {
            Some(DropTarget::Pane(target)) => target == index,
            Some(DropTarget::Tab(id)) => pane.tabs.iter().any(|tab| tab.id == id),
            None => false,
        };
        mouse_area(
            container(view_pane(app, index, pane))
                .padding(4)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(move |theme| theme::pane(theme, focused, dropping)),
        )
        .on_enter(Message::DragEntered(DropTarget::Pane(index)))
        .on_exit(Message::DragLeft(DropTarget::Pane(index), None))
        .into()
    });
    let panes = Row::with_children(panes).spacing(8);
    match ui::compare::bar(app) {
        Some(bar) => container(column![bar, panes].spacing(8)).padding(8).into(),
        None => container(panes).padding(8).into(),
    }
}

fn highlighted_target(app: &Hoplix) -> Option<DropTarget> {
    if app.drag.as_ref().is_some_and(|drag| drag.moved) {
        app.drop_target
    } else {
        app.file_drop_target()
    }
}

fn view_pane<'a>(app: &'a Hoplix, index: usize, pane: &'a Pane) -> Element<'a, Message> {
    let strings = &app.strings;
    let content = match pane.active_tab() {
        Some(tab) => view_tab(app, index, tab),
        None => container(text(strings.get("tabs.empty")))
            .center(Length::Fill)
            .into(),
    };
    column![tab_bar(app, index, pane), content]
        .spacing(4)
        .into()
}

// Tabs

fn tab_bar<'a>(app: &'a Hoplix, index: usize, pane: &'a Pane) -> Element<'a, Message> {
    let strings = &app.strings;
    let highlighted = highlighted_target(app);
    let tabs = pane.tabs.iter().enumerate().map(|(position, tab)| {
        let active = position == pane.active || highlighted == Some(DropTarget::Tab(tab.id));
        let title = if tab.path.is_empty() {
            strings.get("browser.loading").to_string()
        } else {
            ui::tab_title(tab)
        };
        let label = row![
            text(title).size(13),
            button(text("×").size(13))
                .padding([0, 4])
                .style(button::text)
                .on_press(Message::CloseTab(tab.id)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        mouse_area(
            button(label)
                .padding([3, 10])
                .style(move |theme, status| theme::tab(theme, status, active))
                .on_press(Message::SelectTab(tab.id)),
        )
        .on_enter(Message::DragEntered(DropTarget::Tab(tab.id)))
        .on_exit(Message::DragLeft(
            DropTarget::Tab(tab.id),
            Some(DropTarget::Pane(index)),
        ))
        .into()
    });
    let new_tab = button(text("+").size(14))
        .padding([2, 10])
        .style(button::text)
        .on_press(Message::NewTab(index));
    scrollable(
        Row::with_children(tabs)
            .push(new_tab)
            .spacing(4)
            .align_y(Alignment::Center)
            .padding([0, 0]),
    )
    .direction(scrollable::Direction::Horizontal(
        scrollable::Scrollbar::new().width(4).scroller_width(4),
    ))
    .into()
}

// Toolbar

fn toolbar<'a>(app: &'a Hoplix, tab: &'a Tab) -> Element<'a, Message> {
    let strings = &app.strings;
    let labels = [
        "←",
        "→",
        strings.get("browser.up"),
        strings.get("browser.refresh"),
    ];
    let buttons: f32 = labels
        .iter()
        .map(|label| BUTTON_PADDING + label.chars().count() as f32 * BUTTON_CHAR_WIDTH)
        .sum();
    let picker = if app.tab_bookmarks(tab).is_some() {
        ui::file_list::BOOKMARKS_WIDTH + TOOLBAR_SPACING
    } else {
        0.0
    };
    let needed = buttons + picker + PATH_MIN_WIDTH + labels.len() as f32 * TOOLBAR_SPACING;
    responsive(move |size| {
        let id = tab.id;
        let [back, forward] = ui::file_list::history_buttons(tab);
        let buttons = row![
            back,
            forward,
            button(action_text(strings.get("browser.up")))
                .style(theme::secondary_button)
                .on_press_maybe(
                    tab.file_system
                        .parent(&tab.path)
                        .map(|_| Message::Tab(id, TabMessage::Up))
                ),
            button(action_text(strings.get("browser.refresh")))
                .style(theme::secondary_button)
                .on_press_maybe(
                    (!tab.path.is_empty()).then_some(Message::Tab(id, TabMessage::Refresh))
                ),
        ]
        .spacing(TOOLBAR_SPACING)
        .align_y(Alignment::Center);
        let input = text_input(strings.get("browser.path"), &tab.path_input)
            .id(ui::path_input_id(id))
            .on_input(move |path| Message::Tab(id, TabMessage::PathEdited(path)))
            .on_submit(Message::Tab(id, TabMessage::PathSubmitted))
            .padding(6);
        let picker = ui::file_list::bookmark_picker(app, tab);
        if size.width >= needed {
            row![buttons, input]
                .push(picker)
                .spacing(TOOLBAR_SPACING)
                .align_y(Alignment::Center)
                .into()
        } else {
            column![
                row![buttons, Space::new().width(Length::Fill)]
                    .push(picker)
                    .spacing(TOOLBAR_SPACING)
                    .align_y(Alignment::Center),
                input,
            ]
            .spacing(TOOLBAR_SPACING)
            .into()
        }
    })
    .height(Length::Shrink)
    .into()
}

fn action_text<'a>(label: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(label).wrapping(text::Wrapping::None)
}

// Tab content

fn view_tab<'a>(app: &'a Hoplix, pane: usize, tab: &'a Tab) -> Element<'a, Message> {
    let strings = &app.strings;
    let id = tab.id;
    let ready = !tab.path.is_empty() && tab.state != LoadState::Loading;
    let has_single = ready && tab.selected_entry().is_some();
    let has_any = ready && !tab.selection.is_empty();
    let can_transfer = ready && !tab.transfer_sources().is_empty();
    let has_file = ready
        && tab
            .selected_entry()
            .is_some_and(|entry| !entry.is_directory());
    let mut actions = row![
        button(action_label(strings.get("operation.new_folder")))
            .style(button::text)
            .on_press_maybe(ready.then_some(Message::Tab(id, TabMessage::NewFolder))),
        button(action_label(strings.get("operation.rename")))
            .style(button::text)
            .on_press_maybe(has_single.then_some(Message::Tab(id, TabMessage::Rename))),
        button(action_label(strings.get("operation.delete")))
            .style(button::text)
            .on_press_maybe(has_any.then_some(Message::Tab(id, TabMessage::Delete))),
        button(action_label(strings.get("operation.edit")))
            .style(button::text)
            .on_press_maybe(has_file.then_some(Message::Tab(id, TabMessage::Edit))),
        button(action_label(strings.get("preview.toggle")))
            .style(if app.preview.open {
                theme::secondary_button
            } else {
                button::text
            })
            .on_press(Message::Preview(PreviewMessage::Toggle)),
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    if app.settings.layout == Layout::Classic && app.panes.len() > 1 {
        let label = if pane == 0 {
            format!("{} →", strings.get("transfer.copy_to_other"))
        } else {
            format!("← {}", strings.get("transfer.copy_to_other"))
        };
        actions = actions.push(
            button(action_label(label))
                .style(theme::secondary_button)
                .on_press_maybe(
                    can_transfer.then_some(Message::Tab(id, TabMessage::CopyToOtherPane)),
                ),
        );
    }
    let destinations: Vec<Choice<TabId>> = app
        .transfer_destinations(id)
        .into_iter()
        .map(|(value, label)| Choice { value, label })
        .collect();
    if !destinations.is_empty() {
        let picker = pick_list(destinations, None::<Choice<TabId>>, move |choice| {
            Message::TransferTo {
                from: id,
                to: choice.value,
            }
        })
        .placeholder(strings.get("transfer.copy_to"))
        .text_size(13)
        .width(DESTINATION_WIDTH);
        actions = actions.push(if can_transfer {
            Element::from(picker)
        } else {
            Element::from(
                button(action_label(strings.get("transfer.copy_to")))
                    .width(DESTINATION_WIDTH)
                    .style(theme::secondary_button),
            )
        });
    }

    if tab.state == LoadState::Disconnected {
        let message = column![
            text(strings.get("session.disconnected")).size(15),
            button(text(strings.get("session.reconnect")))
                .style(button::primary)
                .on_press(Message::Reconnect(id)),
        ]
        .spacing(12)
        .align_x(Alignment::Center);
        return container(message).center(Length::Fill).into();
    }
    let mut content = column![toolbar(app, tab), actions.wrap().vertical_spacing(4)]
        .spacing(6)
        .padding(4);
    if let LoadState::Failed(error) = &tab.state {
        content = content.push(ui::notice(ui::error_text(strings, error)));
    }
    content = content.push(ui::file_list::filter_row(strings, tab));
    content.push(content_area(app, tab)).into()
}

fn action_label<'a>(label: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(label).size(13).wrapping(text::Wrapping::None)
}

// Content area

fn content_area<'a>(app: &'a Hoplix, tab: &'a Tab) -> Element<'a, Message> {
    responsive(move |size| {
        let available = size.width - PREVIEW_GAP;
        let fit = ui::preview::fit(app, tab, available, Columns::min_list_width());
        let list_width = fit.map_or(size.width, |fit| {
            available - ui::preview::DIVIDER_WIDTH - fit.width
        });
        Row::new()
            .push(
                container(list_view(app, tab, list_width))
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .push(fit.map(|fit| ui::preview::panel(app, fit)))
            .spacing(PREVIEW_GAP)
            .height(Length::Fill)
            .into()
    })
    .into()
}

fn list_view<'a>(app: &'a Hoplix, tab: &'a Tab, width: f32) -> Element<'a, Message> {
    let strings = &app.strings;
    match &tab.search {
        Some(results) => {
            let columns = Columns::fit(width, false, false);
            ui::file_list::results_view(strings, tab, results, columns.modified)
        }
        None => {
            let comparison = app
                .compare
                .comparison
                .as_ref()
                .filter(|comparison| comparison.involves(tab.id));
            let mark = if comparison.is_some() {
                MARK_WIDTH
            } else {
                0.0
            };
            let columns = Columns::fit(
                width - mark,
                app.settings.show_permissions,
                app.settings.show_owner,
            );
            column![
                header(strings, tab, comparison.is_some(), columns),
                entry_list(strings, tab, comparison, columns)
            ]
            .spacing(6)
            .into()
        }
    }
}

fn header<'a>(
    strings: &'a Strings,
    tab: &'a Tab,
    comparing: bool,
    columns: Columns,
) -> Element<'a, Message> {
    let id = tab.id;
    let column_button = |key: SortKey, label: &'a str, width: Length, alignment: Alignment| {
        let marker = match (tab.sort_key == key, tab.sort_order) {
            (true, SortOrder::Ascending) => " ▲",
            (true, SortOrder::Descending) => " ▼",
            (false, _) => "",
        };
        button(
            text(format!("{label}{marker}"))
                .size(13)
                .width(Length::Fill)
                .align_x(alignment)
                .wrapping(text::Wrapping::None),
        )
        .padding([ROW_PADDING[0], 0])
        .width(width)
        .style(button::text)
        .on_press(Message::Tab(id, TabMessage::SortBy(key)))
    };
    let mut header = row![
        Space::new().width(if comparing { MARK_WIDTH } else { 0.0 }),
        container(column_button(
            SortKey::Name,
            strings.get("browser.name"),
            Length::Fill,
            Alignment::Start
        ))
        .width(Length::Fill)
        .clip(true),
        Space::new().width(NAME_GAP),
    ];
    if columns.kind {
        header = header.push(column_button(
            SortKey::Kind,
            strings.get("browser.kind"),
            Length::Fixed(KIND_WIDTH),
            Alignment::Start,
        ));
    }
    header = header.push(column_button(
        SortKey::Size,
        strings.get("browser.size"),
        Length::Fixed(SIZE_WIDTH),
        Alignment::End,
    ));
    if columns.modified {
        header = header
            .push(Space::new().width(MODIFIED_GAP))
            .push(column_button(
                SortKey::Modified,
                strings.get("browser.modified"),
                Length::Fixed(MODIFIED_WIDTH),
                Alignment::Start,
            ));
    }
    let column_label = |label: &'a str, width: f32| {
        text(label)
            .size(13)
            .width(width)
            .wrapping(text::Wrapping::None)
    };
    if columns.permissions {
        header = header
            .push(Space::new().width(MODIFIED_GAP))
            .push(column_label(
                strings.get("browser.permissions"),
                PERMISSIONS_WIDTH,
            ));
    }
    if columns.owner {
        header = header
            .push(Space::new().width(MODIFIED_GAP))
            .push(column_label(strings.get("browser.owner"), OWNER_WIDTH));
    }
    header
        .push(Space::new().width(SCROLLBAR_GAP))
        .padding([0, ROW_PADDING[1]])
        .into()
}

fn entry_list<'a>(
    strings: &'a Strings,
    tab: &'a Tab,
    comparison: Option<&'a Comparison>,
    columns: Columns,
) -> Element<'a, Message> {
    if tab.entries.is_empty() {
        let message = if tab.state == LoadState::Loading {
            strings.get("browser.loading")
        } else {
            strings.get("browser.empty")
        };
        return container(text(message)).center(Length::Fill).into();
    }
    let style = RowStyle {
        separator: strings.get("number.decimal_separator"),
        date_pattern: strings.get("date.format"),
        directory_marker: if tab.file_system.label().is_some() {
            '/'
        } else {
            std::path::MAIN_SEPARATOR
        },
    };
    let visible: Vec<(usize, &Entry)> = tab
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            tab.shows(entry)
                && !comparison.is_some_and(|comparison| comparison.hides(tab.id, entry))
        })
        .collect();
    if visible.is_empty() && tab.is_filtered() {
        return container(text(strings.get("filter.nothing")))
            .center(Length::Fill)
            .into();
    }
    if visible.is_empty() {
        return container(text(strings.get("compare.no_differences")))
            .center(Length::Fill)
            .into();
    }
    let rows = visible.into_iter().map(|(index, entry)| {
        let mark =
            comparison.map(|comparison| comparison.mark(tab.id, entry).unwrap_or(Mark::Equal));
        entry_row(
            strings,
            &style,
            tab.id,
            (index, entry),
            tab.is_selected(index),
            mark,
            columns,
        )
    });
    let id = tab.id;
    scrollable(Column::with_children(rows).padding(iced::Padding::ZERO.right(SCROLLBAR_GAP)))
        .id(ui::entry_list_id(id))
        .on_scroll(move |viewport| Message::Tab(id, TabMessage::Scrolled(viewport)))
        .height(Length::Fill)
        .into()
}

struct RowStyle<'a> {
    separator: &'a str,
    date_pattern: &'a str,
    directory_marker: char,
}

fn entry_row<'a>(
    strings: &'a Strings,
    style: &RowStyle<'_>,
    id: TabId,
    (index, entry): (usize, &'a Entry),
    selected: bool,
    mark: Option<Mark>,
    columns: Columns,
) -> Element<'a, Message> {
    let size = entry
        .size
        .map(|bytes| format::size(bytes, style.separator))
        .unwrap_or_default();
    let modified = entry
        .modified
        .and_then(local_time::local)
        .map(|time| local_time::format(&time, style.date_pattern))
        .unwrap_or_default();
    let name = if entry.is_directory() {
        format!("{}{}", entry.name, style.directory_marker)
    } else {
        entry.name.clone()
    };
    let marker = mark.map(|mark| {
        text(mark.symbol())
            .size(14)
            .width(MARK_WIDTH)
            .style(move |theme| text::Style {
                color: (!selected).then(|| theme::mark_color(theme, mark)),
            })
    });
    let mut cells = row![
        container(text(name).size(14).wrapping(text::Wrapping::None))
            .width(Length::Fill)
            .clip(true),
        Space::new().width(NAME_GAP),
    ];
    if columns.kind {
        cells = cells.push(
            text(kind_label(strings, entry))
                .size(13)
                .width(KIND_WIDTH)
                .wrapping(text::Wrapping::None),
        );
    }
    cells = cells.push(
        text(size)
            .size(13)
            .width(SIZE_WIDTH)
            .align_x(Alignment::End)
            .wrapping(text::Wrapping::None),
    );
    if columns.modified {
        cells = cells.push(Space::new().width(MODIFIED_GAP)).push(
            text(modified)
                .size(13)
                .width(MODIFIED_WIDTH)
                .wrapping(text::Wrapping::None),
        );
    }
    if columns.permissions {
        let permissions = entry
            .permissions
            .map(hoplix_core::permission_text)
            .unwrap_or_default();
        cells = cells.push(Space::new().width(MODIFIED_GAP)).push(
            text(permissions)
                .size(13)
                .font(iced::Font::MONOSPACE)
                .width(PERMISSIONS_WIDTH)
                .wrapping(text::Wrapping::None),
        );
    }
    if columns.owner {
        cells = cells.push(Space::new().width(MODIFIED_GAP)).push(
            container(
                text(entry.owner.as_deref().unwrap_or_default())
                    .size(13)
                    .wrapping(text::Wrapping::None),
            )
            .width(OWNER_WIDTH)
            .clip(true),
        );
    }
    let content = Row::new()
        .push(marker)
        .push(cells.align_y(Alignment::Center))
        .align_y(Alignment::Center);
    let even = index.is_multiple_of(2);
    mouse_area(
        container(content)
            .padding(ROW_PADDING)
            .width(Length::Fill)
            .style(move |theme| match mark {
                Some(mark) => theme::compared_row(theme, selected, even, mark),
                None => theme::row(theme, selected, even),
            }),
    )
    .on_press(Message::Tab(id, TabMessage::Select(index)))
    .on_double_click(Message::Tab(id, TabMessage::Open(index)))
    .into()
}

pub(super) fn kind_label(strings: &Strings, entry: &Entry) -> String {
    if entry.is_directory() {
        return strings.get("browser.kind.folder").to_string();
    }
    match entry
        .name
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
    {
        Some((_, extension)) => strings.format(
            "browser.kind.file_with_extension",
            &[("extension", &extension.to_uppercase())],
        ),
        None => strings.get("browser.kind.file").to_string(),
    }
}
