use hoplix_core::{ChecksumMode, MAX_HISTORY_LIMIT};

use crate::app::Hoplix;
use crate::settings::{
    MAX_PARALLEL_TRANSFERS, MAX_RETRY_ATTEMPTS, MAX_RETRY_DELAY, MAX_SPEED_LIMIT, MIN_RETRY_DELAY,
    RateUnit,
};

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferSetting {
    Parallel(usize),
    Downloads(Option<usize>),
    Uploads(Option<usize>),
    DownloadLimit(String),
    UploadLimit(String),
    DownloadUnit(RateUnit),
    UploadUnit(RateUnit),
    RetryAttempts(u32),
    RetryDelay(u32),
    AutoReconnect(bool),
    RestoreTransfers(bool),
    Checksum(ChecksumMode),
    History(bool),
    HistoryLimit(usize),
}

// Messages

impl Hoplix {
    pub(crate) fn update_transfer_setting(&mut self, setting: TransferSetting) {
        let settings = &mut self.settings;
        let direction =
            |limit: Option<usize>| limit.map(|count| count.clamp(1, MAX_PARALLEL_TRANSFERS));
        match setting {
            TransferSetting::Parallel(count) => {
                settings.parallel_transfers = count.clamp(1, MAX_PARALLEL_TRANSFERS);
            }
            TransferSetting::Downloads(limit) => settings.parallel_downloads = direction(limit),
            TransferSetting::Uploads(limit) => settings.parallel_uploads = direction(limit),
            TransferSetting::DownloadLimit(text) => match parse_amount(&text) {
                Some(amount) => settings.download_limit.amount = amount,
                None => return,
            },
            TransferSetting::UploadLimit(text) => match parse_amount(&text) {
                Some(amount) => settings.upload_limit.amount = amount,
                None => return,
            },
            TransferSetting::DownloadUnit(unit) => settings.download_limit.unit = unit,
            TransferSetting::UploadUnit(unit) => settings.upload_limit.unit = unit,
            TransferSetting::RetryAttempts(attempts) => {
                settings.retry_attempts = attempts.min(MAX_RETRY_ATTEMPTS);
            }
            TransferSetting::RetryDelay(seconds) => {
                settings.retry_delay = seconds.clamp(MIN_RETRY_DELAY, MAX_RETRY_DELAY);
            }
            TransferSetting::AutoReconnect(enabled) => settings.auto_reconnect = enabled,
            TransferSetting::RestoreTransfers(enabled) => settings.restore_transfers = enabled,
            TransferSetting::Checksum(mode) => settings.checksum = mode,
            TransferSetting::History(enabled) => settings.history_enabled = enabled,
            TransferSetting::HistoryLimit(limit) => {
                settings.history_limit = limit.clamp(1, MAX_HISTORY_LIMIT);
            }
        }
        self.apply_transfer_settings();
        self.apply_store_settings();
        self.save_settings();
    }

    pub(crate) fn apply_transfer_settings(&self) {
        let settings = &self.settings;
        let queue = &self.transfers;
        queue.set_parallelism(settings.parallel_transfers);
        queue.set_direction_limits(settings.parallel_downloads, settings.parallel_uploads);
        queue.set_retry_policy(settings.retry_policy());
        queue.set_speed_limits(settings.speed_limits());
        queue.set_checksum_mode(settings.checksum);
        hoplix_core::set_auto_reconnect(settings.auto_reconnect);
    }
}

// Input

fn parse_amount(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u32>()
        .ok()
        .filter(|amount| *amount <= MAX_SPEED_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_accept_digits_only() {
        assert_eq!(parse_amount(""), Some(0));
        assert_eq!(parse_amount(" 25 "), Some(25));
        assert_eq!(parse_amount("1000000"), Some(1_000_000));
        assert_eq!(parse_amount("1000001"), None);
        assert_eq!(parse_amount("-1"), None);
        assert_eq!(parse_amount("2.5"), None);
        assert_eq!(parse_amount("abc"), None);
    }
}
