use hoplix_core::{
    ChecksumMode, ConflictPolicy, HISTORY_FILE, JobStatus, Location, Protocol, QUEUE_FILE,
    RecentConnection, ResumePoint, SavedJob, SavedQueue, Site, SiteAuth, SiteList,
};
use iced::keyboard::key::Named;

use super::fakes::{fingerprint, password};
use super::*;
use crate::app::Page;
use crate::queue_store::HistoryMessage;
use crate::transfer_settings::TransferSetting;

// Constants

const HOST: &str = "backup.example.test";

// Helpers

fn backup_site() -> Site {
    Site {
        host: HOST.to_string(),
        username: "demo".to_string(),
        save_secrets: true,
        ..Site::new("Backup")
    }
}

fn prepare_root(root: &TestRoot, site: &Site) {
    fs::write(
        root.config().join("known_hosts"),
        format!("{HOST}:22 {}\n", fingerprint(HOST)),
    )
    .expect("writing failed");
    write_files(&root.data(), &["a.txt", "b.txt", "target/"]);
    let mut list = SiteList::default();
    list.upsert(site.clone());
    list.save(&root.config()).expect("saving the sites failed");
}

fn with_site(name: &str, prepare: impl FnOnce(&TestRoot)) -> (Harness, String) {
    let site = backup_site();
    let id = site.id.clone();
    let harness = Harness::start(name, Layout::Classic, |root| {
        prepare_root(root, &site);
        prepare(root);
    });
    harness.store_keyring_secret(&id, password());
    (harness, id)
}

fn local_copy(name: &str) -> Harness {
    let mut harness = Harness::start(name, Layout::Classic, |root| {
        write_files(&root.data(), &["a.txt", "b.txt", "target/"]);
    });
    let target = text_of(&harness.root.data().join("target"));
    let right = harness.app.panes[1].tabs[0].id;
    let input = harness.input_bounds(&crate::ui::path_input_id(right));
    harness.fill_at(input.center(), &target);
    harness.key(Named::Enter);
    harness
}

fn copy_file(harness: &mut Harness, name: &str) {
    let file = harness.find_all(name)[0];
    harness.click_at(file.center());
    harness.click("Copy to other side →");
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

fn queue_file(harness: &Harness) -> Option<String> {
    fs::read_to_string(harness.root.config().join(QUEUE_FILE)).ok()
}

fn flush(harness: &mut Harness) {
    harness.send(Message::History(HistoryMessage::Flush));
}

fn click_last(harness: &mut Harness, label: &str) {
    let found = harness.find_all(label);
    let last = found
        .last()
        .unwrap_or_else(|| panic!("{label:?} not found in {:?}", harness.texts()));
    harness.click_at(last.center());
}

fn saved_upload(site: &str, root: &TestRoot, partial: Option<&str>) -> SavedJob {
    let target = root.remote().join("big.bin");
    SavedJob {
        source: Location::Local,
        source_path: text_of(&root.data().join("big.bin")),
        target: Location::Site(site.to_string()),
        target_directory: text_of(&root.remote()),
        target_name: None,
        policy: None,
        keep_modified: false,
        resume: ResumePoint {
            partial: partial
                .map(|_| vec![(text_of(&target), text_of(&target))])
                .unwrap_or_default(),
            ..ResumePoint::default()
        },
    }
}

fn big_content() -> Vec<u8> {
    (0..60_000u32).map(|index| (index % 251) as u8).collect()
}

// Restoring

#[test]
fn restarting_restores_paused_jobs_and_connect_resumes_them() {
    let (mut harness, id) = with_site("restore-roundtrip", |_| {});
    harness.app.focused_pane = 1;
    harness.click("Sites");
    harness.double_click("Backup");
    assert_eq!(harness.app.page, Page::Browser);
    let left = harness.app.panes[0].tabs[0].id;
    let right = harness.app.panes[1].active_tab().expect("no site tab").id;
    let file = harness.find_all("a.txt")[0].center();
    harness.click_at(file);
    harness.send(Message::TransferTo {
        from: left,
        to: right,
    });
    harness.click("Pause");
    assert_eq!(statuses(&harness), [JobStatus::Paused]);
    flush(&mut harness);
    let saved = queue_file(&harness).expect("queue.conf was not written");
    assert!(saved.contains("source = local"), "{saved}");
    assert!(saved.contains("target = site"), "{saved}");
    assert!(saved.contains(&id), "{saved}");
    assert!(!saved.contains(password()));

    harness.restart();
    assert_eq!(statuses(&harness), [JobStatus::Paused]);
    assert!(harness.app.transfer_panel_open);
    assert!(harness.has("Waiting for a connection to Backup"));
    assert!(harness.contains("Waiting for connection"));
    click_last(&mut harness, "Connect");
    assert_eq!(harness.app.page, Page::Browser);
    assert!(harness.app.store.waiting.is_empty());
    harness.drive_transfers_until(finished);
    assert_eq!(statuses(&harness), [JobStatus::Completed]);
    assert_eq!(
        fs::read_to_string(harness.root.remote().join("a.txt")).expect("not uploaded"),
        "a.txt"
    );
    flush(&mut harness);
    assert_eq!(queue_file(&harness), None);
}

#[test]
fn restored_uploads_continue_from_the_partial_file() {
    let site = backup_site();
    let id = site.id.clone();
    let mut harness = Harness::start("restore-partial", Layout::Classic, |root| {
        prepare_root(root, &site);
        fs::write(root.data().join("big.bin"), big_content()).expect("writing failed");
        fs::write(root.remote().join("big.bin"), vec![b'X'; 20_000]).expect("writing failed");
        SavedQueue {
            jobs: vec![saved_upload(&id, root, Some("partial"))],
        }
        .save(&root.config())
        .expect("saving the queue failed");
    });
    harness.store_keyring_secret(&id, password());
    assert_eq!(statuses(&harness), [JobStatus::Paused]);
    assert!(harness.app.is_waiting(harness.app.transfer_jobs[0].id));
    click_last(&mut harness, "Connect");
    harness.drive_transfers_until(finished);
    assert_eq!(statuses(&harness), [JobStatus::Completed]);
    let uploaded = fs::read(harness.root.remote().join("big.bin")).expect("not uploaded");
    assert_eq!(uploaded.len(), 60_000);
    assert!(uploaded[..20_000].iter().all(|byte| *byte == b'X'));
    assert_eq!(&uploaded[20_000..], &big_content()[20_000..]);
}

#[test]
fn direct_connections_open_the_prefilled_connect_page() {
    let mut harness = Harness::start("restore-direct", Layout::Classic, |root| {
        fs::write(
            root.config().join("known_hosts"),
            format!("{HOST}:22 {}\n", fingerprint(HOST)),
        )
        .expect("writing failed");
        write_files(&root.data(), &["a.txt"]);
        let connection = RecentConnection {
            protocol: Protocol::Sftp,
            host: HOST.to_string(),
            port: 22,
            username: "demo".to_string(),
            auth: SiteAuth::Password,
            ..RecentConnection::default()
        };
        SavedQueue {
            jobs: vec![SavedJob {
                source: Location::Local,
                source_path: text_of(&root.data().join("a.txt")),
                target: Location::Direct(connection),
                target_directory: text_of(&root.remote()),
                target_name: Some("copy.txt".to_string()),
                policy: Some(ConflictPolicy::Overwrite),
                keep_modified: false,
                resume: ResumePoint::default(),
            }],
        }
        .save(&root.config())
        .expect("saving the queue failed");
    });
    assert!(harness.has(&format!("Waiting for a connection to sftp://demo@{HOST}")));
    click_last(&mut harness, "Connect");
    assert_eq!(harness.app.page, Page::Connect);
    assert_eq!(harness.app.connect_form.host, HOST);
    assert_eq!(harness.app.connect_form.username, "demo");
    harness.fill("Password", password());
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Browser);
    harness.drive_transfers_until(finished);
    assert_eq!(statuses(&harness), [JobStatus::Completed]);
    assert!(harness.root.remote().join("copy.txt").is_file());
}

#[test]
fn corrupt_queue_files_and_the_setting_are_respected() {
    let site = backup_site();
    let id = site.id.clone();
    let mut harness = Harness::start("restore-setting", Layout::Classic, |root| {
        prepare_root(root, &site);
        fs::write(root.data().join("big.bin"), big_content()).expect("writing failed");
        let mut text = SavedQueue {
            jobs: vec![saved_upload(&id, root, None)],
        }
        .to_text();
        text.push_str("\n[job]\nsource = nowhere\n\u{0}\u{1}garbage\n[target]\n");
        fs::write(root.config().join(QUEUE_FILE), text).expect("writing failed");
    });
    assert_eq!(statuses(&harness), [JobStatus::Paused]);

    harness.click("Settings");
    harness.click("Restore unfinished transfers");
    assert!(!harness.app.settings.restore_transfers);
    assert_eq!(queue_file(&harness), None);
    harness.restart();
    assert!(harness.app.transfer_jobs.is_empty());

    fs::write(harness.root.config().join(QUEUE_FILE), [0xff, 0x00, 0xfe]).expect("writing failed");
    harness.send(Message::TransferSetting(TransferSetting::RestoreTransfers(
        true,
    )));
    harness.restart();
    assert!(harness.app.transfer_jobs.is_empty());
}

// History

#[test]
fn history_lists_filters_exports_and_clears_finished_transfers() {
    let mut harness = local_copy("history");
    copy_file(&mut harness, "a.txt");
    harness.drive_transfers_until(finished);
    harness.send(Message::Transfer(
        crate::transfers::TransferAction::PolicySelected(ConflictPolicy::Skip),
    ));
    copy_file(&mut harness, "a.txt");
    harness.drive_transfers_until(finished);
    let data = harness.root.data();
    fs::rename(data.join("b.txt"), data.join("gone.txt")).expect("moving failed");
    let file = harness.find_all("b.txt")[0];
    harness.click_at(file.center());
    harness.click("Copy to other side →");
    harness.drive_transfers_until(finished);

    harness.click("History (3)");
    assert!(harness.contains("Done · SHA-256 verified"));
    assert!(harness.has("Skipped"));
    assert!(harness.contains("Failed: The path does not exist"));
    assert!(harness.contains("SHA-256 verified"));
    assert!(harness.contains(&format!(
        "{} → {}",
        text_of(&data.join("a.txt")),
        text_of(&data.join("target").join("a.txt"))
    )));

    harness.send(Message::History(HistoryMessage::Filter(Some(
        hoplix_core::HistoryResult::Failed,
    ))));
    assert_eq!(harness.app.visible_history().len(), 1);
    assert!(!harness.has("Skipped"));
    let csv = harness.root.data().join("export.csv");
    let last = harness.inputs().len() - 1;
    harness.fill_input(last, &text_of(&csv));
    harness.click("Export CSV");
    let exported = fs::read_to_string(&csv).expect("the CSV was not written");
    assert!(exported.starts_with("\u{feff}Time,Direction,Source,Target,Size (bytes)"));
    assert_eq!(exported.lines().count(), 2);
    assert!(exported.contains(",Local copy,"));
    assert!(harness.contains("1 entries exported to"));
    let plain = harness.root.data().join("export.txt");
    harness.fill_input(last, &text_of(&plain));
    harness.click("Export text");
    let exported = fs::read_to_string(&plain).expect("the text was not written");
    assert!(exported.contains("Result: Failed: "));
    assert!(harness.app.history_export(false).contains("Source: "));

    flush(&mut harness);
    let saved = fs::read_to_string(harness.root.config().join(HISTORY_FILE)).expect("no history");
    assert!(saved.contains("result = skipped"));
    harness.restart();
    assert_eq!(harness.app.history.history.len(), 3);
    assert_eq!(statuses(&harness), [JobStatus::Paused]);
    harness.click("History (3)");
    harness.click("Clear");
    assert!(harness.has("No finished transfers yet."));
    assert!(!harness.root.config().join(HISTORY_FILE).exists());
}

#[test]
fn the_history_setting_stops_logging_and_limits_entries() {
    let mut harness = local_copy("history-setting");
    harness.send(Message::TransferSetting(TransferSetting::HistoryLimit(1)));
    copy_file(&mut harness, "a.txt");
    harness.drive_transfers_until(finished);
    copy_file(&mut harness, "b.txt");
    harness.drive_transfers_until(finished);
    assert_eq!(harness.app.history.history.len(), 1);
    harness.click("Settings");
    harness.click("Keep a transfer history");
    assert!(!harness.app.settings.history_enabled);
    harness.send(Message::ShowPage(Page::Browser));
    harness.send(Message::Transfer(
        crate::transfers::TransferAction::PolicySelected(ConflictPolicy::Overwrite),
    ));
    copy_file(&mut harness, "a.txt");
    harness.drive_transfers_until(finished);
    assert_eq!(harness.app.history.history.len(), 1);
    let text = fs::read_to_string(harness.root.config().join("settings.conf")).expect("settings");
    assert!(text.contains("transfer_history = false"));
    assert!(text.contains("transfer_history_limit = 1"));
}

// Checksums

#[test]
fn checksum_results_are_shown_and_can_be_switched_off() {
    let mut harness = local_copy("checksum");
    copy_file(&mut harness, "a.txt");
    harness.drive_transfers_until(finished);
    assert!(harness.contains("SHA-256 verified"));
    assert!(harness.contains("Done · "));
    assert_eq!(
        harness.app.transfers.checksum_mode(),
        ChecksumMode::WhenSupported
    );

    harness.click("Settings");
    harness.pick_of("Verify after transfer", 0, 2);
    assert_eq!(harness.app.settings.checksum, ChecksumMode::Off);
    assert_eq!(harness.app.transfers.checksum_mode(), ChecksumMode::Off);
    harness.send(Message::ShowPage(Page::Browser));
    harness.click("Clear finished");
    copy_file(&mut harness, "b.txt");
    harness.drive_transfers_until(finished);
    assert!(harness.contains("Done · "));
    assert!(!harness.contains("verified"));
}
