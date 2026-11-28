use hoplix_core::JobStatus;
use iced::keyboard::key::Named;

use super::*;

// Helpers

fn classic(name: &str, prepare: impl FnOnce(&TestRoot)) -> Harness {
    let mut harness = Harness::start(name, Layout::Classic, |root| {
        write_files(&root.data(), &["docs/old/report.txt", "top.txt"]);
        write_files(&root.remote(), &["target/"]);
        prepare(root);
    });
    let target = text_of(&harness.root.remote().join("target"));
    let right = harness.app.panes[1].tabs[0].id;
    let input = harness.input_bounds(&crate::ui::path_input_id(right));
    harness.fill_at(input.center(), &target);
    harness.key(Named::Enter);
    assert_eq!(harness.app.panes[1].tabs[0].path, target);
    harness
}

fn search_left(harness: &mut Harness, pattern: &str) {
    let left = harness.find_all("top.txt")[0];
    harness.click_at(left.center());
    harness.tap(
        Key::Character("f".into()),
        Modifiers::CTRL | Modifiers::SHIFT,
    );
    harness.type_text(pattern);
    harness.key(Named::Enter);
    let results = harness.app.panes[0].tabs[0]
        .search
        .as_ref()
        .expect("no results");
    assert_eq!(results.hits.len(), 1);
}

fn finished(app: &Hoplix) -> bool {
    !app.transfer_jobs.is_empty() && app.transfer_jobs.iter().all(|job| job.status.is_finished())
}

fn copied(harness: &Harness) -> PathBuf {
    harness.root.remote().join("target").join("report.txt")
}

// Search results

#[test]
fn a_search_result_is_copied_with_the_copy_button() {
    let mut harness = classic("search-copy", |_| {});
    search_left(&mut harness, "report");
    harness.click("Copy to other side →");
    assert!(harness.app.transfers.snapshots().is_empty());

    harness.click("report.txt");
    harness.click("Copy to other side →");
    assert_eq!(harness.app.transfers.snapshots().len(), 1);
    harness.drive_transfers_until(finished);
    assert_eq!(
        fs::read_to_string(copied(&harness)).expect("missing"),
        "docs/old/report.txt"
    );
    assert!(
        !harness
            .root
            .remote()
            .join("target")
            .join("top.txt")
            .exists()
    );
}

#[test]
fn a_search_result_is_copied_by_dragging() {
    let mut harness = classic("search-drag", |_| {});
    search_left(&mut harness, "report");
    let from = harness.bounds("report.txt").center();
    let to = harness.bounds("This folder is empty.").center();
    harness.move_to(from);
    harness.press();
    harness.move_to(Point::new(from.x + 4.0, from.y));
    harness.move_to(Point::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0));
    harness.move_to(to);
    assert!(harness.app.drag.as_ref().is_some_and(|drag| drag.moved));
    harness.release();
    assert!(harness.app.drag.is_none());
    assert_eq!(harness.app.transfers.snapshots().len(), 1);
    harness.drive_transfers_until(finished);
    assert!(copied(&harness).is_file());
}

#[test]
fn copying_a_search_result_asks_about_conflicts() {
    let mut harness = classic("search-conflict", |root| {
        fs::write(root.remote().join("target").join("report.txt"), "old").expect("writing failed");
    });
    search_left(&mut harness, "report");
    harness.click("report.txt");
    harness.click("Copy to other side →");
    harness.drive_transfers_until(|app| app.dialog.is_some());
    assert!(harness.has("File already exists"));
    harness.click("Overwrite");
    harness.drive_transfers_until(finished);
    assert_eq!(
        harness
            .app
            .transfer_jobs
            .iter()
            .map(|job| job.status.clone())
            .collect::<Vec<_>>(),
        [JobStatus::Completed]
    );
    assert_eq!(
        fs::read_to_string(copied(&harness)).expect("missing"),
        "docs/old/report.txt"
    );
}

#[test]
fn a_result_in_the_target_folder_is_not_copied_onto_itself() {
    let mut harness = classic("search-same", |_| {});
    let right = harness.app.panes[1].tabs[0].id;
    let data = text_of(&harness.root.data());
    let input = harness.input_bounds(&crate::ui::path_input_id(right));
    harness.fill_at(input.center(), &data);
    harness.key(Named::Enter);
    search_left(&mut harness, "top");
    harness.click_nth("top.txt", 0);
    harness.click("Copy to other side →");
    assert!(harness.app.transfers.snapshots().is_empty());
    assert!(harness.has("Source and target are the same folder."));
}
