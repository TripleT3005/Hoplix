use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use super::client::{self, Call, Client, ErrorInfo, Part};
use super::http::{Connector, Stream, find};
use super::sigv4::{self, CanonicalRequest, Credentials, Scope};
use super::upload::PART_SIZE;
use super::*;
use crate::key_value::to_hex;
use crate::test_support::test_password;
use crate::{HashAlgorithm, Verification, verify_copy};

// Constants

const BUCKET: &str = "demo";
const HOST: &str = "s3.example.test";
const KEY_ID: &str = "demo-key";
const PAGE_SIZE: usize = 2;
const LAST_MODIFIED: &str = "Sun, 15 Mar 2026 13:00:00 GMT";
const LISTED_MODIFIED: &str = "2026-03-15T13:00:00.000Z";

// Fake server

#[derive(Default)]
struct Store {
    objects: BTreeMap<String, Vec<u8>>,
    etags: HashMap<String, String>,
    uploads: HashMap<String, (String, BTreeMap<u32, Vec<u8>>)>,
    log: Vec<String>,
    skew_once: Option<i64>,
    reject: Option<(u16, &'static str)>,
    next_upload: usize,
    bad_signatures: usize,
}

struct Fake {
    store: Arc<Mutex<Store>>,
    connections: Arc<AtomicUsize>,
    secret: String,
}

struct Received {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn query_value(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            output.push(u8::from_str_radix(&text[index + 1..index + 3], 16).expect("escape"));
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).expect("UTF-8")
}

async fn read_request(stream: &mut DuplexStream, pending: &mut Vec<u8>) -> Option<Received> {
    let mut buffer = [0u8; 64 * 1024];
    let end = loop {
        if let Some(end) = find(pending, b"\r\n\r\n") {
            break end;
        }
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        pending.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8(pending[..end].to_vec()).ok()?;
    pending.drain(..end + 4);
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_string();
    let target = request_line.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(": "))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map_or(0, |(_, value)| value.parse().expect("length"));
    while pending.len() < length {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        pending.extend_from_slice(&buffer[..read]);
    }
    let body: Vec<u8> = pending.drain(..length).collect();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let query = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect();
    Some(Received {
        method,
        path: path.to_string(),
        query,
        headers,
        body,
    })
}

fn response(status: u16, headers: &[(&str, String)], body: &[u8], head_only: bool) -> Vec<u8> {
    let mut text = format!("HTTP/1.1 {status} Fake\r\n");
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    if !headers.iter().any(|(name, _)| *name == "Content-Length") {
        text.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    text.push_str("\r\n");
    let mut bytes = text.into_bytes();
    if !head_only {
        bytes.extend_from_slice(body);
    }
    bytes
}

fn error_body(code: &str, extra: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{code}</Code><Message>{code} message</Message>{extra}</Error>"
    )
    .into_bytes()
}

fn md5_hex(data: &[u8]) -> String {
    to_hex(&HashAlgorithm::Md5.digest(data))
}

impl Fake {
    fn new() -> Self {
        Self {
            store: Arc::new(Mutex::new(Store::default())),
            connections: Arc::new(AtomicUsize::new(0)),
            secret: test_password(),
        }
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().expect("the store is poisoned")
    }

    fn connector(&self) -> Connector {
        let store = Arc::clone(&self.store);
        let connections = Arc::clone(&self.connections);
        let secret = self.secret.clone();
        Arc::new(move || {
            let store = Arc::clone(&store);
            let secret = secret.clone();
            connections.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let (client, server) = tokio::io::duplex(256 * 1024);
                tokio::spawn(serve(server, store, secret));
                Ok(Box::new(client) as Stream)
            })
        })
    }

    fn file_system(&self, path_style: bool) -> S3FileSystem {
        self.file_system_with(path_style, None)
    }

    fn file_system_with(&self, path_style: bool, token: Option<String>) -> S3FileSystem {
        let target = Target {
            host: HOST.to_string(),
            port: 443,
            https: true,
            bucket: BUCKET.to_string(),
            path_style,
            region: "eu-central-1".to_string(),
        };
        let credentials = Credentials {
            access_key_id: KEY_ID.to_string(),
            secret_access_key: self.secret.clone(),
            session_token: token,
        };
        S3FileSystem::with_connector(
            target,
            credentials,
            self.connector(),
            Duration::from_secs(10),
        )
    }

    fn insert(&self, key: &str, data: &[u8]) {
        let mut store = self.store();
        store
            .etags
            .insert(key.to_string(), format!("\"{}\"", md5_hex(data)));
        store.objects.insert(key.to_string(), data.to_vec());
    }

    fn object(&self, key: &str) -> Option<Vec<u8>> {
        self.store().objects.get(key).cloned()
    }

    fn keys(&self) -> Vec<String> {
        self.store().objects.keys().cloned().collect()
    }

    fn log(&self) -> Vec<String> {
        self.store().log.clone()
    }
}

fn signature_matches(request: &Received, secret: &str) -> bool {
    let Some(authorization) = request.header("Authorization") else {
        return false;
    };
    let Some((_, signed)) = authorization.split_once("SignedHeaders=") else {
        return false;
    };
    let signed: Vec<&str> = signed
        .split(',')
        .next()
        .unwrap_or_default()
        .split(';')
        .collect();
    let headers: Vec<(String, String)> = request
        .headers
        .iter()
        .filter(|(name, _)| signed.contains(&name.to_ascii_lowercase().as_str()))
        .cloned()
        .collect();
    let timestamp = request.header("x-amz-date").unwrap_or_default();
    let payload = request.header("x-amz-content-sha256").unwrap_or_default();
    if payload != sigv4::sha256_hex(&request.body) {
        return false;
    }
    let raw_path = request.path.clone();
    let canonical = CanonicalRequest {
        method: &request.method,
        path: &raw_path,
        query: &request.query,
        headers: &headers,
        payload_hash: payload,
    };
    let scope = Scope {
        date: timestamp.get(..8).unwrap_or_default(),
        region: "eu-central-1",
        service: "s3",
    };
    let expected = sigv4::signature(secret, timestamp, &scope, &canonical);
    authorization.ends_with(&format!("Signature={expected}"))
        && authorization.contains(&format!("Credential={KEY_ID}/"))
}

async fn serve(mut stream: DuplexStream, store: Arc<Mutex<Store>>, secret: String) {
    let mut pending = Vec::new();
    while let Some(request) = read_request(&mut stream, &mut pending).await {
        let bytes = handle(&request, &store, &secret);
        if stream.write_all(&bytes).await.is_err() {
            return;
        }
    }
}

fn handle(request: &Received, store: &Arc<Mutex<Store>>, secret: &str) -> Vec<u8> {
    let mut store = store.lock().expect("the store is poisoned");
    let head_only = request.method == "HEAD";
    let query_names: Vec<&str> = request
        .query
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    store.log.push(format!(
        "{} {} {}",
        request.method,
        request.path,
        query_names.join(",")
    ));
    if !signature_matches(request, secret) {
        store.bad_signatures += 1;
        return response(
            403,
            &[],
            &error_body("SignatureDoesNotMatch", ""),
            head_only,
        );
    }
    if let Some(offset) = store.skew_once.take() {
        let server = time::timestamp(time::unix_seconds(SystemTime::now()) + offset);
        let iso = format!(
            "{}-{}-{}T{}:{}:{}Z",
            &server.full[..4],
            &server.full[4..6],
            &server.full[6..8],
            &server.full[9..11],
            &server.full[11..13],
            &server.full[13..15]
        );
        return response(
            403,
            &[],
            &error_body(
                "RequestTimeTooSkewed",
                &format!("<ServerTime>{iso}</ServerTime>"),
            ),
            head_only,
        );
    }
    if let Some((status, code)) = store.reject {
        return response(
            status,
            &[],
            &error_body(code, "<Region>us-west-2</Region>"),
            head_only,
        );
    }
    let path = request
        .path
        .strip_prefix(&format!("/{BUCKET}"))
        .expect("path-style request");
    let key = decode(path.strip_prefix('/').unwrap_or(path));
    match (request.method.as_str(), key.is_empty()) {
        ("GET", true) => list(&store, request),
        ("POST", true) => delete_many(&mut store, request),
        ("HEAD", false) => match store.objects.get(&key) {
            Some(data) => response(
                200,
                &[
                    ("Content-Length", data.len().to_string()),
                    ("ETag", store.etags[&key].clone()),
                    ("Last-Modified", LAST_MODIFIED.to_string()),
                ],
                b"",
                true,
            ),
            None => response(404, &[], b"", true),
        },
        ("GET", false) => match store.objects.get(&key) {
            Some(data) => {
                let start = request
                    .header("Range")
                    .and_then(|range| range.strip_prefix("bytes="))
                    .and_then(|range| range.split('-').next())
                    .map_or(0, |start| start.parse::<usize>().expect("range"));
                let end = request
                    .header("Range")
                    .and_then(|range| range.split('-').nth(1))
                    .filter(|end| !end.is_empty())
                    .map_or(data.len(), |end| end.parse::<usize>().expect("range") + 1);
                let status = if request.header("Range").is_some() {
                    206
                } else {
                    200
                };
                response(status, &[], &data[start..end], false)
            }
            None => response(404, &[], &error_body("NoSuchKey", ""), false),
        },
        ("PUT", false) => put(&mut store, request, &key),
        ("POST", false) if request.query_value("uploads").is_some() => {
            store.next_upload += 1;
            let id = format!("upload-{}", store.next_upload);
            store.uploads.insert(id.clone(), (key, BTreeMap::new()));
            let body = format!(
                "<InitiateMultipartUploadResult><Bucket>{BUCKET}</Bucket><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"
            );
            response(200, &[], body.as_bytes(), false)
        }
        ("POST", false) => complete(&mut store, request, &key),
        ("DELETE", false) => {
            if let Some(id) = request.query_value("uploadId") {
                return match store.uploads.remove(id) {
                    Some(_) => response(204, &[], b"", false),
                    None => response(404, &[], &error_body("NoSuchUpload", ""), false),
                };
            }
            store.objects.remove(&key);
            response(204, &[], b"", false)
        }
        _ => response(400, &[], &error_body("NotImplemented", ""), false),
    }
}

fn copy_source(store: &Store, request: &Received) -> Option<Vec<u8>> {
    let source = request.header("x-amz-copy-source")?;
    let key = decode(source.strip_prefix(&format!("/{BUCKET}/")).expect("source"));
    let data = store
        .objects
        .get(&key)
        .expect("missing copy source")
        .clone();
    Some(match request.header("x-amz-copy-source-range") {
        Some(range) => {
            let (start, end) = range
                .strip_prefix("bytes=")
                .expect("range")
                .split_once('-')
                .expect("range");
            data[start.parse::<usize>().expect("start")..=end.parse::<usize>().expect("end")]
                .to_vec()
        }
        None => data,
    })
}

fn put(store: &mut Store, request: &Received, key: &str) -> Vec<u8> {
    let copied = copy_source(store, request);
    if let (Some(id), Some(number)) = (
        request.query_value("uploadId"),
        request.query_value("partNumber"),
    ) {
        let data = copied.clone().unwrap_or_else(|| request.body.clone());
        let etag = format!("\"{}\"", md5_hex(&data));
        let Some((_, parts)) = store.uploads.get_mut(id) else {
            return response(404, &[], &error_body("NoSuchUpload", ""), false);
        };
        parts.insert(number.parse().expect("part number"), data);
        return match copied {
            Some(_) => response(
                200,
                &[],
                format!(
                    "<CopyPartResult><ETag>{}</ETag></CopyPartResult>",
                    xml::escape(&etag)
                )
                .as_bytes(),
                false,
            ),
            None => response(200, &[("ETag", etag)], b"", false),
        };
    }
    let data = copied.clone().unwrap_or_else(|| request.body.clone());
    let etag = format!("\"{}\"", md5_hex(&data));
    store.objects.insert(key.to_string(), data);
    store.etags.insert(key.to_string(), etag.clone());
    match copied {
        Some(_) => response(
            200,
            &[],
            format!(
                "<CopyObjectResult><ETag>{}</ETag></CopyObjectResult>",
                xml::escape(&etag)
            )
            .as_bytes(),
            false,
        ),
        None => response(200, &[("ETag", etag)], b"", false),
    }
}

fn complete(store: &mut Store, request: &Received, key: &str) -> Vec<u8> {
    let id = request.query_value("uploadId").expect("upload id");
    let Some((upload_key, parts)) = store.uploads.remove(id) else {
        return response(404, &[], &error_body("NoSuchUpload", ""), false);
    };
    assert_eq!(upload_key, key);
    let document = xml::parse(&String::from_utf8_lossy(&request.body)).expect("document");
    let mut data = Vec::new();
    let mut count = 0;
    for part in document.children_named("Part") {
        let number: u32 = part
            .text_of("PartNumber")
            .expect("number")
            .parse()
            .expect("number");
        let stored = &parts[&number];
        assert_eq!(
            part.text_of("ETag"),
            Some(format!("\"{}\"", md5_hex(stored)).as_str())
        );
        data.extend_from_slice(stored);
        count += 1;
    }
    store
        .etags
        .insert(key.to_string(), format!("\"{}-{count}\"", md5_hex(&data)));
    store.objects.insert(key.to_string(), data);
    response(
        200,
        &[],
        b"<CompleteMultipartUploadResult><ETag>\"x-2\"</ETag></CompleteMultipartUploadResult>",
        false,
    )
}

fn delete_many(store: &mut Store, request: &Received) -> Vec<u8> {
    assert!(request.query_value("delete").is_some());
    let md5 = crate::proxy::base64(&HashAlgorithm::Md5.digest(&request.body));
    assert_eq!(request.header("Content-MD5"), Some(md5.as_str()));
    let document = xml::parse(&String::from_utf8_lossy(&request.body)).expect("document");
    for object in document.children_named("Object") {
        let key = object.text_of("Key").expect("key").to_string();
        store.objects.remove(&key);
    }
    response(200, &[], b"<DeleteResult></DeleteResult>", false)
}

fn list(store: &Store, request: &Received) -> Vec<u8> {
    assert_eq!(request.query_value("list-type"), Some("2"));
    let prefix = request.query_value("prefix").unwrap_or_default();
    let delimiter = request.query_value("delimiter");
    let after = request
        .query_value("continuation-token")
        .unwrap_or_default();
    let limit = request.query_value("max-keys").map_or(PAGE_SIZE, |value| {
        value.parse::<usize>().expect("max-keys").min(PAGE_SIZE)
    });
    let mut entries: Vec<(String, bool)> = Vec::new();
    for key in store.objects.keys().filter(|key| key.starts_with(prefix)) {
        let rest = &key[prefix.len()..];
        match delimiter.and_then(|delimiter| rest.find(delimiter)) {
            Some(index) => {
                let common = format!("{prefix}{}", &rest[..=index]);
                if !entries.iter().any(|(existing, _)| *existing == common) {
                    entries.push((common, true));
                }
            }
            None => entries.push((key.clone(), false)),
        }
    }
    entries.retain(|(name, _)| name.as_str() > after);
    let truncated = entries.len() > limit;
    entries.truncate(limit);
    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
    );
    body.push_str(&format!(
        "<Name>{BUCKET}</Name><IsTruncated>{truncated}</IsTruncated>"
    ));
    for (name, common) in &entries {
        if *common {
            body.push_str(&format!(
                "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
                xml::escape(name)
            ));
        } else {
            body.push_str(&format!(
                "<Contents><Key>{}</Key><LastModified>{LISTED_MODIFIED}</LastModified><ETag>{}</ETag><Size>{}</Size></Contents>",
                xml::escape(name),
                xml::escape(&store.etags[name]),
                store.objects[name].len()
            ));
        }
    }
    if truncated && let Some((last, _)) = entries.last() {
        body.push_str(&format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            xml::escape(last)
        ));
    }
    body.push_str("</ListBucketResult>");
    response(
        200,
        &[("Content-Type", "application/xml".to_string())],
        body.as_bytes(),
        false,
    )
}

// Helpers

fn data(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

async fn write_object(file_system: &S3FileSystem, path: &str, offset: u64, data: &[u8]) {
    let mut writer = file_system
        .open_write(path, offset)
        .await
        .expect("opening failed");
    for piece in data.chunks(100_000) {
        writer.write_all(piece).await.expect("writing failed");
    }
    writer.shutdown().await.expect("finishing failed");
}

async fn read_object(file_system: &S3FileSystem, path: &str, offset: u64) -> Vec<u8> {
    let mut reader = file_system
        .open_read(path, offset)
        .await
        .expect("opening failed");
    let mut data = Vec::new();
    reader.read_to_end(&mut data).await.expect("reading failed");
    data
}

fn names(entries: &[Entry]) -> Vec<String> {
    let mut names: Vec<String> = entries
        .iter()
        .map(|entry| {
            if entry.is_directory() {
                format!("{}/", entry.name)
            } else {
                entry.name.clone()
            }
        })
        .collect();
    names.sort();
    names
}

// Requests

#[test]
fn targets_build_hosts_and_paths() {
    let mut target = Target {
        host: HOST.to_string(),
        port: 443,
        https: true,
        bucket: BUCKET.to_string(),
        path_style: false,
        region: "us-east-1".to_string(),
    };
    assert_eq!(target.request_host(), "demo.s3.example.test");
    assert_eq!(target.host_header(), "demo.s3.example.test");
    assert_eq!(target.path(Some("a b/ü+.txt")), "/a%20b/%C3%BC%2B.txt");
    assert_eq!(target.path(None), "/");
    target.path_style = true;
    target.port = 9000;
    assert_eq!(target.host_header(), "s3.example.test:9000");
    assert_eq!(target.path(Some("a/b")), "/demo/a/b");
    assert_eq!(target.path(None), "/demo");
    assert_eq!(target.copy_source("a b"), "/demo/a%20b");
}

#[test]
fn connection_settings_pick_the_addressing_style() {
    let params = ConnectionParams {
        protocol: crate::Protocol::S3,
        host: String::new(),
        port: 443,
        username: KEY_ID.to_string(),
        authentication: Authentication::Password(test_password()),
    };
    let mut settings = S3Options {
        bucket: BUCKET.to_string(),
        region: "eu-west-1".to_string(),
        ..S3Options::default()
    };
    let target = target_for(&params, &settings).expect("valid");
    assert_eq!(target.host, "s3.eu-west-1.amazonaws.com");
    assert!(!target.path_style && target.https);
    assert_eq!(label_for(&target), "demo @ s3.eu-west-1.amazonaws.com");

    settings.bucket = "dotted.bucket".to_string();
    assert!(target_for(&params, &settings).expect("valid").path_style);
    let local = ConnectionParams {
        host: "192.168.1.5".to_string(),
        port: 9000,
        ..params.clone()
    };
    settings.bucket = BUCKET.to_string();
    settings.plain_http = true;
    let target = target_for(&local, &settings).expect("valid");
    assert!(target.path_style && !target.https);
    assert_eq!(label_for(&target), "demo @ 192.168.1.5:9000");

    settings.bucket = "a/b".to_string();
    assert!(target_for(&params, &settings).is_err());
}

#[test]
fn signed_requests_carry_the_session_token() {
    let fake = Fake::new();
    let token = test_password();
    let file_system = fake.file_system_with(true, Some(token.clone()));
    let request = file_system
        .client
        .prepare(&Call::new("GET", Some("a b.txt")), 1_773_579_600);
    assert_eq!(request.target, "/demo/a%20b.txt");
    let header = |name: &str| {
        request
            .headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(header("x-amz-date").as_deref(), Some("20260315T130000Z"));
    assert_eq!(header("x-amz-security-token"), Some(token));
    let authorization = header("Authorization").expect("signed");
    assert!(authorization.starts_with(
        "AWS4-HMAC-SHA256 Credential=demo-key/20260315/eu-central-1/s3/aws4_request, \
         SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token, Signature="
    ));
    assert!(!authorization.contains(&fake.secret));
    assert!(!format!("{file_system:?}").contains(&fake.secret));
}

// Documents

#[test]
fn list_responses_become_pages() {
    let page = client::parse_list(
        b"<ListBucketResult><IsTruncated>true</IsTruncated>\
          <Contents><Key>a &amp; b.txt</Key><LastModified>2026-03-15T13:00:00.000Z</LastModified><ETag>&quot;x&quot;</ETag><Size>12</Size></Contents>\
          <CommonPrefixes><Prefix>docs/</Prefix></CommonPrefixes>\
          <NextContinuationToken>next+token</NextContinuationToken></ListBucketResult>",
    )
    .expect("parsing failed");
    assert_eq!(page.objects.len(), 1);
    assert_eq!(page.objects[0].key, "a & b.txt");
    assert_eq!(page.objects[0].size, 12);
    assert_eq!(page.objects[0].etag.as_deref(), Some("\"x\""));
    assert_eq!(page.objects[0].modified, time::system_time(1_773_579_600));
    assert_eq!(page.prefixes, ["docs/"]);
    assert_eq!(page.next.as_deref(), Some("next+token"));
    assert_eq!(
        client::parse_list(b"<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>"),
        None
    );
    assert_eq!(client::parse_list(b"<Error/>"), None);
}

#[test]
fn error_documents_are_read() {
    let info = ErrorInfo::parse(
        b"<?xml version=\"1.0\"?><Error><Code>AuthorizationHeaderMalformed</Code>\
          <Message>wrong region</Message><Region>eu-west-1</Region>\
          <ServerTime>2026-03-15T13:00:00Z</ServerTime></Error>",
    );
    assert_eq!(info.code, "AuthorizationHeaderMalformed");
    assert_eq!(info.message, "wrong region");
    assert_eq!(info.region.as_deref(), Some("eu-west-1"));
    assert_eq!(info.server_time.as_deref(), Some("2026-03-15T13:00:00Z"));
    assert_eq!(ErrorInfo::parse(b"not xml"), ErrorInfo::default());
    assert_eq!(
        ErrorInfo::parse(b"<ErrorResponse><Error><Code>X</Code></Error></ErrorResponse>").code,
        "X"
    );
}

#[test]
fn multipart_documents_are_written_and_read() {
    let parts = [
        Part {
            number: 1,
            etag: "\"a\"".to_string(),
            comparable: None,
        },
        Part {
            number: 2,
            etag: "\"b\"".to_string(),
            comparable: None,
        },
    ];
    assert_eq!(
        client::complete_document(&parts),
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>&quot;a&quot;</ETag></Part>\
         <Part><PartNumber>2</PartNumber><ETag>&quot;b&quot;</ETag></Part></CompleteMultipartUpload>"
    );
    assert_eq!(
        client::delete_document(&["a&b".to_string(), "c".to_string()]),
        "<Delete><Quiet>true</Quiet><Object><Key>a&amp;b</Key></Object><Object><Key>c</Key></Object></Delete>"
    );
    assert_eq!(
        client::parse_delete_errors(b"<DeleteResult/>"),
        Some(Vec::new())
    );
    assert_eq!(
        client::parse_delete_errors(
            b"<DeleteResult><Error><Key>a</Key><Code>AccessDenied</Code></Error></DeleteResult>"
        ),
        Some(vec!["/a: AccessDenied".to_string()])
    );
    assert_eq!(client::parse_delete_errors(b"<Other/>"), None);
}

// Objects

#[tokio::test]
async fn small_files_are_uploaded_read_and_verified() {
    let fake = Fake::new();
    let file_system = fake.file_system(true);
    let content = data(300_000);
    write_object(&file_system, "/docs/report.pdf", 0, &content).await;
    assert_eq!(fake.object("docs/report.pdf"), Some(content.clone()));
    assert!(
        fake.log()
            .iter()
            .any(|line| line == "PUT /demo/docs/report.pdf ")
    );

    let entry = file_system
        .metadata("/docs/report.pdf")
        .await
        .expect("metadata failed");
    assert_eq!(entry.size, Some(300_000));
    assert_eq!(entry.modified, time::system_time(1_773_579_600));
    assert_eq!(
        read_object(&file_system, "/docs/report.pdf", 0).await,
        content
    );
    assert_eq!(
        read_object(&file_system, "/docs/report.pdf", 299_990).await,
        content[299_990..]
    );
    assert_eq!(
        file_system
            .checksum("/docs/report.pdf", HashAlgorithm::Md5)
            .await,
        Ok(HashAlgorithm::Md5.digest(&content))
    );
    assert!(
        file_system
            .checksum("/docs/report.pdf", HashAlgorithm::Sha256)
            .await
            .is_err()
    );
    assert_eq!(
        file_system.url("/docs/a b"),
        Some("s3://demo/docs/a%20b".to_string())
    );
    assert_eq!(fake.store().bad_signatures, 0);
    assert!(fake.connections.load(Ordering::SeqCst) < fake.log().len());
}

#[tokio::test]
async fn large_files_use_multipart_uploads() {
    let fake = Fake::new();
    let file_system = fake.file_system(true);
    let content = data(2 * PART_SIZE + 123_456);
    write_object(&file_system, "/big.bin", 0, &content).await;
    assert_eq!(fake.object("big.bin"), Some(content.clone()));
    let log = fake.log();
    assert_eq!(
        log.iter()
            .filter(|line| line.contains("partNumber"))
            .count(),
        3
    );
    assert!(log.iter().any(|line| line == "POST /demo/big.bin uploads"));
    assert!(log.iter().any(|line| line == "POST /demo/big.bin uploadId"));
    assert!(fake.store().uploads.is_empty());
    assert!(
        file_system
            .checksum("/big.bin", HashAlgorithm::Md5)
            .await
            .is_err()
    );

    let local_dir = std::env::temp_dir().join(format!("hoplix-s3-verify-{}", std::process::id()));
    std::fs::create_dir_all(&local_dir).expect("creating failed");
    let local_path = local_dir.join("big.bin");
    std::fs::write(&local_path, &content).expect("writing failed");
    let local_text = local_path.to_string_lossy().into_owned();
    let verification = verify_copy(
        &crate::LocalFileSystem,
        &local_text,
        &file_system,
        "/big.bin",
        content.len() as u64,
    )
    .await
    .expect("verifying failed");
    assert_eq!(verification, Verification::SizeChecked);
    let small = data(1000);
    std::fs::write(&local_path, &small).expect("writing failed");
    write_object(&file_system, "/small.bin", 0, &small).await;
    let verification = verify_copy(
        &crate::LocalFileSystem,
        &local_text,
        &file_system,
        "/small.bin",
        1000,
    )
    .await
    .expect("verifying failed");
    assert_eq!(verification, Verification::Matched(HashAlgorithm::Md5));
    std::fs::remove_dir_all(local_dir).expect("cleaning up failed");
}

#[tokio::test]
async fn interrupted_multipart_uploads_are_aborted() {
    let fake = Fake::new();
    let file_system = fake.file_system(true);
    let mut writer = file_system
        .open_write("/cancelled.bin", 0)
        .await
        .expect("opening failed");
    writer
        .write_all(&data(PART_SIZE + 10))
        .await
        .expect("writing failed");
    writer.flush().await.expect("flushing failed");
    assert_eq!(fake.store().uploads.len(), 1);
    drop(writer);
    for _ in 0..100 {
        if fake.store().uploads.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(fake.store().uploads.is_empty());
    assert_eq!(fake.object("cancelled.bin"), None);
}

#[tokio::test]
async fn uploads_continue_after_existing_data() {
    let fake = Fake::new();
    let file_system = fake.file_system(true);
    let content = data(6 * 1024 * 1024 + 500);
    let split = 6 * 1024 * 1024;
    fake.insert("resume.bin", &content[..split]);
    write_object(&file_system, "/resume.bin", split as u64, &content[split..]).await;
    assert_eq!(fake.object("resume.bin"), Some(content.clone()));
    assert!(fake.log().iter().any(|line| line.contains("partNumber")));

    fake.insert("short.bin", &content[..1000]);
    write_object(&file_system, "/short.bin", 1000, &content[1000..5000]).await;
    assert_eq!(fake.object("short.bin"), Some(content[..5000].to_vec()));
    let Err(error) = file_system.open_write("/short.bin", 9000).await else {
        panic!("writing after the end was accepted");
    };
    assert_eq!(error.kind(), ErrorKind::Other);
}

// Folders

#[tokio::test]
async fn folders_come_from_prefixes_and_markers() {
    let fake = Fake::new();
    for key in [
        "a.txt",
        "docs/",
        "docs/b.txt",
        "docs/sub/c.txt",
        "photos/x.jpg",
        "z.txt",
    ] {
        fake.insert(key, key.as_bytes());
    }
    let file_system = fake.file_system(true);
    let root = file_system.list("/").await.expect("listing failed");
    assert_eq!(names(&root), ["a.txt", "docs/", "photos/", "z.txt"]);
    let file = root
        .iter()
        .find(|entry| entry.name == "a.txt")
        .expect("file");
    assert_eq!(file.size, Some(5));
    assert_eq!(file.modified, time::system_time(1_773_579_600));
    assert_eq!(
        names(&file_system.list("/docs").await.expect("listing failed")),
        ["b.txt", "sub/"]
    );
    assert!(
        fake.log()
            .iter()
            .filter(|line| line.starts_with("GET /demo ") && line.contains("list-type"))
            .count()
            >= 3
    );

    assert!(
        file_system
            .metadata("/photos")
            .await
            .expect("metadata failed")
            .is_directory()
    );
    assert_eq!(
        file_system
            .metadata("/")
            .await
            .map(|entry| entry.is_directory()),
        Ok(true)
    );
    assert_eq!(
        file_system
            .metadata("/missing")
            .await
            .map_err(|error| error.kind()),
        Err(ErrorKind::NotFound)
    );
    assert_eq!(
        file_system
            .list("/missing")
            .await
            .map_err(|error| error.kind()),
        Err(ErrorKind::NotFound)
    );
    assert_eq!(
        file_system
            .list("/a.txt")
            .await
            .map_err(|error| error.kind()),
        Err(ErrorKind::NotADirectory)
    );
    fake.insert("empty/", b"");
    assert_eq!(file_system.list("/empty").await, Ok(Vec::new()));
}

#[tokio::test]
async fn folders_are_created_removed_and_renamed() {
    let fake = Fake::new();
    let file_system = fake.file_system(true);
    file_system
        .create_directory("/new")
        .await
        .expect("creating failed");
    assert_eq!(fake.object("new/"), Some(Vec::new()));
    assert_eq!(
        file_system
            .create_directory("/new")
            .await
            .map_err(|error| error.kind()),
        Err(ErrorKind::AlreadyExists)
    );
    fake.insert("new/a.txt", b"a");
    fake.insert("new/deep/b.txt", b"b");
    assert_eq!(
        file_system
            .remove_directory("/new")
            .await
            .map_err(|error| error.kind()),
        Err(ErrorKind::DirectoryNotEmpty)
    );

    file_system
        .rename("/new/a.txt", "/new/c.txt")
        .await
        .expect("renaming failed");
    assert_eq!(fake.object("new/c.txt"), Some(b"a".to_vec()));
    assert_eq!(fake.object("new/a.txt"), None);
    fake.insert("taken.txt", b"t");
    assert_eq!(
        file_system
            .rename("/new/c.txt", "/taken.txt")
            .await
            .map_err(|error| error.kind()),
        Err(ErrorKind::AlreadyExists)
    );

    file_system
        .rename("/new", "/moved")
        .await
        .expect("renaming failed");
    assert_eq!(
        fake.keys(),
        ["moved/", "moved/c.txt", "moved/deep/b.txt", "taken.txt"]
    );
    assert!(file_system.rename("/moved", "/moved/inner").await.is_err());

    crate::remove_recursive(&file_system, "/moved")
        .await
        .expect("removing failed");
    assert_eq!(fake.keys(), ["taken.txt"]);
    assert!(fake.log().iter().any(|line| line == "POST /demo delete"));
    file_system
        .remove_file("/taken.txt")
        .await
        .expect("removing failed");
    assert_eq!(
        file_system
            .remove_file("/taken.txt")
            .await
            .map_err(|error| error.kind()),
        Err(ErrorKind::NotFound)
    );
    fake.insert("gone/", b"");
    file_system
        .remove_directory("/gone")
        .await
        .expect("removing failed");
    assert!(fake.keys().is_empty());
}

// Errors

#[tokio::test]
async fn clock_skew_is_corrected_once() {
    let fake = Fake::new();
    fake.store().skew_once = Some(3600);
    let file_system = fake.file_system(true);
    assert_eq!(file_system.list("/").await, Ok(Vec::new()));
    let request = file_system.client.prepare(&Call::new("GET", None), 0);
    let date = request
        .headers
        .iter()
        .find(|(name, _)| name == "x-amz-date")
        .map(|(_, value)| value.clone())
        .expect("dated");
    assert!(
        ["19700101T0059", "19700101T0100"]
            .iter()
            .any(|start| date.starts_with(start)),
        "{date}"
    );
    assert_eq!(fake.log().len(), 2);
}

#[tokio::test]
async fn server_errors_map_to_error_kinds() {
    let fake = Fake::new();
    let file_system = fake.file_system(true);
    for (status, code, kind) in [
        (
            403,
            "SignatureDoesNotMatch",
            ErrorKind::AuthenticationFailed,
        ),
        (403, "InvalidAccessKeyId", ErrorKind::AuthenticationFailed),
        (403, "AccessDenied", ErrorKind::PermissionDenied),
        (404, "NoSuchBucket", ErrorKind::NotFound),
        (503, "SlowDown", ErrorKind::ConnectionFailed),
        (400, "AuthorizationHeaderMalformed", ErrorKind::Other),
    ] {
        fake.store().reject = Some((status, code));
        let error = file_system.list("/").await.expect_err("accepted");
        assert_eq!(error.kind(), kind, "{code}");
        assert!(!error.detail().contains(&fake.secret));
        if kind == ErrorKind::Other {
            assert!(error.detail().contains("us-west-2"), "{}", error.detail());
        }
    }
    fake.store().reject = None;
    let wrong = Fake {
        secret: test_password(),
        store: Arc::clone(&fake.store),
        connections: Arc::clone(&fake.connections),
    };
    let target = file_system.client.target().clone();
    let credentials = Credentials {
        access_key_id: KEY_ID.to_string(),
        secret_access_key: test_password(),
        session_token: None,
    };
    let rejected = S3FileSystem::with_connector(
        target,
        credentials,
        wrong.connector(),
        Duration::from_secs(5),
    );
    let error = rejected.list("/").await.expect_err("accepted");
    assert_eq!(error.kind(), ErrorKind::AuthenticationFailed);
    assert_eq!(
        error.detail(),
        "demo @ s3.example.test (SignatureDoesNotMatch)"
    );
}

#[tokio::test]
async fn stale_pooled_connections_are_replaced() {
    let fake = Fake::new();
    let file_system = fake.file_system(true);
    fake.insert("a.txt", b"a");
    file_system
        .metadata("/a.txt")
        .await
        .expect("metadata failed");
    let client: &Client = &file_system.client;
    let (stale, server) = tokio::io::duplex(1024);
    drop(server);
    if let Ok(mut idle) = client_idle(client) {
        idle.clear();
        idle.push(Box::new(stale));
    }
    assert!(file_system.metadata("/a.txt").await.is_ok());
}

fn client_idle(client: &Client) -> std::sync::LockResult<std::sync::MutexGuard<'_, Vec<Stream>>> {
    client.idle_for_tests().lock()
}
