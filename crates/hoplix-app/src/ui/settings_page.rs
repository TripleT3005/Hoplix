use hoplix_core::SecretBackend;
use iced::widget::{Space, button, checkbox, column, container, pick_list, row, text, text_input};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, Message};
use crate::edits::EditMessage;
use crate::settings::{Layout, MAX_PARALLEL_TRANSFERS, ThemePreference};
use crate::theme;
use crate::ui::Choice;

// Constants

const LABEL_WIDTH: f32 = 160.0;
const PICKER_WIDTH: f32 = 380.0;

// Page

pub fn view(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;

    let themes: Vec<Choice<ThemePreference>> = ThemePreference::ALL
        .into_iter()
        .map(|value| Choice {
            value,
            label: strings
                .get(match value {
                    ThemePreference::System => "settings.theme.system",
                    ThemePreference::Light => "settings.theme.light",
                    ThemePreference::Dark => "settings.theme.dark",
                })
                .to_string(),
        })
        .collect();
    let selected_theme = themes
        .iter()
        .find(|c| c.value == app.settings.theme)
        .cloned();

    let mut languages = vec![Choice {
        value: None,
        label: strings.get("settings.language.system").to_string(),
    }];
    languages.extend(app.languages.iter().map(|language| Choice {
        value: Some(language.code.clone()),
        label: language.name.clone(),
    }));
    let selected_language = languages
        .iter()
        .find(|c| c.value == app.settings.language)
        .cloned();

    let layouts: Vec<Choice<Layout>> = Layout::ALL
        .into_iter()
        .map(|value| Choice {
            value,
            label: strings
                .get(match value {
                    Layout::Modern => "settings.layout.modern",
                    Layout::Classic => "settings.layout.classic",
                })
                .to_string(),
        })
        .collect();
    let selected_layout = layouts
        .iter()
        .find(|c| c.value == app.settings.layout)
        .cloned();

    let backends: Vec<Choice<SecretBackend>> = SecretBackend::ALL
        .into_iter()
        .map(|value| Choice {
            value,
            label: strings
                .get(match value {
                    SecretBackend::Keyring if cfg!(windows) => "settings.secrets.keyring.windows",
                    SecretBackend::Keyring => "settings.secrets.keyring",
                    SecretBackend::Vault => "settings.secrets.vault",
                })
                .to_string(),
        })
        .collect();
    let selected_backend = backends
        .iter()
        .find(|c| c.value == app.secret_backend)
        .cloned();
    let mut secrets = row![
        pick_list(backends, selected_backend, |choice| {
            Message::SecretBackendSelected(choice.value)
        })
        .width(PICKER_WIDTH)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if app.secret_backend == SecretBackend::Vault {
        secrets = secrets.push(
            button(text(strings.get("settings.secrets.change")))
                .style(theme::secondary_button)
                .on_press(Message::ChangeMasterPassword),
        );
    }

    let content = column![
        text(strings.get("settings.title")).size(24),
        setting(
            strings.get("settings.theme"),
            pick_list(themes, selected_theme, |choice| Message::ThemeSelected(
                choice.value
            ))
            .width(PICKER_WIDTH)
            .into(),
        ),
        setting(
            strings.get("settings.language"),
            pick_list(languages, selected_language, |choice| {
                Message::LanguageSelected(choice.value)
            })
            .width(PICKER_WIDTH)
            .into(),
        ),
        setting(
            strings.get("settings.layout"),
            pick_list(layouts, selected_layout, |choice| Message::LayoutSelected(
                choice.value
            ))
            .width(PICKER_WIDTH)
            .into(),
        ),
        setting(
            strings.get("settings.parallel_transfers"),
            pick_list(
                (1..=MAX_PARALLEL_TRANSFERS).collect::<Vec<usize>>(),
                Some(app.settings.parallel_transfers),
                Message::ParallelTransfersSelected
            )
            .width(PICKER_WIDTH)
            .into(),
        ),
        setting(
            strings.get("settings.editor"),
            text_input(
                strings.get("settings.editor.placeholder"),
                &app.settings.editor
            )
            .on_input(|editor| Message::Edit(EditMessage::EditorEdited(editor)))
            .padding(6)
            .width(PICKER_WIDTH)
            .into(),
        ),
        row![
            Space::new().width(LABEL_WIDTH),
            checkbox(app.settings.upload_changes_automatically)
                .label(strings.get("settings.upload_changes"))
                .on_toggle(|value| Message::Edit(EditMessage::UploadAutomaticallyToggled(value))),
        ]
        .spacing(12),
        text(strings.get("settings.editor.hint")).size(13),
        setting(strings.get("settings.secrets"), secrets.into()),
        text(strings.get("settings.secrets.hint")).size(13),
    ]
    .spacing(16)
    .max_width(780);

    container(content)
        .padding(24)
        .width(Length::Fill)
        .center_x(Length::Fill)
        .into()
}

fn setting<'a>(label: &'a str, control: Element<'a, Message>) -> Element<'a, Message> {
    row![text(label).width(LABEL_WIDTH), control]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
}
