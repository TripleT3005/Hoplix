use std::fmt;
use std::future::Future;
use std::io::SeekFrom;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use russh::client::Handle;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::{RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};
use tokio::io::AsyncSeekExt;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};

use crate::connection::{ConnectionParams, HostKeyTrust};
use crate::file_system::{Reader, Writer};
use crate::keyboard_interactive::SharedPrompter;
use crate::route::Route;
use crate::ssh::{HostKeyCheck, SshLink, SshTarget, auto_reconnect, connection_error};
use crate::{
    BoxFuture, ConnectionOptions, Entry, EntryKind, Error, ErrorKind, FileSystem, HashAlgorithm,
    PERMISSION_BITS, Properties, Result,
};

// Types

pub struct SftpFileSystem {
    target: SshTarget,
    connection: AsyncMutex<Arc<Connection>>,
    label: String,
}

struct Connection {
    session: SftpSession,
    link: SshLink,
    hashes: OnceCell<HashChannel>,
}

#[derive(Default)]
struct HashChannel {
    session: Option<RawSftpSession>,
    check_file: Vec<HashAlgorithm>,
    md5_hash: bool,
}

// Connecting

impl SftpFileSystem {
    pub async fn connect(
        params: &ConnectionParams,
        known_hosts: &Path,
        trust: Option<HostKeyTrust>,
    ) -> Result<Self> {
        let trusted: Vec<HostKeyTrust> = trust.into_iter().collect();
        Self::connect_via(params, &Route::default(), known_hosts, &trusted).await
    }

    pub async fn connect_via(
        params: &ConnectionParams,
        route: &Route,
        known_hosts: &Path,
        trusted: &[HostKeyTrust],
    ) -> Result<Self> {
        Self::connect_via_with(
            params,
            route,
            &ConnectionOptions::default(),
            known_hosts,
            trusted,
        )
        .await
    }

    pub async fn connect_via_with(
        params: &ConnectionParams,
        route: &Route,
        options: &ConnectionOptions,
        known_hosts: &Path,
        trusted: &[HostKeyTrust],
    ) -> Result<Self> {
        Self::connect_prompting(params, route, options, known_hosts, trusted, None).await
    }

    pub async fn connect_prompting(
        params: &ConnectionParams,
        route: &Route,
        options: &ConnectionOptions,
        known_hosts: &Path,
        trusted: &[HostKeyTrust],
        prompter: Option<SharedPrompter>,
    ) -> Result<Self> {
        let (target, link) =
            SshTarget::connect(params, route, options, known_hosts, trusted, prompter).await?;
        let connection = open_sftp(link, &params.host).await?;
        Ok(Self {
            target,
            connection: AsyncMutex::new(Arc::new(connection)),
            label: params.label(),
        })
    }

    async fn current(&self) -> Result<Arc<Connection>> {
        let mut slot = self.connection.lock().await;
        if slot.link.is_closed() {
            if !auto_reconnect() {
                return Err(self.target.closed_error());
            }
            *slot = Arc::new(self.reestablish().await?);
        }
        Ok(Arc::clone(&slot))
    }

    async fn reconnect(&self, stale: &Arc<Connection>) -> Result<Arc<Connection>> {
        let mut slot = self.connection.lock().await;
        if Arc::ptr_eq(&slot, stale) {
            *slot = Arc::new(self.reestablish().await?);
        }
        Ok(Arc::clone(&slot))
    }

    async fn reestablish(&self) -> Result<Connection> {
        let link = self.target.reconnect().await?;
        open_sftp(link, &self.target.params.host).await
    }

    async fn run<T, F, Fut>(&self, operation: F) -> Result<T>
    where
        F: Fn(Arc<Connection>) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let connection = self.current().await?;
        match operation(Arc::clone(&connection)).await {
            Err(error) if error.kind() == ErrorKind::ConnectionFailed && auto_reconnect() => {
                let connection = self.reconnect(&connection).await?;
                operation(connection).await
            }
            result => result,
        }
    }
}

async fn open_sftp(link: SshLink, host: &str) -> Result<Connection> {
    let channel = link
        .handle
        .channel_open_session()
        .await
        .map_err(|error| connection_error(host, &error))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|error| connection_error(host, &error))?;
    let session = SftpSession::new(channel.into_stream())
        .await
        .map_err(|error| sftp_error(&error, host))?;
    Ok(Connection {
        session,
        link,
        hashes: OnceCell::new(),
    })
}

// Interface

impl FileSystem for SftpFileSystem {
    fn label(&self) -> Option<String> {
        Some(self.label.clone())
    }

    fn url(&self, path: &str) -> Option<String> {
        Some(self.target.params.url(path))
    }

    fn home(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(self.run(|connection| async move {
            connection
                .session
                .canonicalize(".")
                .await
                .map_err(|error| sftp_error(&error, "."))
        }))
    }

    fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
        let path = path.to_string();
        Box::pin(self.run(move |connection| {
            let path = path.clone();
            async move { list_directory(&connection.session, &path).await }
        }))
    }

    fn metadata(&self, path: &str) -> BoxFuture<'_, Result<Entry>> {
        let path = path.to_string();
        Box::pin(self.run(move |connection| {
            let path = path.clone();
            async move {
                let session = &connection.session;
                let link = session
                    .symlink_metadata(path.as_str())
                    .await
                    .map_err(|error| sftp_error(&error, &path))?;
                let is_link = link.is_symlink();
                let attributes = if is_link {
                    session.metadata(path.as_str()).await.unwrap_or(link)
                } else {
                    link
                };
                Ok(entry_from(
                    file_name_posix(&path).to_string(),
                    &attributes,
                    is_link,
                ))
            }
        }))
    }

    fn create_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(self.run(move |connection| {
            let path = path.clone();
            async move {
                let session = &connection.session;
                if session.try_exists(path.as_str()).await.unwrap_or(false) {
                    return Err(Error::new(ErrorKind::AlreadyExists, path));
                }
                session
                    .create_dir(path.as_str())
                    .await
                    .map_err(|error| sftp_error(&error, &path))
            }
        }))
    }

    fn remove_file(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(self.run(move |connection| {
            let path = path.clone();
            async move {
                connection
                    .session
                    .remove_file(path.as_str())
                    .await
                    .map_err(|error| sftp_error(&error, &path))
            }
        }))
    }

    fn remove_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(self.run(move |connection| {
            let path = path.clone();
            async move {
                let session = &connection.session;
                let Err(error) = session.remove_dir(path.as_str()).await else {
                    return Ok(());
                };
                let error = sftp_error(&error, &path);
                if error.kind() == ErrorKind::Other
                    && session
                        .read_dir(path.as_str())
                        .await
                        .is_ok_and(|mut entries| {
                            entries.any(|item| item.file_name() != "." && item.file_name() != "..")
                        })
                {
                    return Err(Error::new(ErrorKind::DirectoryNotEmpty, path));
                }
                Err(error)
            }
        }))
    }

    fn rename(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        let (from, to) = (from.to_string(), to.to_string());
        Box::pin(self.run(move |connection| {
            let (from, to) = (from.clone(), to.clone());
            async move {
                let session = &connection.session;
                if session.try_exists(to.as_str()).await.unwrap_or(false) {
                    return Err(Error::new(ErrorKind::AlreadyExists, to));
                }
                session
                    .rename(from.as_str(), to.as_str())
                    .await
                    .map_err(|error| sftp_error(&error, &from))
            }
        }))
    }

    fn open_read(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Reader>> {
        let path = path.to_string();
        Box::pin(self.run(move |connection| {
            let path = path.clone();
            async move {
                let mut file = connection
                    .session
                    .open_with_flags(path.as_str(), OpenFlags::READ)
                    .await
                    .map_err(|error| sftp_error(&error, &path))?;
                if offset > 0 {
                    file.seek(SeekFrom::Start(offset))
                        .await
                        .map_err(|error| Error::from_io(&error, &path))?;
                }
                Ok(Box::new(file) as Reader)
            }
        }))
    }

    fn open_write(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Writer>> {
        let path = path.to_string();
        Box::pin(self.run(move |connection| {
            let path = path.clone();
            async move {
                let flags = if offset == 0 {
                    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE
                } else {
                    OpenFlags::WRITE
                };
                let mut file = connection
                    .session
                    .open_with_flags(path.as_str(), flags)
                    .await
                    .map_err(|error| sftp_error(&error, &path))?;
                if offset > 0 {
                    let attributes = FileAttributes {
                        size: Some(offset),
                        ..FileAttributes::empty()
                    };
                    file.set_metadata(attributes)
                        .await
                        .map_err(|error| sftp_error(&error, &path))?;
                    file.seek(SeekFrom::Start(offset))
                        .await
                        .map_err(|error| Error::from_io(&error, &path))?;
                }
                Ok(Box::new(file) as Writer)
            }
        }))
    }

    fn set_modified(&self, path: &str, modified: SystemTime) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        let seconds = modified
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u32::try_from(elapsed.as_secs()).ok());
        Box::pin(self.run(move |connection| {
            let path = path.clone();
            async move {
                let seconds =
                    seconds.ok_or_else(|| Error::new(ErrorKind::Other, format!("{path}: time")))?;
                let attributes = FileAttributes {
                    atime: Some(seconds),
                    mtime: Some(seconds),
                    ..FileAttributes::empty()
                };
                connection
                    .session
                    .set_metadata(path.as_str(), attributes)
                    .await
                    .map_err(|error| sftp_error(&error, &path))
            }
        }))
    }

    fn properties(&self, path: &str) -> BoxFuture<'_, Result<Properties>> {
        let path = path.to_string();
        Box::pin(self.run(move |connection| {
            let path = path.clone();
            async move {
                let session = &connection.session;
                let link = session
                    .symlink_metadata(path.as_str())
                    .await
                    .map_err(|error| sftp_error(&error, &path))?;
                let is_link = link.is_symlink();
                let attributes = if is_link {
                    session.metadata(path.as_str()).await.unwrap_or(link)
                } else {
                    link
                };
                Ok(properties_from(
                    file_name_posix(&path).to_string(),
                    &attributes,
                    is_link,
                ))
            }
        }))
    }

    fn can_set_permissions(&self) -> bool {
        true
    }

    fn set_permissions(&self, path: &str, mode: u32) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(self.run(move |connection| {
            let path = path.clone();
            async move {
                let attributes = FileAttributes {
                    permissions: Some(mode & PERMISSION_BITS),
                    ..FileAttributes::empty()
                };
                connection
                    .session
                    .set_metadata(path.as_str(), attributes)
                    .await
                    .map_err(|error| sftp_error(&error, &path))
            }
        }))
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let connection = Arc::clone(&*self.connection.lock().await);
            let _ = connection.session.close().await;
            connection.link.disconnect().await;
        })
    }

    fn checksum_algorithms(&self) -> BoxFuture<'_, Vec<HashAlgorithm>> {
        Box::pin(async {
            let Ok(connection) = self.current().await else {
                return Vec::new();
            };
            connection.hash_channel().await.algorithms()
        })
    }

    fn checksum(&self, path: &str, algorithm: HashAlgorithm) -> BoxFuture<'_, Result<Vec<u8>>> {
        let path = path.to_string();
        Box::pin(async move {
            let connection = self.current().await?;
            let channel = connection.hash_channel().await;
            let Some(session) = &channel.session else {
                return Err(crate::checksum::unsupported(&path));
            };
            let (request, data) = if channel.check_file.contains(&algorithm) {
                (CHECK_FILE, check_file_request(&path, algorithm))
            } else if algorithm == HashAlgorithm::Md5 && channel.md5_hash {
                (MD5_HASH, md5_hash_request(&path))
            } else {
                return Err(crate::checksum::unsupported(&path));
            };
            let reply = match session.extended(request, data).await {
                Ok(Packet::ExtendedReply(reply)) => reply.data,
                Ok(_) => return Err(crate::checksum::unsupported(&path)),
                Err(error) => return Err(sftp_error(&error, &path)),
            };
            let parsed = if request == CHECK_FILE {
                parse_check_file_reply(&reply, algorithm)
            } else {
                parse_md5_hash_reply(&reply)
            };
            parsed.ok_or_else(|| crate::checksum::unsupported(&path))
        })
    }

    fn join(&self, base: &str, name: &str) -> String {
        join_posix(base, name)
    }

    fn parent(&self, path: &str) -> Option<String> {
        parent_posix(path)
    }

    fn file_name<'a>(&self, path: &'a str) -> &'a str {
        file_name_posix(path)
    }
}

// Checksums

const CHECK_FILE: &str = "check-file-name";
const CHECK_FILE_SHORT: &str = "check-file";
const MD5_HASH: &str = "md5-hash";

impl Connection {
    async fn hash_channel(&self) -> &HashChannel {
        self.hashes
            .get_or_init(|| async {
                probe_hash_channel(&self.link.handle)
                    .await
                    .unwrap_or_default()
            })
            .await
    }
}

impl HashChannel {
    fn algorithms(&self) -> Vec<HashAlgorithm> {
        HashAlgorithm::PREFERENCE
            .into_iter()
            .filter(|algorithm| {
                self.check_file.contains(algorithm)
                    || (*algorithm == HashAlgorithm::Md5 && self.md5_hash)
            })
            .collect()
    }
}

async fn probe_hash_channel(handle: &Handle<HostKeyCheck>) -> Option<HashChannel> {
    let channel = handle.channel_open_session().await.ok()?;
    channel.request_subsystem(true, "sftp").await.ok()?;
    let session = RawSftpSession::new(channel.into_stream());
    let version = session.init().await.ok()?;
    let check_file = [CHECK_FILE, CHECK_FILE_SHORT]
        .into_iter()
        .find_map(|name| version.extensions.get(name))
        .map(|value| check_file_algorithms(value))
        .unwrap_or_default();
    let md5_hash = version.extensions.contains_key(MD5_HASH);
    if check_file.is_empty() && !md5_hash {
        drop(session);
        return None;
    }
    Some(HashChannel {
        session: Some(session),
        check_file,
        md5_hash,
    })
}

fn check_file_algorithms(value: &str) -> Vec<HashAlgorithm> {
    let named: Vec<HashAlgorithm> = value
        .split(',')
        .filter_map(HashAlgorithm::from_name)
        .collect();
    if named.is_empty() {
        HashAlgorithm::PREFERENCE.to_vec()
    } else {
        HashAlgorithm::PREFERENCE
            .into_iter()
            .filter(|algorithm| named.contains(algorithm))
            .collect()
    }
}

fn put_string(buffer: &mut Vec<u8>, value: &[u8]) {
    let length = u32::try_from(value.len()).unwrap_or(u32::MAX);
    buffer.extend_from_slice(&length.to_be_bytes());
    buffer.extend_from_slice(value);
}

fn take_string<'a>(data: &mut &'a [u8]) -> Option<&'a [u8]> {
    let length = usize::try_from(u32::from_be_bytes(data.get(..4)?.try_into().ok()?)).ok()?;
    let value = data.get(4..4 + length)?;
    *data = &data[4 + length..];
    Some(value)
}

fn check_file_request(path: &str, algorithm: HashAlgorithm) -> Vec<u8> {
    let mut data = Vec::new();
    put_string(&mut data, path.as_bytes());
    put_string(&mut data, algorithm.key().as_bytes());
    data.extend_from_slice(&0u64.to_be_bytes());
    data.extend_from_slice(&0u64.to_be_bytes());
    data.extend_from_slice(&0u32.to_be_bytes());
    data
}

fn parse_check_file_reply(reply: &[u8], algorithm: HashAlgorithm) -> Option<Vec<u8>> {
    let mut data = reply;
    let mut used = take_string(&mut data)?;
    if used == CHECK_FILE_SHORT.as_bytes() {
        used = take_string(&mut data)?;
    }
    let used = HashAlgorithm::from_name(std::str::from_utf8(used).ok()?)?;
    let expected = algorithm.digest(b"").len();
    (used == algorithm && data.len() >= expected).then(|| data[..expected].to_vec())
}

fn md5_hash_request(path: &str) -> Vec<u8> {
    let mut data = Vec::new();
    put_string(&mut data, path.as_bytes());
    data.extend_from_slice(&0u64.to_be_bytes());
    data.extend_from_slice(&0u64.to_be_bytes());
    put_string(&mut data, b"");
    data
}

fn parse_md5_hash_reply(reply: &[u8]) -> Option<Vec<u8>> {
    let mut data = reply;
    let mut hash = take_string(&mut data)?;
    if hash == MD5_HASH.as_bytes() {
        hash = take_string(&mut data)?;
    }
    (hash.len() == 16).then(|| hash.to_vec())
}

async fn list_directory(session: &SftpSession, path: &str) -> Result<Vec<Entry>> {
    let entries = session
        .read_dir(path)
        .await
        .map_err(|error| sftp_error(&error, path))?;
    let mut result = Vec::new();
    for item in entries {
        let name = item.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let attributes = item.metadata();
        let (attributes, is_link) = if attributes.is_symlink() {
            let target = join_posix(path, &name);
            let followed = session.metadata(target.as_str()).await.ok();
            (followed.unwrap_or(attributes), true)
        } else {
            (attributes, false)
        };
        result.push(entry_from(name, &attributes, is_link));
    }
    Ok(result)
}

impl fmt::Debug for SftpFileSystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SftpFileSystem")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

// Paths

pub(crate) fn join_posix(base: &str, name: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{base}/{name}")
}

pub(crate) fn file_name_posix(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

pub(crate) fn parent_posix(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(index) => Some(trimmed[..index].to_string()),
        None => None,
    }
}

// Conversion

fn entry_from(name: String, attributes: &FileAttributes, is_link: bool) -> Entry {
    let is_directory = attributes.is_dir();
    Entry {
        name,
        kind: if is_directory {
            EntryKind::Directory
        } else {
            EntryKind::File
        },
        size: attributes.size.filter(|_| !is_directory),
        modified: attributes
            .mtime
            .map(|seconds| UNIX_EPOCH + Duration::from_secs(u64::from(seconds))),
        is_link,
        permissions: attributes
            .permissions
            .map(|permissions| permissions & PERMISSION_BITS),
        owner: attributes
            .user
            .clone()
            .or_else(|| attributes.uid.map(|uid| uid.to_string())),
    }
}

fn properties_from(name: String, attributes: &FileAttributes, is_link: bool) -> Properties {
    let entry = entry_from(name, attributes, is_link);
    let mut properties = Properties::from_entry(entry.clone());
    properties.permissions = entry.permissions;
    properties.owner = entry.owner;
    properties.group = attributes
        .group
        .clone()
        .or_else(|| attributes.gid.map(|gid| gid.to_string()));
    properties
}

fn sftp_error(error: &SftpError, path: &str) -> Error {
    match error {
        SftpError::Status(status) => {
            let kind = match status.status_code {
                StatusCode::NoSuchFile => ErrorKind::NotFound,
                StatusCode::PermissionDenied => ErrorKind::PermissionDenied,
                _ => ErrorKind::Other,
            };
            if kind == ErrorKind::Other {
                Error::new(kind, format!("{path}: {error}"))
            } else {
                Error::new(kind, path)
            }
        }
        SftpError::Timeout | SftpError::IO(_) | SftpError::UnexpectedBehavior(_) => {
            Error::new(ErrorKind::ConnectionFailed, format!("{path}: {error}"))
        }
        _ => Error::new(ErrorKind::Other, format!("{path}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checksums

    #[test]
    fn check_file_requests_and_replies_follow_the_draft() {
        let request = check_file_request("/a", HashAlgorithm::Sha256);
        assert_eq!(&request[..4], &2u32.to_be_bytes());
        assert_eq!(&request[4..6], b"/a");
        assert_eq!(&request[10..16], b"sha256");
        assert_eq!(request.len(), 6 + 4 + 6 + 8 + 8 + 4);

        let digest = HashAlgorithm::Sha1.digest(b"abc");
        let mut reply = Vec::new();
        put_string(&mut reply, b"sha1");
        reply.extend_from_slice(&digest);
        assert_eq!(
            parse_check_file_reply(&reply, HashAlgorithm::Sha1),
            Some(digest.clone())
        );
        assert_eq!(parse_check_file_reply(&reply, HashAlgorithm::Md5), None);
        let mut prefixed = Vec::new();
        put_string(&mut prefixed, b"check-file");
        prefixed.extend_from_slice(&reply);
        assert_eq!(
            parse_check_file_reply(&prefixed, HashAlgorithm::Sha1),
            Some(digest)
        );
        assert_eq!(parse_check_file_reply(&[0, 0], HashAlgorithm::Sha1), None);
        assert_eq!(
            check_file_algorithms("md5,sha256,whirlpool"),
            [HashAlgorithm::Sha256, HashAlgorithm::Md5]
        );
        assert_eq!(check_file_algorithms("1").len(), 4);
    }

    #[test]
    fn md5_hash_requests_and_replies_follow_the_draft() {
        let request = md5_hash_request("/f");
        assert_eq!(request.len(), 4 + 2 + 8 + 8 + 4);
        let digest = HashAlgorithm::Md5.digest(b"abc");
        let mut reply = Vec::new();
        put_string(&mut reply, &digest);
        assert_eq!(parse_md5_hash_reply(&reply), Some(digest));
        let mut empty = Vec::new();
        put_string(&mut empty, b"");
        assert_eq!(parse_md5_hash_reply(&empty), None);
    }

    // Paths

    #[test]
    fn posix_paths_join_and_split() {
        assert_eq!(join_posix("/", "etc"), "/etc");
        assert_eq!(join_posix("/home/demo/", "file"), "/home/demo/file");
        assert_eq!(parent_posix("/home/demo"), Some("/home".to_string()));
        assert_eq!(parent_posix("/home/"), Some("/".to_string()));
        assert_eq!(parent_posix("/"), None);
        assert_eq!(parent_posix("relative"), None);
    }

    #[test]
    fn attributes_give_permissions_and_owners() {
        let attributes = FileAttributes {
            size: Some(12),
            uid: Some(1000),
            gid: Some(33),
            group: Some("www-data".to_string()),
            permissions: Some(0o100_640),
            ..FileAttributes::empty()
        };
        let properties = properties_from("index.html".to_string(), &attributes, false);
        assert_eq!(properties.entry.size, Some(12));
        assert_eq!(properties.permissions, Some(0o640));
        assert_eq!(properties.owner.as_deref(), Some("1000"));
        assert_eq!(properties.group.as_deref(), Some("www-data"));
        let bare = properties_from("x".to_string(), &FileAttributes::empty(), false);
        assert_eq!(
            (bare.permissions, bare.owner, bare.group),
            (None, None, None)
        );
    }

    #[test]
    fn status_codes_map_to_error_kinds() {
        let status = |status_code| {
            SftpError::Status(russh_sftp::protocol::Status {
                id: 0,
                status_code,
                error_message: "message".to_string(),
                language_tag: "en".to_string(),
            })
        };
        assert_eq!(
            sftp_error(&status(StatusCode::NoSuchFile), "/x").kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            sftp_error(&status(StatusCode::PermissionDenied), "/x").kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(
            sftp_error(&status(StatusCode::Failure), "/x").kind(),
            ErrorKind::Other
        );
        assert_eq!(
            sftp_error(&SftpError::Timeout, "/x").kind(),
            ErrorKind::ConnectionFailed
        );
    }
}
