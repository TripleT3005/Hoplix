use iced::keyboard::key::Named;
use iced::theme::Mode;

use super::*;
use crate::app::Page;
use crate::settings::ThemePreference;

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
