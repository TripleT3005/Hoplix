use super::*;

// Constants

const SERVER: &str = "demo@files.example.test";
const PERMISSIONS_OPTION: &str = "Show the Permissions column in file lists";
const OWNER_OPTION: &str = "Show the Owner column in file lists";

// Helpers

fn connected(name: &str, files: &[&str]) -> Harness {
    let files: Vec<String> = files.iter().map(ToString::to_string).collect();
    let mut harness = Harness::start(name, Layout::Modern, move |root| {
        let names: Vec<&str> = files.iter().map(String::as_str).collect();
        write_files(&root.remote(), &names);
    });
    let server = fakes::Folder::remote(&harness.root.remote(), SERVER.to_string());
    harness.send(Message::Connected(Ok(server)));
    harness
}

fn toggle(harness: &mut Harness, option: &str) {
    harness.click("Settings");
    harness.click(option);
    harness.click("Back");
}

// Columns

#[test]
fn permissions_and_owner_columns_are_optional_and_kept() {
    let mut harness = connected("columns", &["index.html", "www/"]);
    assert!(!harness.has("Permissions"));
    assert!(!harness.has("rw-r--r--"));
    assert!(!harness.has(fakes::FAKE_OWNER));

    toggle(&mut harness, PERMISSIONS_OPTION);
    assert!(harness.app.settings.show_permissions);
    assert!(harness.has("Permissions"));
    assert!(harness.has("rw-r--r--"));
    assert!(harness.has("rwxr-xr-x"));
    assert!(!harness.has("Owner"));

    toggle(&mut harness, OWNER_OPTION);
    assert!(harness.has("Owner"));
    assert_eq!(harness.find_all(fakes::FAKE_OWNER).len(), 2);

    harness.restart();
    assert!(harness.app.settings.show_permissions);
    assert!(harness.app.settings.show_owner);

    toggle(&mut harness, PERMISSIONS_OPTION);
    assert!(!harness.app.settings.show_permissions);
    let server = fakes::Folder::remote(&harness.root.remote(), SERVER.to_string());
    harness.send(Message::Connected(Ok(server)));
    assert!(!harness.has("Permissions"));
    assert!(harness.has("Owner"));
}

#[test]
fn narrow_lists_drop_the_optional_columns_first() {
    let mut harness = connected("columns-narrow", &["index.html"]);
    toggle(&mut harness, PERMISSIONS_OPTION);
    toggle(&mut harness, OWNER_OPTION);
    assert!(harness.has("Owner"));
    harness.resize(Size::new(640.0, 600.0));
    assert!(!harness.has("Owner"));
    assert!(harness.has("Permissions"));
    assert!(harness.has("Modified"));
    harness.resize(Size::new(1280.0, 1000.0));
    assert!(harness.has("Owner"));
}
