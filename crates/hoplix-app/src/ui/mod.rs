mod browser;
mod compare;
mod connect_page;
mod dialog;
mod edits;
mod exchange;
mod palette;
mod settings_page;
mod sites_page;
mod transfers;

use std::fmt;

use hoplix_core::{AGENT_EMPTY, AGENT_MISSING, AGENT_REJECTED, Error, ErrorKind};
use iced::widget::{Space, button, column, container, row, stack, text};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, LoadState, Message, Page, Tab, TabId};
use crate::i18n::Strings;
use crate::theme;
use crate::updates::UpdateMessage;

pub use dialog::{focus_id, focus_input};
#[cfg(test)]
pub use settings_page::SCROLL_ID as SETTINGS_SCROLL_ID;
#[cfg(test)]
pub use sites_page::EDITOR_SCROLL_ID;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice<T> {
    pub value: T,
    pub label: String,
}

impl<T> fmt::Display for Choice<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

// Layout

pub fn view(app: &Hoplix) -> Element<'_, Message> {
    let body = match app.page {
        Page::Browser => browser::view(app),
        Page::Connect => connect_page::view(app),
        Page::Sites => sites_page::view(app),
        Page::Settings => settings_page::view(app),
    };
    let mut base = column![
        top_bar(app),
        container(body).width(Length::Fill).height(Length::Fill),
    ];
    if let Some(panel) = edits::view(app) {
        base = base.push(panel);
    }
    if let Some(panel) = transfers::view(app) {
        base = base.push(panel);
    }
    let base = base.push(status_bar(app));
    match (&app.dialog, &app.palette) {
        (Some(current), _) => stack![base, dialog::view(app, current)].into(),
        (None, Some(palette)) => stack![base, palette::view(app, palette)].into(),
        (None, None) => base.into(),
    }
}

fn top_bar(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let actions: Element<'_, Message> = if app.page == Page::Browser {
        row![
            compare::top_bar_control(app),
            button(text(strings.get("connect.open")))
                .style(button::primary)
                .on_press(Message::ShowPage(Page::Connect)),
            button(text(strings.get("sites.open")))
                .style(theme::secondary_button)
                .on_press(Message::ShowPage(Page::Sites)),
            button(text(strings.get("settings.open")))
                .style(button::text)
                .on_press(Message::ShowPage(Page::Settings)),
            button(text(strings.get("command.open")))
                .style(button::text)
                .on_press(Message::RunAction(crate::commands::Action::CommandPalette)),
        ]
        .spacing(8)
        .into()
    } else {
        button(text(strings.get("settings.back")))
            .style(button::text)
            .on_press(Message::ShowPage(Page::Browser))
            .into()
    };
    container(
        row![
            text(strings.get("app.name")).size(18),
            Space::new().width(Length::Fill),
            actions
        ]
        .align_y(Alignment::Center)
        .spacing(8),
    )
    .padding([6, 12])
    .width(Length::Fill)
    .style(theme::bar)
    .into()
}

fn status_bar(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let status = app
        .notice
        .clone()
        .unwrap_or_else(|| match app.focused_tab() {
            _ if app.drag.as_ref().is_some_and(|drag| drag.moved) => {
                strings.get("transfer.drag_hint").to_string()
            }
            _ if app.connect_form.connecting => strings.get("connect.connecting").to_string(),
            Some(tab) if tab.state == LoadState::Disconnected => {
                strings.get("session.disconnected").to_string()
            }
            Some(tab) if tab.state == LoadState::Loading => {
                strings.format("status.loading", &[("path", &tab.path_input)])
            }
            Some(tab) if tab.entries.len() == 1 => strings.get("status.items.one").to_string(),
            Some(tab) => strings.format(
                "status.items.other",
                &[("count", &tab.entries.len().to_string())],
            ),
            None => String::new(),
        });
    let mut content = row![text(status).size(13).width(Length::Fill)]
        .spacing(12)
        .align_y(Alignment::Center);
    if let Some(update) = app.available_update() {
        content = content.push(
            button(text(strings.format("update.hint", &[("version", &update.version)])).size(13))
                .padding([0, 4])
                .style(button::text)
                .on_press(Message::Update(UpdateMessage::OpenReleasePage(update.url))),
        );
    }
    container(content)
        .padding([4, 12])
        .width(Length::Fill)
        .style(theme::bar)
        .into()
}

// Shared

pub fn path_input_id(tab: TabId) -> String {
    format!("path-{tab}")
}

pub fn entry_list_id(tab: TabId) -> String {
    format!("entries-{tab}")
}

pub fn tab_title(tab: &Tab) -> String {
    let name = tab.file_system.file_name(&tab.path);
    if let Some(title) = &tab.title {
        return format!("{title}: {name}");
    }
    match tab.file_system.label() {
        Some(label) => format!("{label}: {name}"),
        None => name.to_string(),
    }
}

pub fn error_text(strings: &Strings, error: &Error) -> String {
    let key = match error.kind() {
        ErrorKind::NotFound => "error.not_found",
        ErrorKind::PermissionDenied => "error.permission_denied",
        ErrorKind::NotADirectory => "error.not_a_directory",
        ErrorKind::AlreadyExists => "error.already_exists",
        ErrorKind::DirectoryNotEmpty => "error.directory_not_empty",
        ErrorKind::ConnectionFailed => "error.connection_failed",
        ErrorKind::AuthenticationFailed => "error.authentication_failed",
        ErrorKind::InvalidKey => "error.invalid_key",
        ErrorKind::UnknownHostKey => "error.unknown_host_key",
        ErrorKind::HostKeyChanged => "error.host_key_changed",
        ErrorKind::UntrustedCertificate => "error.untrusted_certificate",
        ErrorKind::CertificateChanged => "error.certificate_changed",
        ErrorKind::WrongMasterPassword => "error.wrong_master_password",
        ErrorKind::VaultLocked => "error.vault_locked",
        ErrorKind::SecretStorage => "error.secret_storage",
        ErrorKind::Other => "error.other",
    };
    strings.format(key, &[("detail", error.detail())])
}

pub fn connect_error_text(strings: &Strings, error: &Error) -> String {
    let detail = error.detail();
    if error.kind() == ErrorKind::AuthenticationFailed {
        let agent = [
            (AGENT_MISSING, "error.agent_missing"),
            (AGENT_EMPTY, "error.agent_empty"),
            (AGENT_REJECTED, "error.agent_rejected"),
        ];
        for (marker, key) in agent {
            if let Some((target, _)) = detail.split_once(&format!(": {marker}")) {
                return strings.format(key, &[("target", target)]);
            }
        }
    }
    let text = error_text(strings, error);
    match error.hop() {
        Some(hop) if !detail.contains(hop) => {
            strings.format("error.at_hop", &[("hop", hop), ("detail", &text)])
        }
        _ => text,
    }
}

pub fn notice<'a>(content: String) -> Element<'a, Message> {
    container(text(content).size(13))
        .padding([6, 10])
        .width(Length::Fill)
        .style(theme::notice)
        .into()
}

pub fn warning<'a>(content: String) -> Element<'a, Message> {
    container(text(content).size(13))
        .padding([6, 10])
        .width(Length::Fill)
        .style(theme::warning)
        .into()
}
