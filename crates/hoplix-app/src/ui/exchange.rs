use std::path::Path;

use hoplix_core::ExportScope;
use iced::widget::{checkbox, column, pick_list, text, text_input};
use iced::{Element, Length};

use crate::app::{Hoplix, Message, SiteSelection};
use crate::site_exchange::{
    CONFIRM_INPUT, ExchangeMessage, ExportDialog, ImportDialog, PASSWORD_INPUT, PATH_INPUT,
    clean_path, error_text,
};
use crate::ui::{self, Choice};

// Constants

const HINT_SIZE: u32 = 13;

// Export

pub fn export_body<'a>(
    app: &'a Hoplix,
    dialog: &'a ExportDialog,
) -> (String, Element<'a, Message>, &'static str, bool) {
    let strings = &app.strings;
    let enabled = !dialog.busy;
    let submit = dialog
        .can_confirm()
        .then_some(Message::DialogConfirmed);
    let scopes = scope_choices(app, &dialog.scope);
    let selected = scopes
        .iter()
        .find(|choice| choice.value == dialog.scope)
        .cloned();
    let mut body = column![
        text(strings.get("sites.export.text")),
        pick_list(scopes, selected, |choice| {
            wrap(ExchangeMessage::Scope(choice.value))
        })
        .width(Length::Fill),
        path_input(app, &dialog.path, enabled, submit.clone()),
        checkbox(dialog.include_passwords)
            .label(strings.get("sites.export.include_passwords"))
            .on_toggle_maybe(
                enabled.then_some(|include| wrap(ExchangeMessage::IncludePasswords(include)))
            ),
    ]
    .spacing(10);
    if dialog.include_passwords {
        body = body
            .push(text(strings.get("sites.export.encrypted")).size(HINT_SIZE))
            .push(
                text_input(strings.get("sites.export.password"), &dialog.password)
                    .id(PASSWORD_INPUT)
                    .secure(true)
                    .on_input_maybe(
                        enabled.then_some(|value| wrap(ExchangeMessage::Password(value))),
                    )
                    .on_submit_maybe(submit.clone())
                    .padding(8),
            )
            .push(
                text_input(strings.get("master.confirm"), &dialog.confirm)
                    .id(CONFIRM_INPUT)
                    .secure(true)
                    .on_input_maybe(enabled.then_some(|value| wrap(ExchangeMessage::Confirm(value))))
                    .on_submit_maybe(submit)
                    .padding(8),
            );
        if !dialog.confirm.is_empty() && dialog.password != dialog.confirm {
            body = body.push(text(strings.get("master.mismatch")).size(HINT_SIZE));
        }
    } else {
        body = body.push(text(strings.get("sites.export.plain")).size(HINT_SIZE));
    }
    if Path::new(&clean_path(&dialog.path)).is_file() {
        body = body.push(text(strings.get("sites.export.replace")).size(HINT_SIZE));
    }
    if dialog.busy {
        body = body.push(text(strings.get("master.working")).size(HINT_SIZE));
    }
    if let Some(error) = &dialog.error {
        body = body.push(ui::notice(error_text(strings, error)));
    }
    (
        strings.get("sites.export.title").to_string(),
        body.into(),
        "sites.export.confirm",
        false,
    )
}

fn scope_choices(app: &Hoplix, current: &ExportScope) -> Vec<Choice<ExportScope>> {
    let strings = &app.strings;
    let mut scopes = vec![ExportScope::All];
    match &app.site_selection {
        Some(SiteSelection::Folder(folder)) => scopes.push(ExportScope::Folder(folder.clone())),
        Some(SiteSelection::Site(id)) => {
            if let Some(site) = app.sites.get(id) {
                if !site.folder.is_empty() {
                    scopes.push(ExportScope::Folder(site.folder.clone()));
                }
                scopes.push(ExportScope::Site(id.clone()));
            }
        }
        None => {}
    }
    if !scopes.contains(current) {
        scopes.push(current.clone());
    }
    scopes
        .into_iter()
        .map(|value| {
            let label = match &value {
                ExportScope::All => strings.format(
                    "sites.export.all",
                    &[("count", &app.sites.sites.len().to_string())],
                ),
                ExportScope::Folder(folder) => {
                    strings.format("sites.export.folder", &[("folder", folder)])
                }
                ExportScope::Site(id) => strings.format(
                    "sites.export.site",
                    &[(
                        "name",
                        &app.sites
                            .get(id)
                            .map(|site| site.display_path())
                            .unwrap_or_default(),
                    )],
                ),
            };
            Choice { value, label }
        })
        .collect()
}

// Import

pub fn import_body<'a>(
    app: &'a Hoplix,
    dialog: &'a ImportDialog,
) -> (String, Element<'a, Message>, &'static str, bool) {
    let strings = &app.strings;
    let enabled = !dialog.busy;
    let submit = dialog
        .can_confirm()
        .then_some(Message::DialogConfirmed);
    let mut folders = vec![
        Choice {
            value: dialog.new_folder.clone(),
            label: strings.format("sites.import.new_folder", &[("folder", &dialog.new_folder)]),
        },
        Choice {
            value: String::new(),
            label: strings.get("sites.folder.root").to_string(),
        },
    ];
    folders.extend(
        app.sites
            .all_folders()
            .into_iter()
            .filter(|folder| *folder != dialog.new_folder)
            .map(|folder| Choice {
                label: folder.clone(),
                value: folder,
            }),
    );
    let selected = folders
        .iter()
        .find(|choice| choice.value == dialog.folder)
        .cloned();
    let mut body = column![
        text(strings.get("sites.import.text")),
        path_input(app, &dialog.path, enabled, submit.clone()),
        text(strings.get("sites.import.target")).size(HINT_SIZE),
        pick_list(folders, selected, |choice| {
            wrap(ExchangeMessage::Folder(choice.value))
        })
        .width(Length::Fill),
    ]
    .spacing(10);
    if dialog.sealed.is_some() {
        body = body
            .push(text(strings.get("sites.import.encrypted")).size(HINT_SIZE))
            .push(
                text_input(strings.get("sites.export.password"), &dialog.password)
                    .id(PASSWORD_INPUT)
                    .secure(true)
                    .on_input_maybe(
                        enabled.then_some(|value| wrap(ExchangeMessage::Password(value))),
                    )
                    .on_submit_maybe(submit)
                    .padding(8),
            );
    }
    if dialog.busy {
        body = body.push(text(strings.get("master.working")).size(HINT_SIZE));
    }
    if let Some(error) = &dialog.error {
        body = body.push(ui::notice(error_text(strings, error)));
    }
    (
        strings.get("sites.import.title").to_string(),
        body.into(),
        "sites.import.confirm",
        false,
    )
}

// Shared

fn wrap(message: ExchangeMessage) -> Message {
    Message::SiteExchange(message)
}

fn path_input<'a>(
    app: &'a Hoplix,
    path: &'a str,
    enabled: bool,
    submit: Option<Message>,
) -> Element<'a, Message> {
    text_input(app.strings.get("sites.exchange.path"), path)
        .id(PATH_INPUT)
        .on_input_maybe(enabled.then_some(|value| wrap(ExchangeMessage::Path(value))))
        .on_submit_maybe(submit)
        .padding(8)
        .into()
}
