use std::sync::OnceLock;

use hoplix_core::{SecretBackend, Site, SiteList, Vault, secret_name};
use iced::keyboard::key::Named;

use super::fakes::{fingerprint, password};
use super::*;
use crate::app::{Dialog, MasterMode, Page};

// Constants

const HOST: &str = "files.example.test";

// Helpers

fn master() -> &'static str {
    static MASTER: OnceLock<String> = OnceLock::new();
    MASTER.get_or_init(test_password)
}

fn prepare(root: &TestRoot, vault: bool) -> String {
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
    if vault {
        let mut vault = Vault::create_with_iterations(&root.config(), master(), 1_000)
            .expect("creating the vault failed");
        vault
            .set(&secret_name(&site.id), password())
            .expect("storing failed");
    }
    site.id
}

fn with_vault(name: &str) -> (Harness, String) {
    let mut id = String::new();
    let harness = Harness::start(name, Layout::Modern, |root| id = prepare(root, true));
    assert_eq!(harness.app.secret_backend, SecretBackend::Vault);
    (harness, id)
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

// Master password

#[test]
fn a_master_password_moves_keyring_secrets_into_the_vault() {
    let mut id = String::new();
    let mut harness = Harness::start("vault-create", Layout::Modern, |root| {
        id = prepare(root, false);
    });
    harness.store_keyring_secret(&id, password());
    harness.click("Settings");
    harness.pick_of("Saved passwords", 1, 2);
    assert_eq!(master_mode(&harness), Some(MasterMode::Create));
    assert!(harness.has("Set a master password"));

    let chosen = test_password();
    harness.type_text(&chosen);
    confirm_input(&mut harness, &test_password());
    assert!(harness.has("The passwords do not match."));
    harness.key(Named::Enter);
    assert_eq!(master_mode(&harness), Some(MasterMode::Create));

    confirm_input(&mut harness, &chosen);
    assert!(!harness.has("The passwords do not match."));
    harness.click("Set master password");
    assert!(harness.app.dialog.is_none());
    assert_eq!(harness.app.secret_backend, SecretBackend::Vault);
    assert!(harness.has("Saved passwords are now protected by your master password."));
    assert!(harness.has("Change master password"));
    assert_eq!(harness.keyring_secret(&id), None);
    let vault = Vault::unlock(&harness.root.config(), &chosen).expect("vault missing");
    assert_eq!(
        vault.get(&secret_name(&id)).expect("readable").as_deref(),
        Some(password())
    );
}

#[test]
fn wrong_master_passwords_are_rejected_before_connecting() {
    let (mut harness, _) = with_vault("vault-unlock");
    harness.click("Sites");
    harness.double_click("Web");
    assert_eq!(master_mode(&harness), Some(MasterMode::Unlock));
    assert!(harness.has("Unlock saved passwords"));
    harness.type_text(&test_password());
    harness.key(Named::Enter);
    assert_eq!(master_mode(&harness), Some(MasterMode::Unlock));
    assert!(harness.has("The master password is wrong."));
    assert!(harness.app.vault.is_none());

    harness.type_text(master());
    assert!(!harness.has("The master password is wrong."));
    harness.key(Named::Enter);
    assert!(harness.app.dialog.is_none());
    assert!(harness.app.vault.is_some());
    assert_eq!(harness.app.page, Page::Browser);
    assert!(harness.has("Web: remote"));
}

#[test]
fn saving_a_password_into_a_locked_vault_asks_to_unlock() {
    let (mut harness, id) = with_vault("vault-store");
    harness.click("Sites");
    harness.click("Web");
    let changed = test_password();
    harness.fill("Password", &changed);
    harness.click("Save and connect");
    assert_eq!(master_mode(&harness), Some(MasterMode::Unlock));
    harness.type_text(master());
    harness.key(Named::Enter);
    assert!(harness.app.dialog.is_none());
    assert!(harness.app.pending.is_none());
    let vault = Vault::unlock(&harness.root.config(), master()).expect("unlock failed");
    assert_eq!(
        vault.get(&secret_name(&id)).expect("readable").as_deref(),
        Some(changed.as_str())
    );
}

#[test]
fn cancelling_the_unlock_dialog_keeps_everything_locked() {
    let (mut harness, _) = with_vault("vault-cancel");
    harness.click("Sites");
    harness.double_click("Web");
    harness.key(Named::Escape);
    assert!(harness.app.dialog.is_none());
    assert!(harness.app.pending.is_none());
    assert!(harness.app.vault.is_none());
    assert_eq!(harness.app.page, Page::Sites);
}

#[test]
fn the_master_password_can_be_changed() {
    let (mut harness, id) = with_vault("vault-change");
    harness.click("Settings");
    harness.click("Change master password");
    assert_eq!(master_mode(&harness), Some(MasterMode::Unlock));
    harness.type_text(master());
    harness.key(Named::Enter);
    assert_eq!(master_mode(&harness), Some(MasterMode::Change));
    let renewed = test_password();
    harness.type_text(&renewed);
    confirm_input(&mut harness, &renewed);
    harness.click("Change");
    assert!(harness.app.dialog.is_none());
    assert!(harness.has("The master password has been changed."));
    assert!(Vault::unlock(&harness.root.config(), master()).is_err());
    let vault = Vault::unlock(&harness.root.config(), &renewed).expect("unlock failed");
    assert_eq!(
        vault.get(&secret_name(&id)).expect("readable").as_deref(),
        Some(password())
    );
}

#[test]
fn switching_back_moves_secrets_into_the_keyring() {
    let (mut harness, id) = with_vault("vault-keyring");
    harness.click("Settings");
    harness.pick_of("Saved passwords", 0, 2);
    assert_eq!(master_mode(&harness), Some(MasterMode::Unlock));
    harness.type_text(master());
    harness.key(Named::Enter);
    assert!(harness.app.dialog.is_none());
    assert_eq!(harness.app.secret_backend, SecretBackend::Keyring);
    assert!(harness.has("Saved passwords are now kept in the system keyring."));
    assert!(!Vault::exists(&harness.root.config()));
    assert_eq!(harness.keyring_secret(&id).as_deref(), Some(password()));
}
