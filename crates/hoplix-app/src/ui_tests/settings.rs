use iced::keyboard::key::Named;
use iced::theme::Mode;

use hoplix_core::{RetryPolicy, SpeedLimits};

use super::*;
use crate::app::Page;
use crate::settings::{RateUnit, SpeedLimit, ThemePreference};

// Helpers

fn settings_page(name: &str) -> Harness {
    let mut harness = Harness::new(name);
    harness.click("Settings");
    assert_eq!(harness.app.page, Page::Settings);
    harness
}

fn saved(harness: &Harness) -> Settings {
    Settings::load(Some(&harness.root.config()))
}

fn is_dark(harness: &Harness) -> bool {
    harness.app.theme().extended_palette().is_dark
}

// Theme

#[test]
fn the_theme_switches_at_runtime() {
    let mut harness = settings_page("theme");
    harness.pick("Appearance", 2);
    assert_eq!(harness.app.settings.theme, ThemePreference::Dark);
    assert!(is_dark(&harness));
    assert_eq!(saved(&harness).theme, ThemePreference::Dark);
    harness.pick("Appearance", 1);
    assert_eq!(harness.app.settings.theme, ThemePreference::Light);
    assert!(!is_dark(&harness));

    harness.pick("Appearance", 0);
    assert_eq!(harness.app.settings.theme, ThemePreference::System);
    assert_eq!(saved(&harness).theme, ThemePreference::System);
    if crate::theme::mode_override().is_none() {
        harness.send(Message::SystemThemeChanged(Mode::Dark));
        assert!(is_dark(&harness));
        harness.send(Message::SystemThemeChanged(Mode::Light));
        assert!(!is_dark(&harness));
    }
}

#[test]
fn theme_changes_update_the_title_bar() {
    let mut harness = Harness::new("title-bar");
    let task = harness
        .app
        .update(Message::ThemeSelected(ThemePreference::Dark));
    let first = task::into_stream(task)
        .and_then(|mut stream| iced_test::futures::futures::executor::block_on(stream.next()));
    if cfg!(windows) {
        assert!(matches!(first, Some(Action::Window(_))));
    } else {
        assert!(first.is_none());
    }
    let task = harness
        .app
        .update(Message::ThemeSelected(ThemePreference::Light));
    assert_eq!(task::into_stream(task).is_some(), cfg!(windows));

    let task = harness
        .app
        .update(Message::RunAction(crate::commands::Action::ToggleTheme));
    assert!(is_dark(&harness));
    let first = task::into_stream(task)
        .and_then(|mut stream| iced_test::futures::futures::executor::block_on(stream.next()));
    assert_eq!(matches!(first, Some(Action::Window(_))), cfg!(windows));
}

// Language

#[test]
fn the_language_switches_at_runtime() {
    let mut harness = settings_page("language");
    harness.pick("Language", 1);
    assert_eq!(harness.app.settings.language.as_deref(), Some("de"));
    assert!(harness.has("Einstellungen"));
    assert!(harness.has("Zurück"));
    assert_eq!(saved(&harness).language.as_deref(), Some("de"));
    harness.pick("Sprache", 2);
    assert_eq!(harness.app.settings.language.as_deref(), Some("en"));
    assert!(harness.has("Settings"));
    assert!(harness.has("Back"));
}

// Layout and transfers

#[test]
fn layout_and_parallel_transfers_are_saved() {
    let mut harness = settings_page("layout");
    harness.pick("Layout", 1);
    assert_eq!(harness.app.panes.len(), 2);
    assert_eq!(saved(&harness).layout, Layout::Classic);
    harness.pick("Parallel transfers", 3);
    assert_eq!(harness.app.settings.parallel_transfers, 4);
    assert_eq!(saved(&harness).parallel_transfers, 4);

    harness.key(Named::Escape);
    assert_eq!(harness.app.page, Page::Browser);
    assert!(harness.has("Copy to other side →"));
    harness.click("Settings");
    harness.pick("Layout", 0);
    assert_eq!(harness.app.panes.len(), 1);
    assert_eq!(harness.app.panes[0].tabs.len(), 2);
    harness.click("Back");
    assert_eq!(harness.app.page, Page::Browser);
}

#[test]
fn the_palette_toggles_theme_and_layout() {
    let mut harness = Harness::new("toggles");
    let dark = is_dark(&harness);
    harness.shortcut("k", Modifiers::CTRL);
    harness.type_text("light and dark");
    harness.key(Named::Enter);
    assert_ne!(is_dark(&harness), dark);
    harness.shortcut("k", Modifiers::CTRL);
    harness.type_text("classic layout");
    harness.key(Named::Enter);
    assert_eq!(harness.app.settings.layout, Layout::Classic);
    assert_eq!(harness.app.panes.len(), 2);
    assert_eq!(saved(&harness).layout, Layout::Classic);
}

// Sections

const UNIT_OFFSET: f32 = 160.0 + 12.0 + 140.0 + 8.0 + 24.0;

#[test]
fn settings_are_grouped_in_sections_in_both_languages() {
    let mut harness = settings_page("sections");
    for title in [
        "General",
        "Transfers",
        "Connections",
        "Passwords",
        "Editing",
        "Upload changes automatically",
    ] {
        assert!(harness.has(title), "{title}");
    }
    harness.scroll_settings(0.0);
    harness.pick("Language", 1);
    for title in [
        "Allgemein",
        "Übertragungen",
        "Gleichzeitige Downloads",
        "Download-Limit",
        "Verbindungen",
        "Abgebrochene SFTP-Sitzungen automatisch neu verbinden",
        "Passwörter",
        "Bearbeiten",
    ] {
        assert!(harness.has(title), "{title}");
    }
}

// Transfer settings

#[test]
fn transfer_limits_update_the_queue_and_persist() {
    let mut harness = settings_page("transfer-limits");
    harness.pick("Parallel downloads", 2);
    harness.pick("Parallel uploads", 1);
    assert_eq!(harness.app.settings.parallel_downloads, Some(2));
    assert_eq!(harness.app.settings.parallel_uploads, Some(1));
    let parallelism = harness.app.transfers.parallelism();
    assert_eq!(
        (parallelism.downloads, parallelism.uploads),
        (Some(2), Some(1))
    );
    assert_eq!(parallelism.download_limit(), 2);
    assert_eq!(parallelism.upload_limit(), 1);

    harness.pick("Parallel downloads", 0);
    assert_eq!(harness.app.transfers.parallelism().downloads, None);
    assert_eq!(saved(&harness).parallel_downloads, None);
    assert_eq!(saved(&harness).parallel_uploads, Some(1));

    harness.fill("Download limit", "2");
    assert_eq!(
        harness.app.transfers.speed_limits().download,
        Some(2 * 1024 * 1024)
    );
    let row = harness.bounds("Download limit");
    harness.pick_at(
        Point::new(row.x + UNIT_OFFSET, row.center_y()),
        0,
        2,
        FONT_SIZE,
    );
    assert_eq!(harness.app.settings.download_limit.unit, RateUnit::KiB);
    assert_eq!(
        harness.app.transfers.speed_limits().download,
        Some(2 * 1024)
    );

    harness.fill("Upload limit", "12x");
    assert_eq!(harness.app.settings.upload_limit.amount, 12);
    harness.fill("Upload limit", "0");
    assert_eq!(harness.app.transfers.speed_limits().upload, None);
    harness.fill("Upload limit", "750");
    assert_eq!(
        harness.app.transfers.speed_limits(),
        SpeedLimits {
            download: Some(2 * 1024),
            upload: Some(750 * 1024 * 1024),
        }
    );
    assert_eq!(
        saved(&harness).download_limit,
        SpeedLimit {
            amount: 2,
            unit: RateUnit::KiB,
        }
    );
    assert_eq!(saved(&harness).upload_limit.amount, 750);

    harness.fill("Upload limit", "");
    assert_eq!(harness.app.transfers.speed_limits().upload, None);
    assert_eq!(saved(&harness).upload_limit.amount, 0);
}

#[test]
fn retry_settings_update_the_queue_and_persist() {
    let mut harness = settings_page("retry-settings");
    assert!(harness.has("First retry after"));
    harness.pick("Retries", 0);
    assert_eq!(harness.app.settings.retry_attempts, 0);
    harness.pick("Retries", 7);
    harness.pick("First retry after", 4);
    assert_eq!(
        harness.app.transfers.retry_policy(),
        RetryPolicy {
            attempts: 7,
            delay: Duration::from_secs(5),
        }
    );

    assert!(harness.app.settings.auto_reconnect);
    harness.click("Reconnect dropped SFTP sessions automatically");
    assert!(!harness.app.settings.auto_reconnect);
    let stored = saved(&harness);
    assert_eq!((stored.retry_attempts, stored.retry_delay), (7, 5));
    assert!(!stored.auto_reconnect);
    assert_eq!(stored, harness.app.settings);
    harness.click("Reconnect dropped SFTP sessions automatically");
    assert!(saved(&harness).auto_reconnect);
}

#[test]
fn saved_transfer_settings_apply_at_start() {
    let harness = Harness::start("transfer-start", Layout::Modern, |root| {
        Settings {
            language: Some("en".to_string()),
            parallel_transfers: 5,
            parallel_downloads: Some(3),
            parallel_uploads: None,
            download_limit: SpeedLimit {
                amount: 300,
                unit: RateUnit::KiB,
            },
            retry_attempts: 9,
            retry_delay: 20,
            ..Settings::default()
        }
        .save(&root.config())
        .expect("saving the test settings failed");
    });
    let queue = &harness.app.transfers;
    assert_eq!(queue.parallelism().total, 5);
    assert_eq!(queue.parallelism().download_limit(), 3);
    assert_eq!(queue.parallelism().upload_limit(), 5);
    assert_eq!(queue.speed_limits().download, Some(300 * 1024));
    assert_eq!(queue.speed_limits().upload, None);
    assert_eq!(
        queue.retry_policy(),
        RetryPolicy {
            attempts: 9,
            delay: Duration::from_secs(20),
        }
    );
}
