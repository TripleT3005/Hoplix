use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime};

use super::http::{self, Body, Connector, Head, Request, Stream};
use super::sigv4::{self, CanonicalRequest, Credentials, EMPTY_SHA256, SERVICE, Scope};
use super::time;
use super::xml::{self, Element};
use crate::{Error, ErrorKind, HashAlgorithm, Result};

// Constants

const MAX_RESPONSE: usize = 16 * 1024 * 1024;
const MAX_IDLE: usize = 8;
const USER_AGENT: &str = concat!("Hoplix/", env!("CARGO_PKG_VERSION"));
const SKEW_HINT: &str = "the clock of this computer differs too much from the server's clock";

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub host: String,
    pub port: u16,
    pub https: bool,
    pub bucket: String,
    pub path_style: bool,
    pub region: String,
}

pub(crate) struct Client {
    target: Target,
    credentials: Credentials,
    connector: Connector,
    idle: Mutex<Vec<Stream>>,
    skew: AtomicI64,
    timeout: Duration,
    label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    pub method: &'static str,
    pub key: Option<String>,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Response {
    pub head: Head,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ErrorInfo {
    pub code: String,
    pub message: String,
    pub region: Option<String>,
    pub server_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectInfo {
    pub key: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub etag: Option<String>,
    pub comparable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ListPage {
    pub objects: Vec<ObjectInfo>,
    pub prefixes: Vec<String>,
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Part {
    pub number: u32,
    pub etag: String,
    pub comparable: Option<String>,
}

enum Outcome {
    Streaming(Head, Body),
    Buffered(Response),
    Retry,
}

// Calls

impl Call {
    pub(crate) fn new(method: &'static str, key: Option<&str>) -> Self {
        Self {
            method,
            key: key.map(str::to_string),
            query: Vec::new(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub(crate) fn query(mut self, name: &str, value: &str) -> Self {
        self.query.push((name.to_string(), value.to_string()));
        self
    }

    pub(crate) fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub(crate) fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }
}

// Targets

impl Target {
    pub(crate) fn request_host(&self) -> String {
        if self.path_style {
            self.host.clone()
        } else {
            format!("{}.{}", self.bucket, self.host)
        }
    }

    pub(crate) fn host_header(&self) -> String {
        let default = if self.https { 443 } else { 80 };
        let host = self.request_host();
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        if self.port == default {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    pub(crate) fn path(&self, key: Option<&str>) -> String {
        let key = key.map(|key| sigv4::uri_encode(key, true));
        match (self.path_style, key) {
            (true, Some(key)) => format!("/{}/{key}", sigv4::uri_encode(&self.bucket, false)),
            (true, None) => format!("/{}", sigv4::uri_encode(&self.bucket, false)),
            (false, Some(key)) => format!("/{key}"),
            (false, None) => "/".to_string(),
        }
    }

    pub(crate) fn copy_source(&self, key: &str) -> String {
        format!(
            "/{}/{}",
            sigv4::uri_encode(&self.bucket, false),
            sigv4::uri_encode(key, true)
        )
    }
}

// Client

impl Client {
    pub(crate) fn new(
        target: Target,
        credentials: Credentials,
        connector: Connector,
        timeout: Duration,
        label: String,
    ) -> Self {
        Self {
            target,
            credentials,
            connector,
            idle: Mutex::new(Vec::new()),
            skew: AtomicI64::new(0),
            timeout,
            label,
        }
    }

    pub(crate) fn target(&self) -> &Target {
        &self.target
    }

    #[cfg(test)]
    pub(crate) fn idle_for_tests(&self) -> &Mutex<Vec<Stream>> {
        &self.idle
    }

    pub(crate) fn prepare(&self, call: &Call, now: i64) -> Request {
        let stamp = time::timestamp(now + self.skew.load(Ordering::Relaxed));
        let payload_hash = if call.body.is_empty() {
            EMPTY_SHA256.to_string()
        } else {
            sigv4::sha256_hex(&call.body)
        };
        let mut headers = vec![
            ("Host".to_string(), self.target.host_header()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), stamp.full.clone()),
        ];
        if let Some(token) = &self.credentials.session_token {
            headers.push(("x-amz-security-token".to_string(), token.clone()));
        }
        headers.extend(call.headers.iter().cloned());
        let path = self.target.path(call.key.as_deref());
        let scope = Scope {
            date: &stamp.date,
            region: &self.target.region,
            service: SERVICE,
        };
        let canonical = CanonicalRequest {
            method: call.method,
            path: &path,
            query: &call.query,
            headers: &headers,
            payload_hash: &payload_hash,
        };
        let authorization =
            sigv4::authorization(&self.credentials, &stamp.full, &scope, &canonical);
        headers.push(("Authorization".to_string(), authorization));
        headers.push(("User-Agent".to_string(), USER_AGENT.to_string()));
        let query = sigv4::canonical_query(&call.query);
        let target = if query.is_empty() {
            path
        } else {
            format!("{path}?{query}")
        };
        Request {
            method: call.method,
            target,
            headers,
            body: call.body.clone(),
        }
    }

    fn recycle(&self, stream: Stream) {
        if let Ok(mut idle) = self.idle.lock()
            && idle.len() < MAX_IDLE
        {
            idle.push(stream);
        }
    }

    async fn exchange(&self, request: &Request, head_only: bool) -> Result<(Head, Body)> {
        loop {
            let pooled = self.idle.lock().ok().and_then(|mut idle| idle.pop());
            let reused = pooled.is_some();
            let mut stream = match pooled {
                Some(stream) => stream,
                None => (self.connector)().await?,
            };
            let attempt = async {
                http::send(&mut stream, request).await?;
                http::receive(stream, head_only).await
            };
            match attempt.await {
                Ok(response) => return Ok(response),
                Err(_) if reused => {}
                Err(error) => return Err(self.connection_error(&error)),
            }
        }
    }

    async fn attempt(&self, call: &Call, retried: &mut bool, stream: bool) -> Result<Outcome> {
        let request = self.prepare(call, time::unix_seconds(SystemTime::now()));
        let head_only = call.method == "HEAD";
        let (head, mut body) = self.exchange(&request, head_only).await?;
        if head.status < 300 && stream {
            return Ok(Outcome::Streaming(head, body));
        }
        let data = body
            .read_all(MAX_RESPONSE)
            .await
            .map_err(|error| self.connection_error(&error))?;
        let reusable = body.into_stream();
        if let Some(connection) = reusable {
            self.recycle(connection);
        }
        let failed = if head.status < 300 {
            error_document(&data)
        } else {
            Some(ErrorInfo::parse(&data))
        };
        let Some(info) = failed else {
            return Ok(Outcome::Buffered(Response { head, body: data }));
        };
        if info.code == "RequestTimeTooSkewed" && !*retried {
            let server = info
                .server_time
                .as_deref()
                .and_then(time::parse_iso8601)
                .or_else(|| head.header("date").and_then(time::parse_http_date));
            if let Some(server) = server {
                *retried = true;
                self.skew.store(
                    server - time::unix_seconds(SystemTime::now()),
                    Ordering::Relaxed,
                );
                return Ok(Outcome::Retry);
            }
        }
        Err(self.error(head.status, &info, &head, call))
    }

    async fn run(&self, call: &Call, stream: bool) -> Result<Outcome> {
        let mut retried = false;
        loop {
            let outcome =
                tokio::time::timeout(self.timeout, self.attempt(call, &mut retried, stream))
                    .await
                    .map_err(|_| {
                        Error::new(
                            ErrorKind::ConnectionFailed,
                            format!("{}: timed out", self.label),
                        )
                    })??;
            if !matches!(outcome, Outcome::Retry) {
                return Ok(outcome);
            }
        }
    }

    pub(crate) async fn call(&self, call: &Call) -> Result<Response> {
        match self.run(call, false).await? {
            Outcome::Buffered(response) => Ok(response),
            Outcome::Streaming(head, mut body) => {
                let data = body
                    .read_all(MAX_RESPONSE)
                    .await
                    .map_err(|error| self.connection_error(&error))?;
                Ok(Response { head, body: data })
            }
            Outcome::Retry => Err(self.invalid(call.method)),
        }
    }

    pub(crate) async fn open(&self, call: &Call) -> Result<(Head, Body)> {
        match self.run(call, true).await? {
            Outcome::Streaming(head, body) => Ok((head, body)),
            Outcome::Buffered(_) | Outcome::Retry => Err(self.invalid("GetObject")),
        }
    }

    // Errors

    fn connection_error(&self, error: &io::Error) -> Error {
        Error::new(
            ErrorKind::ConnectionFailed,
            format!("{}: {error}", self.label),
        )
    }

    fn error(&self, status: u16, info: &ErrorInfo, head: &Head, call: &Call) -> Error {
        let what = call
            .key
            .as_deref()
            .map_or_else(|| self.target.bucket.clone(), |key| format!("/{key}"));
        let kind = match info.code.as_str() {
            "NoSuchKey" | "NoSuchBucket" | "NoSuchUpload" => ErrorKind::NotFound,
            "AccessDenied" | "AllAccessDisabled" | "AccountProblem" => ErrorKind::PermissionDenied,
            "InvalidAccessKeyId"
            | "SignatureDoesNotMatch"
            | "InvalidToken"
            | "ExpiredToken"
            | "TokenRefreshRequired"
            | "InvalidSecurity" => ErrorKind::AuthenticationFailed,
            "" => match status {
                404 => ErrorKind::NotFound,
                401 => ErrorKind::AuthenticationFailed,
                403 => ErrorKind::PermissionDenied,
                500.. => ErrorKind::ConnectionFailed,
                _ => ErrorKind::Other,
            },
            _ if status >= 500 => ErrorKind::ConnectionFailed,
            _ => ErrorKind::Other,
        };
        let region = info
            .region
            .as_deref()
            .or_else(|| head.header("x-amz-bucket-region"))
            .filter(|region| *region != self.target.region);
        let mut detail = match kind {
            ErrorKind::NotFound if info.code == "NoSuchBucket" => {
                format!("bucket {}", self.target.bucket)
            }
            ErrorKind::NotFound | ErrorKind::PermissionDenied => what,
            ErrorKind::AuthenticationFailed => format!("{} ({})", self.label, info.code),
            _ => {
                let code = if info.code.is_empty() {
                    format!("HTTP {status}")
                } else {
                    info.code.clone()
                };
                let message = if info.message.is_empty() {
                    String::new()
                } else {
                    format!(": {}", info.message)
                };
                format!("{what}: {code}{message}")
            }
        };
        if info.code == "RequestTimeTooSkewed" {
            detail = format!("{detail} ({SKEW_HINT})");
        }
        if let Some(region) = region {
            detail = format!("{detail} (the bucket is in region {region})");
        }
        Error::new(kind, detail)
    }

    // Objects

    pub(crate) async fn list_page(
        &self,
        prefix: &str,
        delimiter: bool,
        token: Option<&str>,
        max_keys: Option<usize>,
    ) -> Result<ListPage> {
        let mut call = Call::new("GET", None).query("list-type", "2");
        if !prefix.is_empty() {
            call = call.query("prefix", prefix);
        }
        if delimiter {
            call = call.query("delimiter", "/");
        }
        if let Some(token) = token {
            call = call.query("continuation-token", token);
        }
        if let Some(max_keys) = max_keys {
            call = call.query("max-keys", &max_keys.to_string());
        }
        let response = self.call(&call).await?;
        parse_list(&response.body).ok_or_else(|| self.invalid("ListObjectsV2"))
    }

    pub(crate) async fn list_all(&self, prefix: &str, delimiter: bool) -> Result<ListPage> {
        let mut all = ListPage::default();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .list_page(prefix, delimiter, token.as_deref(), None)
                .await?;
            all.objects.extend(page.objects);
            all.prefixes.extend(page.prefixes);
            match page.next {
                Some(next) if Some(&next) != token.as_ref() => token = Some(next),
                _ => return Ok(all),
            }
        }
    }

    pub(crate) async fn head(&self, key: &str) -> Result<Option<ObjectInfo>> {
        match self.call(&Call::new("HEAD", Some(key))).await {
            Ok(response) => Ok(Some(ObjectInfo {
                comparable: comparable_etag(&response.head),
                key: key.to_string(),
                size: response
                    .head
                    .header("content-length")
                    .and_then(|length| length.parse().ok())
                    .unwrap_or(0),
                modified: response
                    .head
                    .header("last-modified")
                    .and_then(time::parse_http_date)
                    .and_then(time::system_time),
                etag: response.head.header("etag").map(str::to_string),
            })),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn put(&self, key: &str, body: Vec<u8>) -> Result<Option<String>> {
        let response = self.call(&Call::new("PUT", Some(key)).body(body)).await?;
        Ok(comparable_etag(&response.head))
    }

    pub(crate) async fn delete(&self, key: &str) -> Result<()> {
        self.call(&Call::new("DELETE", Some(key))).await.map(|_| ())
    }

    pub(crate) async fn delete_many(&self, keys: &[String]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let body = delete_document(keys).into_bytes();
        let md5 = crate::proxy::base64(&HashAlgorithm::Md5.digest(&body));
        let call = Call::new("POST", None)
            .query("delete", "")
            .header("Content-MD5", &md5)
            .header("Content-Type", "application/xml")
            .body(body);
        let response = self.call(&call).await?;
        match parse_delete_errors(&response.body) {
            Some(errors) if errors.is_empty() => Ok(()),
            Some(errors) => Err(Error::new(ErrorKind::Other, errors.join(", "))),
            None => Err(self.invalid("DeleteObjects")),
        }
    }

    pub(crate) async fn copy(&self, from: &str, to: &str) -> Result<()> {
        let call =
            Call::new("PUT", Some(to)).header("x-amz-copy-source", &self.target.copy_source(from));
        self.call(&call).await.map(|_| ())
    }

    // Multipart uploads

    pub(crate) async fn create_upload(&self, key: &str) -> Result<String> {
        let call = Call::new("POST", Some(key)).query("uploads", "");
        let response = self.call(&call).await?;
        xml::parse(&String::from_utf8_lossy(&response.body))
            .and_then(|root| root.text_of("UploadId").map(str::to_string))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| self.invalid("CreateMultipartUpload"))
    }

    pub(crate) async fn upload_part(
        &self,
        key: &str,
        upload: &str,
        number: u32,
        body: Vec<u8>,
    ) -> Result<Part> {
        let call = Call::new("PUT", Some(key))
            .query("partNumber", &number.to_string())
            .query("uploadId", upload)
            .body(body);
        let response = self.call(&call).await?;
        let etag = response
            .head
            .header("etag")
            .map(str::to_string)
            .ok_or_else(|| self.invalid("UploadPart"))?;
        Ok(Part {
            number,
            etag,
            comparable: comparable_etag(&response.head),
        })
    }

    pub(crate) async fn copy_part(
        &self,
        key: &str,
        upload: &str,
        number: u32,
        source: &str,
        range: (u64, u64),
    ) -> Result<Part> {
        let call = Call::new("PUT", Some(key))
            .query("partNumber", &number.to_string())
            .query("uploadId", upload)
            .header("x-amz-copy-source", &self.target.copy_source(source))
            .header(
                "x-amz-copy-source-range",
                &format!("bytes={}-{}", range.0, range.1),
            );
        let response = self.call(&call).await?;
        let etag = xml::parse(&String::from_utf8_lossy(&response.body))
            .and_then(|root| root.text_of("ETag").map(str::to_string))
            .ok_or_else(|| self.invalid("UploadPartCopy"))?;
        Ok(Part {
            number,
            etag,
            comparable: None,
        })
    }

    pub(crate) async fn complete_upload(
        &self,
        key: &str,
        upload: &str,
        parts: &[Part],
    ) -> Result<()> {
        let call = Call::new("POST", Some(key))
            .query("uploadId", upload)
            .header("Content-Type", "application/xml")
            .body(complete_document(parts).into_bytes());
        self.call(&call).await.map(|_| ())
    }

    pub(crate) async fn abort_upload(&self, key: &str, upload: &str) -> Result<()> {
        let call = Call::new("DELETE", Some(key)).query("uploadId", upload);
        self.call(&call).await.map(|_| ())
    }

    fn invalid(&self, operation: &str) -> Error {
        Error::new(
            ErrorKind::Other,
            format!("{}: invalid {operation} response", self.label),
        )
    }
}

// Documents

pub(crate) fn comparable_etag(head: &Head) -> Option<String> {
    let encrypted = head
        .header("x-amz-server-side-encryption")
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("aws:kms"))
        || head
            .header("x-amz-server-side-encryption-customer-algorithm")
            .is_some();
    head.header("etag")
        .filter(|_| !encrypted)
        .map(str::to_string)
}

impl ErrorInfo {
    pub(crate) fn parse(data: &[u8]) -> Self {
        let Some(root) = xml::parse(&String::from_utf8_lossy(data)) else {
            return Self::default();
        };
        let error = if root.name == "Error" {
            &root
        } else {
            match root.child("Error") {
                Some(error) => error,
                None => return Self::default(),
            }
        };
        Self::from_element(error)
    }

    fn from_element(error: &Element) -> Self {
        let text = |name: &str| error.text_of(name).map(|text| text.trim().to_string());
        Self {
            code: text("Code").unwrap_or_default(),
            message: text("Message").unwrap_or_default(),
            region: text("Region").filter(|region| !region.is_empty()),
            server_time: text("ServerTime"),
        }
    }
}

fn error_document(data: &[u8]) -> Option<ErrorInfo> {
    let start = data.iter().position(|byte| !byte.is_ascii_whitespace())?;
    if data[start] != b'<' {
        return None;
    }
    let root = xml::parse(&String::from_utf8_lossy(data))?;
    (root.name == "Error").then(|| ErrorInfo::from_element(&root))
}

pub(crate) fn parse_list(data: &[u8]) -> Option<ListPage> {
    let root = xml::parse(&String::from_utf8_lossy(data))?;
    if root.name != "ListBucketResult" {
        return None;
    }
    let objects = root
        .children_named("Contents")
        .map(|contents| {
            Some(ObjectInfo {
                key: contents.text_of("Key")?.to_string(),
                size: contents
                    .text_of("Size")
                    .and_then(|size| size.trim().parse().ok())
                    .unwrap_or(0),
                modified: contents
                    .text_of("LastModified")
                    .and_then(time::parse_iso8601)
                    .and_then(time::system_time),
                etag: contents.text_of("ETag").map(str::to_string),
                comparable: None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let prefixes = root
        .children_named("CommonPrefixes")
        .filter_map(|prefix| prefix.text_of("Prefix").map(str::to_string))
        .collect();
    let truncated = root.text_of("IsTruncated").map(str::trim) == Some("true");
    let next = root
        .text_of("NextContinuationToken")
        .filter(|token| truncated && !token.is_empty())
        .map(str::to_string);
    if truncated && next.is_none() {
        return None;
    }
    Some(ListPage {
        objects,
        prefixes,
        next,
    })
}

pub(crate) fn parse_delete_errors(data: &[u8]) -> Option<Vec<String>> {
    if data.iter().all(u8::is_ascii_whitespace) {
        return Some(Vec::new());
    }
    let root = xml::parse(&String::from_utf8_lossy(data))?;
    if root.name != "DeleteResult" {
        return None;
    }
    Some(
        root.children_named("Error")
            .map(|error| {
                format!(
                    "/{}: {}",
                    error.text_of("Key").unwrap_or_default(),
                    error.text_of("Code").unwrap_or_default()
                )
            })
            .collect(),
    )
}

pub(crate) fn delete_document(keys: &[String]) -> String {
    let objects: String = keys
        .iter()
        .map(|key| format!("<Object><Key>{}</Key></Object>", xml::escape(key)))
        .collect();
    format!("<Delete><Quiet>true</Quiet>{objects}</Delete>")
}

pub(crate) fn complete_document(parts: &[Part]) -> String {
    let parts: String = parts
        .iter()
        .map(|part| {
            format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                part.number,
                xml::escape(&part.etag)
            )
        })
        .collect();
    format!("<CompleteMultipartUpload>{parts}</CompleteMultipartUpload>")
}
