use std::time::Duration;

// Constants

pub const MAX_TIMEOUT_SECONDS: u64 = 3600;

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FtpMode {
    #[default]
    Passive,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileNameEncoding {
    #[default]
    Utf8,
    Windows1252,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionOptions {
    pub connect_timeout: Option<Duration>,
    pub keepalive_interval: Option<Duration>,
    pub ftp_mode: FtpMode,
    pub ftp_external_ip: bool,
    pub ftp_encoding: FileNameEncoding,
}

// Defaults

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            connect_timeout: None,
            keepalive_interval: None,
            ftp_mode: FtpMode::default(),
            ftp_external_ip: true,
            ftp_encoding: FileNameEncoding::default(),
        }
    }
}

// Keys

impl FtpMode {
    pub const ALL: [Self; 2] = [Self::Passive, Self::Active];

    pub fn key(self) -> &'static str {
        match self {
            Self::Passive => "passive",
            Self::Active => "active",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }
}

impl FileNameEncoding {
    pub const ALL: [Self; 2] = [Self::Utf8, Self::Windows1252];

    pub fn key(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Windows1252 => "windows-1252",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        let key = key.to_ascii_lowercase();
        match key.as_str() {
            "latin-1" | "latin1" | "iso-8859-1" | "cp1252" => Some(Self::Windows1252),
            _ => Self::ALL.into_iter().find(|encoding| encoding.key() == key),
        }
    }
}

impl ConnectionOptions {
    pub fn timeout_or(&self, default: Duration) -> Duration {
        self.connect_timeout
            .filter(|timeout| !timeout.is_zero())
            .unwrap_or(default)
    }

    pub fn keepalive(&self) -> Option<Duration> {
        self.keepalive_interval
            .filter(|interval| !interval.is_zero())
    }
}

// Seconds

pub fn seconds_from_text(text: &str) -> Option<Option<Duration>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(None);
    }
    let seconds = text
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds <= MAX_TIMEOUT_SECONDS)?;
    Some((seconds > 0).then(|| Duration::from_secs(seconds)))
}

pub fn seconds_text(value: Option<Duration>) -> String {
    value
        .filter(|duration| !duration.is_zero())
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_keep_the_previous_behaviour() {
        let options = ConnectionOptions::default();
        assert_eq!(options.ftp_mode, FtpMode::Passive);
        assert!(options.ftp_external_ip);
        assert_eq!(options.ftp_encoding, FileNameEncoding::Utf8);
        assert_eq!(
            options.timeout_or(Duration::from_secs(20)),
            Duration::from_secs(20)
        );
        assert_eq!(options.keepalive(), None);
    }

    #[test]
    fn keys_round_trip() {
        for mode in FtpMode::ALL {
            assert_eq!(FtpMode::from_key(mode.key()), Some(mode));
        }
        for encoding in FileNameEncoding::ALL {
            assert_eq!(FileNameEncoding::from_key(encoding.key()), Some(encoding));
        }
        assert_eq!(
            FileNameEncoding::from_key("Latin-1"),
            Some(FileNameEncoding::Windows1252)
        );
        assert_eq!(FtpMode::from_key("extended"), None);
    }

    #[test]
    fn seconds_parse_with_limits() {
        assert_eq!(seconds_from_text(""), Some(None));
        assert_eq!(seconds_from_text(" 0 "), Some(None));
        assert_eq!(seconds_from_text("45"), Some(Some(Duration::from_secs(45))));
        assert_eq!(seconds_from_text("3601"), None);
        assert_eq!(seconds_from_text("-1"), None);
        assert_eq!(seconds_text(Some(Duration::from_secs(45))), "45");
        assert_eq!(seconds_text(Some(Duration::ZERO)), "");
        assert_eq!(seconds_text(None), "");
    }
}
