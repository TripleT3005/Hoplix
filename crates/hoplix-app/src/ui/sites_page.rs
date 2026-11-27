use hoplix_core::{FileNameEncoding, FtpMode, Site};
use iced::widget::{
    Column, Space, Text, button, checkbox, column, container, mouse_area, pick_list, row,
    scrollable, text, text_editor, text_input,
};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, Message, SiteSelection};
use crate::connect_form::Field;
use crate::i18n::Strings;
use crate::site_exchange::ExchangeMessage;
use crate::site_form::{OptionField, SiteField, SiteForm};
use crate::theme;
use crate::ui::connect_page::{
    LABEL_WIDTH, RouteSaving, connection_fields, hint, route_fields, save_secret_checkbox, setting,
};
use crate::ui::{self, Choice};

// Constants

const TREE_WIDTH: f32 = 320.0;
const INDENT: f32 = 16.0;
const NOTES_HEIGHT: f32 = 90.0;
const ROW_PADDING: [u16; 2] = [4, 8];
const NUMBER_WIDTH: f32 = 120.0;
const DEFAULT_TIMEOUT: u32 = 20;
const DEFAULT_SFTP_KEEPALIVE: u32 = 30;
pub const EDITOR_SCROLL_ID: &str = "site-editor";

// Page

pub fn view(app: &Hoplix) -> Element<'_, Message> {
    row![
        container(tree(app)).width(TREE_WIDTH).height(Length::Fill),
        container(editor(app))
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .spacing(12)
    .padding(12)
    .into()
}

// Tree

fn tree(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let mut items = Column::new().spacing(1);
    let sites = app.sites.sorted();
    let mut has_items = false;
    for site in sites.iter().filter(|site| site.folder.is_empty()) {
        items = items.push(site_row(app, site, 0));
        has_items = true;
    }
    for folder in app.sites.all_folders() {
        let depth = folder.matches('/').count();
        items = items.push(folder_row(app, &folder, depth));
        for site in sites.iter().filter(|site| site.folder == folder) {
            items = items.push(site_row(app, site, depth + 1));
        }
        has_items = true;
    }
    let list: Element<'_, Message> = if has_items {
        scrollable(items.padding(4)).height(Length::Fill).into()
    } else {
        container(text(strings.get("sites.empty")).size(13))
            .padding(12)
            .height(Length::Fill)
            .into()
    };

    let selected_site = match &app.site_selection {
        Some(SiteSelection::Site(id)) => Some(id.clone()),
        _ => None,
    };
    let busy = app.connect_form.connecting;
    column![
        row![
            button(label(strings.get("sites.new")))
                .style(button::primary)
                .on_press(Message::SiteNew),
            button(label(strings.get("sites.new_folder")))
                .style(theme::secondary_button)
                .on_press(Message::SiteFolderNew),
        ]
        .spacing(6),
        container(list)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|theme| theme::pane(theme, false, false)),
        row![
            button(label(strings.get("sites.connect")))
                .style(button::primary)
                .on_press_maybe(
                    selected_site
                        .clone()
                        .filter(|_| !busy)
                        .map(Message::SiteConnect)
                ),
            button(label(strings.get("sites.duplicate")))
                .style(theme::secondary_button)
                .on_press_maybe(selected_site.as_ref().map(|_| Message::SiteDuplicate)),
            button(label(strings.get("operation.delete")))
                .style(theme::secondary_button)
                .on_press_maybe(app.site_selection.as_ref().map(|_| Message::SiteDelete)),
        ]
        .spacing(6),
        row![
            button(label(strings.get("sites.import")))
                .style(theme::secondary_button)
                .on_press(Message::SiteExchange(ExchangeMessage::OpenImport)),
            button(label(strings.get("sites.export")))
                .style(theme::secondary_button)
                .on_press_maybe(
                    (!app.sites.sites.is_empty())
                        .then_some(Message::SiteExchange(ExchangeMessage::OpenExport))
                ),
        ]
        .spacing(6),
    ]
    .spacing(8)
    .into()
}

fn label(content: &str) -> Text<'_> {
    text(content).wrapping(text::Wrapping::None)
}

fn site_row<'a>(app: &'a Hoplix, site: &'a Site, depth: usize) -> Element<'a, Message> {
    let selected = app.site_selection == Some(SiteSelection::Site(site.id.clone()));
    let content = row![
        Space::new().width(indent(depth)),
        column![
            text(&site.name).size(14),
            text(format!("{}://{}", site.protocol.scheme(), site.host)).size(11),
        ],
    ]
    .align_y(Alignment::Center);
    mouse_area(
        container(content)
            .padding(ROW_PADDING)
            .width(Length::Fill)
            .style(move |theme| theme::row(theme, selected, true)),
    )
    .on_press(Message::SiteSelected(SiteSelection::Site(site.id.clone())))
    .on_double_click(Message::SiteConnect(site.id.clone()))
    .into()
}

fn folder_row<'a>(app: &'a Hoplix, folder: &str, depth: usize) -> Element<'a, Message> {
    let selected = app.site_selection == Some(SiteSelection::Folder(folder.to_string()));
    let name = folder.rsplit('/').next().unwrap_or(folder).to_string();
    let content = row![
        Space::new().width(indent(depth)),
        text(format!("▾ {name}")).size(14),
    ]
    .align_y(Alignment::Center);
    mouse_area(
        container(content)
            .padding(ROW_PADDING)
            .width(Length::Fill)
            .style(move |theme| theme::row(theme, selected, true)),
    )
    .on_press(Message::SiteSelected(SiteSelection::Folder(
        folder.to_string(),
    )))
    .into()
}

fn indent(depth: usize) -> f32 {
    f32::from(u16::try_from(depth).unwrap_or(u16::MAX)) * INDENT
}

// Editor

fn editor(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let Some(form) = &app.site_form else {
        return container(text(strings.get("sites.hint")))
            .center(Length::Fill)
            .into();
    };
    let enabled = !app.connect_form.connecting;
    let wrap: fn(Field) -> Message = |field| Message::SiteField(SiteField::Connection(field));
    let title = if form.is_new {
        strings.get("sites.new_title").to_string()
    } else {
        form.name.clone()
    };

    let mut content = column![
        text(title).size(22),
        setting(
            strings.get("sites.name"),
            text_input(strings.get("sites.name"), &form.name)
                .on_input(|value| Message::SiteField(SiteField::Name(value)))
                .padding(8)
                .into(),
        ),
        setting(
            strings.get("sites.folder"),
            folder_picker(app, strings, form)
        ),
    ]
    .spacing(12);
    for element in connection_fields(strings, &form.connection, wrap, true, None, form.had_secret) {
        content = content.push(element);
    }
    if let Some(checkbox) = save_secret_checkbox(strings, &form.connection, wrap, true) {
        content = content.push(checkbox);
    }
    content = content
        .push(setting(
            strings.get("sites.remote_path"),
            text_input(strings.get("sites.remote_path.hint"), &form.remote_path)
                .on_input(|value| Message::SiteField(SiteField::RemotePath(value)))
                .padding(8)
                .into(),
        ))
        .push(setting(
            strings.get("sites.local_path"),
            text_input(strings.get("sites.local_path.hint"), &form.local_path)
                .on_input(|value| Message::SiteField(SiteField::LocalPath(value)))
                .padding(8)
                .into(),
        ))
        .push(setting(
            strings.get("sites.notes"),
            text_editor(&form.notes)
                .on_action(|action| Message::SiteField(SiteField::Notes(action)))
                .height(NOTES_HEIGHT)
                .padding(8)
                .into(),
        ))
        .push(advanced(app, form));
    if let Some(invalid) = form.invalid {
        content = content.push(ui::notice(strings.get(invalid.key()).to_string()));
    }
    content = content.push(
        row![
            Space::new().width(Length::Fill),
            button(text(strings.get("dialog.cancel")))
                .style(theme::secondary_button)
                .on_press(Message::SiteEditCancelled),
            button(text(strings.get("sites.save")))
                .style(theme::secondary_button)
                .on_press(Message::SiteSave { connect: false }),
            button(text(strings.get("sites.save_connect")))
                .style(button::primary)
                .on_press_maybe(enabled.then_some(Message::SiteSave { connect: true })),
        ]
        .spacing(8),
    );

    scrollable(container(content.max_width(680)).padding([4, 16]))
        .id(EDITOR_SCROLL_ID)
        .height(Length::Fill)
        .into()
}

// Advanced settings

fn option(field: OptionField) -> Message {
    Message::SiteField(SiteField::Option(field))
}

fn advanced<'a>(app: &'a Hoplix, form: &'a SiteForm) -> Element<'a, Message> {
    let strings = &app.strings;
    let options = &form.options;
    let arrow = if options.open { "▾" } else { "▸" };
    let mut title = strings.get("sites.advanced").to_string();
    if !options.open && (!options.is_default() || form.connection.has_route()) {
        title = format!("{title} {}", strings.get("sites.advanced.changed"));
    }
    let header = button(text(format!("{arrow} {title}")))
        .style(button::text)
        .padding([4, 0])
        .on_press(option(OptionField::Toggle));
    let mut content = column![header].spacing(12);
    if !options.open {
        return content.into();
    }
    content = content.push(subheading(strings.get("sites.advanced.connection")));
    for element in connection_options(strings, form) {
        content = content.push(element);
    }
    let wrap: fn(Field) -> Message = |field| Message::SiteField(SiteField::Connection(field));
    let saving = RouteSaving {
        show: true,
        keep_jump: form.had_jump_secret,
        keep_proxy: form.had_proxy_secret,
    };
    for element in route_fields(strings, &form.connection, wrap, true, None, saving) {
        content = content.push(element);
    }
    if !form.connection.protocol.uses_ssh() {
        content = content.push(subheading(strings.get("sites.advanced.ftp")));
        for element in ftp_options(strings, form) {
            content = content.push(element);
        }
    }
    content = content.push(subheading(strings.get("sites.advanced.transfers")));
    content = content.push(setting(
        strings.get("sites.advanced.max_transfers"),
        number_input(
            &strings.format(
                "sites.advanced.max_transfers.global",
                &[("count", &app.settings.parallel_transfers.to_string())],
            ),
            &options.max_transfers,
            OptionField::MaxTransfers,
        ),
    ));
    content = content.push(hint(strings.get("sites.advanced.max_transfers.hint")));
    content.into()
}

fn connection_options<'a>(strings: &'a Strings, form: &'a SiteForm) -> Vec<Element<'a, Message>> {
    let options = &form.options;
    let keepalive_placeholder = if form.connection.protocol.uses_ssh() {
        strings.format(
            "sites.advanced.default_seconds",
            &[("count", &DEFAULT_SFTP_KEEPALIVE.to_string())],
        )
    } else {
        strings.get("sites.advanced.keepalive.off").to_string()
    };
    vec![
        setting(
            strings.get("sites.advanced.timeout"),
            number_input(
                &strings.format(
                    "sites.advanced.default_seconds",
                    &[("count", &DEFAULT_TIMEOUT.to_string())],
                ),
                &options.connect_timeout,
                OptionField::ConnectTimeout,
            ),
        ),
        setting(
            strings.get("sites.advanced.keepalive"),
            number_input(
                &keepalive_placeholder,
                &options.keepalive,
                OptionField::Keepalive,
            ),
        ),
        hint(strings.get("sites.advanced.keepalive.hint")),
    ]
}

fn ftp_options<'a>(strings: &'a Strings, form: &'a SiteForm) -> Vec<Element<'a, Message>> {
    let options = &form.options;
    let modes: Vec<Choice<FtpMode>> = FtpMode::ALL
        .into_iter()
        .map(|value| Choice {
            value,
            label: strings
                .get(match value {
                    FtpMode::Passive => "sites.advanced.mode.passive",
                    FtpMode::Active => "sites.advanced.mode.active",
                })
                .to_string(),
        })
        .collect();
    let selected_mode = modes.iter().find(|c| c.value == options.ftp_mode).cloned();
    let encodings: Vec<Choice<FileNameEncoding>> = FileNameEncoding::ALL
        .into_iter()
        .map(|value| Choice {
            value,
            label: strings
                .get(match value {
                    FileNameEncoding::Utf8 => "sites.advanced.encoding.utf8",
                    FileNameEncoding::Windows1252 => "sites.advanced.encoding.windows1252",
                })
                .to_string(),
        })
        .collect();
    let selected_encoding = encodings
        .iter()
        .find(|c| c.value == options.ftp_encoding)
        .cloned();
    let mut rows = vec![setting(
        strings.get("sites.advanced.mode"),
        pick_list(modes, selected_mode, |choice| {
            option(OptionField::FtpMode(choice.value))
        })
        .width(Length::Fill)
        .into(),
    )];
    if options.ftp_mode == FtpMode::Passive {
        rows.push(
            row![
                Space::new().width(LABEL_WIDTH),
                checkbox(options.ftp_external_ip)
                    .label(strings.get("sites.advanced.external_ip"))
                    .on_toggle(|enabled| option(OptionField::ExternalIp(enabled))),
            ]
            .spacing(12)
            .into(),
        );
    } else {
        rows.push(hint(strings.get("sites.advanced.mode.active.hint")));
        if form.connection.proxy.kind.is_some() {
            rows.push(ui::warning(
                strings.get("sites.invalid.active_proxy").to_string(),
            ));
        }
    }
    rows.push(setting(
        strings.get("sites.advanced.encoding"),
        pick_list(encodings, selected_encoding, |choice| {
            option(OptionField::Encoding(choice.value))
        })
        .width(Length::Fill)
        .into(),
    ));
    rows
}

fn number_input<'a>(
    placeholder: &str,
    value: &'a str,
    build: fn(String) -> OptionField,
) -> Element<'a, Message> {
    row![
        text_input(placeholder, value)
            .on_input(move |value| option(build(value)))
            .padding(8)
            .width(Length::Fill),
        Space::new().width(NUMBER_WIDTH),
    ]
    .into()
}

fn subheading(label: &str) -> Element<'_, Message> {
    text(label).size(15).style(text::primary).into()
}

fn folder_picker<'a>(
    app: &'a Hoplix,
    strings: &'a Strings,
    form: &'a SiteForm,
) -> Element<'a, Message> {
    let mut folders = vec![Choice {
        value: String::new(),
        label: strings.get("sites.folder.root").to_string(),
    }];
    folders.extend(app.sites.all_folders().into_iter().map(|folder| Choice {
        label: folder.clone(),
        value: folder,
    }));
    let current = hoplix_core::normalize_folder(&form.folder);
    if !folders.iter().any(|choice| choice.value == current) {
        folders.push(Choice {
            label: current.clone(),
            value: current.clone(),
        });
    }
    let selected = folders
        .iter()
        .find(|choice| choice.value == current)
        .cloned();
    pick_list(folders, selected, |choice| {
        Message::SiteField(SiteField::Folder(choice.value))
    })
    .width(Length::Fill)
    .into()
}
