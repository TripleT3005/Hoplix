use hoplix_core::{SearchHit, permission_text};
use iced::widget::{
    Column, Row, Space, button, checkbox, column, container, mouse_area, pick_list, row,
    scrollable, text, text_input,
};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, Message, Tab, TabId};
use crate::file_list::{BookmarkChoice, BookmarksDialog, ListMessage};
use crate::i18n::Strings;
use crate::properties::{Loaded, PropertiesDialog, PropertiesMessage, SizeState};
use crate::search::{SearchDialog, SearchMessage, SearchResults};
use crate::ui::Choice;
use crate::{format, local_time, theme, ui};

// Constants

const BOOKMARKS_WIDTH: f32 = 150.0;
const RESULT_SIZE_WIDTH: f32 = 90.0;
const RESULT_MODIFIED_WIDTH: f32 = 130.0;
const LABEL_WIDTH: f32 = 120.0;
const PERMISSION_COLUMN: f32 = 80.0;
const BOOKMARK_LIST_HEIGHT: f32 = 320.0;
const ROW_PADDING: [u16; 2] = [3, 8];
const PERMISSION_BITS: [(&str, [u32; 3]); 3] = [
    ("properties.owner_access", [0o400, 0o200, 0o100]),
    ("properties.group_access", [0o040, 0o020, 0o010]),
    ("properties.other_access", [0o004, 0o002, 0o001]),
];

// Toolbar

pub fn history_buttons<'a>(tab: &'a Tab) -> [Element<'a, Message>; 2] {
    let id = tab.id;
    let back = button(text("←"))
        .style(theme::secondary_button)
        .on_press_maybe(
            (!tab.list.back.is_empty()).then_some(Message::List(ListMessage::Back(id))),
        );
    let forward = button(text("→"))
        .style(theme::secondary_button)
        .on_press_maybe(
            (!tab.list.forward.is_empty()).then_some(Message::List(ListMessage::Forward(id))),
        );
    [back.into(), forward.into()]
}

pub fn bookmark_picker<'a>(app: &'a Hoplix, tab: &'a Tab) -> Option<Element<'a, Message>> {
    let strings = &app.strings;
    let bookmarks = app.tab_bookmarks(tab)?;
    let id = tab.id;
    let mut choices: Vec<Choice<BookmarkChoice>> = bookmarks
        .items
        .iter()
        .map(|bookmark| Choice {
            value: BookmarkChoice::Open(bookmark.path.clone()),
            label: bookmark.name.clone(),
        })
        .collect();
    if !tab.path.is_empty() && !bookmarks.contains(&tab.path) {
        choices.push(Choice {
            value: BookmarkChoice::Add,
            label: strings.get("bookmarks.add").to_string(),
        });
    }
    choices.push(Choice {
        value: BookmarkChoice::Edit,
        label: strings.get("bookmarks.edit").to_string(),
    });
    let marker = if bookmarks.contains(&tab.path) {
        "★"
    } else {
        "☆"
    };
    Some(
        pick_list(choices, None::<Choice<BookmarkChoice>>, move |choice| {
            Message::List(ListMessage::Bookmark(id, choice.value))
        })
        .placeholder(format!("{marker} {}", strings.get("bookmarks.title")))
        .text_size(13)
        .width(BOOKMARKS_WIDTH)
        .into(),
    )
}

pub fn filter_row<'a>(strings: &'a Strings, tab: &'a Tab) -> Element<'a, Message> {
    let id = tab.id;
    let mut filter = row![
        text_input(strings.get("filter.placeholder"), &tab.list.filter)
            .id(ui::filter_input_id(id))
            .on_input(move |value| Message::List(ListMessage::Filter(id, value)))
            .size(13)
            .padding(5),
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    if !tab.list.filter.is_empty() {
        filter = filter.push(
            button(text("×").size(13))
                .padding([2, 8])
                .style(button::text)
                .on_press(Message::List(ListMessage::ClearFilter(id))),
        );
    }
    filter
        .push(
            button(text(strings.get("search.open")).size(13))
                .style(button::text)
                .on_press_maybe(
                    (!tab.path.is_empty()).then_some(Message::Search(SearchMessage::Open)),
                ),
        )
        .into()
}

// Search results

pub fn results_view<'a>(
    strings: &'a Strings,
    tab: &'a Tab,
    results: &'a SearchResults,
    show_modified: bool,
) -> Element<'a, Message> {
    let id = tab.id;
    let count = results.hits.len().to_string();
    let summary = if results.running {
        strings.format(
            "search.running",
            &[("pattern", &results.pattern), ("count", &count)],
        )
    } else {
        strings.format(
            "search.done",
            &[("pattern", &results.pattern), ("count", &count)],
        )
    };
    let mut bar = row![
        text(summary)
            .size(13)
            .width(Length::Fill)
            .wrapping(text::Wrapping::WordOrGlyph)
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if results.running {
        bar = bar.push(
            button(text(strings.get("search.cancel")).size(13))
                .style(theme::secondary_button)
                .on_press(Message::Search(SearchMessage::Cancel(id))),
        );
    }
    bar = bar.push(
        button(text(strings.get("search.close")).size(13))
            .style(theme::secondary_button)
            .on_press(Message::Search(SearchMessage::Close(id))),
    );
    let mut view = column![bar].spacing(6);
    if results.unreadable > 0 {
        view = view.push(
            text(strings.format(
                "search.unreadable",
                &[("count", &results.unreadable.to_string())],
            ))
            .size(12),
        );
    }
    if results.truncated {
        view = view.push(
            text(strings.format(
                "search.truncated",
                &[("count", &crate::search::MAX_RESULTS.to_string())],
            ))
            .size(12),
        );
    }
    if results.hits.is_empty() {
        let message = if results.running {
            strings.get("browser.loading")
        } else {
            strings.get("search.nothing")
        };
        return view
            .push(container(text(message)).center(Length::Fill))
            .into();
    }
    let mut header = row![
        text(strings.get("browser.name"))
            .size(13)
            .width(Length::FillPortion(2))
            .wrapping(text::Wrapping::None),
        text(strings.get("search.folder"))
            .size(13)
            .width(Length::FillPortion(3))
            .wrapping(text::Wrapping::None),
        text(strings.get("browser.size"))
            .size(13)
            .width(RESULT_SIZE_WIDTH)
            .align_x(Alignment::End)
            .wrapping(text::Wrapping::None),
    ];
    if show_modified {
        header = header.push(Space::new().width(16)).push(
            text(strings.get("browser.modified"))
                .size(13)
                .width(RESULT_MODIFIED_WIDTH)
                .wrapping(text::Wrapping::None),
        );
    }
    let header = header
        .push(Space::new().width(12))
        .padding([0, ROW_PADDING[1]]);
    let marker = if tab.file_system.is_remote() {
        '/'
    } else {
        std::path::MAIN_SEPARATOR
    };
    let rows =
        results.hits.iter().enumerate().map(|(index, hit)| {
            result_row(strings, id, results, (index, hit), marker, show_modified)
        });
    view.push(header)
        .push(
            scrollable(Column::with_children(rows).padding(iced::Padding::ZERO.right(12)))
                .id(ui::results_list_id(id))
                .height(Length::Fill),
        )
        .into()
}

fn result_row<'a>(
    strings: &'a Strings,
    id: TabId,
    results: &'a SearchResults,
    (index, hit): (usize, &'a SearchHit),
    marker: char,
    show_modified: bool,
) -> Element<'a, Message> {
    let selected = results.selected == Some(index);
    let name = if hit.entry.is_directory() {
        format!("{}{marker}", hit.entry.name)
    } else {
        hit.entry.name.clone()
    };
    let folder = relative_folder(&results.root, &hit.directory);
    let size = hit
        .entry
        .size
        .map(|bytes| format::size(bytes, strings.get("number.decimal_separator")))
        .unwrap_or_default();
    let modified = hit
        .entry
        .modified
        .and_then(local_time::local)
        .map(|time| local_time::format(&time, strings.get("date.format")))
        .unwrap_or_default();
    let mut content = row![
        container(text(name).size(14).wrapping(text::Wrapping::None))
            .width(Length::FillPortion(2))
            .clip(true),
        container(
            text(if folder.is_empty() {
                strings.get("search.here").to_string()
            } else {
                folder
            })
            .size(13)
            .wrapping(text::Wrapping::None)
        )
        .width(Length::FillPortion(3))
        .clip(true),
        text(size)
            .size(13)
            .width(RESULT_SIZE_WIDTH)
            .align_x(Alignment::End)
            .wrapping(text::Wrapping::None),
    ]
    .align_y(Alignment::Center);
    if show_modified {
        content = content.push(Space::new().width(16)).push(
            text(modified)
                .size(13)
                .width(RESULT_MODIFIED_WIDTH)
                .wrapping(text::Wrapping::None),
        );
    }
    let even = index.is_multiple_of(2);
    mouse_area(
        container(content)
            .padding(ROW_PADDING)
            .width(Length::Fill)
            .style(move |theme| theme::row(theme, selected, even)),
    )
    .on_press(Message::Search(SearchMessage::Select(id, index)))
    .on_double_click(Message::Search(SearchMessage::Reveal(id, index)))
    .into()
}

fn relative_folder(root: &str, directory: &str) -> String {
    directory
        .strip_prefix(root)
        .unwrap_or(directory)
        .trim_start_matches(['/', '\\'])
        .to_string()
}

// Search dialog

pub fn search_body<'a>(app: &'a Hoplix, dialog: &'a SearchDialog) -> Element<'a, Message> {
    let strings = &app.strings;
    let path = app
        .tab_by_id(dialog.tab)
        .map(|tab| tab.path.as_str())
        .unwrap_or_default();
    let submit = dialog.can_start().then_some(Message::DialogConfirmed);
    let mut body = column![
        text(strings.format("search.text", &[("path", path)]))
            .wrapping(text::Wrapping::WordOrGlyph),
        text_input(strings.get("search.pattern"), &dialog.pattern)
            .id(ui::dialog_input_id())
            .on_input(|value| Message::Search(SearchMessage::Pattern(value)))
            .on_submit_maybe(submit.clone())
            .padding(8),
        text(strings.get("search.pattern_hint")).size(12),
        row![
            text(strings.get("search.depth")).width(Length::Fill),
            text_input(strings.get("search.depth_unlimited"), &dialog.depth)
                .on_input(|value| Message::Search(SearchMessage::Depth(value)))
                .on_submit_maybe(submit)
                .width(140)
                .padding(6),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(10);
    if !dialog.can_start() {
        body = body.push(text(strings.get("search.depth_invalid")).size(13));
    }
    body.into()
}

// Bookmarks dialog

pub fn bookmarks_body<'a>(app: &'a Hoplix, dialog: &'a BookmarksDialog) -> Element<'a, Message> {
    let strings = &app.strings;
    if dialog.items.is_empty() {
        return text(strings.get("bookmarks.none")).into();
    }
    let items = dialog.items.iter().enumerate().map(|(index, bookmark)| {
        column![
            row![
                text_input(strings.get("operation.name"), &bookmark.name)
                    .on_input(move |name| Message::List(ListMessage::BookmarkRenamed(index, name)))
                    .padding(6),
                button(text(strings.get("bookmarks.remove")).size(13))
                    .style(theme::secondary_button)
                    .on_press(Message::List(ListMessage::BookmarkRemoved(index))),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
            text(&bookmark.path)
                .size(12)
                .wrapping(text::Wrapping::WordOrGlyph),
        ]
        .spacing(2)
        .into()
    });
    container(scrollable(
        Column::with_children(items)
            .spacing(10)
            .padding(iced::Padding::ZERO.right(12)),
    ))
    .max_height(BOOKMARK_LIST_HEIGHT)
    .into()
}

// Properties dialog

pub fn properties_body<'a>(app: &'a Hoplix, dialog: &'a PropertiesDialog) -> Element<'a, Message> {
    let strings = &app.strings;
    let properties = match &dialog.loaded {
        Loaded::Loading => return text(strings.get("browser.loading")).into(),
        Loaded::Failed(error) => return ui::notice(ui::error_text(strings, error)),
        Loaded::Ready(properties) => properties,
    };
    let entry = &properties.entry;
    let kind = match (entry.is_directory(), entry.is_link) {
        (true, false) => strings.get("browser.kind.folder"),
        (false, false) => strings.get("browser.kind.file"),
        (true, true) => strings.get("properties.folder_link"),
        (false, true) => strings.get("properties.file_link"),
    };
    let separator = strings.get("number.decimal_separator");
    let mut body = column![
        field(strings.get("operation.name"), text(&dialog.name).into()),
        field(
            strings.get("browser.path"),
            text(&dialog.path)
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph)
                .into()
        ),
        field(strings.get("browser.kind"), text(kind).into()),
    ]
    .spacing(8);
    let size: Element<'a, Message> = if entry.is_directory() {
        folder_size(strings, dialog.size)
    } else {
        text(
            entry
                .size
                .map(|bytes| {
                    strings.format(
                        "properties.bytes",
                        &[
                            ("size", &format::size(bytes, separator)),
                            ("bytes", &bytes.to_string()),
                        ],
                    )
                })
                .unwrap_or_default(),
        )
        .into()
    };
    body = body.push(field(strings.get("browser.size"), size));
    if let Some(modified) = entry
        .modified
        .and_then(local_time::local)
        .map(|time| local_time::format(&time, strings.get("date.format")))
    {
        body = body.push(field(
            strings.get("browser.modified"),
            text(modified).into(),
        ));
    }
    if let Some(owner) = &properties.owner {
        body = body.push(field(strings.get("properties.owner"), text(owner).into()));
    }
    if let Some(group) = &properties.group {
        body = body.push(field(strings.get("properties.group"), text(group).into()));
    }
    if let Some(attributes) = properties.attributes {
        let yes_no = |value: bool| {
            strings.get(if value {
                "properties.yes"
            } else {
                "properties.no"
            })
        };
        body = body
            .push(field(
                strings.get("properties.read_only"),
                text(yes_no(attributes.read_only)).into(),
            ))
            .push(field(
                strings.get("properties.hidden"),
                text(yes_no(attributes.hidden)).into(),
            ));
    }
    if dialog.shows_permissions() {
        body = body.push(permissions(strings, dialog, properties.permissions));
    }
    if dialog.applying {
        body = body.push(text(strings.get("properties.applying")).size(13));
    }
    if let Some(error) = &dialog.error {
        body = body.push(ui::notice(ui::error_text(strings, error)));
    }
    body.into()
}

pub fn properties_confirm(dialog: &PropertiesDialog) -> &'static str {
    if dialog.can_change && dialog.shows_permissions() {
        "properties.apply"
    } else {
        "properties.close"
    }
}

fn field<'a>(label: &'a str, value: Element<'a, Message>) -> Element<'a, Message> {
    row![text(label).size(13).width(LABEL_WIDTH), value]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

fn folder_size<'a>(strings: &'a Strings, size: SizeState) -> Element<'a, Message> {
    match size {
        SizeState::Idle => button(text(strings.get("properties.count")).size(13))
            .style(theme::secondary_button)
            .on_press(Message::Properties(PropertiesMessage::CountSize))
            .into(),
        SizeState::Counting => row![
            text(strings.get("properties.counting")).size(13),
            button(text(strings.get("search.cancel")).size(13))
                .style(theme::secondary_button)
                .on_press(Message::Properties(PropertiesMessage::CancelSize)),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into(),
        SizeState::Done(size) => {
            let separator = strings.get("number.decimal_separator");
            let mut summary = strings.format(
                "properties.folder_size",
                &[
                    ("size", &format::size(size.bytes, separator)),
                    ("files", &size.files.to_string()),
                    ("folders", &size.folders.to_string()),
                ],
            );
            if size.unreadable > 0 {
                summary.push(' ');
                summary.push_str(&strings.format(
                    "properties.unreadable",
                    &[("count", &size.unreadable.to_string())],
                ));
            }
            text(summary)
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph)
                .into()
        }
    }
}

fn permissions<'a>(
    strings: &'a Strings,
    dialog: &'a PropertiesDialog,
    original: Option<u32>,
) -> Element<'a, Message> {
    let mode = dialog.edited_mode().or(original);
    let editable = dialog.can_change && !dialog.applying;
    let letters = mode.map(permission_text).unwrap_or_default();
    let mut octal = text_input("755", &dialog.mode).width(80).padding(6);
    if editable {
        octal = octal.on_input(|value| Message::Properties(PropertiesMessage::Mode(value)));
    }
    let mut grid = column![
        row![
            Space::new().width(LABEL_WIDTH),
            text(strings.get("properties.read"))
                .size(12)
                .width(PERMISSION_COLUMN),
            text(strings.get("properties.write"))
                .size(12)
                .width(PERMISSION_COLUMN),
            text(strings.get("properties.execute"))
                .size(12)
                .width(PERMISSION_COLUMN),
        ]
        .spacing(8)
    ]
    .spacing(4);
    for (label, bits) in PERMISSION_BITS {
        let mut line = Row::new()
            .push(text(strings.get(label)).size(13).width(LABEL_WIDTH))
            .spacing(8)
            .align_y(Alignment::Center);
        for bit in bits {
            let checked = mode.is_some_and(|mode| mode & bit != 0);
            let mut cell = checkbox(checked).size(16);
            if editable {
                cell =
                    cell.on_toggle(move |on| Message::Properties(PropertiesMessage::Bit(bit, on)));
            }
            line = line.push(container(cell).width(PERMISSION_COLUMN));
        }
        grid = grid.push(line);
    }
    let mut section = column![
        field(
            strings.get("properties.permissions"),
            row![octal, text(letters).font(iced::Font::MONOSPACE)]
                .spacing(10)
                .align_y(Alignment::Center)
                .into()
        ),
        grid,
    ]
    .spacing(8);
    if dialog.edited_mode().is_none() && editable {
        section = section.push(text(strings.get("properties.invalid_mode")).size(13));
    }
    if !dialog.can_change {
        section = section.push(text(strings.get("properties.read_only_server")).size(13));
    } else if dialog.is_directory() {
        let mut recursive = column![
            checkbox(dialog.recursive)
                .label(strings.get("properties.recursive"))
                .on_toggle_maybe(editable.then_some(|value| {
                    Message::Properties(PropertiesMessage::Recursive(value))
                })),
        ]
        .spacing(6);
        if dialog.recursive {
            recursive = recursive.push(
                row![
                    Space::new().width(24),
                    checkbox(dialog.files)
                        .label(strings.get("properties.apply_files"))
                        .on_toggle_maybe(editable.then_some(|value| {
                            Message::Properties(PropertiesMessage::Files(value))
                        })),
                    checkbox(dialog.folders)
                        .label(strings.get("properties.apply_folders"))
                        .on_toggle_maybe(editable.then_some(|value| {
                            Message::Properties(PropertiesMessage::Folders(value))
                        })),
                ]
                .spacing(16),
            );
        }
        section = section.push(recursive);
    }
    section.into()
}
