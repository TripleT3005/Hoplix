use hoplix_core::{JumpHost, Protocol, ProxyKind};
use iced::widget::{
    Column, Space, button, checkbox, column, container, pick_list, row, rule, scrollable, text,
    text_input,
};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, Message};
use crate::connect_form::{AuthMethod, ConnectForm, Field};
use crate::i18n::Strings;
use crate::route_form::{JumpField, ProxyField};
use crate::s3_form::S3Field;
use crate::theme;
use crate::ui::{self, Choice};

// Constants

pub const LABEL_WIDTH: f32 = 160.0;
const FORM_WIDTH: f32 = 640.0;
const PORT_WIDTH: f32 = 90.0;
const HINT_SIZE: u32 = 13;

// Page

pub fn view(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let form = &app.connect_form;
    let enabled = !form.connecting;
    let submit = enabled.then_some(Message::ConnectSubmitted);

    let mut content = column![text(strings.get("connect.title")).size(24)]
        .spacing(14)
        .max_width(FORM_WIDTH);
    for element in connection_fields(
        strings,
        form,
        Message::ConnectField,
        enabled,
        submit.clone(),
        false,
    ) {
        content = content.push(element);
    }
    if form.site.is_some()
        && let Some(checkbox) = save_secret_checkbox(strings, form, Message::ConnectField, enabled)
    {
        content = content.push(checkbox);
    }
    content = content.push(advanced_toggle(
        strings,
        form,
        Message::ConnectField,
        enabled,
    ));
    if form.advanced_open {
        let saving = RouteSaving {
            show: form.site.is_some(),
            ..RouteSaving::default()
        };
        for element in route_fields(
            strings,
            form,
            Message::ConnectField,
            enabled,
            submit.clone(),
            saving,
        ) {
            content = content.push(element);
        }
    }

    if let Some(invalid) = form.invalid {
        content = content.push(ui::notice(strings.get(invalid.key()).to_string()));
    }
    if let Some(error) = &form.error {
        content = content.push(ui::notice(ui::connect_error_text(strings, error)));
    }

    let label = if form.connecting {
        strings.get("connect.connecting")
    } else {
        strings.get("connect.submit")
    };
    let save_as_site = (form.site.is_none()).then(|| {
        button(text(strings.get("connect.save_as_site")))
            .style(theme::secondary_button)
            .padding([8, 14])
            .on_press_maybe(enabled.then_some(Message::ConnectSaveAsSite))
    });
    let mut actions = row![Space::new().width(Length::Fill)]
        .spacing(8)
        .width(Length::Fill);
    if let Some(save_as_site) = save_as_site {
        actions = actions.push(save_as_site);
    }
    actions = actions.push(
        button(text(label))
            .style(button::primary)
            .padding([8, 18])
            .on_press_maybe(submit),
    );
    content = content.push(actions);
    if let Some(recent) = recent_list(app) {
        content = content.push(recent);
    }

    scrollable(
        container(content)
            .padding(24)
            .width(Length::Fill)
            .center_x(Length::Fill),
    )
    .height(Length::Fill)
    .into()
}

// Recent connections

fn recent_list(app: &Hoplix) -> Option<Element<'_, Message>> {
    let strings = &app.strings;
    if !app.settings.remember_recent || app.recent.entries.is_empty() {
        return None;
    }
    let enabled = !app.connect_form.connecting;
    let header = row![
        text(strings.get("connect.recent")).size(18),
        Space::new().width(Length::Fill),
        button(text(strings.get("connect.recent.clear")).size(13))
            .style(button::text)
            .on_press_maybe(enabled.then_some(Message::RecentCleared)),
    ]
    .align_y(Alignment::Center);
    let rows = app.recent.entries.iter().enumerate().map(|(index, entry)| {
        let mut detail = if entry.uses_key_file() {
            strings.format("connect.recent.key", &[("path", &entry.key_path)])
        } else if entry.uses_agent() {
            strings.get("connect.recent.agent").to_string()
        } else if entry.protocol.is_s3() {
            strings.get("connect.recent.access_key").to_string()
        } else {
            strings.get("connect.recent.password").to_string()
        };
        if let Some(jump) = &entry.jump {
            detail = strings.format(
                "connect.recent.jump",
                &[("detail", &detail), ("jump", &jump.label())],
            );
        }
        if let Some(proxy) = &entry.proxy {
            detail = strings.format(
                "connect.recent.proxy",
                &[("detail", &detail), ("proxy", &proxy.label())],
            );
        }
        row![
            button(
                column![
                    text(entry.label()).size(14),
                    text(detail).size(12).style(text::secondary),
                ]
                .spacing(2)
            )
            .style(button::text)
            .width(Length::Fill)
            .on_press_maybe(enabled.then_some(Message::RecentSelected(index))),
            button(text(strings.get("connect.recent.remove")).size(13))
                .style(button::text)
                .on_press_maybe(enabled.then_some(Message::RecentRemoved(index))),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    });
    Some(
        column![
            rule::horizontal(1),
            header,
            Column::with_children(rows).spacing(2),
        ]
        .spacing(8)
        .into(),
    )
}

// Shared fields

fn method_choices(strings: &Strings) -> Vec<Choice<AuthMethod>> {
    AuthMethod::ALL
        .into_iter()
        .map(|value| Choice {
            value,
            label: strings
                .get(match value {
                    AuthMethod::Password => "connect.method.password",
                    AuthMethod::KeyFile => "connect.method.key",
                    AuthMethod::Agent => "connect.method.agent",
                })
                .to_string(),
        })
        .collect()
}

pub fn connection_fields<'a>(
    strings: &'a Strings,
    form: &'a ConnectForm,
    wrap: fn(Field) -> Message,
    enabled: bool,
    submit: Option<Message>,
    keep_saved_secret: bool,
) -> Vec<Element<'a, Message>> {
    let protocols: Vec<Choice<Protocol>> = Protocol::ALL
        .into_iter()
        .map(|value| Choice {
            value,
            label: strings
                .get(match value {
                    Protocol::Sftp => "connect.protocol.sftp",
                    Protocol::Ftp => "connect.protocol.ftp",
                    Protocol::FtpsExplicit => "connect.protocol.ftpes",
                    Protocol::FtpsImplicit => "connect.protocol.ftps",
                    Protocol::WebDav => "connect.protocol.dav",
                    Protocol::WebDavs => "connect.protocol.davs",
                    Protocol::S3 => "connect.protocol.s3",
                })
                .to_string(),
        })
        .collect();
    let selected_protocol = protocols.iter().find(|c| c.value == form.protocol).cloned();

    let methods = method_choices(strings);
    let selected_method = methods.iter().find(|c| c.value == form.method).cloned();

    let input = |placeholder: &str, value: &str, build: fn(String) -> Field| {
        text_input(placeholder, value)
            .on_input_maybe(enabled.then_some(move |value| wrap(build(value))))
            .on_submit_maybe(submit.clone())
            .padding(8)
    };
    let secret_placeholder = |key: &'a str| {
        if keep_saved_secret {
            strings.get("sites.secret.keep")
        } else {
            strings.get(key)
        }
    };

    let s3 = form.protocol.is_s3();
    let default_port = if s3 {
        form.s3.options().default_port().to_string()
    } else {
        form.protocol.default_port().to_string()
    };
    let (host_label, host_hint, username_label, password_label) = if s3 {
        (
            "connect.s3.endpoint",
            "connect.s3.endpoint.hint",
            "connect.s3.access_key",
            "connect.s3.secret",
        )
    } else {
        (
            "connect.host",
            "connect.host",
            "connect.username",
            "connect.password",
        )
    };
    let mut fields: Vec<Element<'a, Message>> = vec![
        setting(
            strings.get("connect.protocol"),
            pick_list(protocols, selected_protocol, move |choice| {
                wrap(Field::Protocol(choice.value))
            })
            .width(Length::Fill)
            .into(),
        ),
        setting(
            strings.get(host_label),
            row![
                input(strings.get(host_hint), &form.host, Field::Host),
                input(&default_port, &form.port, Field::Port).width(PORT_WIDTH),
            ]
            .spacing(8)
            .into(),
        ),
        setting(
            strings.get(username_label),
            input(strings.get(username_label), &form.username, Field::Username).into(),
        ),
    ];
    if s3 {
        fields.push(setting(
            strings.get("connect.s3.bucket"),
            input(strings.get("connect.s3.bucket"), &form.s3.bucket, |value| {
                Field::S3(S3Field::Bucket(value))
            })
            .into(),
        ));
        fields.push(setting(
            strings.get("connect.s3.region"),
            input(hoplix_core::S3_DEFAULT_REGION, &form.s3.region, |value| {
                Field::S3(S3Field::Region(value))
            })
            .into(),
        ));
        fields.push(indented_checkbox(
            strings.get("connect.s3.path_style"),
            form.s3.path_style,
            enabled.then_some(move |value| wrap(Field::S3(S3Field::PathStyle(value)))),
        ));
        fields.push(indented_checkbox(
            strings.get("connect.s3.http"),
            form.s3.plain_http,
            enabled.then_some(move |value| wrap(Field::S3(S3Field::PlainHttp(value)))),
        ));
        if form.s3.plain_http {
            fields.push(ui::warning(
                strings.get("connect.s3.http.warning").to_string(),
            ));
        }
    }

    if form.protocol.is_webdav() {
        fields.insert(
            2,
            setting(
                strings.get("connect.base_path"),
                input(
                    strings.get("connect.base_path.placeholder"),
                    &form.base_path,
                    Field::BasePath,
                )
                .into(),
            ),
        );
    }
    if !form.protocol.is_encrypted() {
        let key = if form.protocol.is_webdav() {
            "connect.unencrypted.dav"
        } else {
            "connect.unencrypted"
        };
        fields.push(ui::warning(strings.get(key).to_string()));
    }
    if form.protocol.supports_key_files() {
        fields.push(setting(
            strings.get("connect.method"),
            pick_list(methods, selected_method, move |choice| {
                wrap(Field::Method(choice.value))
            })
            .width(Length::Fill)
            .into(),
        ));
    }

    match form.effective_method() {
        AuthMethod::Password => fields.push(setting(
            strings.get(password_label),
            input(
                secret_placeholder(password_label),
                &form.password,
                Field::Password,
            )
            .secure(true)
            .into(),
        )),
        AuthMethod::KeyFile => {
            fields.push(setting(
                strings.get("connect.key_path"),
                input(
                    strings.get("connect.key_path"),
                    &form.key_path,
                    Field::KeyPath,
                )
                .into(),
            ));
            fields.push(setting(
                strings.get("connect.passphrase"),
                input(
                    secret_placeholder("connect.passphrase"),
                    &form.passphrase,
                    Field::Passphrase,
                )
                .secure(true)
                .into(),
            ));
        }
        AuthMethod::Agent => fields.push(hint(strings.get("connect.agent.hint"))),
    }
    fields
}

pub fn save_secret_checkbox<'a>(
    strings: &'a Strings,
    form: &ConnectForm,
    wrap: fn(Field) -> Message,
    enabled: bool,
) -> Option<Element<'a, Message>> {
    let label = match form.effective_method() {
        AuthMethod::Password if form.protocol.is_s3() => strings.get("connect.s3.save_secret"),
        AuthMethod::Password => strings.get("connect.save_password"),
        AuthMethod::KeyFile => strings.get("connect.save_passphrase"),
        AuthMethod::Agent => return None,
    };
    Some(indented_checkbox(
        label,
        form.save_password,
        enabled.then_some(move |save| wrap(Field::SavePassword(save))),
    ))
}

// Routes

#[derive(Debug, Clone, Copy, Default)]
pub struct RouteSaving {
    pub show: bool,
    pub keep_jump: bool,
    pub keep_proxy: bool,
}

pub fn advanced_toggle<'a>(
    strings: &'a Strings,
    form: &ConnectForm,
    wrap: fn(Field) -> Message,
    enabled: bool,
) -> Element<'a, Message> {
    let arrow = if form.advanced_open { "▾" } else { "▸" };
    let mut title = strings.get("connect.advanced").to_string();
    if !form.advanced_open && form.has_route() {
        title = format!("{title} {}", strings.get("sites.advanced.changed"));
    }
    button(text(format!("{arrow} {title}")))
        .style(button::text)
        .padding([4, 0])
        .on_press_maybe(enabled.then_some(wrap(Field::ToggleAdvanced)))
        .into()
}

pub fn route_fields<'a>(
    strings: &'a Strings,
    form: &'a ConnectForm,
    wrap: fn(Field) -> Message,
    enabled: bool,
    submit: Option<Message>,
    saving: RouteSaving,
) -> Vec<Element<'a, Message>> {
    let mut fields = Vec::new();
    if form.protocol.supports_key_files() {
        fields.extend(jump_fields(
            strings,
            form,
            wrap,
            enabled,
            submit.clone(),
            saving,
        ));
    }
    fields.extend(proxy_fields(strings, form, wrap, enabled, submit, saving));
    fields
}

fn jump_fields<'a>(
    strings: &'a Strings,
    form: &'a ConnectForm,
    wrap: fn(Field) -> Message,
    enabled: bool,
    submit: Option<Message>,
    saving: RouteSaving,
) -> Vec<Element<'a, Message>> {
    let jump = &form.jump;
    let mut fields = vec![indented_checkbox(
        strings.get("connect.jump"),
        jump.enabled,
        enabled.then_some(move |value| wrap(Field::Jump(JumpField::Enabled(value)))),
    )];
    if !jump.enabled {
        return fields;
    }
    let input = |placeholder: &str, value: &str, build: fn(String) -> JumpField| {
        text_input(placeholder, value)
            .on_input_maybe(enabled.then_some(move |value| wrap(Field::Jump(build(value)))))
            .on_submit_maybe(submit.clone())
            .padding(8)
    };
    let secret_placeholder = |key: &'a str| {
        if saving.keep_jump {
            strings.get("sites.secret.keep")
        } else {
            strings.get(key)
        }
    };
    let methods = method_choices(strings);
    let selected = methods.iter().find(|c| c.value == jump.method).cloned();
    let default_port = JumpHost::DEFAULT_PORT.to_string();
    fields.push(setting(
        strings.get("connect.jump.host"),
        row![
            input(
                strings.get("connect.jump.host"),
                &jump.host,
                JumpField::Host
            ),
            input(&default_port, &jump.port, JumpField::Port).width(PORT_WIDTH),
        ]
        .spacing(8)
        .into(),
    ));
    fields.push(setting(
        strings.get("connect.jump.username"),
        input(
            strings.get("connect.username"),
            &jump.username,
            JumpField::Username,
        )
        .into(),
    ));
    fields.push(setting(
        strings.get("connect.jump.method"),
        pick_list(methods, selected, move |choice| {
            wrap(Field::Jump(JumpField::Method(choice.value)))
        })
        .width(Length::Fill)
        .into(),
    ));
    let save_label = match jump.method {
        AuthMethod::Password => {
            fields.push(setting(
                strings.get("connect.jump.password"),
                input(
                    secret_placeholder("connect.password"),
                    &jump.password,
                    JumpField::Password,
                )
                .secure(true)
                .into(),
            ));
            Some(strings.get("connect.save_password"))
        }
        AuthMethod::KeyFile => {
            fields.push(setting(
                strings.get("connect.jump.key_path"),
                input(
                    strings.get("connect.key_path"),
                    &jump.key_path,
                    JumpField::KeyPath,
                )
                .into(),
            ));
            fields.push(setting(
                strings.get("connect.jump.passphrase"),
                input(
                    secret_placeholder("connect.passphrase"),
                    &jump.passphrase,
                    JumpField::Passphrase,
                )
                .secure(true)
                .into(),
            ));
            Some(strings.get("connect.save_passphrase"))
        }
        AuthMethod::Agent => {
            fields.push(hint(strings.get("connect.agent.hint")));
            None
        }
    };
    if let Some(label) = save_label.filter(|_| saving.show) {
        fields.push(indented_checkbox(
            label,
            jump.save_password,
            enabled.then_some(move |value| wrap(Field::Jump(JumpField::SavePassword(value)))),
        ));
    }
    fields
}

fn proxy_fields<'a>(
    strings: &'a Strings,
    form: &'a ConnectForm,
    wrap: fn(Field) -> Message,
    enabled: bool,
    submit: Option<Message>,
    saving: RouteSaving,
) -> Vec<Element<'a, Message>> {
    let proxy = &form.proxy;
    let kinds: Vec<Choice<Option<ProxyKind>>> =
        [None, Some(ProxyKind::Socks5), Some(ProxyKind::Http)]
            .into_iter()
            .map(|value| Choice {
                value,
                label: strings
                    .get(match value {
                        None => "connect.proxy.none",
                        Some(ProxyKind::Socks5) => "connect.proxy.socks5",
                        Some(ProxyKind::Http) => "connect.proxy.http",
                    })
                    .to_string(),
            })
            .collect();
    let selected = kinds.iter().find(|c| c.value == proxy.kind).cloned();
    let mut fields = vec![setting(
        strings.get("connect.proxy"),
        pick_list(kinds, selected, move |choice| {
            wrap(Field::Proxy(ProxyField::Kind(choice.value)))
        })
        .width(Length::Fill)
        .into(),
    )];
    let Some(kind) = proxy.kind else {
        return fields;
    };
    let input = |placeholder: &str, value: &str, build: fn(String) -> ProxyField| {
        text_input(placeholder, value)
            .on_input_maybe(enabled.then_some(move |value| wrap(Field::Proxy(build(value)))))
            .on_submit_maybe(submit.clone())
            .padding(8)
    };
    let default_port = kind.default_port().to_string();
    fields.push(setting(
        strings.get("connect.proxy.host"),
        row![
            input(
                strings.get("connect.proxy.host"),
                &proxy.host,
                ProxyField::Host
            ),
            input(&default_port, &proxy.port, ProxyField::Port).width(PORT_WIDTH),
        ]
        .spacing(8)
        .into(),
    ));
    fields.push(setting(
        strings.get("connect.proxy.username"),
        input(
            strings.get("connect.proxy.username.hint"),
            &proxy.username,
            ProxyField::Username,
        )
        .into(),
    ));
    if proxy.uses_secret() {
        let placeholder = if saving.keep_proxy {
            strings.get("sites.secret.keep")
        } else {
            strings.get("connect.password")
        };
        fields.push(setting(
            strings.get("connect.proxy.password"),
            input(placeholder, &proxy.password, ProxyField::Password)
                .secure(true)
                .into(),
        ));
        if saving.show {
            fields.push(indented_checkbox(
                strings.get("connect.save_password"),
                proxy.save_password,
                enabled.then_some(move |value| wrap(Field::Proxy(ProxyField::SavePassword(value)))),
            ));
        }
    }
    if !form.protocol.supports_key_files() {
        fields.push(hint(strings.get("connect.proxy.passive")));
    }
    fields
}

// Layout helpers

fn indented_checkbox<'a>(
    label: &'a str,
    checked: bool,
    on_toggle: Option<impl Fn(bool) -> Message + 'a>,
) -> Element<'a, Message> {
    row![
        Space::new().width(LABEL_WIDTH),
        checkbox(checked).label(label).on_toggle_maybe(on_toggle),
    ]
    .spacing(12)
    .into()
}

pub fn hint(content: &str) -> Element<'_, Message> {
    row![
        Space::new().width(LABEL_WIDTH),
        text(content).size(HINT_SIZE).width(Length::Fill),
    ]
    .spacing(12)
    .into()
}

pub fn setting<'a>(label: &'a str, control: Element<'a, Message>) -> Element<'a, Message> {
    row![text(label).width(LABEL_WIDTH), control]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
}
