use hoplix_core::{SecretBackend, Site, SiteList, Vault, secret_name};

use super::fakes::fingerprint;
use super::*;
use crate::app::{Dialog, MasterMode};

// Constants

const HOST: &str = "files.example.test";
const PORTABLE_HINT: &str = "Portable mode: settings are kept in the config folder";

// Helpers

fn prepare(root: &TestRoot) -> String {
    fs::write(
        root.config().join("known_hosts"),
        format!("{HOST}:22 {}\n", fingerprint(HOST)),
    )
    .expect("writing failed");
    let site = Site {
        host: HOST.to_string(),
        username: "demo".to_string(),
        save_secrets: true,
        ..Site::new("Web")
    };
    let mut list = SiteList::default();
    list.upsert(site.clone());
    list.save(&root.config()).expect("saving the sites failed");
    site.id
}

fn master_mode(harness: &Harness) -> Option<MasterMode> {
    match &harness.app.dialog {
        Some(Dialog::MasterPassword { mode, .. }) => Some(*mode),
        _ => None,
    }
}

fn confirm_input(harness: &mut Harness, text: &str) {
    let inputs = harness.inputs();
    let input = *inputs.last().expect("no confirm input");
    harness.fill_at(input.center(), text);
}

// Portable mode

#[test]
fn portable_mode_uses_the_vault_for_new_passwords() {
    let mut id = String::new();
    let mut harness = Harness::portable("portable-vault", |root| id = prepare(root));
    assert_eq!(harness.app.secret_backend, SecretBackend::Vault);
    assert!(!Vault::exists(&harness.root.config()));

    harness.click("Settings");
    assert!(harness.contains(PORTABLE_HINT));
    assert!(harness.has("Set master password"));

    harness.click("Back");
    harness.click("Sites");
    harness.click("Web");
    let chosen = test_password();
    harness.fill("Password", &chosen);
    harness.click("Save and connect");
    assert_eq!(master_mode(&harness), Some(MasterMode::Create));

    let master = test_password();
    harness.type_text(&master);
    confirm_input(&mut harness, &master);
    harness.click("Set master password");
    assert!(harness.app.pending.is_none());
    assert_eq!(harness.app.secret_backend, SecretBackend::Vault);
    assert_eq!(harness.keyring_secret(&id), None);
    let vault = Vault::unlock(&harness.root.config(), &master).expect("vault missing");
    assert_eq!(
        vault.get(&secret_name(&id)).expect("readable").as_deref(),
        Some(chosen.as_str())
    );
}

#[test]
fn choosing_the_keyring_in_portable_mode_is_remembered() {
    let mut harness = Harness::portable("portable-keyring", |root| {
        prepare(root);
    });
    harness.click("Settings");
    harness.pick_of("Saved passwords", 0, 2);
    assert!(harness.app.dialog.is_none());
    assert_eq!(harness.app.secret_backend, SecretBackend::Keyring);
    assert!(harness.app.settings.portable_keyring);
    assert!(!harness.has("Set master password"));

    harness.restart();
    assert_eq!(harness.app.secret_backend, SecretBackend::Keyring);
}

#[test]
fn installed_mode_keeps_the_keyring_without_a_hint() {
    let mut harness = Harness::start("installed-keyring", Layout::Modern, |root| {
        prepare(root);
    });
    assert_eq!(harness.app.secret_backend, SecretBackend::Keyring);
    harness.click("Settings");
    assert!(!harness.contains(PORTABLE_HINT));
}
