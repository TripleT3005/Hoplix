use hoplix_core::{
    Authentication, FtpMode, HostAddress, JumpHost, Protocol, ProxyCredentials, ProxyKind,
    RECENT_FILE, SecretBackend, Site, SiteAuth, SiteJump, SiteList, SiteProxy, Vault,
    jump_secret_name, proxy_secret_name, secret_name,
};
use iced::keyboard::key::Named;

use super::fakes::{AGENT_USER, SHARED_KEY_DOMAIN, fingerprint, password, route_used_by};
use super::*;
use crate::app::{Dialog, LoadState, MasterMode, Page, SiteSelection};
use crate::connect_form::AuthMethod;
use crate::session::{SavedTab, Session, TabOrigin};
use crate::site_exchange::{CONFIRM_INPUT, PASSWORD_INPUT, PATH_INPUT};
use crate::ui::EDITOR_SCROLL_ID;

// Constants

const HOST: &str = "inner.example.test";
const JUMP: &str = "bastion.example.test";
const PROXY: &str = "proxy.example.test";
const SCROLL_STEP: f32 = 200.0;
const SCROLL_LIMIT: f32 = 6000.0;

// Helpers

fn trust(root: &TestRoot, hosts: &[&str]) {
    let lines: String = hosts
        .iter()
        .map(|host| format!("{host}:22 {}\n", fingerprint(host)))
        .collect();
    fs::write(root.config().join("known_hosts"), lines).expect("writing failed");
}

fn routed_site(name: &str) -> Site {
    Site {
        host: HOST.to_string(),
        username: "demo".to_string(),
        save_secrets: true,
        jump: Some(SiteJump {
            host: JUMP.to_string(),
            port: 22,
            username: "hop".to_string(),
            auth: SiteAuth::Password,
            key_path: String::new(),
            save_secret: true,
        }),
        proxy: Some(SiteProxy {
            kind: ProxyKind::Socks5,
            host: PROXY.to_string(),
            port: 1080,
            username: "me".to_string(),
            save_secret: true,
        }),
        ..Site::new(name)
    }
}

fn start(name: &str, trusted: &[&str], sites: Vec<Site>) -> Harness {
    let trusted: Vec<String> = trusted.iter().map(|host| (*host).to_string()).collect();
    Harness::start(name, Layout::Modern, move |root| {
        let hosts: Vec<&str> = trusted.iter().map(String::as_str).collect();
        trust(root, &hosts);
        let mut list = SiteList::default();
        for site in sites {
            list.upsert(site);
        }
        list.save(&root.config()).expect("saving the sites failed");
    })
}

fn store_route_secrets(harness: &Harness, site: &Site) {
    harness.store_keyring_secret(&site.id, password());
    harness.store_keyring_named(&jump_secret_name(&site.id, 0), password());
    harness.store_keyring_named(&proxy_secret_name(&site.id), password());
}

fn stored(harness: &Harness) -> SiteList {
    SiteList::load(&harness.root.config()).expect("reading the sites failed")
}

fn open_connect_page(harness: &mut Harness) {
    harness.click("Connect");
    assert_eq!(harness.app.page, Page::Connect);
    harness.fill("Server", HOST);
    harness.fill("User name", "demo");
}

fn open_route_area(harness: &mut Harness) {
    harness.click("▸ Jump host and proxy");
    assert!(harness.app.connect_form.advanced_open);
}

fn reveal(harness: &mut Harness, label: &str) {
    let mut offset = 0.0;
    while !harness.has(label) {
        offset += SCROLL_STEP;
        assert!(offset <= SCROLL_LIMIT, "{label:?} never became visible");
        harness.scroll(EDITOR_SCROLL_ID, offset);
    }
}

fn fill_revealed(harness: &mut Harness, label: &str, text: &str) {
    reveal(harness, label);
    harness.fill(label, text);
}

fn click_revealed(harness: &mut Harness, label: &str) {
    reveal(harness, label);
    harness.click(label);
}

fn edit_site(harness: &mut Harness, name: &str) {
    harness.click("Sites");
    harness.click(name);
    harness.scroll(EDITOR_SCROLL_ID, 0.0);
    harness.send(Message::SiteField(crate::site_form::SiteField::Option(
        crate::site_form::OptionField::Toggle,
    )));
    assert!(
        harness
            .app
            .site_form
            .as_ref()
            .is_some_and(|form| form.options.open)
    );
}

fn submit(harness: &mut Harness) {
    let buttons = harness.find_all("Connect");
    harness.click_at(buttons.last().expect("no connect button").center());
}

fn error_shown(harness: &mut Harness, part: &str) -> bool {
    harness.contains(part)
}

// SSH agent

#[test]
fn agent_sign_in_is_offered_and_passed_through() {
    let mut harness = start("agent", &[HOST], Vec::new());
    open_connect_page(&mut harness);
    harness.pick_of("Sign in with", 2, 3);
    assert_eq!(harness.app.connect_form.method, AuthMethod::Agent);
    assert!(harness.contains("signs in with the keys of the running SSH agent"));
    assert!(!harness.has("Password"));
    submit(&mut harness);
    assert_eq!(harness.app.page, Page::Connect);
    assert!(error_shown(
        &mut harness,
        "No SSH agent is running for demo@inner.example.test."
    ));

    harness.fill("User name", AGENT_USER);
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Browser);
    let route = route_used_by(&harness.root.remote()).expect("no connection was made");
    assert!(route.is_direct());
    let recent = fs::read_to_string(harness.root.config().join(RECENT_FILE)).expect("recent");
    assert!(recent.contains("auth = agent"));

    harness.click("Connect");
    assert!(harness.has("SSH agent"));
}

// Jump hosts

#[test]
fn jump_host_keys_are_confirmed_before_the_target() {
    let mut harness = start("jump-keys", &[], Vec::new());
    open_connect_page(&mut harness);
    harness.fill("Password", password());
    open_route_area(&mut harness);
    harness.click("Connect through an SSH jump host");
    harness.fill("Jump host", JUMP);
    harness.fill("Jump host user", "hop");
    harness.fill("Jump host password", password());
    harness.key(Named::Enter);
    assert_eq!(
        harness.app.dialog,
        Some(Dialog::HostKey {
            fingerprint: fingerprint(JUMP),
            certificate: false,
            host: format!("hop@{JUMP}"),
            jump: true,
            address: HostAddress::new(JUMP, 22),
        })
    );
    assert!(harness.has("Unknown jump host"));
    assert!(harness.contains(&format!("the jump host hop@{JUMP}")));
    harness.click("Trust once");
    assert_eq!(
        harness.app.dialog,
        Some(Dialog::HostKey {
            fingerprint: fingerprint(HOST),
            certificate: false,
            host: HOST.to_string(),
            jump: false,
            address: HostAddress::new(HOST, 22),
        })
    );
    assert!(harness.has("Unknown server"));
    harness.click("Always trust");
    assert!(harness.app.dialog.is_none());
    assert_eq!(harness.app.page, Page::Browser);

    let route = route_used_by(&harness.root.remote()).expect("no connection was made");
    assert_eq!(
        route.jump_hosts,
        [JumpHost {
            host: JUMP.to_string(),
            port: 22,
            username: "hop".to_string(),
            authentication: Authentication::Password(password().to_string()),
        }]
    );
    let known = fs::read_to_string(harness.root.config().join("known_hosts")).expect("known");
    assert!(known.contains(HOST));
    assert!(!known.contains(JUMP));
    let recent = fs::read_to_string(harness.root.config().join(RECENT_FILE)).expect("recent");
    assert!(recent.contains(&format!("jump_host = {JUMP}")));
    assert!(!recent.contains(password()));
    assert!(harness.app.connect_form.jump.password.is_empty());

    harness.click("Connect");
    assert!(harness.contains(&format!("via hop@{JUMP}")));
    harness.click(&format!("sftp://demo@{HOST}"));
    assert!(harness.app.connect_form.jump.enabled);
    assert_eq!(harness.app.connect_form.jump.host, JUMP);
    assert!(harness.app.connect_form.jump.password.is_empty());
}

#[test]
fn a_jump_host_and_target_with_the_same_key_are_confirmed_separately() {
    let jump = format!("bastion{SHARED_KEY_DOMAIN}");
    let target = format!("inner{SHARED_KEY_DOMAIN}");
    assert_eq!(fingerprint(&jump), fingerprint(&target));
    let mut harness = start("jump-shared-key", &[], Vec::new());
    harness.click("Connect");
    harness.fill("Server", &target);
    harness.fill("User name", "demo");
    harness.fill("Password", password());
    open_route_area(&mut harness);
    harness.click("Connect through an SSH jump host");
    harness.fill("Jump host", &jump);
    harness.fill("Jump host user", "hop");
    harness.fill("Jump host password", password());
    harness.key(Named::Enter);
    assert_eq!(
        harness.app.dialog,
        Some(Dialog::HostKey {
            fingerprint: fingerprint(&jump),
            certificate: false,
            host: format!("hop@{jump}"),
            jump: true,
            address: HostAddress::new(&jump, 22),
        })
    );
    assert!(harness.has("Unknown jump host"));
    harness.click("Always trust");
    assert_eq!(
        harness.app.dialog,
        Some(Dialog::HostKey {
            fingerprint: fingerprint(&target),
            certificate: false,
            host: target.clone(),
            jump: false,
            address: HostAddress::new(&target, 22),
        })
    );
    assert!(harness.has("Unknown server"));
    assert_ne!(harness.app.page, Page::Browser);
    let known = fs::read_to_string(harness.root.config().join("known_hosts")).expect("known");
    assert!(known.contains(&jump));
    assert!(!known.contains(&target));

    harness.click("Cancel");
    assert!(harness.app.dialog.is_none());
    assert_ne!(harness.app.page, Page::Browser);
    submit(&mut harness);
    assert!(matches!(
        harness.app.dialog,
        Some(Dialog::HostKey { jump: false, .. })
    ));
    harness.click("Trust once");
    assert!(harness.app.dialog.is_none());
    assert_eq!(harness.app.page, Page::Browser);
    let known = fs::read_to_string(harness.root.config().join("known_hosts")).expect("known");
    assert!(!known.contains(&target));
}

#[test]
fn jump_host_fields_are_checked_and_failures_name_the_hop() {
    let mut harness = start("jump-errors", &[HOST, JUMP], Vec::new());
    open_connect_page(&mut harness);
    harness.fill("Password", password());
    open_route_area(&mut harness);
    harness.click("Connect through an SSH jump host");
    submit(&mut harness);
    assert!(harness.has("Enter the address of the jump host."));
    harness.fill("Jump host", JUMP);
    harness.fill("Jump host user", "hop");
    harness.fill("Jump host password", &test_password());
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Connect);
    assert!(error_shown(
        &mut harness,
        &format!("Sign-in failed for hop@{JUMP}.")
    ));
    harness.fill("Jump host", "offline.jump.test");
    harness.key(Named::Enter);
    assert!(error_shown(
        &mut harness,
        "At hop@offline.jump.test: The connection failed: offline.jump.test"
    ));
    harness.fill("Jump host", JUMP);

    harness.pick_of("Jump host sign-in", 2, 3);
    assert_eq!(harness.app.connect_form.jump.method, AuthMethod::Agent);
    submit(&mut harness);
    assert!(error_shown(
        &mut harness,
        &format!("No SSH agent is running for hop@{JUMP}.")
    ));

    harness.pick("Protocol", 1);
    assert!(!harness.has("Connect through an SSH jump host"));
    harness.fill("Password", password());
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Browser);
    assert!(
        route_used_by(&harness.root.remote())
            .expect("connected")
            .is_direct()
    );
}

// Proxies

#[test]
fn proxy_errors_and_sign_in_are_shown() {
    let mut harness = start("proxy", &[HOST], Vec::new());
    open_connect_page(&mut harness);
    harness.fill("Password", password());
    open_route_area(&mut harness);
    harness.pick_of("Proxy", 1, 3);
    assert_eq!(harness.app.connect_form.proxy.kind, Some(ProxyKind::Socks5));
    submit(&mut harness);
    assert!(harness.has("Enter the address of the proxy."));
    harness.fill("Proxy server", "offline.proxy.test");
    harness.key(Named::Enter);
    assert!(error_shown(
        &mut harness,
        "proxy socks5://offline.proxy.test:1080: connection refused"
    ));
    assert!(!harness.contains("At socks5://"));

    harness.fill("Proxy server", PROXY);
    harness.fill("Proxy user", "me");
    harness.fill("Proxy password", &test_password());
    harness.key(Named::Enter);
    assert!(error_shown(&mut harness, "was rejected"));

    harness.fill("Proxy password", password());
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Browser);
    let route = route_used_by(&harness.root.remote()).expect("no connection was made");
    let proxy = route.proxy.expect("no proxy was used");
    assert_eq!(proxy.label(), format!("socks5://{PROXY}:1080"));
    assert_eq!(
        proxy.credentials,
        Some(ProxyCredentials {
            username: "me".to_string(),
            password: password().to_string(),
        })
    );

    harness.click("Connect");
    harness.pick("Protocol", 1);
    assert!(harness.contains("work through a proxy in passive mode only"));
}

#[test]
fn active_ftp_through_a_proxy_cannot_be_saved_or_used() {
    let mut legacy = Site {
        protocol: Protocol::Ftp,
        port: 21,
        host: HOST.to_string(),
        username: "demo".to_string(),
        ..Site::new("Legacy")
    };
    legacy.options.ftp_mode = FtpMode::Active;
    let mut broken = legacy.clone();
    broken.id = hoplix_core::new_id();
    broken.name = "Broken".to_string();
    broken.proxy = Some(SiteProxy {
        kind: ProxyKind::Http,
        host: PROXY.to_string(),
        port: 8080,
        username: String::new(),
        save_secret: false,
    });
    let mut harness = start("active-proxy", &[], vec![legacy, broken]);
    edit_site(&mut harness, "Legacy");
    reveal(&mut harness, "Proxy");
    harness.pick_of("Proxy", 2, 3);
    fill_revealed(&mut harness, "Proxy server", PROXY);
    reveal(&mut harness, "Transfer mode");
    assert!(harness.contains("Active FTP mode does not work through a proxy."));
    click_revealed(&mut harness, "Save");
    assert!(
        harness
            .app
            .site_form
            .as_ref()
            .is_some_and(|form| form.invalid.is_some())
    );
    let saved = stored(&harness);
    assert!(
        saved
            .sites
            .iter()
            .all(|site| site.name != "Legacy" || site.proxy.is_none())
    );

    harness.scroll(EDITOR_SCROLL_ID, 0.0);
    harness.click("Broken");
    harness.scroll(EDITOR_SCROLL_ID, 0.0);
    harness.fill("Password", password());
    click_revealed(&mut harness, "Save and connect");
    assert_eq!(harness.app.page, Page::Sites);
    let form = harness.app.site_form.as_ref().expect("form");
    assert_eq!(
        form.invalid,
        Some(crate::site_form::SiteFormError::ActiveModeWithProxy)
    );
    harness.send(Message::SiteConnect(
        stored(&harness)
            .sites
            .iter()
            .find(|site| site.name == "Broken")
            .expect("broken site")
            .id
            .clone(),
    ));
    harness.fill("Password", password());
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Connect);
    assert!(error_shown(
        &mut harness,
        "active FTP mode does not work through the proxy"
    ));
}

// Sites

#[test]
fn jump_hosts_and_proxies_are_saved_with_the_site_and_removed_with_it() {
    let plain = Site {
        host: HOST.to_string(),
        username: "demo".to_string(),
        ..Site::new("Web")
    };
    let id = plain.id.clone();
    let mut harness = start("site-route", &[HOST, JUMP], vec![plain]);
    edit_site(&mut harness, "Web");
    harness.scroll(EDITOR_SCROLL_ID, 0.0);
    harness.fill("Password", password());
    harness.click("Save password");
    click_revealed(&mut harness, "Connect through an SSH jump host");
    fill_revealed(&mut harness, "Jump host", JUMP);
    fill_revealed(&mut harness, "Jump host user", "hop");
    fill_revealed(&mut harness, "Jump host password", password());
    reveal(&mut harness, "Jump host password");
    let checkboxes = harness.find_all("Save password");
    harness.click_at(checkboxes.last().expect("jump save checkbox").center());
    reveal(&mut harness, "Proxy");
    harness.pick_of("Proxy", 2, 3);
    fill_revealed(&mut harness, "Proxy server", PROXY);
    fill_revealed(&mut harness, "Proxy user", "me");
    fill_revealed(&mut harness, "Proxy password", password());
    reveal(&mut harness, "Proxy password");
    let checkboxes = harness.find_all("Save password");
    harness.click_at(checkboxes.last().expect("proxy save checkbox").center());
    click_revealed(&mut harness, "Save");

    let site = stored(&harness).get(&id).cloned().expect("site saved");
    assert_eq!(
        site.jump,
        Some(SiteJump {
            host: JUMP.to_string(),
            port: 22,
            username: "hop".to_string(),
            auth: SiteAuth::Password,
            key_path: String::new(),
            save_secret: true,
        })
    );
    assert_eq!(
        site.proxy,
        Some(SiteProxy {
            kind: ProxyKind::Http,
            host: PROXY.to_string(),
            port: 8080,
            username: "me".to_string(),
            save_secret: true,
        })
    );
    let text = fs::read_to_string(harness.root.config().join("sites.conf")).expect("sites");
    assert!(!text.contains(password()));
    assert_eq!(harness.keyring_secret(&id).as_deref(), Some(password()));
    assert_eq!(
        harness.keyring_named(&jump_secret_name(&id, 0)).as_deref(),
        Some(password())
    );
    assert_eq!(
        harness.keyring_named(&proxy_secret_name(&id)).as_deref(),
        Some(password())
    );

    harness.restart();
    harness.click("Sites");
    harness.double_click("Web");
    assert_eq!(harness.app.page, Page::Browser);
    let route = route_used_by(&harness.root.remote()).expect("no connection was made");
    assert_eq!(route.jump_hosts[0].host, JUMP);
    assert_eq!(
        route.jump_hosts[0].authentication,
        Authentication::Password(password().to_string())
    );
    assert_eq!(
        route.proxy.and_then(|proxy| proxy.credentials),
        Some(ProxyCredentials {
            username: "me".to_string(),
            password: password().to_string(),
        })
    );

    harness.click("Sites");
    harness.send(Message::SiteSelected(SiteSelection::Site(id.clone())));
    harness.send(Message::SiteDelete);
    harness.send(Message::DialogConfirmed);
    assert!(stored(&harness).sites.is_empty());
    assert_eq!(harness.keyring_secret(&id), None);
    assert_eq!(harness.keyring_named(&jump_secret_name(&id, 0)), None);
    assert_eq!(harness.keyring_named(&proxy_secret_name(&id)), None);
}

#[test]
fn unticking_save_password_forgets_the_route_secrets() {
    let site = routed_site("Routed");
    let id = site.id.clone();
    let mut harness = start("route-forget", &[HOST, JUMP], vec![site.clone()]);
    store_route_secrets(&harness, &site);
    edit_site(&mut harness, "Routed");
    reveal(&mut harness, "Jump host password");
    assert!(
        harness
            .app
            .site_form
            .as_ref()
            .is_some_and(|form| form.had_jump_secret)
    );
    let checkboxes = harness.find_all("Save password");
    harness.click_at(checkboxes.last().expect("save checkbox").center());
    click_revealed(&mut harness, "Save");
    let saved = stored(&harness).get(&id).cloned().expect("site");
    assert!(!saved.jump.expect("jump").save_secret);
    assert_eq!(harness.keyring_named(&jump_secret_name(&id, 0)), None);
    assert_eq!(
        harness.keyring_named(&proxy_secret_name(&id)).as_deref(),
        Some(password())
    );

    harness.send(Message::SiteConnect(id.clone()));
    assert_eq!(harness.app.page, Page::Connect);
    assert!(harness.app.connect_form.advanced_open);
    harness.fill("Jump host password", password());
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Browser);
}

#[test]
fn restored_site_tabs_reconnect_through_the_saved_route() {
    let site = routed_site("Routed");
    let id = site.id.clone();
    let prepared = site.clone();
    let prepared_id = id.clone();
    let mut harness = Harness::start("route-session", Layout::Modern, move |root| {
        trust(root, &[HOST, JUMP]);
        let mut list = SiteList::default();
        list.upsert(prepared);
        list.save(&root.config()).expect("saving the sites failed");
        Session {
            window: None,
            focused_pane: 0,
            preview: None,
            tabs: vec![SavedTab {
                pane: 0,
                origin: TabOrigin::Site(prepared_id),
                path: text_of(&root.remote()),
                active: true,
            }],
        }
        .save(&root.config())
        .expect("saving the session failed");
    });
    store_route_secrets(&harness, &site);
    assert_eq!(harness.tab().state, LoadState::Disconnected);
    harness.click_nth("Connect", 1);
    assert_eq!(harness.app.page, Page::Browser);
    assert_eq!(harness.tab().state, LoadState::Ready);
    let route = route_used_by(&harness.root.remote()).expect("no connection was made");
    assert_eq!(route.jump_hosts.len(), 1);
    assert!(route.proxy.is_some());

    harness.shortcut("k", Modifiers::CTRL);
    harness.type_text("Routed");
    harness.key(Named::Enter);
    assert_eq!(harness.app.page, Page::Browser);
    assert_eq!(
        route_used_by(&harness.root.remote())
            .expect("connected")
            .jump_hosts[0]
            .host,
        JUMP
    );
}

// Exchange and storage

#[test]
fn exports_and_imports_carry_jump_and_proxy_passwords() {
    let site = routed_site("Routed");
    let id = site.id.clone();
    let mut harness = start("route-export", &[], vec![site.clone()]);
    store_route_secrets(&harness, &site);
    let file = text_of(&harness.root.data().join("routes.hoplix-sites"));
    harness.click("Sites");
    harness.click("Export…");
    let input = harness.input_bounds(PATH_INPUT);
    harness.fill_at(input.center(), &file);
    harness.click("Include saved passwords");
    let export_password = test_password();
    let input = harness.input_bounds(PASSWORD_INPUT);
    harness.fill_at(input.center(), &export_password);
    let input = harness.input_bounds(CONFIRM_INPUT);
    harness.fill_at(input.center(), &export_password);
    let buttons = harness.find_all("Export");
    harness.click_at(buttons.last().expect("export button").center());
    assert!(harness.contains("1 site(s) with 3 password(s) exported to"));
    let text = fs::read_to_string(&file).expect("the export was not written");
    assert!(!text.contains(password()));
    assert!(!text.contains(JUMP));

    harness.click("Import…");
    let input = harness.input_bounds(PATH_INPUT);
    harness.fill_at(input.center(), &file);
    let buttons = harness.find_all("Import");
    harness.click_at(buttons.last().expect("import button").center());
    let input = harness.input_bounds(PASSWORD_INPUT);
    harness.fill_at(input.center(), &export_password);
    harness.key(Named::Enter);
    assert!(harness.contains("1 site(s) and 3 password(s) imported"));
    let list = stored(&harness);
    let copy = list
        .sites
        .iter()
        .find(|candidate| candidate.id != id)
        .expect("not imported");
    assert_eq!(
        copy.jump.as_ref().map(|jump| jump.host.as_str()),
        Some(JUMP)
    );
    assert_eq!(
        copy.saved_secret_names(),
        [
            secret_name(&copy.id),
            jump_secret_name(&copy.id, 0),
            proxy_secret_name(&copy.id)
        ]
    );
    for name in copy.saved_secret_names() {
        assert_eq!(harness.keyring_named(&name).as_deref(), Some(password()));
    }
}

#[test]
fn switching_the_password_store_moves_route_secrets() {
    let site = routed_site("Routed");
    let id = site.id.clone();
    let mut harness = start("route-vault", &[], vec![site.clone()]);
    store_route_secrets(&harness, &site);
    harness.click("Settings");
    harness.pick_of("Saved passwords", 1, 2);
    assert!(matches!(
        harness.app.dialog,
        Some(Dialog::MasterPassword {
            mode: MasterMode::Create,
            ..
        })
    ));
    let master = test_password();
    harness.type_text(&master);
    let inputs = harness.inputs();
    harness.fill_at(inputs.last().expect("confirm input").center(), &master);
    harness.click("Set master password");
    assert_eq!(harness.app.secret_backend, SecretBackend::Vault);
    let names = site.saved_secret_names();
    assert_eq!(names.len(), 3);
    let vault = Vault::unlock(&harness.root.config(), &master).expect("vault");
    for name in &names {
        assert_eq!(harness.keyring_named(name), None);
        assert_eq!(
            vault.get(name).expect("readable").as_deref(),
            Some(password())
        );
    }

    harness.restart();
    harness.click("Settings");
    harness.pick_of("Saved passwords", 0, 2);
    harness.type_text(&master);
    harness.key(Named::Enter);
    assert_eq!(harness.app.secret_backend, SecretBackend::Keyring);
    assert!(!Vault::exists(&harness.root.config()));
    for name in &names {
        assert_eq!(harness.keyring_named(name).as_deref(), Some(password()));
    }
    assert_eq!(harness.keyring_secret(&id).as_deref(), Some(password()));
}
