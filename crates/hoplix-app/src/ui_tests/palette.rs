use hoplix_core::{Site, SiteList};
use iced::keyboard::key::Named;

use super::*;
use crate::app::Page;

// Helpers

fn with_site(name: &str) -> Harness {
    Harness::start(name, Layout::Modern, |root| {
        write_files(&root.data(), &["a.txt"]);
        let mut list = SiteList::default();
        list.upsert(Site {
            host: "backup.example.test".to_string(),
            username: "demo".to_string(),
            ..Site::new("Backup server")
        });
        list.save(&root.config()).expect("saving the sites failed");
    })
}

// Palette

#[test]
fn the_palette_opens_filters_and_runs_commands() {
    let mut harness = with_site("palette-run");
    harness.shortcut("k", Modifiers::CTRL);
    assert!(harness.app.palette.is_some());
    assert!(harness.has("Connect to a server"));
    harness.type_text("settings");
    assert_eq!(
        harness
            .app
            .palette
            .as_ref()
            .map(|palette| palette.query.as_str()),
        Some("settings")
    );
    assert!(harness.has("Open settings"));
    assert!(!harness.has("New tab"));
    harness.key(Named::Enter);
    assert!(harness.app.palette.is_none());
    assert_eq!(harness.app.page, Page::Settings);
}

#[test]
fn escape_closes_the_palette() {
    let mut harness = with_site("palette-escape");
    harness.click("Commands");
    assert!(harness.app.palette.is_some());
    harness.type_text("xyz");
    assert!(harness.has("No matching command."));
    harness.key(Named::Escape);
    assert!(harness.app.palette.is_none());
    harness.shortcut("k", Modifiers::CTRL);
    harness.click_at(Point::new(10.0, 990.0));
    assert!(harness.app.palette.is_none());
}

#[test]
fn arrows_choose_the_command() {
    let mut harness = with_site("palette-arrows");
    harness.shortcut("k", Modifiers::CTRL);
    harness.type_text("tab");
    assert!(harness.has("New tab"));
    harness.key(Named::ArrowDown);
    assert_eq!(
        harness.app.palette.as_ref().map(|palette| palette.selected),
        Some(1)
    );
    harness.key(Named::ArrowUp);
    harness.key(Named::Enter);
    assert_eq!(harness.app.panes[0].tabs.len(), 2);
}

#[test]
fn clicking_a_command_runs_it_and_closes_the_palette() {
    let mut harness = with_site("palette-click");
    harness.shortcut("k", Modifiers::CTRL);
    harness.type_text("select");
    harness.click("Select all");
    assert!(harness.app.palette.is_none());
    assert_eq!(harness.selected(), ["a.txt"]);
}

#[test]
fn saved_sites_can_be_connected_from_the_palette() {
    let mut harness = with_site("palette-site");
    harness.shortcut("k", Modifiers::CTRL);
    harness.type_text("backup");
    assert!(harness.has("Connect to Backup server"));
    harness.key(Named::Enter);
    assert!(harness.app.palette.is_none());
    assert_eq!(harness.app.page, Page::Connect);
    assert_eq!(harness.app.connect_form.host, "backup.example.test");
}
