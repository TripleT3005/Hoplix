mod certificates;
mod connection;
mod entry;
mod error;
mod file_system;
mod ftp;
pub mod key_value;
pub mod language;
mod local;
mod paths;
mod secrets;
mod sftp;
mod sites;
mod sync;
mod transfer;

pub use certificates::certificate_fingerprint;
pub use connection::{Authentication, ConnectionParams, HostKeyTrust, Protocol, connect};
pub use entry::{Entry, EntryKind, SortKey, SortOrder, compare_names, sort_entries};
pub use error::{Error, ErrorKind, Result};
pub use file_system::{
    BoxFuture, FileSystem, Reader, Writer, copy_file, is_valid_name, remove_recursive,
};
pub use ftp::FtpFileSystem;
pub use local::LocalFileSystem;
pub use paths::{TrustStore, config_dir};
pub use secrets::{
    DEFAULT_ITERATIONS, Keyring, Moved, SecretBackend, SecretStore, VAULT_FILE, Vault,
    change_master_password, create_vault, move_to_keyring, move_to_vault, secret_name, site_id_of,
    unlock_vault,
};
pub use sftp::{SftpFileSystem, fingerprint};
pub use sites::{SITES_FILE, Site, SiteAuth, SiteList, new_id, normalize_folder};
pub use sync::{
    CompareOptions, CompareStatus, ComparedEntry, DEFAULT_TIME_TOLERANCE, Pattern, Side,
    SyncAction, SyncDirection, SyncEndpoint, SyncMode, SyncOptions, SyncPlan, SyncSummary,
    build_plan, compare, compare_entries, execute_plan, plan, refine,
};
pub use transfer::{
    Conflict, ConflictDecision, ConflictPolicy, DEFAULT_PARALLELISM, JobId, JobOptions,
    JobSnapshot, JobStatus, MAX_RETRIES, TransferEvent, TransferQueue, TransferRequest,
};
