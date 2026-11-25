use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use hoplix_core::update::{self, Version};
use hoplix_core::{
    DEFAULT_PARALLELISM, DEFAULT_RETRIES, DEFAULT_RETRY_DELAY, RetryPolicy, SpeedLimits, key_value,
};

// Constants

const FILE_NAME: &str = "settings.conf";
const SAME_AS_TOTAL: &str = "same";
const ON: &str = "on";
const OFF: &str = "off";
pub const MAX_PARALLEL_TRANSFERS: usize = 8;
pub const MAX_RETRY_ATTEMPTS: u32 = 10;
pub const MIN_RETRY_DELAY: u32 = 1;
pub const MAX_RETRY_DELAY: u32 = 60;
pub const MAX_SPEED_LIMIT: u32 = 1_000_000;

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    Modern,
    Classic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateUnit {
    KiB,
    #[default]
    MiB,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpeedLimit {
    pub amount: u32,
    pub unit: RateUnit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub theme: ThemePreference,
    pub language: Option<String>,
    pub layout: Layout,
    pub parallel_transfers: usize,
    pub parallel_downloads: Option<usize>,
    pub parallel_uploads: Option<usize>,
    pub download_limit: SpeedLimit,
    pub upload_limit: SpeedLimit,
    pub retry_attempts: u32,
    pub retry_delay: u32,
    pub auto_reconnect: bool,
    pub remember_recent: bool,
    pub editor: String,
    pub upload_changes_automatically: bool,
    pub update_check: bool,
    pub update_last_check: Option<u64>,
    pub update_notified_version: Option<String>,
    pub update_latest_version: Option<String>,
    pub update_latest_url: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::default(),
            language: None,
            layout: Layout::default(),
            parallel_transfers: DEFAULT_PARALLELISM,
            parallel_downloads: None,
            parallel_uploads: None,
            download_limit: SpeedLimit::default(),
            upload_limit: SpeedLimit::default(),
            retry_attempts: DEFAULT_RETRIES,
            retry_delay: u32::try_from(DEFAULT_RETRY_DELAY.as_secs()).unwrap_or(MIN_RETRY_DELAY),
            auto_reconnect: true,
            remember_recent: true,
            editor: String::new(),
            upload_changes_automatically: false,
            update_check: true,
            update_last_check: None,
            update_notified_version: None,
            update_latest_version: None,
            update_latest_url: None,
        }
    }
}

impl ThemePreference {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

impl Layout {
    pub const ALL: [Self; 2] = [Self::Modern, Self::Classic];

    pub fn pane_count(self) -> usize {
        match self {
            Self::Modern => 1,
            Self::Classic => 2,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Modern => "modern",
            Self::Classic => "classic",
        }
    }
}

impl RateUnit {
    pub const ALL: [Self; 2] = [Self::KiB, Self::MiB];

    pub fn bytes(self) -> u64 {
        match self {
            Self::KiB => 1024,
            Self::MiB => 1024 * 1024,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::KiB => "kib",
            Self::MiB => "mib",
        }
    }
}

impl SpeedLimit {
    pub fn bytes_per_second(self) -> Option<u64> {
        Some(u64::from(self.amount) * self.unit.bytes()).filter(|rate| *rate > 0)
    }
}

// Transfers

impl Settings {
    pub fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy {
            attempts: self.retry_attempts,
            delay: Duration::from_secs(u64::from(self.retry_delay)),
        }
    }

    pub fn speed_limits(&self) -> SpeedLimits {
        SpeedLimits {
            download: self.download_limit.bytes_per_second(),
            upload: self.upload_limit.bytes_per_second(),
        }
    }
}

// Serialization

fn parse_in<T: std::str::FromStr + PartialOrd>(value: &str, range: (T, T)) -> Option<T> {
    value
        .parse::<T>()
        .ok()
        .filter(|number| *number >= range.0 && *number <= range.1)
}

fn parse_direction_limit(value: &str) -> Option<Option<usize>> {
    if value == SAME_AS_TOTAL {
        return Some(None);
    }
    parse_in(value, (1, MAX_PARALLEL_TRANSFERS)).map(Some)
}

fn parse_version(value: &str) -> Option<String> {
    Version::parse(value).map(|version| version.to_string())
}

fn optional_text(value: Option<&String>) -> String {
    value.cloned().unwrap_or_default()
}

fn direction_limit_text(limit: Option<usize>) -> String {
    limit.map_or_else(|| SAME_AS_TOTAL.to_string(), |count| count.to_string())
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for (key, value) in key_value::parse(text) {
            match key.as_str() {
                "theme" => {
                    if let Some(theme) = ThemePreference::ALL.into_iter().find(|t| t.key() == value)
                    {
                        settings.theme = theme;
                    }
                }
                "language" => settings.language = Some(value).filter(|code| !code.is_empty()),
                "layout" => {
                    if let Some(layout) = Layout::ALL.into_iter().find(|l| l.key() == value) {
                        settings.layout = layout;
                    }
                }
                "parallel_transfers" => {
                    if let Some(count) = parse_in(&value, (1, MAX_PARALLEL_TRANSFERS)) {
                        settings.parallel_transfers = count;
                    }
                }
                "parallel_downloads" => {
                    if let Some(limit) = parse_direction_limit(&value) {
                        settings.parallel_downloads = limit;
                    }
                }
                "parallel_uploads" => {
                    if let Some(limit) = parse_direction_limit(&value) {
                        settings.parallel_uploads = limit;
                    }
                }
                "download_limit" => {
                    if let Some(amount) = parse_in(&value, (0, MAX_SPEED_LIMIT)) {
                        settings.download_limit.amount = amount;
                    }
                }
                "download_limit_unit" => {
                    if let Some(unit) = RateUnit::ALL.into_iter().find(|u| u.key() == value) {
                        settings.download_limit.unit = unit;
                    }
                }
                "upload_limit" => {
                    if let Some(amount) = parse_in(&value, (0, MAX_SPEED_LIMIT)) {
                        settings.upload_limit.amount = amount;
                    }
                }
                "upload_limit_unit" => {
                    if let Some(unit) = RateUnit::ALL.into_iter().find(|u| u.key() == value) {
                        settings.upload_limit.unit = unit;
                    }
                }
                "retry_attempts" => {
                    if let Some(attempts) = parse_in(&value, (0, MAX_RETRY_ATTEMPTS)) {
                        settings.retry_attempts = attempts;
                    }
                }
                "retry_delay" => {
                    if let Some(seconds) = parse_in(&value, (MIN_RETRY_DELAY, MAX_RETRY_DELAY)) {
                        settings.retry_delay = seconds;
                    }
                }
                "reconnect_automatically" => {
                    if let Ok(enabled) = value.parse::<bool>() {
                        settings.auto_reconnect = enabled;
                    }
                }
                "remember_recent_connections" => {
                    if let Ok(enabled) = value.parse::<bool>() {
                        settings.remember_recent = enabled;
                    }
                }
                "editor" => settings.editor = value,
                "upload_changes_automatically" => {
                    settings.upload_changes_automatically = value == "true";
                }
                "update_check" => match value.as_str() {
                    ON => settings.update_check = true,
                    OFF => settings.update_check = false,
                    _ => {}
                },
                "update_last_check" => settings.update_last_check = value.parse().ok(),
                "update_notified_version" => {
                    settings.update_notified_version = parse_version(&value);
                }
                "update_latest_version" => settings.update_latest_version = parse_version(&value),
                "update_latest_url" => {
                    settings.update_latest_url =
                        Some(value).filter(|url| update::is_release_page(url));
                }
                _ => {}
            }
        }
        settings
    }

    pub fn to_text(&self) -> String {
        key_value::write(&[
            ("theme", self.theme.key().to_string()),
            ("language", self.language.clone().unwrap_or_default()),
            ("layout", self.layout.key().to_string()),
            ("parallel_transfers", self.parallel_transfers.to_string()),
            (
                "parallel_downloads",
                direction_limit_text(self.parallel_downloads),
            ),
            (
                "parallel_uploads",
                direction_limit_text(self.parallel_uploads),
            ),
            ("download_limit", self.download_limit.amount.to_string()),
            (
                "download_limit_unit",
                self.download_limit.unit.key().to_string(),
            ),
            ("upload_limit", self.upload_limit.amount.to_string()),
            (
                "upload_limit_unit",
                self.upload_limit.unit.key().to_string(),
            ),
            ("retry_attempts", self.retry_attempts.to_string()),
            ("retry_delay", self.retry_delay.to_string()),
            ("reconnect_automatically", self.auto_reconnect.to_string()),
            (
                "remember_recent_connections",
                self.remember_recent.to_string(),
            ),
            ("editor", self.editor.clone()),
            (
                "upload_changes_automatically",
                self.upload_changes_automatically.to_string(),
            ),
            (
                "update_check",
                (if self.update_check { ON } else { OFF }).to_string(),
            ),
            (
                "update_last_check",
                self.update_last_check
                    .map(|seconds| seconds.to_string())
                    .unwrap_or_default(),
            ),
            (
                "update_notified_version",
                optional_text(self.update_notified_version.as_ref()),
            ),
            (
                "update_latest_version",
                optional_text(self.update_latest_version.as_ref()),
            ),
            (
                "update_latest_url",
                optional_text(self.update_latest_url.as_ref()),
            ),
        ])
    }

    // Storage

    pub fn load(directory: Option<&Path>) -> Self {
        directory
            .and_then(|directory| fs::read_to_string(directory.join(FILE_NAME)).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn save(&self, directory: &Path) -> io::Result<()> {
        fs::create_dir_all(directory)?;
        let temporary = directory.join(format!("{FILE_NAME}.tmp"));
        fs::write(&temporary, self.to_text())?;
        fs::rename(temporary, directory.join(FILE_NAME))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let settings = Settings {
            theme: ThemePreference::Dark,
            language: Some("de".to_string()),
            layout: Layout::Classic,
            parallel_transfers: 4,
            parallel_downloads: Some(3),
            parallel_uploads: None,
            download_limit: SpeedLimit {
                amount: 2,
                unit: RateUnit::MiB,
            },
            upload_limit: SpeedLimit {
                amount: 512,
                unit: RateUnit::KiB,
            },
            retry_attempts: 0,
            retry_delay: 30,
            auto_reconnect: false,
            remember_recent: false,
            editor: r#"C:\Tools\My Editor = "new".exe"#.to_string(),
            upload_changes_automatically: true,
            update_check: false,
            update_last_check: Some(1_790_000_000),
            update_notified_version: Some("1.0.0-beta.1".to_string()),
            update_latest_version: Some("1.0.0".to_string()),
            update_latest_url: Some(
                "https://github.com/TripleT3005/Hoplix/releases/tag/v1.0.0".to_string(),
            ),
        };
        assert_eq!(Settings::parse(&settings.to_text()), settings);
        assert!(settings.to_text().contains("update_check = off"));
    }

    #[test]
    fn unknown_values_keep_the_defaults() {
        let settings = Settings::parse(
            "theme = purple\nlayout = spiral\nlanguage =\nother = 1\nparallel_transfers = 99\n\
             parallel_downloads = 0\nparallel_uploads = many\ndownload_limit = -5\n\
             download_limit_unit = gib\nupload_limit = 99999999\nretry_attempts = 11\n\
             retry_delay = 0\nreconnect_automatically = maybe\nupdate_check = maybe\n\
             update_last_check = soon\nupdate_notified_version = new\n\
             remember_recent_connections = sometimes\n\
             update_latest_version = 1.0\nupdate_latest_url = https://example.com/\n",
        );
        assert_eq!(settings, Settings::default());
        let settings = Settings::parse("retry_delay = 61\nretry_attempts = -1\n");
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn missing_values_use_the_defaults() {
        let settings = Settings::parse("theme = dark\n");
        assert_eq!(settings.parallel_transfers, 2);
        assert_eq!(settings.parallel_downloads, None);
        assert_eq!(settings.retry_attempts, 3);
        assert_eq!(settings.retry_delay, 1);
        assert!(settings.auto_reconnect);
        assert!(settings.remember_recent);
        assert!(settings.update_check);
        assert_eq!(settings.update_last_check, None);
        assert_eq!(settings.speed_limits(), SpeedLimits::default());
    }

    #[test]
    fn limits_convert_to_the_queue() {
        let settings = Settings {
            download_limit: SpeedLimit {
                amount: 2,
                unit: RateUnit::MiB,
            },
            upload_limit: SpeedLimit {
                amount: 0,
                unit: RateUnit::KiB,
            },
            retry_attempts: 5,
            retry_delay: 4,
            ..Settings::default()
        };
        assert_eq!(
            settings.speed_limits(),
            SpeedLimits {
                download: Some(2 * 1024 * 1024),
                upload: None,
            }
        );
        assert_eq!(
            settings.retry_policy(),
            RetryPolicy {
                attempts: 5,
                delay: Duration::from_secs(4),
            }
        );
    }

    #[test]
    fn layouts_know_their_pane_count() {
        assert_eq!(Layout::Modern.pane_count(), 1);
        assert_eq!(Layout::Classic.pane_count(), 2);
    }
}
