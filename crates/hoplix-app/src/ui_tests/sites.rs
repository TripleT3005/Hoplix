use hoplix_core::{Site, SiteList};
use iced::keyboard::key::Named;

use super::fakes::{fingerprint, password};
use super::*;
use crate::app::{Dialog, LoadState, Page, SiteSelection};
use crate::session::{SavedTab, Session, TabOrigin};

// Constants

const HOST: &str = "files.example.test";

// Helpers

fn trusted(root: &TestRoot) {
    fs::write(
        root.config().join("known_hosts"),
        format!("{HOST}:22 {}\n", fingerprint(HOST)),
    )
    .expect("writing failed");
}

fn sites_page(name: &str, sites: Vec<Site>) -> Harness {
    let mut harness = Harness::start(name, Layout::Modern, move |root| {
        trusted(root);
        let mut list = SiteList::default();
        for site in sites {
            list.upsert(site);
        }
        list.save(&root.config()).expect("saving the sites failed");
    });
    harness.click("Sites");
    assert_eq!(harness.app.page, Page::Sites);
    harness
}

fn site(name: &str, folder: &str) -> Site {
    Site {
        host: HOST.to_string(),
        username: "demo".to_string(),
        folder: folder.to_string(),
        ..Site::new(name)
    }
}

fn stored(harness: &Harness) -> Vec<Site> {
    SiteList::load(&harness.root.config())
        .expect("reading the sites failed")
        .sites
}

// Editing

#[test]
fn folders_and_sites_are_created_and_saved() {
    let mut harness = sites_page("create", Vec::new());
    assert!(harness.contains("No saved sites yet"));
    harness.click("New folder");
    assert!(matches!(
        harness.app.dialog,
        Some(Dialog::NewSiteFolder { .. })
    ));
    harness.type_text("Work");
    harness.key(Named::Enter);
    assert_eq!(
        harness.app.site_selection,
        Some(SiteSelection::Folder("Work".to_string()))
    );
    assert!(harness.has("▾ Work"));

    harness.click("New site");
    assert!(
        harness
            .app
            .site_form
            .as_ref()
            .is_some_and(|form| form.is_new)
    );
    harness.click("Save");
    assert!(harness.has("Enter a server address."));
    assert!(stored(&harness).is_empty());

    harness.fill("Name", "Web server");
    harness.fill("Server", HOST);
    harness.fill("User name", "demo");
    harness.click("Save");
    let saved = stored(&harness);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "Web server");
    assert_eq!(saved[0].folder, "Work");
    assert!(harness.has(&format!("sftp://{HOST}")));
    assert_eq!(
        harness.app.site_selection,
        Some(SiteSelection::Site(saved[0].id.clone()))
    );
}

#[test]
fn sites_are_edited_duplicated_and_deleted() {
    let mut harness = sites_page("edit", vec![site("Web", "Work")]);
    harness.click("Web");
    harness.fill("Name", "Changed");
    harness.click("Cancel");
    assert_eq!(
        harness
            .app
            .site_form
            .as_ref()
            .map(|form| form.name.as_str()),
        Some("Web")
    );

    harness.fill("Name", "Shop");
    harness.click("Save");
    assert_eq!(stored(&harness)[0].name, "Shop");
    assert!(harness.has("Shop"));

    harness.click("Duplicate");
    assert!(harness.has("Shop (copy)"));
    assert_eq!(stored(&harness).len(), 2);

    harness.click("Shop (copy)");
    harness.click("Delete");
    assert!(harness.has("Delete “Shop (copy)”?"));
    let buttons = harness.find_all("Delete");
    harness.click_at(buttons.last().expect("no delete button").center());
    assert!(harness.app.dialog.is_none());
    assert_eq!(stored(&harness).len(), 1);
    assert!(!harness.has("Shop (copy)"));

    harness.click("▾ Work");
    harness.click("Delete");
    assert!(harness.contains("the 1 site(s) in it will be removed"));
    let buttons = harness.find_all("Delete");
    harness.click_at(buttons.last().expect("no delete button").center());
    assert!(stored(&harness).is_empty());
    assert!(harness.app.site_selection.is_none());
}

// Connecting

#[test]
fn double_clicking_a_site_without_password_opens_the_connect_page() {
    let mut harness = sites_page("double-click", vec![site("Web", "")]);
    harness.double_click("Web");
    assert_eq!(harness.app.page, Page::Connect);
    assert_eq!(harness.app.connect_form.host, HOST);
    assert!(harness.app.connect_form.password.is_empty());
    assert!(!harness.has("Save as site"));
    assert!(harness.has("Save password"));
    harness.fill("Password", password());
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Browser);
    let tab = harness.tab();
    assert_eq!(tab.title.as_deref(), Some("Web"));
    assert!(matches!(tab.origin, TabOrigin::Site(_)));
    assert!(harness.has("Web: remote"));
}

#[test]
fn saved_passwords_connect_directly() {
    let mut harness = sites_page("save-connect", vec![site("Web", "")]);
    harness.click("Web");
    harness.fill("Password", password());
    harness.click("Save password");
    harness.click("Save and connect");
    assert_eq!(harness.app.page, Page::Browser);
    let id = stored(&harness)[0].id.clone();
    assert!(stored(&harness)[0].save_secrets);
    assert_eq!(harness.keyring_secret(&id).as_deref(), Some(password()));

    harness.click("Sites");
    harness.double_click("Web");
    assert_eq!(harness.app.page, Page::Browser);
    assert_eq!(harness.app.panes[0].tabs.len(), 3);

    harness.click("Sites");
    harness.click("Web");
    harness.click("Save password");
    harness.click("Save");
    assert!(!stored(&harness)[0].save_secrets);
    assert_eq!(harness.keyring_secret(&id), None);
}

#[test]
fn the_connect_form_can_be_saved_as_a_site() {
    let mut harness = sites_page("save-as", Vec::new());
    harness.click("Back");
    harness.click("Connect");
    harness.fill("Server", "ftp.example.test");
    harness.fill("User name", "demo");
    harness.pick("Protocol", 1);
    harness.click("Save as site");
    assert_eq!(harness.app.page, Page::Sites);
    assert!(harness.has("New site"));
    harness.click("Save");
    let saved = stored(&harness);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "ftp.example.test");
    assert_eq!(saved[0].protocol, hoplix_core::Protocol::Ftp);
    assert_eq!(saved[0].port, 21);
}

// Sessions

#[test]
fn restored_site_tabs_reconnect_in_place() {
    let saved = site("Web", "");
    let id = saved.id.clone();
    let mut harness = Harness::start("session", Layout::Modern, move |root| {
        trusted(root);
        write_files(&root.remote(), &["deploy/index.html"]);
        let mut list = SiteList::default();
        list.upsert(saved);
        list.save(&root.config()).expect("saving the sites failed");
        Session {
            window: None,
            focused_pane: 0,
            tabs: vec![
                SavedTab {
                    pane: 0,
                    origin: TabOrigin::Local,
                    path: String::new(),
                    active: false,
                },
                SavedTab {
                    pane: 0,
                    origin: TabOrigin::Site(id),
                    path: text_of(&root.remote().join("deploy")),
                    active: true,
                },
            ],
        }
        .save(&root.config())
        .expect("saving the session failed");
    });
    assert_eq!(harness.app.panes[0].tabs.len(), 2);
    assert_eq!(harness.tab().state, LoadState::Disconnected);
    assert!(harness.has("Not connected. Hoplix does not reconnect on its own after a restart."));
    harness.click_nth("Connect", 1);
    assert_eq!(harness.app.page, Page::Connect);
    harness.fill("Password", password());
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Browser);
    assert_eq!(harness.app.panes[0].tabs.len(), 2);
    assert_eq!(harness.tab().state, LoadState::Ready);
    assert_eq!(harness.names(), ["index.html"]);
}

#[test]
fn closing_the_window_saves_the_session() {
    let mut harness = sites_page("session-save", vec![site("Web", "")]);
    harness.click("Back");
    harness.shortcut("t", Modifiers::CTRL);
    harness.send(Message::WindowClosing(window::Id::unique(), true));
    let session = Session::load(&harness.root.config());
    assert_eq!(session.tabs.len(), 2);
    assert!(session.tabs[1].active);
    assert_eq!(session.tabs[0].path, text_of(&harness.root.data()));
    assert!(session.window.is_some_and(|state| state.maximized));
}
