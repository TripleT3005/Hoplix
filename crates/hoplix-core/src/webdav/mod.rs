mod date;
mod xml;

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::connection::{Authentication, ConnectionParams, HostKeyTrust, Protocol};
use crate::file_system::{Reader, Writer};
use crate::http::{Client, Credentials, Endpoint, Head, Request, Response};
use crate::options::ConnectionOptions;
use crate::proxy::Proxy;
use crate::sftp::{join_posix, parent_posix};
use crate::{BoxFuture, Entry, EntryKind, Error, ErrorKind, FileSystem, Result, certificates};

use xml::Resource;

// Constants

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_LISTING: usize = 64 * 1024 * 1024;
const XML_TYPE: &str = "application/xml; charset=utf-8";
const PROPFIND: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<d:propfind xmlns:d=\"DAV:\"><d:prop><d:resourcetype/><d:getcontentlength/>\
<d:getlastmodified/><d:displayname/><d:getetag/></d:prop></d:propfind>";
const PROBE: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<d:propfind xmlns:d=\"DAV:\" xmlns:oc=\"http://owncloud.org/ns\"><d:prop><d:resourcetype/>\
<d:getlastmodified/><oc:id/></d:prop></d:propfind>";

// Types

pub struct WebDavFileSystem {
    client: Arc<Client>,
    params: ConnectionParams,
    base: String,
    owncloud: bool,
}

// Connecting

impl WebDavFileSystem {
    pub async fn connect(
        params: &ConnectionParams,
        proxy: Option<&Proxy>,
        options: &ConnectionOptions,
        certificates: &Path,
        trust: Option<HostKeyTrust>,
    ) -> Result<Self> {
        let password = match &params.authentication {
            Authentication::Password(password) => password.clone(),
            Authentication::KeyFile { path, .. } => {
                return Err(Error::new(
                    ErrorKind::InvalidKey,
                    format!("{}: WebDAV does not support key files", path.display()),
                ));
            }
            Authentication::Agent => {
                return Err(Error::new(
                    ErrorKind::AuthenticationFailed,
                    format!("{}: WebDAV does not support SSH agents", params.label()),
                ));
            }
        };
        let rejection = Arc::new(StdMutex::new(None));
        let tls = if params.protocol == Protocol::WebDavs {
            let config = certificates::client_config(
                &params.host,
                params.port,
                certificates,
                trust,
                Arc::clone(&rejection),
            )?;
            let mut config = (*config).clone();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            Some(Arc::new(config))
        } else {
            None
        };
        let endpoint = Endpoint {
            host: params.host.clone(),
            port: params.port,
            tls,
            proxy: proxy.cloned(),
            rejection,
        };
        let client = Client::new(
            endpoint,
            Some(Credentials {
                username: params.username.clone(),
                password,
            }),
            true,
            options.timeout_or(CONNECT_TIMEOUT),
            crate::update::user_agent(),
            params.label(),
        );
        let mut file_system = Self {
            client: Arc::new(client),
            params: params.clone(),
            base: normalize_base(&options.base_path),
            owncloud: false,
        };
        let root = file_system.base.clone();
        let probe = file_system
            .propfind(&file_system.collection_target("/"), "0", PROBE, &root)
            .await;
        let resources = match probe {
            Err(error) if root == "/" && is_unlisted_root(&error) => return Ok(file_system),
            probe => probe?,
        };
        let first = resources
            .first()
            .ok_or_else(|| Error::new(ErrorKind::NotFound, root.clone()))?;
        if !first.collection {
            return Err(Error::new(ErrorKind::NotADirectory, root));
        }
        file_system.owncloud = first.owncloud;
        Ok(file_system)
    }

    // Paths

    fn full_path(&self, path: &str) -> String {
        format!("{}{}", self.base, path.trim_start_matches('/'))
    }

    fn target(&self, path: &str) -> String {
        let full = self.full_path(path);
        let trimmed = full.trim_end_matches('/');
        encode_path(if trimmed.is_empty() { "/" } else { trimmed })
    }

    fn collection_target(&self, path: &str) -> String {
        let mut full = self.full_path(path);
        if !full.ends_with('/') {
            full.push('/');
        }
        encode_path(&full)
    }

    fn relative(&self, href: &str) -> Option<String> {
        let path = if href.starts_with('/') {
            href
        } else if let Some((_, rest)) = href.split_once("://") {
            &rest[rest.find('/').unwrap_or(rest.len())..]
        } else {
            return None;
        };
        let decoded = decode_path(path.split(['?', '#']).next().unwrap_or_default());
        let base = self.base.trim_end_matches('/');
        let rest = decoded.strip_prefix(base)?;
        if !rest.is_empty() && !rest.starts_with('/') {
            return None;
        }
        let rest = rest.trim_end_matches('/');
        Some(if rest.is_empty() {
            "/".to_string()
        } else {
            rest.to_string()
        })
    }

    // Requests

    async fn send(
        &self,
        method: &str,
        target: String,
        headers: Vec<(&'static str, String)>,
        body: &[u8],
    ) -> Result<Response> {
        self.client
            .send(Request {
                method,
                target,
                headers,
                body,
            })
            .await
    }

    async fn propfind(
        &self,
        target: &str,
        depth: &str,
        body: &str,
        path: &str,
    ) -> Result<Vec<Resource>> {
        let headers = vec![
            ("Depth", depth.to_string()),
            ("Content-Type", XML_TYPE.to_string()),
        ];
        let response = self
            .send("PROPFIND", target.to_string(), headers, body.as_bytes())
            .await?;
        if response.head.status != 207 {
            let status = response.head.status;
            response.discard().await;
            return Err(status_error(status, path));
        }
        let data = response.bytes(MAX_LISTING).await?;
        xml::parse_multistatus(&data)
    }

    async fn simple(
        &self,
        method: &str,
        target: String,
        headers: Vec<(&'static str, String)>,
        path: &str,
    ) -> Result<u16> {
        let response = self.send(method, target, headers, &[]).await?;
        let status = response.head.status;
        response.discard().await;
        if (200..300).contains(&status) && status != 207 {
            Ok(status)
        } else {
            Err(status_error(status, path))
        }
    }

    fn entry(&self, name: String, resource: &Resource) -> Entry {
        Entry {
            name,
            kind: if resource.collection {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            size: (!resource.collection).then_some(resource.length).flatten(),
            modified: resource.modified,
            is_link: false,
            permissions: None,
            owner: None,
        }
    }

    async fn read(&self, path: &str, offset: u64) -> Result<Reader> {
        let mut headers = vec![("Accept-Encoding", "identity".to_string())];
        if offset > 0 {
            headers.push(("Range", format!("bytes={offset}-")));
        }
        let response = self.send("GET", self.target(path), headers, &[]).await?;
        let head = &response.head;
        match head.status {
            200 => Ok(response.into_reader(offset)),
            206 if offset > 0 && range_start(head) == Some(offset) => Ok(response.into_reader(0)),
            416 if offset > 0 => {
                response.discard().await;
                Ok(Box::new(tokio::io::empty()))
            }
            status => {
                response.discard().await;
                Err(status_error(status, path))
            }
        }
    }

    async fn write(&self, path: &str, offset: u64) -> Result<Writer> {
        if offset > 0 {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("{path}: WebDAV cannot resume uploads"),
            ));
        }
        let headers = vec![("Content-Type", "application/octet-stream".to_string())];
        match self.client.upload(self.target(path), headers).await? {
            Ok(upload) => {
                let path = path.to_string();
                Ok(upload.into_writer(move |head: &Head| {
                    if head.is_success() {
                        Ok(())
                    } else {
                        Err(status_error(head.status, &path))
                    }
                }))
            }
            Err(head) => Err(status_error(head.status, path)),
        }
    }

    async fn touch(&self, path: &str, modified: SystemTime) -> Result<()> {
        if !self.owncloud {
            return Err(Error::new(ErrorKind::Unsupported, path.to_string()));
        }
        let seconds = modified
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::new(ErrorKind::Unsupported, path.to_string()))?
            .as_secs();
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:propertyupdate xmlns:d=\"DAV:\">\
             <d:set><d:prop><d:lastmodified>{seconds}</d:lastmodified></d:prop></d:set>\
             </d:propertyupdate>"
        );
        let headers = vec![("Content-Type", XML_TYPE.to_string())];
        let response = self
            .send("PROPPATCH", self.target(path), headers, body.as_bytes())
            .await?;
        if response.head.status != 207 {
            let status = response.head.status;
            response.discard().await;
            return Err(status_error(status, path));
        }
        let data = response.bytes(MAX_LISTING).await?;
        let resources = xml::parse_multistatus(&data)?;
        let updated = !resources.is_empty()
            && resources.iter().all(|resource| {
                !resource.prop_statuses.is_empty()
                    && resource
                        .prop_statuses
                        .iter()
                        .all(|status| (200..300).contains(status))
            });
        if updated {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Unsupported, path.to_string()))
        }
    }
}

// Interface

impl FileSystem for WebDavFileSystem {
    fn label(&self) -> Option<String> {
        Some(self.params.label())
    }

    fn url(&self, path: &str) -> Option<String> {
        let full = self.full_path(path);
        Some(self.params.url(full.trim_end_matches('/')))
    }

    fn home(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async { Ok("/".to_string()) })
    }

    fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
        let path = path.to_string();
        Box::pin(async move {
            let resources = self
                .propfind(&self.collection_target(&path), "1", PROPFIND, &path)
                .await?;
            let own = self
                .relative(&self.collection_target(&path))
                .unwrap_or_else(|| "/".to_string());
            let mut entries = Vec::new();
            for resource in &resources {
                let Some(relative) = self.relative(&resource.href) else {
                    continue;
                };
                if relative == own {
                    if !resource.collection {
                        return Err(Error::new(ErrorKind::NotADirectory, path));
                    }
                    continue;
                }
                if parent_posix(&relative).as_deref() != Some(own.as_str()) {
                    continue;
                }
                let name = self.file_name(&relative).to_string();
                entries.push(self.entry(name, resource));
            }
            Ok(entries)
        })
    }

    fn metadata(&self, path: &str) -> BoxFuture<'_, Result<Entry>> {
        let path = path.to_string();
        Box::pin(async move {
            let resources = self
                .propfind(&self.target(&path), "0", PROPFIND, &path)
                .await?;
            let resource = resources
                .first()
                .ok_or_else(|| Error::new(ErrorKind::NotFound, path.clone()))?;
            if resource.prop_statuses.is_empty() && resource.status.is_some_and(|s| s >= 300) {
                return Err(Error::new(ErrorKind::NotFound, path));
            }
            let name = self.file_name(&path).to_string();
            Ok(self.entry(name, resource))
        })
    }

    fn create_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            let response = self
                .send("MKCOL", self.collection_target(&path), Vec::new(), &[])
                .await?;
            let status = response.head.status;
            response.discard().await;
            match status {
                200..=206 => Ok(()),
                405 => Err(Error::new(ErrorKind::AlreadyExists, path)),
                status => Err(status_error(status, &path)),
            }
        })
    }

    fn remove_file(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            self.simple("DELETE", self.target(&path), Vec::new(), &path)
                .await
                .map(|_| ())
        })
    }

    fn remove_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            if !self.list(&path).await?.is_empty() {
                return Err(Error::new(ErrorKind::DirectoryNotEmpty, path));
            }
            self.simple("DELETE", self.collection_target(&path), Vec::new(), &path)
                .await
                .map(|_| ())
        })
    }

    fn rename(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        let from = from.to_string();
        let to = to.to_string();
        Box::pin(async move {
            let destination = format!("{}{}", self.client.origin(), self.target(&to));
            let headers = vec![("Destination", destination), ("Overwrite", "F".to_string())];
            self.simple("MOVE", self.target(&from), headers, &from)
                .await
                .map(|_| ())
        })
    }

    fn open_read(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Reader>> {
        let path = path.to_string();
        Box::pin(async move { self.read(&path, offset).await })
    }

    fn open_write(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Writer>> {
        let path = path.to_string();
        Box::pin(async move { self.write(&path, offset).await })
    }

    fn can_append(&self) -> bool {
        false
    }

    fn set_modified(&self, path: &str, modified: SystemTime) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move { self.touch(&path, modified).await })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async { self.client.close() })
    }

    fn join(&self, base: &str, name: &str) -> String {
        join_posix(base, name)
    }

    fn parent(&self, path: &str) -> Option<String> {
        parent_posix(path)
    }
}

impl fmt::Debug for WebDavFileSystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebDavFileSystem")
            .field("label", &self.params.label())
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

// Helpers

fn normalize_base(base: &str) -> String {
    let decoded = decode_path(base.trim());
    let parts: Vec<&str> = decoded
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", parts.join("/"))
    }
}

pub(crate) fn encode_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn decode_path(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| text.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

fn is_unlisted_root(error: &Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::PermissionDenied | ErrorKind::NotFound | ErrorKind::Unsupported
    )
}

fn range_start(head: &Head) -> Option<u64> {
    let range = head.header("content-range")?.trim();
    let range = range.strip_prefix("bytes")?.trim_start();
    range.split('-').next()?.trim().parse().ok()
}

fn status_error(status: u16, path: &str) -> Error {
    let kind = match status {
        401 => ErrorKind::AuthenticationFailed,
        403 | 423 => ErrorKind::PermissionDenied,
        404 | 409 | 410 => ErrorKind::NotFound,
        412 => ErrorKind::AlreadyExists,
        405 | 501 => ErrorKind::Unsupported,
        _ => ErrorKind::Other,
    };
    let detail = match status {
        507 => format!("{path}: the server has no space left"),
        _ if kind == ErrorKind::Other => format!("{path}: HTTP {status}"),
        _ => path.to_string(),
    };
    Error::new(kind, detail)
}

#[cfg(test)]
mod tests;
