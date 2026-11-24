use hoplix_core::{ConflictPolicy, JobStatus};
use iced::keyboard::key::Named;

use super::*;
use crate::app::Dialog;
use crate::transfer_settings::TransferSetting;
use crate::transfers::DropTarget;

// Helpers

fn classic(name: &str, prepare: impl FnOnce(&TestRoot)) -> Harness {
    let mut harness = Harness::start(name, Layout::Classic, |root| {
        write_files(&root.data(), &["a.txt", "b.txt", "target/"]);
        prepare(root);
    });
    let target = text_of(&harness.root.data().join("target"));
    let right = harness.app.panes[1].tabs[0].id;
    let input = harness.input_bounds(&crate::ui::path_input_id(right));
    harness.fill_at(input.center(), &target);
    harness.key(Named::Enter);
    assert_eq!(harness.app.panes[1].tabs[0].path, target);
    harness
}

fn target(harness: &Harness) -> PathBuf {
    harness.root.data().join("target")
}

fn finished(app: &Hoplix) -> bool {
    !app.transfer_jobs.is_empty() && app.transfer_jobs.iter().all(|job| job.status.is_finished())
}

fn statuses(harness: &Harness) -> Vec<JobStatus> {
    harness
        .app
        .transfer_jobs
        .iter()
        .map(|job| job.status.clone())
        .collect()
}

fn drag(harness: &mut Harness, from: Point, to: Point) {
    harness.move_to(from);
    harness.press();
    harness.move_to(Point::new(from.x + 1.0, from.y));
    harness.move_to(Point::new(from.x + 4.0, from.y));
    assert!(!harness.app.drag.as_ref().is_some_and(|drag| drag.moved));
    harness.move_to(Point::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0));
    harness.move_to(to);
    assert!(harness.app.drag.as_ref().is_some_and(|drag| drag.moved));
    assert!(harness.has("Release over a tab or pane to copy the selection there."));
    harness.release();
}

// Dragging

#[test]
fn dragging_onto_the_other_pane_copies_the_selection() {
    let mut harness = classic("drag-pane", |_| {});
    let left = harness.find_all("a.txt")[0];
    harness.click_at(left.center());
    harness.click_with("b.txt", Modifiers::CTRL);
    let pane = harness.bounds("This folder is empty.");
    drag(&mut harness, left.center(), pane.center());
    assert!(harness.app.drag.is_none());
    assert_eq!(harness.app.transfers.snapshots().len(), 2);
    assert!(harness.app.transfer_panel_open);
    assert!(harness.has("▾ Transfers"));

    harness.drive_transfers_until(finished);
    assert_eq!(
        statuses(&harness),
        [JobStatus::Completed, JobStatus::Completed]
    );
    assert!(target(&harness).join("a.txt").is_file());
    assert!(target(&harness).join("b.txt").is_file());
    let names: Vec<&str> = harness.app.panes[1].tabs[0]
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, ["a.txt", "b.txt"]);
}

#[test]
fn short_movements_do_not_start_a_drag() {
    let mut harness = classic("drag-short", |_| {});
    let left = harness.find_all("a.txt")[0];
    harness.move_to(left.center());
    harness.press();
    harness.move_to(Point::new(left.center().x + 3.0, left.center().y));
    harness.move_to(Point::new(left.center().x + 5.0, left.center().y + 2.0));
    harness.release();
    assert!(harness.app.drag.is_none());
    assert!(harness.app.transfers.snapshots().is_empty());
    assert_eq!(harness.selected(), ["a.txt"]);
}

#[test]
fn dropping_on_the_source_pane_does_nothing() {
    let mut harness = classic("drag-self", |_| {});
    let left = harness.find_all("a.txt")[0];
    let other = harness.find_all("b.txt")[0];
    harness.click_at(left.center());
    drag(
        &mut harness,
        left.center(),
        Point::new(other.center().x, other.center().y + 40.0),
    );
    assert_eq!(harness.app.drop_target, Some(DropTarget::Pane(0)));
    assert!(harness.app.transfers.snapshots().is_empty());
}

#[test]
fn dragging_from_the_right_pane_onto_the_left_pane_copies() {
    let mut harness = classic("drag-right-left", |root| {
        write_files(&root.data(), &["target/t.txt"]);
    });
    let source = harness.bounds("t.txt");
    let drop = harness.find_all("b.txt")[0];
    harness.click_at(source.center());
    assert_eq!(harness.app.focused_pane, 1);
    drag(&mut harness, source.center(), drop.center());
    assert_eq!(harness.app.drop_target, Some(DropTarget::Pane(0)));
    assert_eq!(harness.app.transfers.snapshots().len(), 1);
    harness.drive_transfers_until(finished);
    assert!(harness.root.data().join("t.txt").is_file());
}

#[test]
fn dragging_onto_a_tab_copies_into_that_tab() {
    let mut harness = Harness::start("drag-tab", Layout::Modern, |root| {
        write_files(&root.data(), &["a.txt", "target/"]);
    });
    harness.shortcut("t", Modifiers::CTRL);
    let second = harness.tab().id;
    let input = harness.input_bounds(&crate::ui::path_input_id(second));
    let target = text_of(&harness.root.data().join("target"));
    harness.fill_at(input.center(), &target);
    harness.key(Named::Enter);
    harness.click_nth("data", 0);
    assert_ne!(harness.tab().id, second);

    let file = harness.bounds("a.txt");
    let tab = harness.bounds("target");
    harness.click_at(file.center());
    drag(&mut harness, file.center(), tab.center());
    assert_eq!(harness.app.transfers.snapshots().len(), 1);
    harness.drive_transfers_until(finished);
    assert!(harness.root.data().join("target").join("a.txt").is_file());
}

#[test]
fn the_copy_button_sends_the_selection_to_the_other_pane() {
    let mut harness = classic("copy-button", |_| {});
    harness.click("Copy to other side →");
    assert!(harness.app.transfers.snapshots().is_empty());
    let left = harness.find_all("a.txt")[0];
    harness.click_at(left.center());
    harness.click("Copy to other side →");
    harness.drive_transfers_until(finished);
    assert!(target(&harness).join("a.txt").is_file());
}

#[test]
fn the_copy_to_list_sends_the_selection_to_another_tab() {
    let mut harness = Harness::start("copy-to", Layout::Modern, |root| {
        write_files(&root.data(), &["a.txt", "target/"]);
    });
    harness.shortcut("t", Modifiers::CTRL);
    let second = harness.tab().id;
    let input = harness.input_bounds(&crate::ui::path_input_id(second));
    let target = text_of(&harness.root.data().join("target"));
    harness.fill_at(input.center(), &target);
    harness.key(Named::Enter);
    harness.click_nth("data", 0);

    let placeholder = harness.bounds("Copy to…");
    harness.click_at(placeholder.center());
    assert!(harness.app.transfers.snapshots().is_empty());
    harness.click("a.txt");
    harness.pick_at(placeholder.center(), 0, 1, 13.0);
    assert_eq!(harness.app.transfers.snapshots().len(), 1);
    harness.drive_transfers_until(finished);
    assert!(harness.root.data().join("target").join("a.txt").is_file());
}

// Panel

#[test]
fn panel_buttons_pause_resume_cancel_and_retry() {
    let mut harness = classic("panel", |_| {});
    let left = harness.find_all("a.txt")[0];
    harness.click_at(left.center());
    harness.click("Copy to other side →");
    assert_eq!(statuses(&harness), [JobStatus::Queued]);
    assert!(harness.has("Waiting"));

    harness.click("Pause");
    assert_eq!(statuses(&harness), [JobStatus::Paused]);
    harness.click("Resume");
    assert_eq!(statuses(&harness), [JobStatus::Queued]);
    harness.click("Cancel");
    assert_eq!(statuses(&harness), [JobStatus::Cancelled]);
    assert!(harness.has("Cancelled"));
    harness.click("Retry");
    assert_eq!(statuses(&harness), [JobStatus::Queued]);

    harness.drive_transfers_until(finished);
    assert_eq!(statuses(&harness), [JobStatus::Completed]);
    harness.click("Remove");
    assert!(harness.app.transfer_jobs.is_empty());
    assert!(!harness.has("▾ Transfers"));
}

#[test]
fn the_panel_collapses_and_changes_the_policy() {
    let mut harness = classic("panel-policy", |_| {});
    let left = harness.find_all("a.txt")[0];
    harness.click_at(left.center());
    harness.click("Copy to other side →");
    harness.click("Pause all");
    assert_eq!(statuses(&harness), [JobStatus::Paused]);
    harness.click("Resume all");
    assert_eq!(statuses(&harness), [JobStatus::Queued]);

    let label = harness.bounds("If a file exists:");
    harness.pick_at(
        Point::new(label.x + label.width + 40.0, label.center_y()),
        3,
        5,
        13.0,
    );
    assert_eq!(harness.app.transfers.policy(), ConflictPolicy::Rename);

    harness.click("Cancel all");
    assert_eq!(statuses(&harness), [JobStatus::Cancelled]);
    harness.click("Clear finished");
    assert!(harness.app.transfer_jobs.is_empty());

    harness.click("Copy to other side →");
    harness.click("▾ Transfers");
    assert!(!harness.app.transfer_panel_open);
    assert!(harness.has("▸ Transfers"));
    assert!(!harness.has("Pause"));
}

#[test]
fn failed_transfers_can_be_retried() {
    let mut harness = classic("retry", |_| {});
    let left = harness.find_all("a.txt")[0];
    harness.click_at(left.center());
    harness.click("Copy to other side →");
    let data = harness.root.data();
    fs::rename(data.join("a.txt"), data.join("moved.txt")).expect("moving failed");
    harness.drive_transfers_until(finished);
    assert!(matches!(statuses(&harness)[0], JobStatus::Failed(_)));
    assert!(harness.contains("1 failed"));
    assert!(harness.contains("The path does not exist"));

    fs::rename(data.join("moved.txt"), data.join("a.txt")).expect("moving back failed");
    harness.click("Retry");
    harness.drive_transfers_until(|app| {
        app.transfer_jobs
            .iter()
            .all(|job| job.status == JobStatus::Completed)
    });
    assert!(target(&harness).join("a.txt").is_file());
}

// Conflicts

#[test]
fn conflicts_ask_and_can_apply_to_all() {
    let mut harness = classic("conflicts", |root| {
        write_files(&root.data(), &["c.txt"]);
        fs::write(root.data().join("target").join("a.txt"), "x").expect("writing failed");
        fs::write(root.data().join("target").join("b.txt"), "y").expect("writing failed");
        fs::write(root.data().join("target").join("c.txt"), "z").expect("writing failed");
    });
    harness.send(Message::TransferSetting(TransferSetting::Parallel(1)));
    let left = harness.find_all("a.txt")[0];
    harness.click_at(left.center());
    harness.shortcut("a", Modifiers::CTRL);
    harness.click_with(
        &format!("target{}", std::path::MAIN_SEPARATOR),
        Modifiers::CTRL,
    );
    assert_eq!(harness.selected(), ["a.txt", "b.txt", "c.txt"]);
    harness.click("Copy to other side →");

    harness.drive_transfers_until(|app| app.dialog.is_some());
    assert!(harness.has("File already exists"));
    assert!(harness.contains("New file: 5 B"));
    assert!(harness.has("Resume"));
    harness.click("Keep both");
    harness.drive_transfers_until(|app| app.dialog.is_some());
    let Some(Dialog::Conflict { conflict, .. }) = &harness.app.dialog else {
        panic!("no second conflict");
    };
    assert!(conflict.target_path.ends_with("b.txt"));
    harness.click("Do this for all further conflicts");
    assert!(matches!(
        harness.app.dialog,
        Some(Dialog::Conflict {
            apply_to_all: true,
            ..
        })
    ));
    harness.click("Overwrite");
    harness.drive_transfers_until(finished);
    assert!(harness.app.dialog.is_none());
    assert_eq!(harness.app.transfers.policy(), ConflictPolicy::Overwrite);
    let read = |name: &str| fs::read_to_string(target(&harness).join(name)).expect("missing");
    assert_eq!(read("a.txt"), "x");
    assert_eq!(read("a (1).txt"), "a.txt");
    assert_eq!(read("b.txt"), "b.txt");
    assert_eq!(read("c.txt"), "c.txt");
}

#[test]
fn conflicts_can_skip_resume_or_cancel() {
    let mut harness = classic("conflict-choices", |root| {
        write_files(&root.data(), &["c.txt"]);
        fs::write(root.data().join("target").join("a.txt"), "a.t").expect("writing failed");
        fs::write(root.data().join("target").join("b.txt"), "old").expect("writing failed");
        fs::write(root.data().join("target").join("c.txt"), "old").expect("writing failed");
    });
    harness.send(Message::TransferSetting(TransferSetting::Parallel(1)));
    for name in ["a.txt", "b.txt", "c.txt"] {
        let file = harness.find_all(name)[0];
        harness.click_at(file.center());
        harness.click("Copy to other side →");
    }

    harness.drive_transfers_until(|app| app.dialog.is_some());
    harness.click("Resume");
    harness.drive_transfers_until(|app| app.dialog.is_some());
    harness.click("Skip");
    harness.drive_transfers_until(|app| app.dialog.is_some());
    harness.click("Cancel transfer");
    harness.drive_transfers_until(finished);
    assert!(harness.app.dialog.is_none());
    assert_eq!(
        statuses(&harness),
        [
            JobStatus::Completed,
            JobStatus::Completed,
            JobStatus::Cancelled
        ]
    );
    let read = |name: &str| fs::read_to_string(target(&harness).join(name)).expect("missing");
    assert_eq!(read("a.txt"), "a.txt");
    assert_eq!(read("b.txt"), "old");
    assert_eq!(read("c.txt"), "old");
    assert_eq!(harness.app.transfers.policy(), ConflictPolicy::Ask);
}
