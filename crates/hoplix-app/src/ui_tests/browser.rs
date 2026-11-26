use std::time::{Duration as Age, SystemTime};

use hoplix_core::{SortKey, SortOrder};
use iced::keyboard::key::Named;

use super::*;
use crate::app::{Dialog, LoadState, Page};

// Helpers

fn folder(name: &str) -> String {
    format!("{name}{}", std::path::MAIN_SEPARATOR)
}

fn with_files(name: &str, files: &[&str]) -> Harness {
    let files: Vec<String> = files.iter().map(ToString::to_string).collect();
    Harness::start(name, Layout::Modern, move |root| {
        let names: Vec<&str> = files.iter().map(String::as_str).collect();
        write_files(&root.data(), &names);
    })
}

fn age(path: &Path, seconds: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .expect("opening failed")
        .set_modified(SystemTime::now() - Age::from_secs(seconds))
        .expect("setting the time failed");
}

// Startup

#[test]
fn the_browser_lists_the_test_folder() {
    let mut harness = with_files("startup", &["b.txt", "a.txt", "docs/"]);
    assert_eq!(harness.tab().path, text_of(&harness.root.data()));
    assert_eq!(harness.tab().state, LoadState::Ready);
    assert_eq!(harness.names(), ["docs", "a.txt", "b.txt"]);
    assert!(harness.has(&folder("docs")));
    assert!(harness.has("3 items"));
    assert!(harness.has("TXT file"));
}

// Mouse selection

#[test]
fn clicks_select_and_modifiers_extend_the_selection() {
    let mut harness = with_files("select", &["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);
    harness.click("a.txt");
    assert_eq!(harness.selected(), ["a.txt"]);
    harness.click_with("c.txt", Modifiers::CTRL);
    assert_eq!(harness.selected(), ["a.txt", "c.txt"]);
    harness.click_with("a.txt", Modifiers::CTRL);
    assert_eq!(harness.selected(), ["c.txt"]);
    harness.click_with("b.txt", Modifiers::SHIFT);
    assert_eq!(harness.selected(), ["a.txt", "b.txt"]);
    harness.click("c.txt");
    harness.click_with("e.txt", Modifiers::SHIFT);
    assert_eq!(harness.selected(), ["c.txt", "d.txt", "e.txt"]);
    harness.click_with("b.txt", Modifiers::SHIFT);
    assert_eq!(harness.selected(), ["b.txt", "c.txt"]);
    harness.click("d.txt");
    assert_eq!(harness.selected(), ["d.txt"]);
    assert!(harness.app.drag.is_none());
    assert!(harness.app.transfers.snapshots().is_empty());
}

#[test]
fn double_clicks_open_folders() {
    let mut harness = with_files("double-click", &["docs/inner.txt", "a.txt"]);
    harness.double_click(&folder("docs"));
    assert_eq!(
        harness.tab().path,
        text_of(&harness.root.data().join("docs"))
    );
    assert_eq!(harness.names(), ["inner.txt"]);
    harness.click("Up");
    assert_eq!(harness.tab().path, text_of(&harness.root.data()));
    harness.double_click("a.txt");
    assert_eq!(harness.tab().path, text_of(&harness.root.data()));
}

// Keyboard

#[test]
fn keys_select_navigate_and_refresh() {
    let mut harness = with_files("keys", &["docs/inner.txt", "a.txt", "b.txt", "c.txt"]);
    harness.shortcut("a", Modifiers::CTRL);
    assert_eq!(harness.selected(), ["docs", "a.txt", "b.txt", "c.txt"]);

    harness.click("a.txt");
    harness.key(Named::ArrowDown);
    assert_eq!(harness.selected(), ["b.txt"]);
    harness.tap(Key::Named(Named::ArrowDown), Modifiers::SHIFT);
    assert_eq!(harness.selected(), ["b.txt", "c.txt"]);
    harness.tap(Key::Named(Named::ArrowUp), Modifiers::SHIFT);
    harness.tap(Key::Named(Named::ArrowUp), Modifiers::SHIFT);
    assert_eq!(harness.selected(), ["a.txt", "b.txt"]);
    harness.key(Named::Home);
    assert_eq!(harness.selected(), ["docs"]);
    harness.key(Named::End);
    assert_eq!(harness.selected(), ["c.txt"]);

    harness.key(Named::Home);
    harness.key(Named::Enter);
    assert_eq!(
        harness.tab().path,
        text_of(&harness.root.data().join("docs"))
    );
    harness.key(Named::Backspace);
    assert_eq!(harness.tab().path, text_of(&harness.root.data()));

    write_files(&harness.root.data(), &["new.txt"]);
    assert!(!harness.names().contains(&"new.txt".to_string()));
    harness.key(Named::F5);
    assert!(harness.names().contains(&"new.txt".to_string()));
}

#[test]
fn typing_in_the_path_field_does_not_trigger_shortcuts() {
    let mut harness = with_files("path", &["docs/inner.txt", "a.txt"]);
    let id = harness.tab().id;
    let input = harness.input_bounds(&crate::ui::path_input_id(id));
    harness.click_at(input.center());
    harness.key(Named::Backspace);
    harness.shortcut("a", Modifiers::CTRL);
    assert_eq!(harness.tab().path, text_of(&harness.root.data()));
    assert!(harness.selected().is_empty());

    let target = text_of(&harness.root.data().join("docs"));
    harness.shortcut("a", Modifiers::CTRL);
    harness.key(Named::Backspace);
    harness.type_text(&target);
    harness.key(Named::Enter);
    assert_eq!(harness.tab().path, target);
    assert_eq!(harness.names(), ["inner.txt"]);
}

#[test]
fn arrow_keys_scroll_only_when_the_row_leaves_the_view() {
    let names: Vec<String> = (0..200)
        .map(|index| format!("item {index:03}.txt"))
        .collect();
    let files: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut harness = with_files("scroll", &files);
    harness.redraw();
    let offset = |harness: &Harness| {
        harness
            .tab()
            .viewport
            .map(|viewport| viewport.absolute_offset().y)
    };
    assert_eq!(offset(&harness), Some(0.0));

    for _ in 0..5 {
        harness.key(Named::ArrowDown);
    }
    harness.redraw();
    assert_eq!(offset(&harness), Some(0.0));

    for _ in 0..95 {
        harness.key(Named::ArrowDown);
    }
    harness.redraw();
    assert_eq!(harness.selected(), ["item 099.txt"]);
    let viewport = harness.tab().viewport.expect("the list scrolls");
    let row = viewport.content_bounds().height / 200.0;
    let bottom = viewport.absolute_offset().y + viewport.bounds().height;
    assert!((100.0 * row - bottom).abs() < 1.0);

    harness.key(Named::ArrowUp);
    harness.redraw();
    assert_eq!(offset(&harness), Some(viewport.absolute_offset().y));

    harness.key(Named::Home);
    harness.redraw();
    assert_eq!(offset(&harness), Some(0.0));
}

// Operations

#[test]
fn f2_renames_the_selected_file() {
    let mut harness = with_files("rename", &["a.txt", "b.txt"]);
    harness.key(Named::F2);
    assert!(harness.app.dialog.is_none());
    harness.click("a.txt");
    harness.key(Named::F2);
    assert!(matches!(harness.app.dialog, Some(Dialog::Rename { .. })));
    assert!(harness.has("Rename “a.txt”"));
    harness.shortcut("a", Modifiers::CTRL);
    harness.type_text("renamed.txt");
    harness.key(Named::Enter);
    assert!(harness.app.dialog.is_none());
    assert!(harness.root.data().join("renamed.txt").is_file());
    assert!(!harness.root.data().join("a.txt").exists());
    assert_eq!(harness.names(), ["b.txt", "renamed.txt"]);
    assert_eq!(harness.selected(), Vec::<String>::new());
}

#[test]
fn renaming_to_an_invalid_name_keeps_the_dialog_open() {
    let mut harness = with_files("rename-invalid", &["a.txt"]);
    harness.click("a.txt");
    harness.click("Rename");
    harness.shortcut("a", Modifiers::CTRL);
    harness.type_text("x/y");
    harness.key(Named::Enter);
    assert!(matches!(harness.app.dialog, Some(Dialog::Rename { .. })));
    harness.key(Named::Escape);
    assert!(harness.app.dialog.is_none());
    assert!(harness.root.data().join("a.txt").is_file());
}

#[test]
fn delete_asks_for_confirmation() {
    let mut harness = with_files("delete", &["a.txt", "b.txt", "docs/inner.txt"]);
    harness.click("a.txt");
    harness.key(Named::Delete);
    assert!(harness.has("Delete “a.txt”?"));
    harness.key(Named::Escape);
    assert!(harness.app.dialog.is_none());
    assert!(harness.root.data().join("a.txt").is_file());

    harness.click_with(&folder("docs"), Modifiers::CTRL);
    harness.key(Named::Delete);
    assert!(harness.has("Delete 2 items?"));
    assert!(harness.has("The folder and everything in it will be deleted permanently."));
    let buttons = harness.find_all("Delete");
    harness.click_at(buttons.last().expect("no delete button").center());
    assert!(harness.app.dialog.is_none());
    assert!(!harness.root.data().join("a.txt").exists());
    assert!(!harness.root.data().join("docs").exists());
    assert_eq!(harness.names(), ["b.txt"]);
}

#[test]
fn clicking_outside_a_dialog_cancels_it() {
    let mut harness = with_files("dialog-outside", &["a.txt"]);
    harness.click("New folder");
    assert!(matches!(harness.app.dialog, Some(Dialog::NewFolder { .. })));
    harness.type_text("created");
    harness.click_at(Point::new(10.0, 990.0));
    assert!(harness.app.dialog.is_none());
    assert!(!harness.root.data().join("created").exists());

    harness.click("New folder");
    harness.type_text("created");
    harness.click("Create");
    assert!(harness.root.data().join("created").is_dir());
    assert_eq!(harness.names(), ["created", "a.txt"]);
}

#[test]
fn unknown_paths_show_an_error_and_keep_the_listing() {
    let mut harness = with_files("missing-path", &["a.txt"]);
    let id = harness.tab().id;
    let input = harness.input_bounds(&crate::ui::path_input_id(id));
    let missing = text_of(&harness.root.data().join("missing"));
    harness.fill_at(input.center(), &missing);
    harness.key(Named::Enter);
    assert!(matches!(harness.tab().state, LoadState::Failed(_)));
    assert!(harness.contains("The path does not exist"));
    assert_eq!(harness.tab().path, text_of(&harness.root.data()));
    assert_eq!(harness.tab().path_input, text_of(&harness.root.data()));
    assert_eq!(harness.names(), ["a.txt"]);
    harness.click("Refresh");
    assert_eq!(harness.tab().state, LoadState::Ready);
}

#[test]
fn enter_confirms_the_delete_dialog() {
    let mut harness = with_files("delete-enter", &["a.txt", "b.txt"]);
    harness.click("b.txt");
    harness.click("Delete");
    harness.key(Named::Enter);
    assert!(harness.app.dialog.is_none());
    assert_eq!(harness.names(), ["a.txt"]);
}

// Sorting

#[test]
fn column_headers_sort_the_list() {
    let mut harness = Harness::start("sort", Layout::Modern, |root| {
        write_files(&root.data(), &["b.txt", "a.txt", "c.txt"]);
        fs::write(root.data().join("b.txt"), "a much longer file").expect("writing failed");
        age(&root.data().join("a.txt"), 300);
        age(&root.data().join("b.txt"), 100);
        age(&root.data().join("c.txt"), 200);
    });
    assert_eq!(harness.names(), ["a.txt", "b.txt", "c.txt"]);
    harness.click("Name ▲");
    assert_eq!(harness.tab().sort_order, SortOrder::Descending);
    assert_eq!(harness.names(), ["c.txt", "b.txt", "a.txt"]);

    harness.click("Modified");
    assert_eq!(harness.tab().sort_key, SortKey::Modified);
    assert_eq!(harness.names(), ["a.txt", "c.txt", "b.txt"]);
    harness.click("Modified ▲");
    assert_eq!(harness.names(), ["b.txt", "c.txt", "a.txt"]);
    assert!(harness.has("Modified ▼"));

    harness.click("Size");
    assert_eq!(harness.tab().sort_key, SortKey::Size);
    assert_eq!(harness.names().last().map(String::as_str), Some("b.txt"));
}

// Tabs

#[test]
fn shortcuts_open_cycle_and_close_tabs() {
    let mut harness = with_files("tabs", &["a.txt"]);
    harness.shortcut("t", Modifiers::CTRL);
    assert_eq!(harness.app.panes[0].tabs.len(), 2);
    assert_eq!(harness.app.panes[0].active, 1);
    assert_eq!(harness.tab().path, text_of(&harness.root.data()));
    harness.tap(Key::Named(Named::Tab), Modifiers::CTRL);
    assert_eq!(harness.app.panes[0].active, 0);
    harness.tap(Key::Named(Named::Tab), Modifiers::CTRL | Modifiers::SHIFT);
    assert_eq!(harness.app.panes[0].active, 1);
    harness.shortcut("w", Modifiers::CTRL);
    assert_eq!(harness.app.panes[0].tabs.len(), 1);

    harness.click("+");
    assert_eq!(harness.app.panes[0].tabs.len(), 2);
    harness.click_nth("data", 0);
    assert_eq!(harness.app.panes[0].active, 0);
    harness.click_nth("×", 1);
    assert_eq!(harness.app.panes[0].tabs.len(), 1);
    harness.click("×");
    assert!(harness.app.panes[0].tabs.is_empty());
    assert!(harness.has("No tabs are open."));
    harness.shortcut("w", Modifiers::CTRL);
    harness.shortcut("t", Modifiers::CTRL);
    assert_eq!(harness.app.panes[0].tabs.len(), 1);
}

#[test]
fn the_classic_layout_shows_two_panes() {
    let mut harness = Harness::start("classic", Layout::Classic, |root| {
        write_files(&root.data(), &["a.txt"]);
    });
    assert_eq!(harness.app.panes.len(), 2);
    assert_eq!(harness.find_all("a.txt").len(), 2);
    assert!(harness.has("Copy to other side →"));
    assert!(harness.has("← Copy to other side"));
    let right = harness.find_all("a.txt")[1];
    harness.click_at(right.center());
    assert_eq!(harness.app.focused_pane, 1);
    harness.shortcut("t", Modifiers::CTRL);
    assert_eq!(harness.app.panes[1].tabs.len(), 2);
    assert_eq!(harness.app.panes[0].tabs.len(), 1);
    assert_eq!(harness.app.page, Page::Browser);
}

// Action row

const ACTION_LINE: f32 = 13.0 * LINE_HEIGHT + 0.5;
const OUTER_PADDING: f32 = 8.0;
const PANE_SPACING: f32 = 8.0;

fn action_labels(language: &str) -> [&'static str; 8] {
    if language == "de" {
        [
            "Neuer Ordner",
            "Umbenennen",
            "Löschen",
            "Bearbeiten",
            "Vorschau (F3)",
            "Zur anderen Seite kopieren →",
            "← Zur anderen Seite kopieren",
            "Kopieren nach …",
        ]
    } else {
        [
            "New folder",
            "Rename",
            "Delete",
            "Edit",
            "Preview (F3)",
            "Copy to other side →",
            "← Copy to other side",
            "Copy to…",
        ]
    }
}

fn browser_in(name: &str, layout: Layout, language: &str) -> Harness {
    let language = language.to_string();
    Harness::start(name, layout, move |root| {
        Settings {
            language: Some(language),
            layout,
            ..Settings::default()
        }
        .save(&root.config())
        .expect("saving the settings failed");
        write_files(&root.data(), &["a.txt", "docs/"]);
    })
}

fn assert_actions_fit(harness: &mut Harness, labels: &[&str], case: &str) {
    let width = harness.viewport.width;
    let count = harness.app.panes.len();
    let pane_width =
        (width - 2.0 * OUTER_PADDING - PANE_SPACING * (count - 1) as f32) / count as f32;
    let mut found: Vec<Vec<Rectangle>> = vec![Vec::new(); count];
    for label in labels {
        for (bounds, visible) in harness.find_texts(label) {
            let visible = visible.unwrap_or_else(|| panic!("{case}: {label:?} is hidden"));
            assert!(
                visible.width + 0.5 >= bounds.width && visible.height + 0.5 >= bounds.height,
                "{case}: {label:?} is cut off: {visible:?} of {bounds:?}"
            );
            assert!(
                bounds.height <= ACTION_LINE,
                "{case}: {label:?} wraps: {bounds:?}"
            );
            let pane = ((bounds.x - OUTER_PADDING) / (pane_width + PANE_SPACING)) as usize;
            let left = OUTER_PADDING + pane as f32 * (pane_width + PANE_SPACING);
            assert!(
                bounds.x >= left && bounds.x + bounds.width <= left + pane_width,
                "{case}: {label:?} leaves pane {pane}: {bounds:?}"
            );
            assert!(
                found[pane]
                    .iter()
                    .all(|other| other.intersection(&bounds).is_none()),
                "{case}: {label:?} overlaps another action"
            );
            found[pane].push(bounds);
        }
    }
    for (pane, actions) in found.iter().enumerate() {
        assert!(actions.len() >= 5, "{case}: pane {pane} shows {actions:?}");
    }
}

#[test]
fn the_action_row_fits_its_pane_in_all_layouts_and_languages() {
    for language in ["en", "de"] {
        let labels = action_labels(language);
        for layout in [Layout::Classic, Layout::Modern] {
            for size in [Size::new(1000.0, 700.0), Size::new(1500.0, 950.0)] {
                let case = format!("{language}-{layout:?}-{}", size.width);
                let mut harness = browser_in(&format!("actions-{case}"), layout, language);
                harness.resize(size);
                assert_actions_fit(&mut harness, &labels, &case);
                harness.key(Named::F3);
                assert_actions_fit(&mut harness, &labels, &case);
            }
        }
    }
}
