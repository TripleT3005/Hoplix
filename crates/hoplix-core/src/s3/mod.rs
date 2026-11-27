mod client;
mod http;
mod settings;
mod sigv4;
mod time;
mod upload;
mod xml;

#[cfg(test)]
mod tests;

use std::fmt;
use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::io::AsyncReadExt;
use tokio_rustls::TlsConnector;

pub use settings::{S3_DEFAULT_REGION, S3Options, is_valid_bucket};

use client::{Call, Client, ObjectInfo, Target};
use http::{Connector, Stream};
use sigv4::Credentials;
use upload::{ObjectWriter, etag_digest};

use crate::connection::{Authentication, ConnectionParams, HostKeyTrust, encode_url_path};
use crate::proxy::{Proxy, open_tcp};
use crate::sftp::{join_posix, parent_posix};
use crate::{
    BoxFuture, ConnectionOptions, Entry, EntryKind, Error, ErrorKind, FileSystem, HashAlgorithm,
    Reader, Result, Writer, certificates,
};

// Constants

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DELETE_BATCH: usize = 1000;
const MAX_COPY: u64 = 5 * 1024 * 1024 * 1024;

// Types

pub struct S3FileSystem {
    client: Arc<Client>,
    label: String,
}

type Rejection = Arc<Mutex<Option<Error>>>;

// Connecting

impl S3FileSystem {
    pub async fn connect(
        params: &ConnectionParams,
        proxy: Option<&Proxy>,
        options: &ConnectionOptions,
        certificates: &Path,
        trust: Option<HostKeyTrust>,
    ) -> Result<Self> {
        let settings = &options.s3;
        let target = target_for(params, settings)?;
        let Authentication::Password(secret) = &params.authentication else {
            return Err(Error::new(
                ErrorKind::AuthenticationFailed,
                format!(
                    "{}: S3 needs an access key and a secret",
                    label_for(&target)
                ),
            ));
        };
        let credentials = Credentials {
            access_key_id: params.username.trim().to_string(),
            secret_access_key: secret.clone(),
            session_token: settings
                .session_token
                .clone()
                .filter(|token| !token.is_empty()),
        };
        let timeout = options.timeout_or(CONNECT_TIMEOUT);
        let host = target.request_host();
        let tls = if target.https {
            let rejection: Rejection = Arc::new(Mutex::new(None));
            let config = certificates::client_config(
                &host,
                target.port,
                certificates,
                trust,
                Arc::clone(&rejection),
            )?;
            Some((config, rejection))
        } else {
            None
        };
        let connector = connector(host, target.port, proxy.cloned(), tls, timeout);
        let file_system = Self::with_connector(target, credentials, connector, timeout);
        file_system
            .client
            .list_page("", false, None, Some(1))
            .await?;
        Ok(file_system)
    }

    fn with_connector(
        target: Target,
        credentials: Credentials,
        connector: Connector,
        timeout: Duration,
    ) -> Self {
        let label = label_for(&target);
        Self {
            client: Arc::new(Client::new(
                target,
                credentials,
                connector,
                timeout,
                label.clone(),
            )),
            label,
        }
    }
}

fn target_for(params: &ConnectionParams, settings: &S3Options) -> Result<Target> {
    let bucket = settings.bucket.trim().to_string();
    if !is_valid_bucket(&bucket) {
        return Err(Error::new(
            ErrorKind::Other,
            format!("invalid bucket name: {bucket}"),
        ));
    }
    let host = match params.host.trim() {
        "" => settings.default_endpoint(),
        host => host.trim_matches(['[', ']']).to_ascii_lowercase(),
    };
    let https = !settings.plain_http;
    let numeric = host.parse::<IpAddr>().is_ok() || host == "localhost";
    let path_style = settings.path_style || numeric || (https && bucket.contains('.'));
    Ok(Target {
        host,
        port: params.port,
        https,
        bucket,
        path_style,
        region: settings.region_or_default().to_string(),
    })
}

fn label_for(target: &Target) -> String {
    let default = if target.https { 443 } else { 80 };
    if target.port == default {
        format!("{} @ {}", target.bucket, target.host)
    } else {
        format!("{} @ {}:{}", target.bucket, target.host, target.port)
    }
}

fn connector(
    host: String,
    port: u16,
    proxy: Option<Proxy>,
    tls: Option<(Arc<ClientConfig>, Rejection)>,
    timeout: Duration,
) -> Connector {
    Arc::new(move || {
        let host = host.clone();
        let proxy = proxy.clone();
        let tls = tls.clone();
        Box::pin(async move {
            let timed_out =
                || Error::new(ErrorKind::ConnectionFailed, format!("{host}: timed out"));
            let tcp = tokio::time::timeout(timeout, open_tcp(proxy.as_ref(), &host, port))
                .await
                .map_err(|_| timed_out())??;
            let _ = tcp.set_nodelay(true);
            let Some((config, rejection)) = tls else {
                return Ok(Box::new(tcp) as Stream);
            };
            let name = ServerName::try_from(host.clone()).map_err(|_| {
                Error::new(ErrorKind::ConnectionFailed, format!("invalid host: {host}"))
            })?;
            match tokio::time::timeout(timeout, TlsConnector::from(config).connect(name, tcp)).await
            {
                Ok(Ok(stream)) => Ok(Box::new(stream) as Stream),
                Ok(Err(error)) => Err(rejection
                    .lock()
                    .ok()
                    .and_then(|mut rejected| rejected.take())
                    .unwrap_or_else(|| {
                        Error::new(ErrorKind::ConnectionFailed, format!("{host}: {error}"))
                    })),
                Err(_) => Err(timed_out()),
            }
        })
    })
}

// Paths

fn key_of(path: &str) -> &str {
    path.trim_start_matches('/').trim_end_matches('/')
}

fn prefix_of(path: &str) -> String {
    let key = key_of(path);
    if key.is_empty() {
        String::new()
    } else {
        format!("{key}/")
    }
}

fn entry(name: &str, object: Option<&ObjectInfo>) -> Entry {
    Entry {
        name: name.to_string(),
        kind: if object.is_some() {
            EntryKind::File
        } else {
            EntryKind::Directory
        },
        size: object.map(|object| object.size),
        modified: object.and_then(|object| object.modified),
        is_link: false,
        permissions: None,
        owner: None,
    }
}

fn file_entry(name: &str, object: &ObjectInfo) -> Entry {
    entry(name, Some(object))
}

fn directory_entry(name: &str) -> Entry {
    entry(name, None)
}

fn not_found(path: &str) -> Error {
    Error::new(ErrorKind::NotFound, path.to_string())
}

// Operations

impl S3FileSystem {
    async fn entry(&self, path: &str) -> Result<Entry> {
        let key = key_of(path);
        let name = self.file_name(path).to_string();
        if key.is_empty() {
            return Ok(directory_entry("/"));
        }
        if let Some(object) = self.client.head(key).await? {
            return Ok(file_entry(&name, &object));
        }
        let page = self
            .client
            .list_page(&prefix_of(path), false, None, Some(1))
            .await?;
        if page.objects.is_empty() && page.prefixes.is_empty() {
            return Err(not_found(path));
        }
        Ok(directory_entry(&name))
    }

    async fn entries(&self, path: &str) -> Result<Vec<Entry>> {
        let prefix = prefix_of(path);
        let page = self.client.list_all(&prefix, true).await?;
        let mut marker = false;
        let mut entries = Vec::new();
        for object in &page.objects {
            let Some(name) = object.key.strip_prefix(&prefix) else {
                continue;
            };
            if name.is_empty() {
                marker = true;
            } else if !name.contains('/') {
                entries.push(file_entry(name, object));
            }
        }
        for folder in &page.prefixes {
            let name = folder
                .strip_prefix(&prefix)
                .unwrap_or_default()
                .trim_end_matches('/');
            if !name.is_empty() && !name.contains('/') {
                entries.push(directory_entry(name));
            }
        }
        if entries.is_empty() && !marker && !prefix.is_empty() {
            return Err(match self.client.head(key_of(path)).await? {
                Some(_) => Error::new(ErrorKind::NotADirectory, path.to_string()),
                None => not_found(path),
            });
        }
        Ok(entries)
    }

    async fn make_directory(&self, path: &str) -> Result<()> {
        let prefix = prefix_of(path);
        if prefix.is_empty() {
            return Err(Error::new(ErrorKind::AlreadyExists, path.to_string()));
        }
        match self.entry(path).await {
            Ok(_) => return Err(Error::new(ErrorKind::AlreadyExists, path.to_string())),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.client.put(&prefix, Vec::new()).await.map(|_| ())
    }

    async fn delete_file(&self, path: &str) -> Result<()> {
        let key = key_of(path);
        if key.is_empty() || self.client.head(key).await?.is_none() {
            return Err(not_found(path));
        }
        self.client.delete(key).await
    }

    async fn delete_directory(&self, path: &str) -> Result<()> {
        let prefix = prefix_of(path);
        if prefix.is_empty() {
            return Err(Error::new(ErrorKind::PermissionDenied, path.to_string()));
        }
        let page = self.client.list_page(&prefix, false, None, Some(2)).await?;
        if page.objects.iter().any(|object| object.key != prefix) {
            return Err(Error::new(ErrorKind::DirectoryNotEmpty, path.to_string()));
        }
        if page.objects.is_empty() {
            return Err(not_found(path));
        }
        self.client.delete(&prefix).await
    }

    async fn delete_tree(&self, path: &str) -> Result<()> {
        let entry = self.entry(path).await?;
        if !entry.is_directory() {
            return self.delete_file(path).await;
        }
        let prefix = prefix_of(path);
        if prefix.is_empty() {
            return Err(Error::new(ErrorKind::PermissionDenied, path.to_string()));
        }
        let keys: Vec<String> = self
            .client
            .list_all(&prefix, false)
            .await?
            .objects
            .into_iter()
            .map(|object| object.key)
            .collect();
        for batch in keys.chunks(DELETE_BATCH) {
            self.client.delete_many(batch).await?;
        }
        Ok(())
    }

    async fn copy_object(&self, from: &str, to: &str, size: u64) -> Result<()> {
        if size <= MAX_COPY {
            return self.client.copy(from, to).await;
        }
        upload::copy_large(Arc::clone(&self.client), from, to, size).await
    }

    async fn move_entry(&self, from: &str, to: &str) -> Result<()> {
        let (source, target) = (key_of(from), key_of(to));
        if source.is_empty() || target.is_empty() {
            return Err(Error::new(ErrorKind::PermissionDenied, from.to_string()));
        }
        if source == target {
            return Ok(());
        }
        match self.entry(to).await {
            Ok(_) => return Err(Error::new(ErrorKind::AlreadyExists, to.to_string())),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let entry = self.entry(from).await?;
        if !entry.is_directory() {
            self.copy_object(source, target, entry.size.unwrap_or(0))
                .await?;
            return self.client.delete(source).await;
        }
        let (source_prefix, target_prefix) = (prefix_of(from), prefix_of(to));
        if target_prefix.starts_with(&source_prefix) {
            return Err(Error::new(
                ErrorKind::Other,
                format!("{to}: a folder cannot be moved into itself"),
            ));
        }
        let objects = self.client.list_all(&source_prefix, false).await?.objects;
        for object in &objects {
            let rest = object.key.strip_prefix(&source_prefix).unwrap_or_default();
            self.copy_object(&object.key, &format!("{target_prefix}{rest}"), object.size)
                .await?;
        }
        let keys: Vec<String> = objects.into_iter().map(|object| object.key).collect();
        for batch in keys.chunks(DELETE_BATCH) {
            self.client.delete_many(batch).await?;
        }
        Ok(())
    }

    async fn reader(&self, path: &str, offset: u64) -> Result<Reader> {
        let key = key_of(path);
        if key.is_empty() {
            return Err(Error::new(ErrorKind::NotADirectory, path.to_string()));
        }
        let mut call = Call::new("GET", Some(key));
        if offset > 0 {
            call = call.header("Range", &format!("bytes={offset}-"));
        }
        let (head, mut body) = self.client.open(&call).await?;
        if offset > 0 && head.status == 200 {
            let mut skipped = (&mut body).take(offset);
            tokio::io::copy(&mut skipped, &mut tokio::io::sink())
                .await
                .map_err(|error| Error::from_io(&error, path))?;
        }
        Ok(Box::new(body))
    }

    async fn writer(&self, path: &str, offset: u64) -> Result<Writer> {
        let key = key_of(path);
        if key.is_empty() {
            return Err(Error::new(ErrorKind::PermissionDenied, path.to_string()));
        }
        let writer = ObjectWriter::open(Arc::clone(&self.client), key, offset).await?;
        Ok(Box::new(writer))
    }

    async fn md5(&self, path: &str) -> Result<Vec<u8>> {
        let object = self
            .client
            .head(key_of(path))
            .await?
            .ok_or_else(|| not_found(path))?;
        object
            .comparable
            .as_deref()
            .and_then(etag_digest)
            .ok_or_else(|| crate::checksum::unsupported(path))
    }
}

impl fmt::Debug for S3FileSystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("S3FileSystem")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl FileSystem for S3FileSystem {
    fn label(&self) -> Option<String> {
        Some(self.label.clone())
    }

    fn url(&self, path: &str) -> Option<String> {
        let path = if path.starts_with('/') {
            encode_url_path(path)
        } else {
            format!("/{}", encode_url_path(path))
        };
        Some(format!("s3://{}{path}", self.client.target().bucket))
    }

    fn home(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async { Ok("/".to_string()) })
    }

    fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
        let path = path.to_string();
        Box::pin(async move { self.entries(&path).await })
    }

    fn metadata(&self, path: &str) -> BoxFuture<'_, Result<Entry>> {
        let path = path.to_string();
        Box::pin(async move { self.entry(&path).await })
    }

    fn create_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move { self.make_directory(&path).await })
    }

    fn remove_file(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move { self.delete_file(&path).await })
    }

    fn remove_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move { self.delete_directory(&path).await })
    }

    fn remove_tree(&self, path: &str) -> Option<BoxFuture<'_, Result<()>>> {
        let path = path.to_string();
        Some(Box::pin(async move { self.delete_tree(&path).await }))
    }

    fn rename(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        let (from, to) = (from.to_string(), to.to_string());
        Box::pin(async move { self.move_entry(&from, &to).await })
    }

    fn open_read(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Reader>> {
        let path = path.to_string();
        Box::pin(async move { self.reader(&path, offset).await })
    }

    fn open_write(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Writer>> {
        let path = path.to_string();
        Box::pin(async move { self.writer(&path, offset).await })
    }

    fn checksum_algorithms(&self) -> BoxFuture<'_, Vec<HashAlgorithm>> {
        Box::pin(async { vec![HashAlgorithm::Md5] })
    }

    fn checksum(&self, path: &str, algorithm: HashAlgorithm) -> BoxFuture<'_, Result<Vec<u8>>> {
        let path = path.to_string();
        Box::pin(async move {
            if algorithm == HashAlgorithm::Md5 {
                self.md5(&path).await
            } else {
                Err(crate::checksum::unsupported(&path))
            }
        })
    }

    fn join(&self, base: &str, name: &str) -> String {
        join_posix(base, name)
    }

    fn parent(&self, path: &str) -> Option<String> {
        parent_posix(path)
    }
}
