mod bookmarks;
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
mod properties;
mod proxy;
mod rate;
mod recent;
mod route;
mod search;
mod secrets;
mod sftp;
mod site_export;
mod site_route;
mod sites;
mod ssh_agent;
mod sync;
mod transfer;
pub mod update;

#[cfg(test)]
mod test_support;

pub use bookmarks::{BOOKMARKS_FILE, Bookmark, Bookmarks};
pub use certificates::certificate_fingerprint;
pub use connection::{
    Authentication, ConnectionParams, HostAddress, HostKeyTrust, Protocol, connect, connect_with,
    encode_url_path,
};
pub use encoding::{decode_windows_1252, encode_windows_1252};
pub use entry::{Entry, EntryKind, SortKey, SortOrder, compare_names, sort_entries};
pub use error::{Error, ErrorKind, Result};
pub use file_system::{
    BoxFuture, FileSystem, Reader, Writer, copy_file, is_valid_name, read_head, remove_recursive,
};
pub use ftp::{FtpFileSystem, ensure_passive_mode};
pub use local::LocalFileSystem;
pub use options::{
    ConnectionOptions, FileNameEncoding, FtpMode, MAX_TIMEOUT_SECONDS, seconds_from_text,
    seconds_text,
};
pub use paths::{TrustStore, config_dir};
pub use properties::{
    FolderSize, PERMISSION_BITS, PermissionChange, Properties, WindowsAttributes, folder_size,
    mode_text, parse_mode, parse_permission_text, permission_text, set_permissions_with,
};
pub use proxy::{Proxy, ProxyCredentials, ProxyKind};
pub use rate::RateLimiter;
pub use recent::{MAX_RECENT, RECENT_FILE, RecentConnection, RecentList};
pub use route::{JumpHost, Route, connect_via, connect_via_with};
pub use search::{NameFilter, Search, SearchBatch, SearchHit, Visited, Walk};
pub use secrets::{
    DEFAULT_ITERATIONS, Keyring, Moved, SecretBackend, SecretSlot, SecretStore, VAULT_FILE, Vault,
    change_master_password, create_vault, jump_secret_name, move_to_keyring, move_to_vault,
    proxy_secret_name, secret_name, site_id_of, unlock_vault,
};
pub use sftp::{SftpFileSystem, auto_reconnect, fingerprint, set_auto_reconnect};
pub use site_export::{
    EXPORT_EXTENSION, EXPORT_FILE_NAME, ExportError, ExportFile, ExportScope, ImportSummary,
    SealedExport, SiteExport, decrypt_export, encrypt_export, utc_date,
};
pub use site_route::{SiteJump, SiteProxy};
pub use sites::{
    MAX_SITE_TRANSFERS, SITES_FILE, Site, SiteAuth, SiteList, new_id, normalize_folder,
};
pub use ssh_agent::{AGENT_EMPTY, AGENT_MISSING, AGENT_REJECTED};
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
