use std::fmt;
use std::future::Future;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use russh::client::{self, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use tokio::io::{AsyncRead, AsyncSeekExt, AsyncWrite};
use tokio::sync::Mutex as AsyncMutex;

use crate::connection::{Authentication, ConnectionParams, HostKeyTrust};
use crate::file_system::{Reader, Writer};
use crate::route::Route;
use crate::{BoxFuture, Entry, EntryKind, Error, ErrorKind, FileSystem, Result, proxy, ssh_agent};

// Constants

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

// Reconnecting

static AUTO_RECONNECT: AtomicBool = AtomicBool::new(true);

pub fn set_auto_reconnect(enabled: bool) {
    AUTO_RECONNECT.store(enabled, Ordering::Relaxed);
}

pub fn auto_reconnect() -> bool {
    AUTO_RECONNECT.load(Ordering::Relaxed)
}

// Types

pub struct SftpFileSystem {
    params: ConnectionParams,
    route: Route,
    known_hosts: PathBuf,
    host_fingerprints: Vec<String>,
    connection: AsyncMutex<Arc<Connection>>,
    label: String,
}

struct Connection {
    session: SftpSession,
    handle: Handle<HostKeyCheck>,
    jumps: Vec<Handle<HostKeyCheck>>,
}

struct HostKeyCheck {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    trusted: Vec<HostKeyTrust>,
    rejection: Arc<Mutex<Option<Error>>>,
    accepted: Arc<Mutex<Option<String>>>,
}

// Host keys

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

impl HostKeyCheck {
    fn verify(&self, key: &PublicKey) -> std::result::Result<(), Error> {
        let fingerprint = fingerprint(key);
        match russh::keys::check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(()),
            Err(russh::keys::Error::KeyChanged { .. }) => {
                Err(Error::new(ErrorKind::HostKeyChanged, fingerprint))
            }
            Err(error) => Err(Error::new(ErrorKind::Other, error.to_string())),
            Ok(false) => match self
                .trusted
                .iter()
                .find(|trust| trust.fingerprint == fingerprint)
            {
                Some(trust) => {
                    if trust.remember {
                        russh::keys::known_hosts::learn_known_hosts_path(
                            &self.host,
                            self.port,
                            key,
                            &self.known_hosts,
                        )
                        .map_err(|error| Error::new(ErrorKind::Other, error.to_string()))?;
                    }
                    Ok(())
                }
                None => Err(Error::new(ErrorKind::UnknownHostKey, fingerprint)),
            },
        }
    }
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let key = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(certificate) => {
                PublicKey::from(certificate.public_key().clone())
            }
        };
        match self.verify(&key) {
            Ok(()) => {
                if let Ok(mut accepted) = self.accepted.lock() {
                    *accepted = Some(fingerprint(&key));
                }
                Ok(true)
            }
            Err(error) => {
                if let Ok(mut rejection) = self.rejection.lock() {
                    *rejection = Some(error);
                }
                Ok(false)
            }
        }
    }
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
        let (connection, host_fingerprints) =
            establish(params, route, known_hosts, trusted.to_vec()).await?;
        Ok(Self {
            params: params.clone(),
            route: route.clone(),
            known_hosts: known_hosts.to_path_buf(),
            host_fingerprints,
            connection: AsyncMutex::new(Arc::new(connection)),
            label: params.label(),
        })
    }

    async fn current(&self) -> Result<Arc<Connection>> {
        let mut slot = self.connection.lock().await;
        if slot.handle.is_closed() {
            if !auto_reconnect() {
                return Err(Error::new(
                    ErrorKind::ConnectionFailed,
                    self.params.host.clone(),
                ));
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
        let trusted = self
            .host_fingerprints
            .iter()
            .map(|fingerprint| HostKeyTrust {
                fingerprint: fingerprint.clone(),
                remember: false,
            })
            .collect();
        establish(&self.params, &self.route, &self.known_hosts, trusted)
            .await
            .map(|(connection, _)| connection)
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

async fn establish(
    params: &ConnectionParams,
    route: &Route,
    known_hosts: &Path,
    trusted: Vec<HostKeyTrust>,
) -> Result<(Connection, Vec<String>)> {
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        open_connection(params, route, known_hosts, trusted),
    )
    .await
    .map_err(|_| Error::new(ErrorKind::ConnectionFailed, params.host.clone()))?
}

struct Hop<'a> {
    host: &'a str,
    port: u16,
    username: &'a str,
    authentication: &'a Authentication,
    label: String,
    is_target: bool,
}

type Slot<T> = Arc<Mutex<Option<T>>>;

fn hops<'a>(params: &'a ConnectionParams, route: &'a Route) -> Vec<Hop<'a>> {
    let mut hops: Vec<Hop<'a>> = route
        .jump_hosts
        .iter()
        .map(|jump| Hop {
            host: &jump.host,
            port: jump.port,
            username: &jump.username,
            authentication: &jump.authentication,
            label: jump.label(),
            is_target: false,
        })
        .collect();
    hops.push(Hop {
        host: &params.host,
        port: params.port,
        username: &params.username,
        authentication: &params.authentication,
        label: params.label(),
        is_target: true,
    });
    hops
}

async fn open_connection(
    params: &ConnectionParams,
    route: &Route,
    known_hosts: &Path,
    trusted: Vec<HostKeyTrust>,
) -> Result<(Connection, Vec<String>)> {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(INACTIVITY_TIMEOUT),
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        ..client::Config::default()
    });
    let mut fingerprints = Vec::new();
    let mut jumps: Vec<Handle<HostKeyCheck>> = Vec::new();
    let mut target = None;
    for hop in hops(params, route) {
        let (check, rejection, accepted) = host_key_check(&hop, known_hosts, &trusted);
        let connected = match jumps.last() {
            None => {
                let stream = proxy::open_tcp(route.proxy.as_ref(), hop.host, hop.port)
                    .await
                    .map_err(|error| at_hop(error, &hop))?;
                start_session(&config, stream, check, &hop, &rejection).await
            }
            Some(previous) => {
                let channel = previous
                    .channel_open_direct_tcpip(hop.host, u32::from(hop.port), "127.0.0.1", 0)
                    .await
                    .map_err(|error| at_hop(connection_error(hop.host, &error), &hop))?;
                start_session(&config, channel.into_stream(), check, &hop, &rejection).await
            }
        };
        let mut handle = connected?;
        if let Some(fingerprint) = accepted.lock().ok().and_then(|mut slot| slot.take()) {
            fingerprints.push(fingerprint);
        }
        authenticate(&mut handle, &hop)
            .await
            .map_err(|error| at_hop(error, &hop))?;
        if hop.is_target {
            target = Some(handle);
        } else {
            jumps.push(handle);
        }
    }
    let Some(handle) = target else {
        return Err(Error::new(ErrorKind::ConnectionFailed, params.host.clone()));
    };
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|error| connection_error(&params.host, &error))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|error| connection_error(&params.host, &error))?;
    let session = SftpSession::new(channel.into_stream())
        .await
        .map_err(|error| sftp_error(&error, &params.host))?;
    Ok((
        Connection {
            session,
            handle,
            jumps,
        },
        fingerprints,
    ))
}

fn host_key_check(
    hop: &Hop<'_>,
    known_hosts: &Path,
    trusted: &[HostKeyTrust],
) -> (HostKeyCheck, Slot<Error>, Slot<String>) {
    let rejection = Arc::new(Mutex::new(None));
    let accepted = Arc::new(Mutex::new(None));
    let check = HostKeyCheck {
        host: hop.host.to_string(),
        port: hop.port,
        known_hosts: known_hosts.to_path_buf(),
        trusted: trusted.to_vec(),
        rejection: Arc::clone(&rejection),
        accepted: Arc::clone(&accepted),
    };
    (check, rejection, accepted)
}

async fn start_session<S>(
    config: &Arc<client::Config>,
    stream: S,
    check: HostKeyCheck,
    hop: &Hop<'_>,
    rejection: &Slot<Error>,
) -> Result<Handle<HostKeyCheck>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    client::connect_stream(Arc::clone(config), stream, check)
        .await
        .map_err(|error| {
            let rejected = rejection.lock().ok().and_then(|mut slot| slot.take());
            at_hop(
                rejected.unwrap_or_else(|| connection_error(hop.host, &error)),
                hop,
            )
        })
}

fn at_hop(error: Error, hop: &Hop<'_>) -> Error {
    if hop.is_target || error.hop().is_some() {
        error
    } else {
        error.at_hop(hop.label.clone())
    }
}

async fn authenticate(handle: &mut Handle<HostKeyCheck>, hop: &Hop<'_>) -> Result<()> {
    let user = hop.username.to_string();
    let result = match hop.authentication {
        Authentication::Password(password) => handle
            .authenticate_password(user, password.clone())
            .await
            .map_err(|error| connection_error(hop.host, &error))?,
        Authentication::KeyFile { path, passphrase } => {
            let key =
                russh::keys::load_secret_key(path, passphrase.as_deref()).map_err(|error| {
                    Error::new(
                        ErrorKind::InvalidKey,
                        format!("{}: {error}", path.display()),
                    )
                })?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten();
            handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|error| connection_error(hop.host, &error))?
        }
        Authentication::Agent => {
            return ssh_agent::authenticate(handle, &user, &hop.label).await;
        }
    };
    if result.success() {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::AuthenticationFailed,
            hop.label.clone(),
        ))
    }
}

// Interface

impl FileSystem for SftpFileSystem {
    fn label(&self) -> Option<String> {
        Some(self.label.clone())
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

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let connection = Arc::clone(&*self.connection.lock().await);
            let _ = connection.session.close().await;
            let _ = connection
                .handle
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            for jump in connection.jumps.iter().rev() {
                let _ = jump
                    .disconnect(russh::Disconnect::ByApplication, "", "en")
                    .await;
            }
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

fn file_name_posix(path: &str) -> &str {
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
    }
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

fn connection_error(host: &str, error: &russh::Error) -> Error {
    Error::new(ErrorKind::ConnectionFailed, format!("{host}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
