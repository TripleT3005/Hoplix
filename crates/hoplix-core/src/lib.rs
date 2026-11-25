mod certificates;
mod connection;
mod encoding;
mod entry;
mod error;
mod file_system;
mod ftp;
pub mod key_value;
pub mod language;
mod local;
mod options;
mod paths;
mod rate;
mod recent;
mod secrets;
mod sftp;
mod site_export;
mod sites;
mod sync;
mod transfer;
pub mod update;

#[cfg(test)]
mod test_support;

pub use certificates::certificate_fingerprint;
pub use connection::{
    Authentication, ConnectionParams, HostKeyTrust, Protocol, connect, connect_with,
};
pub use encoding::{decode_windows_1252, encode_windows_1252};
pub use entry::{Entry, EntryKind, SortKey, SortOrder, compare_names, sort_entries};
pub use error::{Error, ErrorKind, Result};
pub use file_system::{
    BoxFuture, FileSystem, Reader, Writer, copy_file, is_valid_name, remove_recursive,
};
pub use ftp::FtpFileSystem;
pub use local::LocalFileSystem;
pub use options::{
    ConnectionOptions, FileNameEncoding, FtpMode, MAX_TIMEOUT_SECONDS, seconds_from_text,
    seconds_text,
};
pub use paths::{TrustStore, config_dir};
pub use rate::RateLimiter;
pub use recent::{MAX_RECENT, RECENT_FILE, RecentConnection, RecentList};
pub use secrets::{
    DEFAULT_ITERATIONS, Keyring, Moved, SecretBackend, SecretStore, VAULT_FILE, Vault,
    change_master_password, create_vault, move_to_keyring, move_to_vault, secret_name, site_id_of,
    unlock_vault,
};
pub use sftp::{SftpFileSystem, auto_reconnect, fingerprint, set_auto_reconnect};
pub use site_export::{
    EXPORT_EXTENSION, EXPORT_FILE_NAME, ExportError, ExportFile, ExportScope, ImportSummary,
    SealedExport, SiteExport, decrypt_export, encrypt_export, utc_date,
};
pub use sites::{
    MAX_SITE_TRANSFERS, SITES_FILE, Site, SiteAuth, SiteList, new_id, normalize_folder,
};
pub use sync::{
    CompareOptions, CompareStatus, ComparedEntry, DEFAULT_TIME_TOLERANCE, Pattern, Side,
    SyncAction, SyncDirection, SyncEndpoint, SyncMode, SyncOptions, SyncPlan, SyncSummary,
    build_plan, compare, compare_entries, execute_plan, execute_plan_with, plan, refine,
};
pub use transfer::{
    Conflict, ConflictDecision, ConflictPolicy, DEFAULT_PARALLELISM, DEFAULT_RETRIES,
    DEFAULT_RETRY_DELAY, JobId, JobOptions, JobSnapshot, JobStatus, Parallelism, RetryPolicy,
    SiteLimit, SpeedLimits, TransferDirection, TransferEvent, TransferQueue, TransferRequest,
};
