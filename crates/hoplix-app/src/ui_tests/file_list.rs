use hoplix_core::{Bookmarks, SiteExport, SiteList};
use iced::keyboard::key::Named;

use super::*;
use crate::app::{Dialog, TabId};
use crate::properties::{Loaded, SizeState};
use crate::session::TabOrigin;

// Constants

const SERVER: &str = "demo@files.example.test";
const PICKER_CENTER_X: f32 = 1280.0 - 16.0 - 75.0;

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

fn connected(name: &str, files: &[&str]) -> Harness {
    let files: Vec<String> = files.iter().map(ToString::to_string).collect();
    let mut harness = Harness::start(name, Layout::Modern, move |root| {
        let names: Vec<&str> = files.iter().map(String::as_str).collect();
        write_files(&root.remote(), &names);
    });
    let server = fakes::Folder::remote(&harness.root.remote(), SERVER.to_string());
    harness.send(Message::Connected(Ok(server)));
    assert!(harness.tab().file_system.label().is_some());
    harness
}

fn type_filter(harness: &mut Harness, text: &str) {
    let id = harness.tab().id;
    let input = harness.input_bounds(&crate::ui::filter_input_id(id));
    harness.fill_at(input.center(), text);
}

fn path_of(harness: &Harness, parts: &[&str]) -> String {
    let mut path = harness.root.data();
    for part in parts {
        path = path.join(part);
    }
    text_of(&path)
}

fn remote_path(harness: &Harness, parts: &[&str]) -> String {
    let mut path = harness.root.remote();
    for part in parts {
        path = path.join(part);
    }
    text_of(&path)
}

fn properties(harness: &Harness) -> &crate::properties::PropertiesDialog {
    match &harness.app.dialog {
        Some(Dialog::Properties(dialog)) => dialog,
        other => panic!("no properties dialog: {other:?}"),
    }
}

fn bookmark_picker(harness: &mut Harness) -> Point {
    let refresh = harness.bounds("Refresh");
    Point::new(PICKER_CENTER_X, refresh.center_y())
}

fn is_bookmarked(harness: &Harness) -> bool {
    harness
        .app
        .tab_bookmarks(harness.tab())
        .is_some_and(|bookmarks| bookmarks.contains(&harness.tab().path))
}

fn local_bookmarks(harness: &Harness) -> Bookmarks {
    Bookmarks::load(&harness.root.config()).expect("reading the bookmarks failed")
}

// Filter

#[test]
fn the_filter_hides_names_and_counts_the_rest() {
    let mut harness = with_files("filter", &["docs/", "alpha.txt", "beta.log", "gamma.TXT"]);
    type_filter(&mut harness, "txt");
    assert!(harness.has("alpha.txt"));
    assert!(harness.has("gamma.TXT"));
    assert!(!harness.has("beta.log"));
    assert!(!harness.has(&folder("docs")));
    assert!(harness.has("2 of 4 items"));

    type_filter(&mut harness, "*.LO?");
    assert!(harness.has("beta.log"));
    assert!(!harness.has("alpha.txt"));
    assert!(harness.has("1 of 4 items"));

    type_filter(&mut harness, "zzz");
    assert!(harness.has("No names match the filter."));

    harness.key(Named::Escape);
    assert!(harness.tab().list.filter.is_empty());
    assert!(harness.has("4 items"));
    assert!(harness.has("beta.log"));
}

#[test]
fn ctrl_f_focuses_the_filter_and_keys_only_reach_visible_entries() {
    let mut harness = with_files(
        "filter-keys",
        &["a1.txt", "b.log", "a2.txt", "c.log", "a3.txt"],
    );
    harness.shortcut("f", Modifiers::CTRL);
    harness.type_text("a");
    assert_eq!(harness.tab().list.filter, "a");
    assert!(harness.has("3 of 5 items"));

    harness.key(Named::ArrowDown);
    assert_eq!(harness.selected(), ["a1.txt"]);
    harness.key(Named::ArrowDown);
    assert_eq!(harness.selected(), ["a2.txt"]);
    harness.tap(Key::Named(Named::ArrowDown), Modifiers::SHIFT);
    assert_eq!(harness.selected(), ["a2.txt", "a3.txt"]);

    harness.click("a1.txt");
    harness.click_with("a3.txt", Modifiers::SHIFT);
    assert_eq!(harness.selected(), ["a1.txt", "a2.txt", "a3.txt"]);

    harness.send(Message::RunAction(crate::commands::Action::SelectAll));
    assert_eq!(harness.selected(), ["a1.txt", "a2.txt", "a3.txt"]);

    type_filter(&mut harness, "a3");
    assert_eq!(harness.selected(), ["a3.txt"]);
}

#[test]
fn changing_the_folder_clears_the_filter() {
    let mut harness = with_files(
        "filter-folder",
        &["docs/inner.txt", "docs/other.md", "a.txt"],
    );
    type_filter(&mut harness, "doc");
    harness.double_click(&folder("docs"));
    assert_eq!(harness.tab().path, path_of(&harness, &["docs"]));
    assert!(harness.tab().list.filter.is_empty());
    assert_eq!(harness.names(), ["inner.txt", "other.md"]);
    assert!(harness.has("other.md"));
}

// History

#[test]
fn back_and_forward_follow_the_visited_folders() {
    let mut harness = with_files("history", &["docs/old/x.txt", "a.txt"]);
    let root = text_of(&harness.root.data());
    harness.double_click(&folder("docs"));
    harness.double_click(&folder("old"));
    assert_eq!(harness.tab().path, path_of(&harness, &["docs", "old"]));

    harness.tap(Key::Named(Named::ArrowLeft), Modifiers::ALT);
    assert_eq!(harness.tab().path, path_of(&harness, &["docs"]));
    harness.click("←");
    assert_eq!(harness.tab().path, root);
    harness.click("←");
    assert_eq!(harness.tab().path, root);

    harness.tap(Key::Named(Named::ArrowRight), Modifiers::ALT);
    assert_eq!(harness.tab().path, path_of(&harness, &["docs"]));
    harness.click("→");
    assert_eq!(harness.tab().path, path_of(&harness, &["docs", "old"]));

    harness.event(Event::Mouse(mouse::Event::ButtonPressed(
        mouse::Button::Back,
    )));
    assert_eq!(harness.tab().path, path_of(&harness, &["docs"]));
    harness.event(Event::Mouse(mouse::Event::ButtonPressed(
        mouse::Button::Forward,
    )));
    assert_eq!(harness.tab().path, path_of(&harness, &["docs", "old"]));

    harness.click("Up");
    harness.click("Up");
    assert_eq!(harness.tab().path, root);
    assert!(harness.tab().list.forward.is_empty());
    harness.click("←");
    assert_eq!(harness.tab().path, path_of(&harness, &["docs"]));
    assert_eq!(harness.tab().list.forward, std::slice::from_ref(&root));

    harness.shortcut("k", Modifiers::CTRL);
    harness.type_text("go forward");
    harness.key(Named::Enter);
    assert_eq!(harness.tab().path, root);
}

#[test]
fn failed_steps_keep_the_history() {
    let mut harness = with_files("history-fail", &["docs/x.txt"]);
    harness.double_click(&folder("docs"));
    fs::remove_dir_all(harness.root.data().join("docs")).expect("removing failed");
    fs::remove_dir_all(harness.root.data()).expect("removing failed");
    harness.tap(Key::Named(Named::ArrowLeft), Modifiers::ALT);
    assert!(matches!(
        harness.tab().state,
        crate::app::LoadState::Failed(_)
    ));
    assert_eq!(harness.tab().list.back.len(), 1);
    assert!(harness.tab().list.forward.is_empty());
}

// Search

#[test]
fn searching_subfolders_lists_results_and_opens_their_folder() {
    let mut harness = with_files(
        "search",
        &[
            "docs/report-1.txt",
            "docs/old/report-0.txt",
            "docs/old/notes.md",
            "top.txt",
        ],
    );
    harness.tap(
        Key::Character("f".into()),
        Modifiers::CTRL | Modifiers::SHIFT,
    );
    assert!(matches!(harness.app.dialog, Some(Dialog::Search(_))));
    assert!(harness.has("Search in subfolders"));
    harness.type_text("REPORT");
    harness.key(Named::Enter);
    assert!(harness.app.dialog.is_none());
    let results = harness.tab().search.as_ref().expect("no results");
    assert!(!results.running);
    let found: Vec<&str> = results
        .hits
        .iter()
        .map(|hit| hit.entry.name.as_str())
        .collect();
    assert_eq!(found, ["report-1.txt", "report-0.txt"]);
    assert!(harness.has("Results for “REPORT”: 2 found"));
    assert!(harness.has("docs"));
    assert!(harness.has(&format!("docs{}old", std::path::MAIN_SEPARATOR)));

    harness.click("report-0.txt");
    assert_eq!(
        harness
            .tab()
            .search
            .as_ref()
            .and_then(|results| results.selected),
        Some(1)
    );
    harness.double_click("report-0.txt");
    assert!(harness.tab().search.is_none());
    assert_eq!(harness.tab().path, path_of(&harness, &["docs", "old"]));
    assert_eq!(harness.selected(), ["report-0.txt"]);
}

#[test]
fn the_depth_limit_and_closing_work() {
    let mut harness = with_files("search-depth", &["a.txt", "one/b.txt", "one/two/c.txt"]);
    harness.click("Search in subfolders…");
    harness.type_text("*.txt");
    let inputs = harness.inputs();
    harness.fill_at(inputs.last().expect("no depth input").center(), "x");
    assert!(harness.has("Enter a whole number of at least 1, or leave the field empty."));
    harness.key(Named::Enter);
    assert!(matches!(harness.app.dialog, Some(Dialog::Search(_))));
    harness.fill_at(inputs.last().expect("no depth input").center(), "2");
    harness.key(Named::Enter);
    let found = harness
        .tab()
        .search
        .as_ref()
        .expect("no results")
        .hits
        .len();
    assert_eq!(found, 2);
    assert!(harness.has("(this folder)"));
    harness.click("Close results");
    assert!(harness.tab().search.is_none());
    assert!(harness.has(&folder("one")));
}

#[test]
fn remote_tabs_are_searched_through_their_connection() {
    let mut harness = connected(
        "search-remote",
        &["www/index.html", "www/css/site.css", "x.html"],
    );
    harness.send(Message::Search(crate::search::SearchMessage::Open));
    harness.type_text("*.html");
    harness.key(Named::Enter);
    let mut found: Vec<String> = harness
        .tab()
        .search
        .as_ref()
        .expect("no results")
        .hits
        .iter()
        .map(|hit| hit.entry.name.clone())
        .collect();
    found.sort();
    assert_eq!(found, ["index.html", "x.html"]);
    harness.send(Message::Search(crate::search::SearchMessage::Cancel(
        harness.tab().id,
    )));
    assert!(harness.tab().search.is_some());
}

// Bookmarks

#[test]
fn bookmarks_are_added_opened_edited_and_kept() {
    let mut harness = with_files("bookmarks", &["docs/inner.txt", "music/song.ogg"]);
    harness.double_click(&folder("docs"));
    harness.shortcut("b", Modifiers::CTRL);
    assert!(is_bookmarked(&harness));
    let docs = path_of(&harness, &["docs"]);
    assert_eq!(local_bookmarks(&harness).items.len(), 1);
    assert_eq!(local_bookmarks(&harness).items[0].path, docs);
    assert_eq!(local_bookmarks(&harness).items[0].name, "docs");

    harness.click("Up");
    harness.double_click(&folder("music"));
    let picker = bookmark_picker(&mut harness);
    assert!(!is_bookmarked(&harness));
    harness.pick_at(picker, 1, 3, 13.0);
    assert!(is_bookmarked(&harness));
    assert_eq!(local_bookmarks(&harness).items.len(), 2);

    let picker = bookmark_picker(&mut harness);
    harness.pick_at(picker, 0, 3, 13.0);
    assert_eq!(harness.tab().path, docs);

    harness.restart();
    assert_eq!(harness.app.bookmarks.items.len(), 2);
    harness.shortcut("k", Modifiers::CTRL);
    harness.type_text("bookmark music");
    harness.key(Named::Enter);
    assert_eq!(harness.tab().path, path_of(&harness, &["music"]));

    harness.send(Message::RunAction(crate::commands::Action::EditBookmarks));
    assert!(matches!(harness.app.dialog, Some(Dialog::Bookmarks(_))));
    assert!(harness.has("Edit bookmarks"));
    let inputs = harness.inputs();
    harness.fill_at(inputs[inputs.len() - 2].center(), "Documents");
    harness.click_nth("Remove", 1);
    harness.click("Save");
    assert!(harness.app.dialog.is_none());
    let stored = local_bookmarks(&harness);
    assert_eq!(stored.items.len(), 1);
    assert_eq!(stored.items[0].name, "Documents");
    assert_eq!(stored.items[0].path, docs);
}

#[test]
fn site_tabs_keep_their_bookmarks_with_the_site() {
    let mut harness = connected("bookmarks-site", &["www/index.html"]);
    let mut site = hoplix_core::Site::new("Web");
    site.host = "files.example.test".to_string();
    let site_id = site.id.clone();
    harness.app.sites.upsert(site);
    let id: TabId = harness.tab().id;
    harness.app.set_origin(id, TabOrigin::Site(site_id.clone()));
    harness.double_click("www/");
    harness.shortcut("b", Modifiers::CTRL);

    let www = remote_path(&harness, &["www"]);
    let stored = SiteList::load(&harness.root.config()).expect("reading the sites failed");
    let saved = stored.get(&site_id).expect("site missing");
    assert_eq!(saved.bookmarks.items.len(), 1);
    assert_eq!(saved.bookmarks.items[0].path, www);
    assert!(local_bookmarks(&harness).is_empty());

    let export = SiteExport {
        list: stored,
        secrets: std::collections::BTreeMap::new(),
    };
    assert!(export.to_plain_text().contains("bookmark = www"));

    harness.send(Message::Connected(Ok(fakes::Folder::remote(
        &harness.root.remote(),
        SERVER.to_string(),
    ))));
    assert!(harness.app.tab_bookmarks(harness.tab()).is_none());
}

// Properties

#[test]
fn local_properties_show_details_and_count_folder_sizes() {
    let mut harness = with_files(
        "properties-local",
        &["docs/a.txt", "docs/sub/b.txt", "c.txt"],
    );
    harness.click("c.txt");
    harness.tap(Key::Named(Named::Enter), Modifiers::ALT);
    assert!(harness.has("Properties of “c.txt”"));
    assert!(matches!(properties(&harness).loaded, Loaded::Ready(_)));
    assert!(harness.has("5 B (5 bytes)"));
    if cfg!(windows) {
        assert!(harness.has("Read-only"));
        assert!(!harness.has("Permissions"));
        assert!(harness.has("Close"));
    }
    harness.key(Named::Escape);
    assert!(harness.app.dialog.is_none());

    harness.click(&folder("docs"));
    harness.send(Message::RunAction(crate::commands::Action::Properties));
    assert!(harness.has("Calculate size"));
    harness.click("Calculate size");
    assert!(matches!(properties(&harness).size, SizeState::Done(_)));
    assert!(harness.contains("in 2 files and 1 folders"));
    harness.click(if cfg!(windows) { "Close" } else { "Apply" });
    assert!(harness.app.dialog.is_none());
}

#[test]
fn remote_permissions_are_shown_and_changed() {
    let mut harness = connected(
        "properties-remote",
        &["index.html", "www/a.html", "www/css/b.css"],
    );
    harness.click("index.html");
    harness.tap(Key::Named(Named::Enter), Modifiers::ALT);
    assert!(harness.has("rw-r--r--"));
    assert!(harness.has(fakes::FAKE_OWNER));
    assert!(harness.has(fakes::FAKE_GROUP));
    assert_eq!(properties(&harness).mode, "644");

    let inputs = harness.inputs();
    harness.fill_at(inputs.last().expect("no mode input").center(), "60");
    assert!(harness.contains("three or four octal digits"));
    harness.type_text("0");
    assert!(harness.has("rw-------"));
    harness.click("Apply");
    assert!(harness.app.dialog.is_none());
    let file = remote_path(&harness, &["index.html"]);
    assert_eq!(fakes::permission_of(&file), Some(0o600));

    harness.click("www/");
    harness.tap(Key::Named(Named::Enter), Modifiers::ALT);
    assert!(harness.has("rwxr-xr-x"));
    harness.click("Also apply to everything inside the folder");
    harness.click("Folders");
    let dialog = properties(&harness);
    assert!(dialog.recursive && dialog.files && !dialog.folders);
    let boxes = harness.find_all("Execute");
    assert_eq!(boxes.len(), 1);
    harness.send(Message::Properties(
        crate::properties::PropertiesMessage::Bit(0o001, false),
    ));
    assert_eq!(properties(&harness).mode, "754");
    harness.click("Apply");
    assert!(harness.app.dialog.is_none());
    assert_eq!(fakes::permission_of(&remote_path(&harness, &["www"])), None);
    assert_eq!(
        fakes::permission_of(&remote_path(&harness, &["www", "a.html"])),
        Some(0o754)
    );
    assert_eq!(
        fakes::permission_of(&remote_path(&harness, &["www", "css", "b.css"])),
        Some(0o754)
    );
    assert_eq!(
        fakes::permission_of(&remote_path(&harness, &["www", "css"])),
        None
    );
}
