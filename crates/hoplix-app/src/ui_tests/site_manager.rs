use std::time::Duration;

use hoplix_core::{FileNameEncoding, FtpMode, JobStatus, Protocol, RECENT_FILE, Site, SiteList};
use iced::keyboard::key::Named;

use super::fakes::{fingerprint, options_used_for, password};
use super::*;
use crate::app::{Dialog, Page, SiteSelection};
use crate::site_exchange::{PASSWORD_INPUT, PATH_INPUT};
use crate::ui::EDITOR_SCROLL_ID;

// Constants

const HOST: &str = "files.example.test";

// Helpers

fn trusted(root: &TestRoot, hosts: &[&str]) {
    let lines: String = hosts
        .iter()
        .map(|host| format!("{host}:22 {}\n", fingerprint(host)))
        .collect();
    fs::write(root.config().join("known_hosts"), lines).expect("writing failed");
}

fn site(name: &str, host: &str) -> Site {
    Site {
        host: host.to_string(),
        username: "demo".to_string(),
        ..Site::new(name)
    }
}

fn with_sites(name: &str, layout: Layout, sites: Vec<Site>) -> Harness {
    Harness::start(name, layout, move |root| {
        trusted(root, &[HOST, "limited.example.test"]);
        write_files(&root.data(), &["a.txt", "b.txt", "c.txt"]);
        let mut list = SiteList::default();
        for site in sites {
            list.upsert(site);
        }
        list.save(&root.config()).expect("saving the sites failed");
    })
}

fn stored(harness: &Harness) -> SiteList {
    SiteList::load(&harness.root.config()).expect("reading the sites failed")
}

fn fill_id(harness: &mut Harness, id: &str, text: &str) {
    let input = harness.input_bounds(id);
    harness.fill_at(input.center(), text);
}

fn confirm_dialog(harness: &mut Harness, label: &str) {
    let buttons = harness.find_all(label);
    harness.click_at(buttons.last().expect("no confirm button").center());
}

fn recent_file(harness: &Harness) -> Option<String> {
    fs::read_to_string(harness.root.config().join(RECENT_FILE)).ok()
}

fn connect_directly(harness: &mut Harness, secret: &str) {
    harness.click("Connect");
    assert_eq!(harness.app.page, Page::Connect);
    harness.fill("Server", HOST);
    harness.fill("User name", "demo");
    harness.fill("Password", secret);
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Browser);
}

// Advanced site settings

#[test]
fn advanced_site_settings_are_saved_and_used_for_connecting() {
    let mut legacy = site("Legacy FTP", "advanced.example.test");
    legacy.protocol = Protocol::Ftp;
    legacy.port = 21;
    let mut harness = with_sites("advanced", Layout::Modern, vec![legacy]);
    harness.click("Sites");
    harness.click("Legacy FTP");
    assert!(!harness.has("Transfer mode"));
    harness.click("▸ Advanced settings");
    harness.scroll(EDITOR_SCROLL_ID, 2000.0);
    assert!(harness.has("Connection"));
    assert!(harness.has("FTP"));
    harness.fill("Connection timeout (s)", "45");
    harness.fill("Keep-alive interval (s)", "25");
    harness.pick_of("Transfer mode", 1, 2);
    assert!(harness.contains("Firewalls and routers often block that"));
    harness.pick_of("Transfer mode", 0, 2);
    harness.click(
        "Connect to the server's own address for passive mode instead of the address it reports (helps with servers behind NAT)",
    );
    harness.pick_of("File name encoding", 1, 2);
    harness.fill("Parallel transfers", "9");
    harness.scroll(EDITOR_SCROLL_ID, 4000.0);
    harness.click("Save");
    assert!(
        harness.has(
            "Parallel transfers must be a number from 1 to 8, or empty for the global setting."
        )
    );
    harness.scroll(EDITOR_SCROLL_ID, 2000.0);
    harness.fill("Parallel transfers", "2");
    harness.scroll(EDITOR_SCROLL_ID, 4000.0);
    harness.click("Save");

    let saved = stored(&harness).sites[0].clone();
    assert_eq!(saved.options.connect_timeout, Some(Duration::from_secs(45)));
    assert_eq!(
        saved.options.keepalive_interval,
        Some(Duration::from_secs(25))
    );
    assert_eq!(saved.options.ftp_mode, FtpMode::Passive);
    assert!(!saved.options.ftp_external_ip);
    assert_eq!(saved.options.ftp_encoding, FileNameEncoding::Windows1252);
    assert_eq!(saved.max_transfers, Some(2));
    let text = fs::read_to_string(harness.root.config().join("sites.conf")).expect("readable");
    assert!(text.contains("ftp_encoding = windows-1252"));

    harness.restart();
    harness.click("Sites");
    harness.click("Legacy FTP");
    assert!(harness.has("▸ Advanced settings (changed)"));
    harness.scroll(EDITOR_SCROLL_ID, 0.0);
    harness.fill("Password", password());
    harness.scroll(EDITOR_SCROLL_ID, 4000.0);
    harness.click("Save and connect");
    assert_eq!(harness.app.page, Page::Browser);
    let used = options_used_for("advanced.example.test").expect("no connection was made");
    assert_eq!(used, saved.options);
}

// Recent connections

#[test]
fn recent_connections_are_listed_filled_and_removed() {
    let mut harness = with_sites("recent", Layout::Modern, Vec::new());
    let secret = password().to_string();
    connect_directly(&mut harness, &secret);
    let text = recent_file(&harness).expect("recent.conf was not written");
    assert!(!text.contains(&secret));
    assert!(text.contains(HOST));

    harness.click("Connect");
    assert!(harness.has("Recent connections"));
    assert!(harness.has(&format!("sftp://demo@{HOST}")));
    assert!(harness.has("Password (not saved)"));
    harness.fill("Server", "other.example.test");
    harness.click(&format!("sftp://demo@{HOST}"));
    assert_eq!(harness.app.connect_form.host, HOST);
    assert_eq!(harness.app.connect_form.username, "demo");
    assert!(harness.app.connect_form.password.is_empty());

    harness.click("Back");
    harness.shortcut("k", Modifiers::CTRL);
    harness.type_text("recent");
    assert!(harness.has(&format!("Connect to sftp://demo@{HOST} (recent)")));
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Connect);
    assert_eq!(harness.app.connect_form.host, HOST);

    harness.click("Remove");
    assert!(!harness.has("Recent connections"));
    assert_eq!(recent_file(&harness), None);
}

#[test]
fn sites_are_not_recorded_and_the_setting_stops_recording() {
    let mut harness = with_sites("recent-off", Layout::Modern, vec![site("Web", HOST)]);
    harness.click("Sites");
    harness.double_click("Web");
    assert_eq!(harness.app.page, Page::Connect);
    harness.fill("Password", password());
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Browser);
    assert_eq!(recent_file(&harness), None);
    assert!(harness.app.recent.entries.is_empty());

    connect_directly(&mut harness, password());
    assert_eq!(harness.app.recent.entries.len(), 1);
    harness.click("Settings");
    harness.click("Remember recent connections");
    assert!(!harness.app.settings.remember_recent);
    assert_eq!(recent_file(&harness), None);
    harness.click("Back");
    connect_directly(&mut harness, password());
    assert!(harness.app.recent.entries.is_empty());
    assert_eq!(recent_file(&harness), None);
    harness.click("Connect");
    assert!(!harness.has("Recent connections"));
    harness.restart();
    assert!(!harness.app.settings.remember_recent);
}

// Export and import

#[test]
fn plain_exports_can_be_imported_into_a_new_folder() {
    let mut web = site("Web", HOST);
    web.folder = "Work".to_string();
    web.options.ftp_mode = FtpMode::Active;
    let mut harness = with_sites(
        "export-plain",
        Layout::Modern,
        vec![web, site("Mail", HOST)],
    );
    let file = text_of(&harness.root.data().join("list.hoplix-sites"));
    harness.click("Sites");
    harness.click("Export…");
    let Some(Dialog::ExportSites(dialog)) = &harness.app.dialog else {
        panic!("no export dialog");
    };
    assert!(dialog.path.ends_with("hoplix-sites.hoplix-sites"));
    assert_eq!(dialog.scope, hoplix_core::ExportScope::All);
    assert!(harness.has("Without passwords the file is plain text and contains no secrets."));
    let path = text_of(&harness.root.data().join("missing").join("x"));
    fill_id(&mut harness, PATH_INPUT, &path);
    confirm_dialog(&mut harness, "Export");
    assert!(harness.contains("The folder does not exist"));
    fill_id(&mut harness, PATH_INPUT, &file);
    confirm_dialog(&mut harness, "Export");
    assert!(harness.app.dialog.is_none());
    assert!(harness.contains("2 site(s) with 0 password(s) exported to"));
    let text = fs::read_to_string(&file).expect("the export was not written");
    assert!(text.starts_with("# Hoplix site list"));
    assert!(text.contains("encrypted = false"));

    harness.click("Import…");
    let path = text_of(&harness.root.data().join("nothing.hoplix-sites"));
    fill_id(&mut harness, PATH_INPUT, &path);
    confirm_dialog(&mut harness, "Import");
    assert!(harness.contains("The file does not exist"));
    let path = text_of(&harness.root.data().join("a.txt"));
    fill_id(&mut harness, PATH_INPUT, &path);
    confirm_dialog(&mut harness, "Import");
    assert!(harness.has("The file is not a Hoplix site list."));
    fill_id(&mut harness, PATH_INPUT, &file);
    let Some(Dialog::ImportSites(dialog)) = &harness.app.dialog else {
        panic!("no import dialog");
    };
    let folder = dialog.folder.clone();
    assert!(folder.starts_with("Imported "));
    confirm_dialog(&mut harness, "Import");
    assert!(harness.app.dialog.is_none());
    assert!(harness.contains("2 site(s) and 0 password(s) imported"));
    assert_eq!(
        harness.app.site_selection,
        Some(SiteSelection::Folder(folder.clone()))
    );
    let list = stored(&harness);
    assert_eq!(list.sites.len(), 4);
    let imported: Vec<&Site> = list
        .sites
        .iter()
        .filter(|candidate| candidate.folder.starts_with(&folder))
        .collect();
    assert_eq!(imported.len(), 2);
    assert!(imported.iter().all(|candidate| !candidate.save_secrets));
    let copy = imported
        .iter()
        .find(|candidate| candidate.name == "Web")
        .expect("Web was not imported");
    assert_eq!(copy.folder, format!("{folder}/Work"));
    assert_eq!(copy.options.ftp_mode, FtpMode::Active);
    assert!(harness.has(&format!("▾ {folder}")));

    harness.click("Import…");
    fill_id(&mut harness, PATH_INPUT, &file);
    confirm_dialog(&mut harness, "Import");
    let names: Vec<String> = stored(&harness)
        .sites
        .iter()
        .map(|candidate| candidate.name.clone())
        .collect();
    assert!(names.contains(&"Web (2)".to_string()), "{names:?}");
}

#[test]
fn encrypted_exports_carry_passwords_and_check_the_password() {
    let mut saved = site("Shop", HOST);
    saved.save_secrets = true;
    let id = saved.id.clone();
    let mut harness = with_sites(
        "export-encrypted",
        Layout::Modern,
        vec![saved, site("Wiki", HOST)],
    );
    let secret = test_password();
    harness.store_keyring_secret(&id, &secret);
    let file = text_of(&harness.root.data().join("secure.hoplix-sites"));
    harness.click("Sites");
    harness.click("Shop");
    harness.click("Export…");
    assert!(matches!(
        &harness.app.dialog,
        Some(Dialog::ExportSites(dialog)) if dialog.scope == hoplix_core::ExportScope::Site(id.clone())
    ));
    let text =
        harness.bounds("Save sites to a file, for example to move them to another computer.");
    let scope = Point::new(text.x + 60.0, text.y + text.height + 10.0 + 15.4);
    harness.pick_at(scope, 0, 2, 16.0);
    assert!(matches!(
        &harness.app.dialog,
        Some(Dialog::ExportSites(dialog)) if dialog.scope == hoplix_core::ExportScope::All
    ));
    fill_id(&mut harness, PATH_INPUT, &file);
    harness.click("Include saved passwords");
    assert!(harness.contains("The file will be encrypted with this password"));
    let export_password = test_password();
    fill_id(&mut harness, PASSWORD_INPUT, &export_password);
    fill_id(
        &mut harness,
        crate::site_exchange::CONFIRM_INPUT,
        "different",
    );
    assert!(harness.has("The passwords do not match."));
    fill_id(
        &mut harness,
        crate::site_exchange::CONFIRM_INPUT,
        &export_password,
    );
    confirm_dialog(&mut harness, "Export");
    assert!(harness.app.dialog.is_none());
    assert!(harness.contains("2 site(s) with 1 password(s) exported to"));
    let text = fs::read_to_string(&file).expect("the export was not written");
    assert!(!text.contains(&secret));
    assert!(!text.contains(HOST));
    assert!(text.contains("encrypted = true"));

    harness.click("Import…");
    fill_id(&mut harness, PATH_INPUT, &file);
    confirm_dialog(&mut harness, "Import");
    assert!(harness.has("The file is encrypted. Enter the password that was used for the export."));
    fill_id(&mut harness, PASSWORD_INPUT, "wrong");
    confirm_dialog(&mut harness, "Import");
    assert!(harness.has("The password is wrong."));
    assert!(matches!(&harness.app.dialog, Some(Dialog::ImportSites(_))));
    fill_id(&mut harness, PASSWORD_INPUT, &export_password);
    harness.key(Named::Enter);
    assert!(harness.app.dialog.is_none());
    assert!(harness.contains("2 site(s) and 1 password(s) imported"));
    let list = stored(&harness);
    let copy = list
        .sites
        .iter()
        .find(|candidate| candidate.name == "Shop" && candidate.id != id)
        .expect("Shop was not imported");
    assert!(copy.save_secrets);
    assert_eq!(
        harness.keyring_secret(&copy.id).as_deref(),
        Some(secret.as_str())
    );
    let wiki = list
        .sites
        .iter()
        .find(|candidate| candidate.name == "Wiki" && !candidate.folder.is_empty())
        .expect("Wiki was not imported");
    assert!(!wiki.save_secrets);
}

// Queue

#[test]
fn site_limits_are_shown_in_the_queue() {
    let mut limited = site("Limited", "limited.example.test");
    limited.max_transfers = Some(1);
    limited.save_secrets = true;
    let id = limited.id.clone();
    let mut harness = with_sites("site-limit", Layout::Classic, vec![limited]);
    harness.store_keyring_secret(&id, password());
    harness.app.focused_pane = 1;
    harness.click("Sites");
    harness.double_click("Limited");
    assert_eq!(harness.app.page, Page::Browser);
    assert!(harness.has("Limited: remote"));
    let left = harness.app.panes[0].tabs[0].id;
    let right = harness.app.panes[1].active_tab().expect("no site tab").id;
    let first = harness.find_all("a.txt")[0].center();
    harness.click_at(first);
    harness.click_with("b.txt", Modifiers::CTRL);
    harness.click_with("c.txt", Modifiers::CTRL);
    harness.send(Message::TransferTo {
        from: left,
        to: right,
    });
    assert_eq!(harness.app.transfer_jobs.len(), 3);
    assert_eq!(
        harness
            .find_all("Waiting: this site allows 1 at a time")
            .len(),
        2
    );
    assert!(harness.has("Waiting"));
    harness.drive_transfers_until(|app| {
        app.transfer_jobs.len() == 3 && app.transfer_jobs.iter().all(|job| job.status.is_finished())
    });
    assert!(
        harness
            .app
            .transfer_jobs
            .iter()
            .all(|job| job.status == JobStatus::Completed)
    );
    assert!(harness.root.remote().join("c.txt").is_file());
}
