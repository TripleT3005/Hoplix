use std::time::SystemTime;

use hoplix_core::update::{CHECK_INTERVAL, unix_seconds};
use hoplix_core::{Error, ErrorKind};

use super::fakes::{Updates, release_page, releases};
use super::*;
use crate::app::{Dialog, Page};

// Helpers

const NEWER: &str = "99.0.0";
const NEWEST: &str = "100.0.0-beta.1";
const OFFER_TITLE: &str = "Hoplix 99.0.0 is available";
const HINT: &str = "Update 99.0.0 available";
const AUTOMATIC: &str = "Check for updates automatically (once a day)";

fn launch(name: &str, settings: Settings, updates: Arc<Updates>) -> Harness {
    Harness::launch(
        name,
        Layout::Modern,
        move |root| {
            settings
                .save(&root.config())
                .expect("saving the test settings failed");
        },
        updates,
    )
}

fn english() -> Settings {
    Settings {
        language: Some("en".to_string()),
        ..Settings::default()
    }
}

fn automatic_off() -> Settings {
    Settings {
        update_check: false,
        ..english()
    }
}

fn now() -> u64 {
    unix_seconds(SystemTime::now())
}

fn saved(harness: &Harness) -> Settings {
    Settings::load(Some(&harness.root.config()))
}

fn opened_urls(harness: &Harness) -> Vec<String> {
    harness
        .opened
        .lock()
        .expect("the opener list is poisoned")
        .iter()
        .map(|(path, editor)| {
            assert_eq!(editor, "");
            text_of(path)
        })
        .collect()
}

fn open_settings(harness: &mut Harness) {
    harness.click("Settings");
    assert_eq!(harness.app.page, Page::Settings);
}

// Automatic check

#[test]
fn the_automatic_check_runs_once_a_day_when_enabled() {
    let mut harness = launch("update-due", english(), Updates::up_to_date());
    assert_eq!(harness.updates.requests(), 1);
    let checked = saved(&harness)
        .update_last_check
        .expect("the check time was not saved");
    assert!(checked.abs_diff(now()) < 60);

    harness.restart();
    assert_eq!(harness.updates.requests(), 1);

    let yesterday = Settings {
        update_last_check: Some(now() - CHECK_INTERVAL.as_secs() + 600),
        ..english()
    };
    let harness = launch("update-recent", yesterday, Updates::up_to_date());
    assert_eq!(harness.updates.requests(), 0);

    let earlier = Settings {
        update_last_check: Some(now() - CHECK_INTERVAL.as_secs() - 600),
        ..english()
    };
    let harness = launch("update-old", earlier, Updates::up_to_date());
    assert_eq!(harness.updates.requests(), 1);
}

#[test]
fn nothing_is_requested_when_the_automatic_check_is_off() {
    let mut harness = launch("update-off", automatic_off(), Updates::offering(&[NEWER]));
    assert_eq!(harness.updates.requests(), 0);
    assert_eq!(harness.app.dialog, None);
    assert!(!harness.has(HINT));
    harness.restart();
    open_settings(&mut harness);
    assert!(harness.has("Never"));
    assert_eq!(harness.updates.requests(), 0);
    assert_eq!(saved(&harness).update_last_check, None);
}

// Offers

#[test]
fn a_new_version_is_offered_once_and_stays_in_the_status_bar() {
    let mut harness = launch(
        "update-offer",
        english(),
        Updates::offering(&["0.1.0", NEWER]),
    );
    assert_eq!(harness.updates.requests(), 1);
    assert!(matches!(
        &harness.app.dialog,
        Some(Dialog::Update { version, .. }) if version == NEWER
    ));
    assert!(harness.has(OFFER_TITLE));
    assert!(harness.has("Open release page"));
    harness.click("Later");
    assert_eq!(harness.app.dialog, None);
    assert!(harness.has(HINT));
    assert_eq!(
        saved(&harness).update_notified_version.as_deref(),
        Some(NEWER)
    );
    assert!(opened_urls(&harness).is_empty());

    open_settings(&mut harness);
    harness.click("Check now");
    assert_eq!(harness.updates.requests(), 2);
    assert_eq!(harness.app.dialog, None);
    assert!(harness.has("Hoplix 99.0.0 is available."));

    harness.restart();
    assert_eq!(harness.updates.requests(), 2);
    assert_eq!(harness.app.dialog, None);
    assert!(harness.has(HINT));
    harness.click(HINT);
    assert_eq!(opened_urls(&harness), [release_page(NEWER)]);

    harness.updates.respond(Ok(releases(&[NEWER, NEWEST])));
    open_settings(&mut harness);
    harness.click("Check now");
    assert!(harness.has("Hoplix 100.0.0-beta.1 is available"));
    harness.click("Open release page");
    assert_eq!(harness.app.dialog, None);
    assert_eq!(
        opened_urls(&harness),
        [release_page(NEWER), release_page(NEWEST)]
    );
    harness.click("Back");
    assert!(harness.has("Update 100.0.0-beta.1 available"));
}

#[test]
fn the_hint_disappears_once_the_version_is_installed() {
    let installed = Settings {
        update_last_check: Some(now()),
        update_latest_version: Some("0.0.1".to_string()),
        update_latest_url: Some(release_page("0.0.1")),
        ..english()
    };
    let mut harness = launch("update-installed", installed, Updates::up_to_date());
    assert_eq!(harness.updates.requests(), 0);
    assert_eq!(harness.app.dialog, None);
    assert!(!harness.contains("available"));

    let known = Settings {
        update_last_check: Some(now()),
        update_latest_version: Some(NEWER.to_string()),
        update_latest_url: Some(release_page(NEWER)),
        update_notified_version: Some(NEWER.to_string()),
        ..automatic_off()
    };
    known
        .save(&harness.root.config())
        .expect("saving the test settings failed");
    harness.restart();
    assert_eq!(harness.app.dialog, None);
    assert!(harness.has(HINT));

    open_settings(&mut harness);
    harness.click("Check now");
    assert_eq!(harness.updates.requests(), 1);
    assert!(harness.has("Hoplix is up to date."));
    harness.click("Back");
    assert!(!harness.has(HINT));
    assert_eq!(saved(&harness).update_latest_version, None);
}

// Manual check

#[test]
fn check_now_works_with_the_automatic_check_off() {
    let mut harness = launch("update-manual", automatic_off(), Updates::up_to_date());
    open_settings(&mut harness);
    assert!(harness.has("Never"));
    harness.click("Check now");
    assert_eq!(harness.updates.requests(), 1);
    assert!(harness.has("Hoplix is up to date."));
    assert!(!harness.has("Never"));
    assert!(saved(&harness).update_last_check.is_some());
    assert!(!saved(&harness).update_check);

    harness.click(AUTOMATIC);
    assert!(harness.app.settings.update_check);
    assert!(saved(&harness).update_check);
    harness.click(AUTOMATIC);
    assert!(!saved(&harness).update_check);
}

#[test]
fn failed_checks_show_the_error() {
    let updates = Updates::up_to_date();
    updates.respond(Err(Error::new(
        ErrorKind::ConnectionFailed,
        "api.github.com: timed out",
    )));
    let mut harness = launch("update-error", english(), updates);
    assert_eq!(harness.updates.requests(), 1);
    assert_eq!(harness.app.dialog, None);
    open_settings(&mut harness);
    assert!(harness.has("The update check failed: api.github.com: timed out"));

    harness
        .updates
        .respond(Ok("{\"message\": \"API rate limit exceeded\"}".to_string()));
    harness.click("Check now");
    assert!(harness.has("The update check failed: invalid release list"));
    assert!(!harness.contains("Update 99"));
}
