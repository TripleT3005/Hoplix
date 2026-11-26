use hoplix_core::{Entry, SortKey, SortOrder};
use iced::widget::{
    Column, Row, Space, button, column, container, mouse_area, pick_list, row, scrollable, text,
    text_input,
};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, LoadState, Message, Pane, Tab, TabId, TabMessage};
use crate::compare::{Comparison, Mark};
use crate::i18n::Strings;
use crate::settings::Layout;
use crate::transfers::DropTarget;
use crate::ui::Choice;
use crate::{format, local_time, theme, ui};

// Constants

const KIND_WIDTH: f32 = 110.0;
const SIZE_WIDTH: f32 = 110.0;
const MODIFIED_WIDTH: f32 = 130.0;
const ROW_PADDING: [u16; 2] = [3, 8];
const DESTINATION_WIDTH: f32 = 220.0;
const MARK_WIDTH: f32 = 18.0;

// Browser

pub fn view(app: &Hoplix) -> Element<'_, Message> {
    let show_focus = app.settings.layout == Layout::Classic;
    let dragging = app.drag.as_ref().is_some_and(|drag| drag.moved);
    let panes = app.panes.iter().enumerate().map(|(index, pane)| {
        let focused = show_focus && index == app.focused_pane;
        let dropping = dragging
            && match app.drop_target {
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
    let dragging = app.drag.as_ref().is_some_and(|drag| drag.moved);
    let tabs = pane.tabs.iter().enumerate().map(|(position, tab)| {
        let active = position == pane.active
            || (dragging && app.drop_target == Some(DropTarget::Tab(tab.id)));
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

// Tab content

fn view_tab<'a>(app: &'a Hoplix, pane: usize, tab: &'a Tab) -> Element<'a, Message> {
    let strings = &app.strings;
    let id = tab.id;
    let [back, forward] = ui::file_list::history_buttons(tab);
    let mut toolbar = row![
        back,
        forward,
        button(text(strings.get("browser.up")))
            .style(theme::secondary_button)
            .on_press_maybe(
                tab.file_system
                    .parent(&tab.path)
                    .map(|_| Message::Tab(id, TabMessage::Up))
            ),
        button(text(strings.get("browser.refresh")))
            .style(theme::secondary_button)
            .on_press_maybe(
                (!tab.path.is_empty()).then_some(Message::Tab(id, TabMessage::Refresh))
            ),
        text_input(strings.get("browser.path"), &tab.path_input)
            .id(ui::path_input_id(id))
            .on_input(move |path| Message::Tab(id, TabMessage::PathEdited(path)))
            .on_submit(Message::Tab(id, TabMessage::PathSubmitted))
            .padding(6),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if let Some(picker) = ui::file_list::bookmark_picker(app, tab) {
        toolbar = toolbar.push(picker);
    }
    let ready = !tab.path.is_empty() && tab.state != LoadState::Loading;
    let has_single = ready && tab.selected_entry().is_some();
    let has_any = ready && !tab.selection.is_empty();
    let has_file = ready
        && tab
            .selected_entry()
            .is_some_and(|entry| !entry.is_directory());
    let mut actions = row![
        button(text(strings.get("operation.new_folder")).size(13))
            .style(button::text)
            .on_press_maybe(ready.then_some(Message::Tab(id, TabMessage::NewFolder))),
        button(text(strings.get("operation.rename")).size(13))
            .style(button::text)
            .on_press_maybe(has_single.then_some(Message::Tab(id, TabMessage::Rename))),
        button(text(strings.get("operation.delete")).size(13))
            .style(button::text)
            .on_press_maybe(has_any.then_some(Message::Tab(id, TabMessage::Delete))),
        button(text(strings.get("operation.edit")).size(13))
            .style(button::text)
            .on_press_maybe(has_file.then_some(Message::Tab(id, TabMessage::Edit))),
        Space::new().width(Length::Fill),
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
            button(text(label).size(13))
                .style(theme::secondary_button)
                .on_press_maybe(has_any.then_some(Message::Tab(id, TabMessage::CopyToOtherPane))),
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
        actions = actions.push(if has_any {
            Element::from(picker)
        } else {
            Element::from(
                button(text(strings.get("transfer.copy_to")).size(13))
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
    let mut content = column![toolbar, actions].spacing(6).padding(4);
    if let LoadState::Failed(error) = &tab.state {
        content = content.push(ui::notice(ui::error_text(strings, error)));
    }
    content = content.push(ui::file_list::filter_row(strings, tab));
    let list: Element<'a, Message> = match &tab.search {
        Some(results) => ui::file_list::results_view(strings, tab, results),
        None => {
            let comparison = app
                .compare
                .comparison
                .as_ref()
                .filter(|comparison| comparison.involves(id));
            column![
                header(strings, tab, comparison.is_some()),
                entry_list(strings, tab, comparison)
            ]
            .spacing(6)
            .into()
        }
    };
    content.push(content_area(list)).into()
}

// Content area

fn content_area(list: Element<'_, Message>) -> Element<'_, Message> {
    Row::new()
        .push(container(list).width(Length::Fill).height(Length::Fill))
        .spacing(8)
        .height(Length::Fill)
        .into()
}

fn header<'a>(strings: &'a Strings, tab: &'a Tab, comparing: bool) -> Element<'a, Message> {
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
                .align_x(alignment),
        )
        .padding([ROW_PADDING[0], 0])
        .width(width)
        .style(button::text)
        .on_press(Message::Tab(id, TabMessage::SortBy(key)))
    };
    row![
        Space::new().width(if comparing { MARK_WIDTH } else { 0.0 }),
        column_button(
            SortKey::Name,
            strings.get("browser.name"),
            Length::Fill,
            Alignment::Start
        ),
        Space::new().width(8),
        column_button(
            SortKey::Kind,
            strings.get("browser.kind"),
            Length::Fixed(KIND_WIDTH),
            Alignment::Start
        ),
        column_button(
            SortKey::Size,
            strings.get("browser.size"),
            Length::Fixed(SIZE_WIDTH),
            Alignment::End
        ),
        Space::new().width(16),
        column_button(
            SortKey::Modified,
            strings.get("browser.modified"),
            Length::Fixed(MODIFIED_WIDTH),
            Alignment::Start
        ),
        Space::new().width(12),
    ]
    .padding([0, ROW_PADDING[1]])
    .into()
}

fn entry_list<'a>(
    strings: &'a Strings,
    tab: &'a Tab,
    comparison: Option<&'a Comparison>,
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
        )
    });
    let id = tab.id;
    scrollable(Column::with_children(rows).padding(iced::Padding::ZERO.right(12)))
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
    let content = Row::new()
        .push(marker)
        .push(
            row![
                container(text(name).size(14).wrapping(text::Wrapping::None))
                    .width(Length::Fill)
                    .clip(true),
                Space::new().width(8),
                text(kind_label(strings, entry)).size(13).width(KIND_WIDTH),
                text(size)
                    .size(13)
                    .width(SIZE_WIDTH)
                    .align_x(Alignment::End),
                Space::new().width(16),
                text(modified).size(13).width(MODIFIED_WIDTH),
            ]
            .spacing(0)
            .align_y(Alignment::Center),
        )
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

fn kind_label(strings: &Strings, entry: &Entry) -> String {
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
