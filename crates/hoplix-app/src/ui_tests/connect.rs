use hoplix_core::{HostAddress, Protocol};
use iced::keyboard::key::Named;

use super::fakes::{fingerprint, password};
use super::*;
use crate::app::{Dialog, Page};
use crate::connect_form::AuthMethod;

// Constants

const HOST: &str = "files.example.test";

// Helpers

fn connect_page(name: &str) -> Harness {
    connect_page_with(name, |_| {})
}

fn connect_page_with(name: &str, prepare: impl FnOnce(&TestRoot)) -> Harness {
    let mut harness = Harness::start(name, Layout::Modern, prepare);
    harness.click("Connect");
    assert_eq!(harness.app.page, Page::Connect);
    harness
}

fn fill_login(harness: &mut Harness, password: &str) {
    harness.fill("Server", HOST);
    harness.fill("User name", "demo");
    harness.fill("Password", password);
}

fn known_hosts(harness: &Harness) -> String {
    fs::read_to_string(harness.root.config().join("known_hosts")).unwrap_or_default()
}

fn remote_tabs(harness: &Harness) -> usize {
    harness.app.panes[0]
        .tabs
        .iter()
        .filter(|tab| tab.file_system.label().is_some())
        .count()
}

// Validation

#[test]
fn the_form_reports_missing_and_invalid_fields() {
    let mut harness = connect_page("validation");
    harness.click("Connect");
    assert!(harness.has("Enter a server address."));
    harness.fill("Server", HOST);
    assert!(!harness.has("Enter a server address."));
    harness.key(Named::Enter);
    assert!(harness.has("Enter a user name."));
    harness.fill("User name", "demo");

    harness.fill_input(1, "abc");
    assert_eq!(harness.app.connect_form.port, "");
    harness.fill_input(1, "70000");
    harness.click("Connect");
    assert!(harness.has("The port must be a number from 1 to 65535."));
    assert!(!harness.app.connect_form.connecting);
    assert!(harness.app.dialog.is_none());
}

#[test]
fn tab_moves_between_the_fields() {
    let mut harness = connect_page("tab-focus");
    harness.fill("Server", HOST);
    harness.key(Named::Tab);
    harness.key(Named::Tab);
    harness.type_text("demo");
    assert_eq!(harness.app.connect_form.username, "demo");
    harness.tap(Key::Named(Named::Tab), Modifiers::SHIFT);
    harness.type_text("2222");
    assert_eq!(harness.app.connect_form.port, "2222");
    assert_eq!(harness.app.connect_form.host, HOST);
}

#[test]
fn switching_protocols_changes_the_fields() {
    let mut harness = connect_page("protocols");
    assert!(harness.has("Sign in with"));
    harness.pick("Protocol", 1);
    assert_eq!(harness.app.connect_form.protocol, Protocol::Ftp);
    assert!(harness.contains("FTP is not encrypted"));
    assert!(!harness.has("Sign in with"));
    harness.pick("Protocol", 2);
    assert_eq!(harness.app.connect_form.protocol, Protocol::FtpsExplicit);
    assert!(!harness.contains("FTP is not encrypted"));
    harness.pick("Protocol", 3);
    assert_eq!(harness.app.connect_form.protocol, Protocol::FtpsImplicit);
    harness.pick("Protocol", 0);
    assert_eq!(harness.app.connect_form.protocol, Protocol::Sftp);

    harness.pick("Sign in with", 1);
    assert_eq!(harness.app.connect_form.method, AuthMethod::KeyFile);
    assert!(harness.has("Key file"));
    assert!(harness.has("Key passphrase (optional)"));
    harness.fill("Server", HOST);
    harness.fill("User name", "demo");
    harness.click("Connect");
    assert!(harness.has("Enter the path to a key file."));

    harness.pick("Protocol", 2);
    assert_eq!(
        harness.app.connect_form.effective_method(),
        AuthMethod::Password
    );
    assert!(harness.has("Password"));
    assert!(!harness.has("Key file"));
}

// Host keys

#[test]
fn unknown_host_keys_can_be_trusted_once() {
    let mut harness = connect_page("trust-once");
    fill_login(&mut harness, password());
    harness.key(Named::Enter);
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
    assert!(harness.has(&fingerprint(HOST)));
    harness.click("Trust once");
    assert!(harness.app.dialog.is_none());
    assert_eq!(harness.app.page, Page::Browser);
    assert_eq!(remote_tabs(&harness), 1);
    assert_eq!(harness.tab().path, text_of(&harness.root.remote()));
    assert!(harness.app.connect_form.password.is_empty());
    assert_eq!(known_hosts(&harness), "");
}

#[test]
fn always_trusted_host_keys_are_remembered() {
    let mut harness = connect_page("trust-always");
    fill_login(&mut harness, password());
    harness.click("Connect");
    harness.click("Always trust");
    assert_eq!(remote_tabs(&harness), 1);
    assert!(known_hosts(&harness).contains(&fingerprint(HOST)));

    harness.click("Connect");
    harness.fill("Password", password());
    harness.click("Connect");
    assert!(harness.app.dialog.is_none());
    assert_eq!(remote_tabs(&harness), 2);
}

#[test]
fn rejected_host_keys_do_not_connect() {
    let mut harness = connect_page("trust-reject");
    fill_login(&mut harness, password());
    harness.click("Connect");
    harness.click("Cancel");
    assert!(harness.app.dialog.is_none());
    assert_eq!(remote_tabs(&harness), 0);
    assert!(!harness.app.connect_form.connecting);
    assert_eq!(harness.app.page, Page::Connect);

    harness.click("Connect");
    harness.key(Named::Escape);
    assert!(harness.app.dialog.is_none());
    assert_eq!(remote_tabs(&harness), 0);
    assert_eq!(known_hosts(&harness), "");
}

#[test]
fn changed_host_keys_are_refused() {
    let mut harness = connect_page_with("changed-key", |root| {
        fs::write(
            root.config().join("known_hosts"),
            format!("{HOST}:22 SHA256:another-key\n"),
        )
        .expect("writing failed");
    });
    fill_login(&mut harness, password());
    harness.click("Connect");
    assert!(harness.app.dialog.is_none());
    assert!(harness.contains("Warning: the server key has changed"));
    assert_eq!(remote_tabs(&harness), 0);
    assert!(!harness.app.connect_form.connecting);
}

#[test]
fn untrusted_certificates_ask_before_connecting() {
    let mut harness = connect_page("certificate");
    harness.pick("Protocol", 2);
    fill_login(&mut harness, password());
    harness.click("Connect");
    assert!(harness.has("Untrusted certificate"));
    assert!(matches!(
        harness.app.dialog,
        Some(Dialog::HostKey {
            certificate: true,
            ..
        })
    ));
    harness.click("Always trust");
    assert_eq!(remote_tabs(&harness), 1);
    let saved = fs::read_to_string(harness.root.config().join("trusted_certificates"))
        .expect("certificate not saved");
    assert!(saved.contains(&fingerprint(HOST)));
}

#[test]
fn wrong_passwords_show_an_error() {
    let mut harness = connect_page_with("wrong-password", |root| {
        fs::write(
            root.config().join("known_hosts"),
            format!("{HOST}:22 {}\n", fingerprint(HOST)),
        )
        .expect("writing failed");
    });
    fill_login(&mut harness, &test_password());
    harness.click("Connect");
    assert!(harness.contains("Sign-in failed for demo@files.example.test"));
    assert_eq!(remote_tabs(&harness), 0);
    harness.fill("Password", password());
    harness.click("Connect");
    assert_eq!(remote_tabs(&harness), 1);
    assert!(harness.app.connect_form.error.is_none());
}

#[test]
fn plain_ftp_connects_without_a_prompt() {
    let mut harness = connect_page("ftp");
    harness.pick("Protocol", 1);
    fill_login(&mut harness, password());
    harness.click("Connect");
    assert!(harness.app.dialog.is_none());
    assert_eq!(remote_tabs(&harness), 1);
    assert!(harness.contains("demo@files.example.test"));
}

// S3

#[test]
fn s3_connections_pass_the_bucket_settings_and_keep_the_secret_out_of_recent() {
    use hoplix_core::RECENT_FILE;

    let mut harness = connect_page("s3");
    harness.pick("Protocol", 4);
    assert_eq!(harness.app.connect_form.protocol, Protocol::S3);
    assert!(harness.has("Access key ID") && harness.has("Secret access key"));
    assert!(!harness.has("Sign in with"));
    harness.fill("Endpoint", "storage.example.test");
    harness.fill("Access key ID", "demo-access");
    harness.fill("Secret access key", password());
    harness.click("Connect");
    assert!(harness.has("Enter a bucket name."));
    harness.fill("Bucket", "demo bucket");
    harness.click("Connect");
    assert!(harness.contains("A bucket name has 3 to 255"));
    harness.fill("Bucket", "demo-bucket");
    harness.fill("Region", "eu-central-1");
    harness.click("Path-style addresses (most local servers need this)");
    assert!(!harness.contains("HTTP is not encrypted"));
    harness.click("Unencrypted HTTP");
    assert!(harness.contains("HTTP is not encrypted"));
    harness.fill_input(1, "9000");
    harness.click("Connect");
    assert_eq!(harness.app.page, Page::Browser);

    let options = super::fakes::options_used_for("storage.example.test").expect("no connection");
    assert_eq!(options.s3.bucket, "demo-bucket");
    assert_eq!(options.s3.region, "eu-central-1");
    assert!(options.s3.path_style && options.s3.plain_http);
    let recent = fs::read_to_string(harness.root.config().join(RECENT_FILE)).expect("recent");
    assert!(recent.contains("protocol = s3") && recent.contains("s3_bucket = demo-bucket"));
    assert!(recent.contains("port = 9000"));
    assert!(!recent.contains(password()));

    harness.click("Connect");
    assert!(harness.has("s3://demo-bucket @ storage.example.test:9000"));
    harness.click("s3://demo-bucket @ storage.example.test:9000");
    assert_eq!(harness.app.connect_form.s3.bucket, "demo-bucket");
    assert!(harness.app.connect_form.s3.plain_http);
    assert!(harness.app.connect_form.password.is_empty());
}
