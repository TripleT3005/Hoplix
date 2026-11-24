use iced::keyboard::key::Named;

use super::*;
use crate::app::{Page, TabId};
use crate::edits::{EditMessage, EditState};

// Constants

const FILE: &str = "notes.txt";
const SERVER: &str = "demo@files.example.test";

// Helpers

fn connected(name: &str) -> Harness {
    let mut harness = Harness::start(name, Layout::Modern, |root| {
        write_files(&root.remote(), &[FILE, "other.txt"]);
        write_files(&root.data(), &["local.txt"]);
    });
    let server = fakes::Folder::remote(&harness.root.remote(), SERVER.to_string());
    harness.send(Message::Connected(Ok(server)));
    assert!(harness.tab().file_system.label().is_some());
    assert_eq!(harness.names(), [FILE, "other.txt"]);
    harness
}

fn remote_tab(harness: &Harness) -> TabId {
    harness.tab().id
}

fn state(app: &Hoplix) -> Option<EditState> {
    app.edits.files.first().map(|file| file.state)
}

fn watching(app: &Hoplix) -> bool {
    state(app) == Some(EditState::Watching)
}

fn local_copy(harness: &Harness) -> PathBuf {
    harness.app.edits.files[0].local_path.clone()
}

fn opened(harness: &Harness) -> Vec<(PathBuf, String)> {
    harness
        .opened
        .lock()
        .expect("the opener list is poisoned")
        .clone()
}

fn poll(harness: &mut Harness) {
    harness.send(Message::Edit(EditMessage::Poll));
}

fn edit_with_f4(harness: &mut Harness) {
    harness.click(FILE);
    harness.key(Named::F4);
    assert_eq!(state(&harness.app), Some(EditState::Downloading));
    assert!(harness.has("Edited files (1)"));
    assert!(harness.has("Downloading…"));
    harness.drive_transfers_until(watching);
}

fn remote_text(harness: &Harness) -> String {
    fs::read_to_string(harness.root.remote().join(FILE)).expect("reading the remote file failed")
}

// Remote files

#[test]
fn edited_remote_files_are_uploaded_after_a_change() {
    let mut harness = connected("edit-upload");
    edit_with_f4(&mut harness);
    let copy = local_copy(&harness);
    assert!(copy.starts_with(harness.root.temp()));
    assert_eq!(copy.file_name().and_then(|name| name.to_str()), Some(FILE));
    assert_eq!(fs::read_to_string(&copy).expect("reading failed"), FILE);
    assert_eq!(opened(&harness), [(copy.clone(), String::new())]);
    assert!(harness.has("Open, watching for changes"));

    poll(&mut harness);
    assert!(watching(&harness.app));
    assert!(!harness.has("Upload"));

    fs::write(&copy, "changed in the editor").expect("writing failed");
    poll(&mut harness);
    assert_eq!(state(&harness.app), Some(EditState::Changed));
    assert!(harness.has("“notes.txt” was changed."));
    assert_eq!(remote_text(&harness), FILE);

    harness.click("Upload");
    assert_eq!(state(&harness.app), Some(EditState::Uploading));
    harness.drive_transfers_until(watching);
    assert!(harness.app.dialog.is_none());
    assert_eq!(remote_text(&harness), "changed in the editor");
    assert!(harness.has("Open, watching for changes"));
    assert_eq!(harness.app.transfers.snapshots().len(), 2);

    poll(&mut harness);
    assert!(watching(&harness.app));
}

#[test]
fn ignored_changes_stay_local_and_reopening_reuses_the_copy() {
    let mut harness = connected("edit-ignore");
    harness.click("Settings");
    harness.fill("Editor", "my-editor");
    assert_eq!(harness.app.settings.editor, "my-editor");
    assert_eq!(
        Settings::load(Some(&harness.root.config())).editor,
        "my-editor"
    );
    harness.click("Back");
    assert_eq!(harness.app.page, Page::Browser);

    harness.double_click(FILE);
    assert_eq!(state(&harness.app), Some(EditState::Downloading));
    harness.drive_transfers_until(watching);
    let copy = local_copy(&harness);
    assert_eq!(opened(&harness), [(copy.clone(), String::new())]);

    harness.key(Named::F4);
    assert_eq!(harness.app.edits.files.len(), 1);
    assert_eq!(harness.app.transfers.snapshots().len(), 1);
    assert_eq!(opened(&harness)[1], (copy.clone(), "my-editor".to_string()));

    fs::write(&copy, "local only").expect("writing failed");
    poll(&mut harness);
    harness.click("Ignore");
    assert!(watching(&harness.app));
    poll(&mut harness);
    assert!(watching(&harness.app));
    assert_eq!(remote_text(&harness), FILE);

    harness.click("Stop watching");
    assert!(harness.app.edits.files.is_empty());
    assert!(!harness.has("Edited files (1)"));
}

#[test]
fn automatic_uploads_closing_tabs_and_exiting_clean_up() {
    let mut harness = connected("edit-auto");
    harness.click("Settings");
    harness.click("Upload changes automatically");
    assert!(harness.app.settings.upload_changes_automatically);
    harness.click("Back");

    edit_with_f4(&mut harness);
    let copy = local_copy(&harness);
    fs::write(&copy, "saved and sent").expect("writing failed");
    poll(&mut harness);
    assert_eq!(state(&harness.app), Some(EditState::Uploading));
    harness.drive_transfers_until(watching);
    assert_eq!(remote_text(&harness), "saved and sent");

    let root = harness
        .app
        .edits
        .root()
        .map(Path::to_path_buf)
        .expect("no edit folder");
    assert!(root.is_dir());
    let tab = remote_tab(&harness);
    harness.send(Message::CloseTab(tab));
    assert!(harness.app.edits.files.is_empty());
    assert!(root.is_dir());

    let window = harness.window;
    harness.send(Message::WindowClosing(window, false));
    assert!(!root.exists());
}

#[test]
fn failed_downloads_are_reported_and_forgotten() {
    let mut harness = connected("edit-missing");
    harness.click(FILE);
    fs::remove_file(harness.root.remote().join(FILE)).expect("removing failed");
    harness.key(Named::F4);
    harness.drive_transfers_until(|app| app.edits.files.is_empty());
    assert!(opened(&harness).is_empty());
    assert!(harness.has("“notes.txt” could not be downloaded for editing."));
}

// Local files

#[test]
fn local_files_open_in_place() {
    let mut harness = Harness::start("edit-local", Layout::Modern, |root| {
        write_files(&root.data(), &["local.txt", "docs/"]);
    });
    harness.app.settings.editor = "my-editor".to_string();
    let path = harness.root.data().join("local.txt");
    harness.double_click("local.txt");
    harness.key(Named::F4);
    harness.click("Edit");
    assert_eq!(
        opened(&harness),
        [
            (path.clone(), String::new()),
            (path.clone(), "my-editor".to_string()),
            (path, "my-editor".to_string()),
        ]
    );
    assert!(harness.app.edits.files.is_empty());
    assert!(harness.app.transfers.snapshots().is_empty());

    harness.click(&format!("docs{}", std::path::MAIN_SEPARATOR));
    harness.key(Named::F4);
    assert_eq!(opened(&harness).len(), 3);
}
