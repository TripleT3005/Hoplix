use hoplix_core::{Side, SyncAction, SyncDirection, SyncMode, SyncPlan};
use iced::widget::{
    Column, Space, button, checkbox, column, container, pick_list, row, scrollable, text,
    text_input,
};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, Message, TabId};
use crate::compare::{CompareMessage, Mark, SyncDialog};
use crate::ui::Choice;
use crate::{format, theme, ui};

// Constants

const COMPARE_WIDTH: f32 = 220.0;
const PREVIEW_LIMIT: usize = 200;
const PREVIEW_HEIGHT: f32 = 160.0;
const LEGEND: [Mark; 6] = [
    Mark::OnlyHere,
    Mark::NewerHere,
    Mark::OlderHere,
    Mark::SizeDiffers,
    Mark::KindDiffers,
    Mark::ContentDiffers,
];

// Top bar

pub fn top_bar_control(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    if app.compare.comparison.is_some() {
        return button(text(strings.get("compare.stop")))
            .style(theme::secondary_button)
            .on_press(Message::Compare(CompareMessage::Stop))
            .into();
    }
    if app.panes.len() > 1 {
        return button(text(strings.get("compare.open")))
            .style(theme::secondary_button)
            .on_press(Message::Compare(CompareMessage::Toggle))
            .into();
    }
    let choices: Vec<Choice<TabId>> = app
        .focused_tab()
        .map(|tab| app.transfer_destinations(tab.id))
        .unwrap_or_default()
        .into_iter()
        .map(|(value, label)| Choice { value, label })
        .collect();
    if choices.is_empty() {
        return button(text(strings.get("compare.with")))
            .style(theme::secondary_button)
            .into();
    }
    pick_list(choices, None::<Choice<TabId>>, |choice| {
        Message::Compare(CompareMessage::With(choice.value))
    })
    .placeholder(strings.get("compare.with"))
    .width(COMPARE_WIDTH)
    .into()
}

// Comparison bar

pub fn bar(app: &Hoplix) -> Option<Element<'_, Message>> {
    let comparison = app.compare.comparison.as_ref()?;
    let strings = &app.strings;
    let title = |id| app.tab_by_id(id).map(ui::tab_title).unwrap_or_default();
    let status = if comparison.loading {
        strings.get("compare.loading").to_string()
    } else {
        let counts = comparison.counts();
        let differences = counts.only_left
            + counts.only_right
            + counts.newer_left
            + counts.newer_right
            + counts.different;
        strings.format(
            "compare.summary",
            &[
                ("differences", &differences.to_string()),
                ("equal", &counts.equal.to_string()),
            ],
        )
    };
    let legend = LEGEND.iter().map(|mark| {
        let mark = *mark;
        row![
            text(mark.symbol())
                .size(13)
                .style(move |theme| text::Style {
                    color: Some(theme::mark_color(theme, mark)),
                }),
            text(strings.get(mark.label_key())).size(12),
        ]
        .spacing(3)
        .into()
    });
    let mut content = column![
        row![
            text(strings.format(
                "compare.title",
                &[
                    ("left", &title(comparison.left)),
                    ("right", &title(comparison.right))
                ]
            ))
            .size(14),
            text(status).size(13),
            Space::new().width(Length::Fill),
            checkbox(comparison.only_differences)
                .label(strings.get("compare.only_differences"))
                .text_size(13)
                .on_toggle(|value| Message::Compare(CompareMessage::OnlyDifferences(value))),
            checkbox(comparison.recursive)
                .label(strings.get("compare.recursive"))
                .text_size(13)
                .on_toggle(|value| Message::Compare(CompareMessage::Recursive(value))),
            button(text(strings.get("sync.open")).size(13))
                .style(button::primary)
                .on_press_maybe(
                    (!comparison.loading).then_some(Message::Compare(CompareMessage::OpenSync))
                ),
            button(text(strings.get("compare.stop")).size(13))
                .style(theme::secondary_button)
                .on_press(Message::Compare(CompareMessage::Stop)),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
        iced::widget::Row::with_children(legend).spacing(12),
    ]
    .spacing(4);
    if let Some(error) = &comparison.error {
        content = content.push(ui::notice(strings.format(
            "compare.failed",
            &[("detail", &ui::error_text(strings, error))],
        )));
    }
    Some(
        container(content)
            .padding([6, 10])
            .width(Length::Fill)
            .style(theme::bar)
            .into(),
    )
}

// Sync dialog

pub fn sync_body<'a>(app: &'a Hoplix, dialog: &'a SyncDialog) -> Element<'a, Message> {
    let strings = &app.strings;
    let send = |message: CompareMessage| Message::Compare(message);
    let choice = |label: String, selected: bool, message: CompareMessage| {
        button(text(label).size(13))
            .style(if selected {
                button::primary
            } else {
                theme::secondary_button
            })
            .on_press(send(message))
    };
    let names = [
        ("left", dialog.left_title.as_str()),
        ("right", dialog.right_title.as_str()),
    ];
    let directions = row![
        choice(
            strings.format("sync.direction.to_right", &names),
            dialog.direction == SyncDirection::LeftToRight,
            CompareMessage::Direction(SyncDirection::LeftToRight),
        ),
        choice(
            strings.format("sync.direction.to_left", &names),
            dialog.direction == SyncDirection::RightToLeft,
            CompareMessage::Direction(SyncDirection::RightToLeft),
        ),
        choice(
            strings.get("sync.direction.both").to_string(),
            dialog.direction == SyncDirection::Both,
            CompareMessage::Direction(SyncDirection::Both),
        ),
    ]
    .spacing(6);
    let mut body = column![directions].spacing(10);
    if dialog.direction != SyncDirection::Both {
        body = body.push(
            row![
                choice(
                    strings.get("sync.mode.update").to_string(),
                    dialog.mode == SyncMode::Update,
                    CompareMessage::Mode(SyncMode::Update),
                ),
                choice(
                    strings.get("sync.mode.mirror").to_string(),
                    dialog.mode == SyncMode::Mirror,
                    CompareMessage::Mode(SyncMode::Mirror),
                ),
            ]
            .spacing(6),
        );
    }
    let toggle = |value: bool, key: &'static str, message: fn(bool) -> CompareMessage| {
        checkbox(value)
            .label(strings.get(key))
            .text_size(13)
            .on_toggle(move |value| Message::Compare(message(value)))
    };
    let mut options = column![
        toggle(
            dialog.recursive,
            "sync.recursive",
            CompareMessage::SyncRecursive
        ),
        toggle(dialog.size_only, "sync.size_only", CompareMessage::SizeOnly),
        toggle(
            dialog.skip_hidden,
            "sync.skip_hidden",
            CompareMessage::SkipHidden
        ),
    ]
    .spacing(6);
    if dialog.can_delete() {
        options = options.push(toggle(
            dialog.delete_extra,
            "sync.delete_extra",
            CompareMessage::DeleteExtra,
        ));
    }
    body = body.push(options).push(
        text_input(strings.get("sync.exclude"), &dialog.exclude)
            .on_input(|value| Message::Compare(CompareMessage::Exclude(value)))
            .size(13)
            .padding(6),
    );
    if let Some(error) = &dialog.error {
        return body
            .push(ui::notice(strings.format(
                "compare.failed",
                &[("detail", &ui::error_text(strings, error))],
            )))
            .into();
    }
    let Some(plan) = dialog.plan() else {
        return body.push(text(strings.get("sync.loading")).size(13)).into();
    };
    body = body.push(summary(app, dialog, &plan));
    if plan.is_empty() {
        return body.into();
    }
    body = body.push(preview(strings, &plan));
    let deletions = plan.summary().deletions;
    if deletions > 0 {
        let side = deletion_side(&plan);
        let name = match side {
            Some(Side::Left) => &dialog.left_title,
            _ => &dialog.right_title,
        };
        body = body.push(ui::warning(strings.format(
            "sync.delete_warning",
            &[("count", &deletions.to_string()), ("name", name)],
        )));
        body = body.push(
            checkbox(dialog.confirm_deletions)
                .label(strings.get("sync.confirm_delete"))
                .text_size(13)
                .on_toggle(|value| Message::Compare(CompareMessage::ConfirmDeletions(value))),
        );
    }
    body.into()
}

fn summary<'a>(app: &'a Hoplix, dialog: &SyncDialog, plan: &SyncPlan) -> Element<'a, Message> {
    let strings = &app.strings;
    if plan.is_empty() && plan.conflicts.is_empty() {
        return text(strings.get("sync.summary.nothing")).size(13).into();
    }
    let separator = strings.get("number.decimal_separator");
    let size = |bytes: u64| format::size(bytes, separator);
    let summary = plan.summary();
    let remote = |id| {
        app.tab_by_id(id)
            .is_some_and(|tab| tab.file_system.label().is_some())
    };
    let (left_remote, right_remote) = (remote(dialog.left), remote(dialog.right));
    let copy_line = |count: usize, bytes: u64, to_remote: bool, from_remote: bool, name: &str| {
        let key = match (from_remote, to_remote) {
            (false, true) => "sync.summary.upload",
            (true, false) => "sync.summary.download",
            _ => "sync.summary.copy_to",
        };
        strings.format(
            key,
            &[
                ("count", &count.to_string()),
                ("size", &size(bytes)),
                ("name", name),
            ],
        )
    };
    let mut lines = Vec::new();
    if summary.to_right > 0 {
        lines.push(copy_line(
            summary.to_right,
            summary.bytes_to_right,
            right_remote,
            left_remote,
            &dialog.right_title,
        ));
    }
    if summary.to_left > 0 {
        lines.push(copy_line(
            summary.to_left,
            summary.bytes_to_left,
            left_remote,
            right_remote,
            &dialog.left_title,
        ));
    }
    if summary.directories > 0 {
        lines.push(strings.format(
            "sync.summary.folders",
            &[("count", &summary.directories.to_string())],
        ));
    }
    if summary.deletions > 0 {
        lines.push(strings.format(
            "sync.summary.delete",
            &[("count", &summary.deletions.to_string())],
        ));
    }
    if summary.conflicts > 0 {
        lines.push(strings.format(
            "sync.summary.conflicts",
            &[("count", &summary.conflicts.to_string())],
        ));
    }
    lines.push(strings.format(
        "sync.summary.total",
        &[("size", &size(summary.total_bytes()))],
    ));
    Column::with_children(lines.into_iter().map(|line| text(line).size(13).into()))
        .spacing(2)
        .into()
}

fn preview<'a>(strings: &crate::i18n::Strings, plan: &SyncPlan) -> Element<'a, Message> {
    let lines = plan.actions.iter().take(PREVIEW_LIMIT).map(|action| {
        let line = match action {
            SyncAction::Copy { from, path, .. } => format!("{} {path}", arrow(*from)),
            SyncAction::CreateDirectory { side, path } => {
                format!("{} {path}/", arrow(side.other()))
            }
            SyncAction::Delete {
                path, is_directory, ..
            } => format!("✕ {path}{}", if *is_directory { "/" } else { "" }),
        };
        text(line)
            .size(12)
            .wrapping(text::Wrapping::WordOrGlyph)
            .into()
    });
    let mut list = Column::with_children(lines).spacing(1);
    if plan.actions.len() > PREVIEW_LIMIT {
        let more = (plan.actions.len() - PREVIEW_LIMIT).to_string();
        list = list.push(text(strings.format("sync.preview.more", &[("count", &more)])).size(12));
    }
    container(scrollable(list.padding(iced::Padding::ZERO.right(12))).height(PREVIEW_HEIGHT))
        .padding(6)
        .width(Length::Fill)
        .style(container::rounded_box)
        .into()
}

fn arrow(from: Side) -> &'static str {
    match from {
        Side::Left => "→",
        Side::Right => "←",
    }
}

fn deletion_side(plan: &SyncPlan) -> Option<Side> {
    plan.actions.iter().find_map(|action| match action {
        SyncAction::Delete { side, .. } => Some(*side),
        _ => None,
    })
}
