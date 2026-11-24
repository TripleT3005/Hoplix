use hoplix_core::Protocol;
use iced::widget::{Space, button, checkbox, column, container, pick_list, row, text, text_input};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, Message};
use crate::connect_form::{AuthMethod, ConnectForm, Field};
use crate::i18n::Strings;
use crate::theme;
use crate::ui::{self, Choice};

// Constants

pub const LABEL_WIDTH: f32 = 160.0;
const FORM_WIDTH: f32 = 640.0;
const PORT_WIDTH: f32 = 90.0;

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
    if form.site.is_some() {
        content = content.push(save_secret_checkbox(
            strings,
            form,
            Message::ConnectField,
            enabled,
        ));
    }

    if let Some(invalid) = form.invalid {
        content = content.push(ui::notice(strings.get(invalid.key()).to_string()));
    }
    if let Some(error) = &form.error {
        content = content.push(ui::notice(ui::error_text(strings, error)));
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

    container(content)
        .padding(24)
        .width(Length::Fill)
        .center_x(Length::Fill)
        .into()
}

// Shared fields

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
                })
                .to_string(),
        })
        .collect();
    let selected_protocol = protocols.iter().find(|c| c.value == form.protocol).cloned();

    let methods: Vec<Choice<AuthMethod>> = AuthMethod::ALL
        .into_iter()
        .map(|value| Choice {
            value,
            label: strings
                .get(match value {
                    AuthMethod::Password => "connect.method.password",
                    AuthMethod::KeyFile => "connect.method.key",
                })
                .to_string(),
        })
        .collect();
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

    let default_port = form.protocol.default_port().to_string();
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
            strings.get("connect.host"),
            row![
                input(strings.get("connect.host"), &form.host, Field::Host),
                input(&default_port, &form.port, Field::Port).width(PORT_WIDTH),
            ]
            .spacing(8)
            .into(),
        ),
        setting(
            strings.get("connect.username"),
            input(
                strings.get("connect.username"),
                &form.username,
                Field::Username,
            )
            .into(),
        ),
    ];

    if !form.protocol.is_encrypted() {
        fields.push(ui::warning(strings.get("connect.unencrypted").to_string()));
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
            strings.get("connect.password"),
            input(
                secret_placeholder("connect.password"),
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
    }
    fields
}

pub fn save_secret_checkbox<'a>(
    strings: &'a Strings,
    form: &ConnectForm,
    wrap: fn(Field) -> Message,
    enabled: bool,
) -> Element<'a, Message> {
    let label = match form.effective_method() {
        AuthMethod::Password => strings.get("connect.save_password"),
        AuthMethod::KeyFile => strings.get("connect.save_passphrase"),
    };
    row![
        Space::new().width(LABEL_WIDTH),
        checkbox(form.save_password)
            .label(label)
            .on_toggle_maybe(enabled.then_some(move |save| wrap(Field::SavePassword(save)))),
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
