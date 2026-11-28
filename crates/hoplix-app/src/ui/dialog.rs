use hoplix_core::{Conflict, ConflictDecision};
use iced::widget::{
    Space, button, center, checkbox, column, container, mouse_area, opaque, row, text, text_input,
};
use iced::{Alignment, Background, Color, Element, Length};

use crate::app::{Dialog, Hoplix, MasterMode, Message};
use crate::prompts::{FIRST_INPUT_ID, PromptDialog, PromptMessage};
use crate::{format, theme, ui};

// Constants

const DIALOG_WIDTH: f32 = 460.0;
const CONFLICT_WIDTH: f32 = 620.0;
const INPUT_ID: &str = "dialog-input";

// Dialog

pub fn view<'a>(app: &'a Hoplix, dialog: &'a Dialog) -> Element<'a, Message> {
    let strings = &app.strings;
    let (title, body, confirm, danger): (String, Element<'a, Message>, &str, bool) = match dialog {
        Dialog::NewFolder { name, .. } => (
            strings.get("operation.new_folder.title").to_string(),
            name_input(strings.get("operation.name"), name, dialog.can_confirm()),
            "operation.new_folder.confirm",
            false,
        ),
        Dialog::Rename { original, name, .. } => (
            strings.format("operation.rename.title", &[("name", original)]),
            name_input(strings.get("operation.name"), name, dialog.can_confirm()),
            "operation.rename.confirm",
            false,
        ),
        Dialog::Delete {
            names,
            is_directory,
            ..
        } => (
            match names.as_slice() {
                [name] => strings.format("operation.delete.title", &[("name", name)]),
                _ => strings.format(
                    "operation.delete.many_title",
                    &[("count", &names.len().to_string())],
                ),
            },
            text(strings.get(if *is_directory {
                "operation.delete.folder"
            } else {
                "operation.delete.file"
            }))
            .into(),
            "operation.delete.confirm",
            true,
        ),
        Dialog::HostKey {
            fingerprint,
            certificate,
            host,
            jump,
            ..
        } => (
            strings
                .get(match (*certificate, *jump) {
                    (true, _) => "certificate.title",
                    (false, true) => "hostkey.jump_title",
                    (false, false) => "hostkey.title",
                })
                .to_string(),
            column![
                text(strings.format(
                    match (*certificate, *jump) {
                        (true, _) => "certificate.text",
                        (false, true) => "hostkey.jump_text",
                        (false, false) => "hostkey.text",
                    },
                    &[("host", host)]
                )),
                container(
                    text(fingerprint)
                        .font(iced::Font::MONOSPACE)
                        .size(14)
                        .wrapping(text::Wrapping::WordOrGlyph)
                )
                .padding(8)
                .width(Length::Fill)
                .style(container::rounded_box),
            ]
            .spacing(10)
            .into(),
            "hostkey.once",
            false,
        ),
        Dialog::Conflict {
            conflict,
            apply_to_all,
        } => (
            strings.get("transfer.conflict.title").to_string(),
            conflict_body(app, conflict, *apply_to_all),
            "transfer.conflict.overwrite",
            false,
        ),
        Dialog::NewSiteFolder { parent, name } => (
            if parent.is_empty() {
                strings.get("sites.new_folder.title").to_string()
            } else {
                strings.format("sites.new_folder.title_in", &[("folder", parent)])
            },
            name_input(strings.get("operation.name"), name, dialog.can_confirm()),
            "operation.new_folder.confirm",
            false,
        ),
        Dialog::DeleteSite { name, .. } => (
            strings.format("operation.delete.title", &[("name", name)]),
            text(strings.get("sites.delete.site")).into(),
            "operation.delete.confirm",
            true,
        ),
        Dialog::DeleteSiteFolder { folder, sites } => (
            strings.format("operation.delete.title", &[("name", folder)]),
            text(strings.format("sites.delete.folder", &[("count", &sites.to_string())])).into(),
            "operation.delete.confirm",
            true,
        ),
        Dialog::MasterPassword {
            mode,
            password,
            confirm,
            error,
            busy,
        } => master_password(
            app,
            *mode,
            password,
            confirm,
            error.as_ref(),
            *busy,
            dialog.can_confirm(),
        ),
        Dialog::Sync(sync) => (
            strings.get("sync.title").to_string(),
            ui::compare::sync_body(app, sync),
            "sync.start",
            sync.plan().is_some_and(|plan| plan.has_deletions()),
        ),
        Dialog::Update { version, .. } => (
            strings.format("update.title", &[("version", version)]),
            text(strings.format("update.text", &[("version", version)])).into(),
            "update.open",
            false,
        ),
        Dialog::ExportSites(export) => ui::exchange::export_body(app, export),
        Dialog::ImportSites(import) => ui::exchange::import_body(app, import),
        Dialog::Search(search) => (
            strings.get("search.title").to_string(),
            ui::file_list::search_body(app, search),
            "search.start",
            false,
        ),
        Dialog::Properties(properties) => (
            strings.format("properties.title", &[("name", &properties.name)]),
            ui::file_list::properties_body(app, properties),
            ui::file_list::properties_confirm(properties),
            false,
        ),
        Dialog::Bookmarks(bookmarks) => (
            strings.get("bookmarks.edit_title").to_string(),
            ui::file_list::bookmarks_body(app, bookmarks),
            "bookmarks.save",
            false,
        ),
        Dialog::Prompt(prompt) => (
            strings.format(
                if prompt.request.jump {
                    "prompt.jump_title"
                } else {
                    "prompt.title"
                },
                &[("host", &prompt.request.host)],
            ),
            prompt_body(app, prompt),
            "prompt.confirm",
            false,
        ),
    };

    let buttons = if let Dialog::Conflict { conflict, .. } = dialog {
        conflict_buttons(app, conflict)
    } else {
        standard_buttons(app, dialog, confirm, danger)
    };

    let card = container(
        column![text(title).size(18), body, buttons]
            .spacing(16)
            .align_x(Alignment::Start),
    )
    .padding(20)
    .width(
        if matches!(
            dialog,
            Dialog::Conflict { .. } | Dialog::Sync(_) | Dialog::Properties(_)
        ) {
            CONFLICT_WIDTH
        } else {
            DIALOG_WIDTH
        },
    )
    .style(|theme: &iced::Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(Background::Color(palette.background.base.color)),
            border: iced::Border {
                color: palette.background.strong.color,
                width: 1.0,
                radius: 10.0.into(),
            },
            shadow: iced::Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                offset: iced::Vector::new(0.0, 8.0),
                blur_radius: 24.0,
            },
            ..container::Style::default()
        }
    });

    opaque(
        mouse_area(center(opaque(card)).style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.45))),
            ..container::Style::default()
        }))
        .on_press(Message::DialogCancelled),
    )
}

fn standard_buttons<'a>(
    app: &'a Hoplix,
    dialog: &'a Dialog,
    confirm: &'a str,
    danger: bool,
) -> Element<'a, Message> {
    let strings = &app.strings;
    let mut buttons = row![Space::new().width(Length::Fill)].spacing(8);
    buttons = buttons.push(
        button(text(strings.get(match dialog {
            Dialog::HostKey { .. } => "hostkey.cancel",
            Dialog::Update { .. } => "update.later",
            _ => "dialog.cancel",
        })))
        .style(theme::secondary_button)
        .on_press(Message::DialogCancelled),
    );
    if matches!(dialog, Dialog::HostKey { .. }) {
        buttons = buttons.push(
            button(text(strings.get("hostkey.always")))
                .style(theme::secondary_button)
                .on_press(Message::HostKeyAnswered(Some(true))),
        );
    }
    buttons = buttons.push(
        button(text(strings.get(confirm)))
            .style(if danger {
                theme::danger_button
            } else {
                button::primary
            })
            .on_press_maybe(dialog.can_confirm().then_some(Message::DialogConfirmed)),
    );
    buttons.into()
}

// Conflicts

fn conflict_body<'a>(
    app: &'a Hoplix,
    conflict: &'a Conflict,
    apply_to_all: bool,
) -> Element<'a, Message> {
    let strings = &app.strings;
    let separator = strings.get("number.decimal_separator");
    column![
        text(strings.format("transfer.conflict.text", &[("path", &conflict.target_path)]))
            .wrapping(text::Wrapping::WordOrGlyph),
        text(strings.format(
            "transfer.conflict.sizes",
            &[
                ("source", &format::size(conflict.source_size, separator)),
                ("target", &format::size(conflict.target_size, separator)),
            ]
        ))
        .size(13),
        checkbox(apply_to_all)
            .label(strings.get("transfer.conflict.all"))
            .on_toggle(Message::ConflictApplyToAll),
    ]
    .spacing(10)
    .into()
}

fn conflict_buttons<'a>(app: &'a Hoplix, conflict: &'a Conflict) -> Element<'a, Message> {
    let strings = &app.strings;
    let choice = |key: &str, decision: ConflictDecision| {
        button(text(strings.get(key).to_string()))
            .style(theme::secondary_button)
            .on_press(Message::ConflictAnswered(decision))
    };
    let mut buttons = row![
        button(text(strings.get("transfer.conflict.cancel")))
            .style(button::text)
            .on_press(Message::DialogCancelled),
        Space::new().width(Length::Fill),
        choice("transfer.conflict.skip", ConflictDecision::Skip),
        choice("transfer.conflict.rename", ConflictDecision::Rename),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if conflict.can_resume && conflict.target_size < conflict.source_size {
        buttons = buttons.push(choice("transfer.conflict.resume", ConflictDecision::Resume));
    }
    buttons
        .push(
            button(text(strings.get("transfer.conflict.overwrite")))
                .style(button::primary)
                .on_press(Message::ConflictAnswered(ConflictDecision::Overwrite)),
        )
        .into()
}

#[allow(clippy::too_many_arguments)]
fn master_password<'a>(
    app: &'a Hoplix,
    mode: MasterMode,
    password: &'a str,
    confirm: &'a str,
    error: Option<&hoplix_core::Error>,
    busy: bool,
    valid: bool,
) -> (String, Element<'a, Message>, &'static str, bool) {
    let strings = &app.strings;
    let (title, explanation, action) = match mode {
        MasterMode::Unlock => ("master.unlock.title", "master.unlock.text", "master.unlock"),
        MasterMode::Create => ("master.create.title", "master.create.text", "master.create"),
        MasterMode::Change => ("master.change.title", "master.change.text", "master.change"),
    };
    let submit = valid.then_some(Message::DialogConfirmed);
    let mut body = column![
        text(strings.get(explanation)),
        text_input(strings.get("master.password"), password)
            .id(INPUT_ID)
            .secure(true)
            .on_input_maybe((!busy).then_some(Message::DialogInput))
            .on_submit_maybe(submit.clone())
            .padding(8),
    ]
    .spacing(10);
    if mode != MasterMode::Unlock {
        body = body.push(
            text_input(strings.get("master.confirm"), confirm)
                .secure(true)
                .on_input_maybe((!busy).then_some(Message::DialogConfirmInput))
                .on_submit_maybe(submit)
                .padding(8),
        );
        if !confirm.is_empty() && password != confirm {
            body = body.push(text(strings.get("master.mismatch")).size(13));
        }
    }
    if busy {
        body = body.push(text(strings.get("master.working")).size(13));
    }
    if let Some(error) = error {
        body = body.push(ui::notice(ui::error_text(strings, error)));
    }
    (strings.get(title).to_string(), body.into(), action, false)
}

// Prompts

fn prompt_body<'a>(app: &'a Hoplix, prompt: &'a PromptDialog) -> Element<'a, Message> {
    let strings = &app.strings;
    let request = &prompt.request;
    let mut body = column![].spacing(10);
    for line in [&request.name, &request.instruction] {
        if !line.is_empty() {
            body = body.push(text(line.as_str()).wrapping(text::Wrapping::WordOrGlyph));
        }
    }
    for (index, field) in request.fields.iter().enumerate() {
        let value = prompt.answers.get(index).map_or("", String::as_str);
        let mut input = text_input(field.text.trim(), value)
            .secure(!field.echo)
            .on_input(move |value| Message::Prompt(PromptMessage::Input(index, value)))
            .on_submit(Message::DialogConfirmed)
            .padding(8);
        if index == 0 {
            input = input.id(FIRST_INPUT_ID);
        }
        body = body.push(
            column![
                text(field.text.trim()).wrapping(text::Wrapping::WordOrGlyph),
                input
            ]
            .spacing(4),
        );
    }
    body.push(text(strings.get("prompt.hint")).size(13)).into()
}

fn name_input<'a>(placeholder: &'a str, value: &'a str, valid: bool) -> Element<'a, Message> {
    text_input(placeholder, value)
        .id(INPUT_ID)
        .on_input(Message::DialogInput)
        .on_submit_maybe(valid.then_some(Message::DialogConfirmed))
        .padding(8)
        .into()
}

pub fn dialog_input_id() -> &'static str {
    INPUT_ID
}

pub fn focus_input<T>() -> iced::Task<T> {
    iced::widget::operation::focus(INPUT_ID)
}

pub fn focus_id<T>(id: &'static str) -> iced::Task<T> {
    iced::widget::operation::focus(id)
}
