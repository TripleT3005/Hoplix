use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use hoplix_core::update::{self, CURRENT_VERSION, UpdateStatus, Version};
use hoplix_core::{BoxFuture, Error};
use iced::Task;

use crate::app::{Dialog, Hoplix, Message};

// Constants

pub(crate) const STARTUP_DELAY: Duration = Duration::from_secs(5);

// Types

pub(crate) type UpdateSource =
    Arc<dyn Fn() -> BoxFuture<'static, Result<String, Error>> + Send + Sync>;

#[derive(Debug, Clone)]
pub enum UpdateMessage {
    AutomaticToggled(bool),
    CheckNow,
    Checked(Result<String, Error>),
    OpenReleasePage(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CheckState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available,
    Failed(Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub url: String,
}

// Sources

pub(crate) fn github_source() -> UpdateSource {
    Arc::new(|| Box::pin(update::fetch_releases()))
}

// Messages

impl Hoplix {
    pub(crate) fn update_updates(&mut self, message: UpdateMessage) -> Task<Message> {
        match message {
            UpdateMessage::AutomaticToggled(enabled) => {
                self.settings.update_check = enabled;
                self.save_settings();
            }
            UpdateMessage::CheckNow => {
                if self.update_state != CheckState::Checking {
                    self.update_state = CheckState::Checking;
                    return self.fetch_releases(Duration::ZERO);
                }
            }
            UpdateMessage::Checked(result) => self.finish_update_check(result),
            UpdateMessage::OpenReleasePage(url) => self.open_release_page(&url),
        }
        Task::none()
    }

    pub(crate) fn start_update_check(&mut self) -> Task<Message> {
        let now = update::unix_seconds(SystemTime::now());
        if !self.settings.update_check || !update::is_due(self.settings.update_last_check, now) {
            return Task::none();
        }
        self.fetch_releases(self.services.update_delay)
    }

    fn fetch_releases(&self, delay: Duration) -> Task<Message> {
        let source = Arc::clone(&self.services.update_source);
        Task::perform(
            async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                source().await
            },
            |result| Message::Update(UpdateMessage::Checked(result)),
        )
    }

    fn finish_update_check(&mut self, result: Result<String, Error>) {
        let settings = &mut self.settings;
        settings.update_last_check = Some(update::unix_seconds(SystemTime::now()));
        match result.and_then(|text| update::evaluate(&text, CURRENT_VERSION)) {
            Ok(UpdateStatus::UpToDate) => {
                settings.update_latest_version = None;
                settings.update_latest_url = None;
                self.update_state = CheckState::UpToDate;
            }
            Ok(UpdateStatus::Available(release)) => {
                settings.update_latest_version = Some(release.version.to_string());
                settings.update_latest_url = Some(release.url);
                self.update_state = CheckState::Available;
            }
            Err(error) => self.update_state = CheckState::Failed(error),
        }
        self.save_settings();
        self.offer_update();
    }

    pub(crate) fn offer_update(&mut self) {
        let Some(available) = self.available_update() else {
            return;
        };
        if self.dialog.is_some()
            || self.settings.update_notified_version.as_deref() == Some(available.version.as_str())
        {
            return;
        }
        self.settings.update_notified_version = Some(available.version.clone());
        self.dialog = Some(Dialog::Update {
            version: available.version,
            url: available.url,
        });
        self.save_settings();
    }

    pub(crate) fn open_release_page(&mut self, url: &str) {
        if !update::is_release_page(url) {
            return;
        }
        if let Err(error) = (self.services.opener)(Path::new(url), "") {
            self.notice = Some(
                self.strings
                    .format("update.open_failed", &[("detail", error.detail())]),
            );
        }
    }

    pub fn available_update(&self) -> Option<AvailableUpdate> {
        let latest = self.settings.update_latest_version.as_deref()?;
        let current = Version::parse(CURRENT_VERSION)?;
        (Version::parse(latest)? > current).then(|| AvailableUpdate {
            version: latest.to_string(),
            url: self
                .settings
                .update_latest_url
                .clone()
                .unwrap_or_else(|| update::RELEASES_PAGE.to_string()),
        })
    }
}
