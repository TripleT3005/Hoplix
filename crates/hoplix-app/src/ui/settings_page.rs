use std::time::{Duration, UNIX_EPOCH};

use hoplix_core::update::CURRENT_VERSION;
use hoplix_core::{ChecksumMode, MAX_HISTORY_LIMIT, SecretBackend};
use iced::widget::{
    Column, Space, button, checkbox, column, container, pick_list, row, rule, scrollable, text,
    text_input,
};
use iced::{Alignment, Element, Length};

use crate::app::{Hoplix, Message};
use crate::edits::EditMessage;
use crate::i18n::Strings;
use crate::local_time;
use crate::preview::{LIMIT_CHOICES, PreviewMessage};
use crate::settings::{
    Layout, MAX_PARALLEL_TRANSFERS, MAX_RETRY_ATTEMPTS, MAX_RETRY_DELAY, MIN_RETRY_DELAY, RateUnit,
    SpeedLimit, ThemePreference,
};
use crate::theme;
use crate::transfer_settings::TransferSetting;
use crate::ui::Choice;
use crate::updates::{CheckState, UpdateMessage};

// Constants

pub const SCROLL_ID: &str = "settings-page";
const LABEL_WIDTH: f32 = 160.0;
const PICKER_WIDTH: f32 = 380.0;
const AMOUNT_WIDTH: f32 = 140.0;
const UNIT_WIDTH: f32 = 110.0;
const HINT_SIZE: u32 = 13;
const HISTORY_LIMITS: [usize; 7] = [100, 250, 500, 1000, 2500, 5000, MAX_HISTORY_LIMIT];

// Page

pub fn view(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let content = column![
        text(strings.get("settings.title")).size(24),
        general(app),
        transfers(app),
        connections(app),
        passwords(app),
        editor(app),
        preview(app),
        updates(app),
    ]
    .spacing(32)
    .max_width(780);

    scrollable(
        container(content)
            .padding(24)
            .width(Length::Fill)
            .center_x(Length::Fill),
    )
    .id(SCROLL_ID)
    .height(Length::Fill)
    .into()
}

// Sections

fn general(app: &Hoplix) -> Element<'_, Message> {
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
    let selected_theme = selected(&themes, &app.settings.theme);

    let mut languages = vec![Choice {
        value: None,
        label: strings.get("settings.language.system").to_string(),
    }];
    languages.extend(app.languages.iter().map(|language| Choice {
        value: Some(language.code.clone()),
        label: language.name.clone(),
    }));
    let selected_language = selected(&languages, &app.settings.language);

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
    let selected_layout = selected(&layouts, &app.settings.layout);

    section(
        strings.get("settings.section.general"),
        vec![
            setting(
                strings.get("settings.theme"),
                pick_list(themes, selected_theme, |choice| {
                    Message::ThemeSelected(choice.value)
                })
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
                pick_list(layouts, selected_layout, |choice| {
                    Message::LayoutSelected(choice.value)
                })
                .width(PICKER_WIDTH)
                .into(),
            ),
        ],
    )
}

fn transfers(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let settings = &app.settings;
    section(
        strings.get("settings.section.transfers"),
        vec![
            setting(
                strings.get("settings.parallel_transfers"),
                pick_list(
                    (1..=MAX_PARALLEL_TRANSFERS).collect::<Vec<usize>>(),
                    Some(settings.parallel_transfers),
                    |count| Message::TransferSetting(TransferSetting::Parallel(count)),
                )
                .width(PICKER_WIDTH)
                .into(),
            ),
            setting(
                strings.get("settings.parallel_downloads"),
                direction_picker(
                    strings,
                    settings.parallel_downloads,
                    TransferSetting::Downloads,
                ),
            ),
            setting(
                strings.get("settings.parallel_uploads"),
                direction_picker(strings, settings.parallel_uploads, TransferSetting::Uploads),
            ),
            setting(
                strings.get("settings.download_limit"),
                speed_limit(
                    strings,
                    settings.download_limit,
                    TransferSetting::DownloadLimit,
                    TransferSetting::DownloadUnit,
                ),
            ),
            setting(
                strings.get("settings.upload_limit"),
                speed_limit(
                    strings,
                    settings.upload_limit,
                    TransferSetting::UploadLimit,
                    TransferSetting::UploadUnit,
                ),
            ),
            hint(strings.get("settings.transfers.hint")),
            setting(
                strings.get("settings.checksum"),
                checksum_picker(strings, settings.checksum),
            ),
            hint(strings.get("settings.checksum.hint")),
            checkbox_row(
                checkbox(settings.restore_transfers)
                    .label(strings.get("settings.restore_transfers"))
                    .on_toggle(|value| {
                        Message::TransferSetting(TransferSetting::RestoreTransfers(value))
                    })
                    .into(),
            ),
            hint(strings.get("settings.restore_transfers.hint")),
            checkbox_row(
                checkbox(settings.history_enabled)
                    .label(strings.get("settings.history"))
                    .on_toggle(|value| Message::TransferSetting(TransferSetting::History(value)))
                    .into(),
            ),
            setting(
                strings.get("settings.history_limit"),
                pick_list(
                    history_limits(settings.history_limit),
                    Some(settings.history_limit),
                    |limit| Message::TransferSetting(TransferSetting::HistoryLimit(limit)),
                )
                .width(PICKER_WIDTH)
                .into(),
            ),
            hint(strings.get("settings.history.hint")),
        ],
    )
}

fn checksum_picker(strings: &Strings, current: ChecksumMode) -> Element<'_, Message> {
    let modes: Vec<Choice<ChecksumMode>> = ChecksumMode::ALL
        .into_iter()
        .map(|value| Choice {
            value,
            label: strings
                .get(match value {
                    ChecksumMode::Off => "settings.checksum.off",
                    ChecksumMode::WhenSupported => "settings.checksum.supported",
                })
                .to_string(),
        })
        .collect();
    let current = selected(&modes, &current);
    pick_list(modes, current, |choice| {
        Message::TransferSetting(TransferSetting::Checksum(choice.value))
    })
    .width(PICKER_WIDTH)
    .into()
}

fn history_limits(current: usize) -> Vec<usize> {
    let mut limits = HISTORY_LIMITS.to_vec();
    if !limits.contains(&current) {
        limits.push(current);
        limits.sort_unstable();
    }
    limits
}

fn connections(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let settings = &app.settings;

    let attempts: Vec<Choice<u32>> = (0..=MAX_RETRY_ATTEMPTS)
        .map(|value| Choice {
            value,
            label: if value == 0 {
                strings.get("settings.retries.off").to_string()
            } else {
                value.to_string()
            },
        })
        .collect();
    let selected_attempts = selected(&attempts, &settings.retry_attempts);

    let delays: Vec<Choice<u32>> = (MIN_RETRY_DELAY..=MAX_RETRY_DELAY)
        .map(|value| Choice {
            value,
            label: strings.format("settings.seconds", &[("count", &value.to_string())]),
        })
        .collect();
    let selected_delay = selected(&delays, &settings.retry_delay);

    section(
        strings.get("settings.section.connections"),
        vec![
            setting(
                strings.get("settings.retries"),
                pick_list(attempts, selected_attempts, |choice| {
                    Message::TransferSetting(TransferSetting::RetryAttempts(choice.value))
                })
                .width(PICKER_WIDTH)
                .into(),
            ),
            setting(
                strings.get("settings.retry_delay"),
                pick_list(delays, selected_delay, |choice| {
                    Message::TransferSetting(TransferSetting::RetryDelay(choice.value))
                })
                .width(PICKER_WIDTH)
                .into(),
            ),
            checkbox_row(
                checkbox(settings.auto_reconnect)
                    .label(strings.get("settings.reconnect"))
                    .on_toggle(|value| {
                        Message::TransferSetting(TransferSetting::AutoReconnect(value))
                    })
                    .into(),
            ),
            hint(strings.get("settings.connections.hint")),
            checkbox_row(
                checkbox(settings.remember_recent)
                    .label(strings.get("settings.remember_recent"))
                    .on_toggle(Message::RememberRecent)
                    .into(),
            ),
            hint(strings.get("settings.remember_recent.hint")),
        ],
    )
}

fn passwords(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
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
    let selected_backend = selected(&backends, &app.secret_backend);
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
    section(
        strings.get("settings.section.passwords"),
        vec![
            setting(strings.get("settings.secrets"), secrets.into()),
            hint(strings.get("settings.secrets.hint")),
        ],
    )
}

fn editor(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    section(
        strings.get("settings.section.editor"),
        vec![
            setting(
                strings.get("settings.editor"),
                text_input(
                    strings.get("settings.editor.placeholder"),
                    &app.settings.editor,
                )
                .on_input(|editor| Message::Edit(EditMessage::EditorEdited(editor)))
                .padding(6)
                .width(PICKER_WIDTH)
                .into(),
            ),
            checkbox_row(
                checkbox(app.settings.upload_changes_automatically)
                    .label(strings.get("settings.upload_changes"))
                    .on_toggle(|value| {
                        Message::Edit(EditMessage::UploadAutomaticallyToggled(value))
                    })
                    .into(),
            ),
            hint(strings.get("settings.editor.hint")),
        ],
    )
}

fn preview(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let limits: Vec<Choice<u32>> = LIMIT_CHOICES
        .into_iter()
        .map(|value| Choice {
            value,
            label: if value == 0 {
                strings.get("settings.preview.ask").to_string()
            } else {
                strings.format("settings.preview.mib", &[("count", &value.to_string())])
            },
        })
        .collect();
    let current = selected(&limits, &app.settings.preview_limit);
    section(
        strings.get("settings.section.preview"),
        vec![
            setting(
                strings.get("settings.preview.limit"),
                pick_list(limits, current, |choice| {
                    Message::Preview(PreviewMessage::LimitSelected(choice.value))
                })
                .placeholder(strings.format(
                    "settings.preview.mib",
                    &[("count", &app.settings.preview_limit.to_string())],
                ))
                .width(PICKER_WIDTH)
                .into(),
            ),
            hint(strings.get("settings.preview.hint")),
        ],
    )
}

fn updates(app: &Hoplix) -> Element<'_, Message> {
    let strings = &app.strings;
    let checking = app.update_state == CheckState::Checking;
    let available = app.available_update();
    let status = match (&app.update_state, &available) {
        (CheckState::Checking, _) => strings.get("settings.updates.checking").to_string(),
        (CheckState::Failed(error), _) => {
            strings.format("settings.updates.failed", &[("detail", error.detail())])
        }
        (_, Some(update)) => strings.format(
            "settings.updates.available",
            &[("version", &update.version)],
        ),
        (CheckState::UpToDate | CheckState::Available, None) => {
            strings.get("settings.updates.up_to_date").to_string()
        }
        (CheckState::Idle, None) => String::new(),
    };
    let mut check = row![
        button(text(strings.get("settings.updates.check")))
            .style(theme::secondary_button)
            .on_press_maybe((!checking).then_some(Message::Update(UpdateMessage::CheckNow))),
        text(status),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    if let Some(update) = available {
        check = check.push(
            button(text(strings.get("update.open")))
                .style(button::text)
                .on_press(Message::Update(UpdateMessage::OpenReleasePage(update.url))),
        );
    }
    let last_check = app
        .settings
        .update_last_check
        .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
        .and_then(local_time::local)
        .map_or_else(
            || strings.get("settings.updates.never").to_string(),
            |time| local_time::format(&time, strings.get("date.format")),
        );
    section(
        strings.get("settings.section.updates"),
        vec![
            checkbox_row(
                checkbox(app.settings.update_check)
                    .label(strings.get("settings.updates.automatic"))
                    .on_toggle(|value| Message::Update(UpdateMessage::AutomaticToggled(value)))
                    .into(),
            ),
            setting(
                strings.get("settings.updates.version"),
                text(CURRENT_VERSION).into(),
            ),
            setting(strings.get("settings.updates.label"), check.into()),
            setting(
                strings.get("settings.updates.last_check"),
                text(last_check).into(),
            ),
            hint(strings.get("settings.updates.hint")),
        ],
    )
}

// Controls

fn direction_picker<'a>(
    strings: &'a Strings,
    current: Option<usize>,
    message: fn(Option<usize>) -> TransferSetting,
) -> Element<'a, Message> {
    let mut choices = vec![Choice {
        value: None,
        label: strings.get("settings.parallel.same").to_string(),
    }];
    choices.extend((1..=MAX_PARALLEL_TRANSFERS).map(|count| Choice {
        value: Some(count),
        label: count.to_string(),
    }));
    let current = selected(&choices, &current);
    pick_list(choices, current, move |choice| {
        Message::TransferSetting(message(choice.value))
    })
    .width(PICKER_WIDTH)
    .into()
}

fn speed_limit<'a>(
    strings: &'a Strings,
    limit: SpeedLimit,
    amount: fn(String) -> TransferSetting,
    unit: fn(RateUnit) -> TransferSetting,
) -> Element<'a, Message> {
    let value = if limit.amount == 0 {
        String::new()
    } else {
        limit.amount.to_string()
    };
    let units: Vec<Choice<RateUnit>> = RateUnit::ALL
        .into_iter()
        .map(|value| Choice {
            value,
            label: strings
                .get(match value {
                    RateUnit::KiB => "settings.unit.kib",
                    RateUnit::MiB => "settings.unit.mib",
                })
                .to_string(),
        })
        .collect();
    let current = selected(&units, &limit.unit);
    row![
        text_input(strings.get("settings.limit.unlimited"), &value)
            .on_input(move |text| Message::TransferSetting(amount(text)))
            .padding(6)
            .width(AMOUNT_WIDTH),
        pick_list(units, current, move |choice| {
            Message::TransferSetting(unit(choice.value))
        })
        .width(UNIT_WIDTH),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

// Layout

fn section<'a>(title: &'a str, rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    column![
        text(title).size(18).style(text::primary),
        rule::horizontal(1),
        Column::with_children(rows).spacing(16),
    ]
    .spacing(12)
    .into()
}

fn setting<'a>(label: &'a str, control: Element<'a, Message>) -> Element<'a, Message> {
    row![text(label).width(LABEL_WIDTH), control]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
}

fn checkbox_row(control: Element<'_, Message>) -> Element<'_, Message> {
    row![Space::new().width(LABEL_WIDTH), control]
        .spacing(12)
        .into()
}

fn hint(content: &str) -> Element<'_, Message> {
    text(content).size(HINT_SIZE).into()
}

fn selected<T: Clone + PartialEq>(choices: &[Choice<T>], value: &T) -> Option<Choice<T>> {
    choices
        .iter()
        .find(|choice| choice.value == *value)
        .cloned()
}
