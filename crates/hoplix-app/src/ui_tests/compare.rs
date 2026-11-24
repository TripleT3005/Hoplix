use std::time::{Duration, SystemTime};

use hoplix_core::{Side, SyncAction, SyncDirection, SyncMode};
use iced::keyboard::key::Named;

use super::*;
use crate::app::{Dialog, TabId};
use crate::compare::Mark;

// Helpers

fn file(path: &Path, content: &str, age: u64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("creating a folder failed");
    }
    fs::write(path, content).expect("writing a file failed");
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(SystemTime::now() - Duration::from_secs(age)))
        .expect("setting the time failed");
}

fn prepare(root: &TestRoot) {
    let (left, right) = (root.data().join("left"), root.data().join("right"));
    file(&left.join("same.txt"), "same", 3600);
    file(&right.join("same.txt"), "same", 3600);
    file(&left.join("newer.txt"), "new content", 10);
    file(&right.join("newer.txt"), "old content", 3600);
    file(&left.join("size.txt"), "longer", 3600);
    file(&right.join("size.txt"), "short", 3600);
    file(&left.join("only-left.txt"), "left", 3600);
    file(&right.join("only-right.txt"), "right", 3600);
    file(&left.join("docs/inner.txt"), "inner, changed", 3600);
    file(&right.join("docs/inner.txt"), "inner", 3600);
}

fn open(harness: &mut Harness, tab: TabId, folder: &str) {
    let path = text_of(&harness.root.data().join(folder));
    let input = harness.input_bounds(&crate::ui::path_input_id(tab));
    harness.fill_at(input.center(), &path);
    harness.key(Named::Enter);
    assert_eq!(
        harness.app.tab_by_id(tab).map(|tab| tab.path.clone()),
        Some(path)
    );
}

fn classic(name: &str) -> Harness {
    let mut harness = Harness::start(name, Layout::Classic, prepare);
    let (left, right) = (
        harness.app.panes[0].tabs[0].id,
        harness.app.panes[1].tabs[0].id,
    );
    open(&mut harness, left, "left");
    open(&mut harness, right, "right");
    harness
}

fn mark(harness: &Harness, pane: usize, name: &str) -> Option<Mark> {
    let tab = harness.app.panes[pane].active_tab()?;
    let entry = tab.entries.iter().find(|entry| entry.name == name)?;
    harness.app.compare.comparison.as_ref()?.mark(tab.id, entry)
}

fn folder(name: &str) -> String {
    format!("{name}{}", std::path::MAIN_SEPARATOR)
}

fn finished(app: &Hoplix) -> bool {
    !app.transfer_jobs.is_empty() && app.transfer_jobs.iter().all(|job| job.status.is_finished())
}

fn tree(directory: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for item in fs::read_dir(&folder).expect("listing failed").flatten() {
            let path = item.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(directory)
                    .expect("outside the folder")
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push((relative, fs::read(&path).expect("reading failed")));
            }
        }
    }
    found.sort();
    found
}

// Comparing

#[test]
fn comparing_marks_the_differences_in_both_panes() {
    let mut harness = classic("compare-marks");
    harness.click("Compare");
    assert!(harness.app.compare.comparison.is_some());
    assert_eq!(mark(&harness, 0, "only-left.txt"), Some(Mark::OnlyHere));
    assert_eq!(mark(&harness, 1, "only-right.txt"), Some(Mark::OnlyHere));
    assert_eq!(mark(&harness, 0, "newer.txt"), Some(Mark::NewerHere));
    assert_eq!(mark(&harness, 1, "newer.txt"), Some(Mark::OlderHere));
    assert_eq!(mark(&harness, 0, "size.txt"), Some(Mark::SizeDiffers));
    assert_eq!(mark(&harness, 0, "same.txt"), Some(Mark::Equal));
    assert_eq!(mark(&harness, 0, "docs"), Some(Mark::Equal));
    for symbol in ["+", "»", "«", "≠"] {
        assert!(harness.find_all(symbol).len() >= 2, "{symbol}");
    }
    assert!(harness.has("Comparing left and right"));
    assert!(harness.has("Differences: 4 · Equal: 2"));

    harness.click("Include subfolders");
    assert_eq!(mark(&harness, 0, "docs"), Some(Mark::ContentDiffers));
    assert_eq!(mark(&harness, 1, "docs"), Some(Mark::ContentDiffers));
    assert!(harness.has("Differences: 5 · Equal: 1"));

    harness.click("Stop comparing");
    assert!(harness.app.compare.comparison.is_none());
    assert!(!harness.has("»"));
}

#[test]
fn only_differences_hides_equal_entries_and_limits_the_selection() {
    let mut harness = classic("compare-filter");
    harness.click_nth("same.txt", 0);
    harness.shortcut("d", Modifiers::CTRL);
    assert!(harness.app.compare.comparison.is_some());
    assert_eq!(harness.find_all("same.txt").len(), 2);
    assert_eq!(harness.find_all(&folder("docs")).len(), 2);

    harness.click("Only differences");
    assert!(harness.find_all("same.txt").is_empty());
    assert_eq!(
        harness.find_all(&folder("docs")).len(),
        2,
        "folders stay visible while their contents are not compared"
    );
    assert_eq!(harness.find_all("newer.txt").len(), 2);

    let left = harness.find_all("newer.txt")[0];
    harness.click_at(left.center());
    harness.shortcut("a", Modifiers::CTRL);
    assert_eq!(
        harness.selected(),
        ["docs", "newer.txt", "only-left.txt", "size.txt"]
    );
    harness.key(Named::Home);
    harness.key(Named::ArrowDown);
    harness.key(Named::ArrowDown);
    harness.key(Named::ArrowDown);
    assert_eq!(harness.selected(), ["size.txt"]);

    harness.click("Include subfolders");
    assert_eq!(harness.find_all("newer.txt").len(), 2);
    assert!(harness.find_all("same.txt").is_empty());
    harness.click("Only differences");
    assert_eq!(harness.find_all("same.txt").len(), 2);
}

#[test]
fn listing_another_folder_compares_again() {
    let mut harness = classic("compare-navigate");
    harness.click("Compare");
    assert_eq!(mark(&harness, 0, "size.txt"), Some(Mark::SizeDiffers));
    let right = harness.app.panes[1].tabs[0].id;
    open(&mut harness, right, "left");
    assert_eq!(mark(&harness, 0, "size.txt"), Some(Mark::Equal));
    assert_eq!(mark(&harness, 0, "newer.txt"), Some(Mark::Equal));
}

#[test]
fn the_modern_layout_compares_with_the_chosen_tab() {
    let mut harness = Harness::start("compare-modern", Layout::Modern, prepare);
    let first = harness.tab().id;
    open(&mut harness, first, "left");
    harness.send(Message::NewTab(0));
    let second = harness.tab().id;
    open(&mut harness, second, "right");
    harness.send(Message::NewTab(0));
    let third = harness.tab().id;

    harness.click(&folder("left"));
    harness.shortcut("d", Modifiers::CTRL);
    assert!(harness.app.compare.comparison.is_none());
    assert!(harness.has("Open a second tab or choose one under “Compare with…” first."));

    let connect = harness.bounds("Connect");
    let picker = Point::new(connect.x - 8.0 - 110.0 - 12.0, connect.center_y());
    harness.pick_at(picker, 1, 2, 16.0);
    let comparison = harness
        .app
        .compare
        .comparison
        .as_ref()
        .expect("not comparing");
    assert_eq!((comparison.left, comparison.right), (third, second));

    harness.click("Stop comparing");
    harness.send(Message::SelectTab(first));
    harness.send(Message::CloseTab(third));
    harness.click("same.txt");
    harness.shortcut("d", Modifiers::CTRL);
    let comparison = harness
        .app
        .compare
        .comparison
        .as_ref()
        .expect("not comparing");
    assert_eq!((comparison.left, comparison.right), (first, second));
    assert_eq!(mark(&harness, 0, "newer.txt"), Some(Mark::NewerHere));
    harness.send(Message::SelectTab(second));
    assert!(harness.has("«"));
}

// Synchronising

#[test]
fn the_sync_dialog_builds_the_expected_plan() {
    let mut harness = classic("sync-plan");
    harness.click("Compare");
    harness.click("Synchronise…");
    let dialog = harness.app.sync_dialog().expect("no sync dialog").clone();
    assert_eq!(dialog.direction, SyncDirection::LeftToRight);
    assert_eq!(dialog.mode, SyncMode::Update);
    let plan = dialog.plan().expect("no plan");
    let copied: Vec<&str> = plan
        .actions
        .iter()
        .filter_map(|action| match action {
            SyncAction::Copy {
                from: Side::Left,
                path,
                ..
            } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(copied, ["newer.txt", "only-left.txt", "size.txt"]);
    assert!(harness.has("Copy to right: 3 (21 B)"));
    assert!(harness.has("→ newer.txt"));

    harness.click_nth("Include subfolders", 1);
    assert!(harness.has("Copy to right: 4 (35 B)"));
    assert!(harness.has("→ docs/inner.txt"));

    harness.click("left ← right");
    assert!(harness.has("Copy to left: 3 (15 B)"));
    harness.click("Both ways, newer wins");
    assert!(harness.has("Copy to right: 2 (15 B)"));
    assert!(harness.has("Copy to left: 1 (5 B)"));
    assert!(harness.contains("Skipped: 2"));

    harness.click("left → right");
    harness.fill_input(2, "*.txt");
    assert!(harness.has("Both sides are already the same."));
    harness.key(Named::Escape);
    assert!(harness.app.dialog.is_none());
}

#[test]
fn starting_a_mirror_sync_makes_the_target_match() {
    let mut harness = classic("sync-run");
    harness.click_nth("same.txt", 0);
    harness.shortcut("d", Modifiers::CTRL | Modifiers::SHIFT);
    assert!(matches!(harness.app.dialog, Some(Dialog::Sync(_))));
    harness.click_nth("Include subfolders", 0);
    harness.click("Mirror: make the target identical");
    harness.click("Delete files that are missing on the source");
    assert!(harness.has("1 files and folders on right will be deleted. This cannot be undone."));
    assert!(!harness.app.dialog.as_ref().is_some_and(Dialog::can_confirm));
    harness.click("Start");
    assert!(harness.app.sync_dialog().is_some());

    harness.click("Yes, delete them");
    assert!(harness.app.dialog.as_ref().is_some_and(Dialog::can_confirm));
    harness.click("Start");
    assert!(harness.app.dialog.is_none());
    assert!(harness.app.transfer_panel_open);
    harness.drive_transfers_until(finished);

    let data = harness.root.data();
    assert_eq!(tree(&data.join("right")), tree(&data.join("left")));
    assert!(harness.has("Synchronisation finished."));
    let names: Vec<&str> = harness.app.panes[1].tabs[0]
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["docs", "newer.txt", "only-left.txt", "same.txt", "size.txt"]
    );

    harness.click("Compare");
    let counts = harness
        .app
        .compare
        .comparison
        .as_ref()
        .expect("not comparing")
        .counts();
    assert_eq!(counts.equal, 5, "{counts:?}");
}
