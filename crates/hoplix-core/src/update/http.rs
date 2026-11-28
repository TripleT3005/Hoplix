use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::Verifier;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::http::Framing;
use crate::{Error, ErrorKind, Result};

// Constants

pub(crate) const MAX_RESPONSE: usize = 1024 * 1024;
pub(crate) const MAX_REDIRECTS: usize = 3;
pub(crate) const TIMEOUT: Duration = Duration::from_secs(15);
const ALLOWED_DOMAIN: &str = "github.com";
const HTTPS_PORT: u16 = 443;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Url {
    pub host: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Response {
    Body(Vec<u8>),
    Redirect(String),
}

// Addresses

impl Url {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix("https://")?;
        let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (host, path) = rest.split_at(split);
        let host = host.to_ascii_lowercase();
        if !is_allowed_host(&host) {
            return None;
        }
        Self::with_path(host, path)
    }

    pub(crate) fn resolve(&self, location: &str) -> Option<Self> {
        let location = location.trim();
        if location.starts_with('/') && !location.starts_with("//") {
            return Self::with_path(self.host.clone(), location);
        }
        Self::parse(location)
    }

    fn with_path(host: String, path: &str) -> Option<Self> {
        let path = path.split('#').next().unwrap_or_default();
        let path = if path.is_empty() {
            "/".to_string()
        } else if path.starts_with('?') {
            format!("/{path}")
        } else {
            path.to_string()
        };
        let valid = path.bytes().all(|byte| byte.is_ascii_graphic());
        valid.then_some(Self { host, path })
    }
}

fn is_allowed_host(host: &str) -> bool {
    let valid = !host.is_empty()
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.');
    valid
        && (host == ALLOWED_DOMAIN
            || host
                .strip_suffix(ALLOWED_DOMAIN)
                .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1))
}

// Requests

pub(crate) fn request_text(url: &Url, user_agent: &str) -> String {
    format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {user_agent}\r\nConnection: close\r\n\r\n",
        url.path, url.host
    )
}

pub(crate) async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    url: &Url,
    user_agent: &str,
) -> Result<Response> {
    stream
        .write_all(request_text(url, user_agent).as_bytes())
        .await
        .map_err(|error| connection_error(&url.host, &error))?;
    stream
        .flush()
        .await
        .map_err(|error| connection_error(&url.host, &error))?;
    let raw = read_limited(&mut stream, MAX_RESPONSE, &url.host).await?;
    parse_response(&raw)
}

async fn read_limited<R: AsyncRead + Unpin>(
    reader: &mut R,
    limit: usize,
    host: &str,
) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = match reader.read(&mut buffer).await {
            Ok(0) => return Ok(data),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof && !data.is_empty() => {
                return Ok(data);
            }
            Err(error) => return Err(connection_error(host, &error)),
        };
        data.extend_from_slice(&buffer[..count]);
        if data.len() > limit {
            return Err(too_large());
        }
    }
}

pub(crate) async fn get_with<C, F, S>(
    url: &str,
    user_agent: &str,
    mut connect: C,
) -> Result<Vec<u8>>
where
    C: FnMut(String) -> F,
    F: Future<Output = Result<S>>,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut current = Url::parse(url).ok_or_else(|| invalid_address(url))?;
    for _ in 0..=MAX_REDIRECTS {
        let stream = connect(current.host.clone()).await?;
        match exchange(stream, &current, user_agent).await? {
            Response::Body(body) => return Ok(body),
            Response::Redirect(location) => {
                current = current
                    .resolve(&location)
                    .ok_or_else(|| invalid_address(&location))?;
            }
        }
    }
    Err(Error::new(ErrorKind::Other, "too many redirects"))
}

pub(crate) async fn get(url: &str, user_agent: &str) -> Result<Vec<u8>> {
    let config = tls_config()?;
    let request = get_with(url, user_agent, |host| connect(Arc::clone(&config), host));
    tokio::time::timeout(TIMEOUT, request)
        .await
        .map_err(|_| Error::new(ErrorKind::ConnectionFailed, "timed out"))?
}

// Responses

pub(crate) fn parse_response(raw: &[u8]) -> Result<Response> {
    let end = find(raw, b"\r\n\r\n").ok_or_else(|| malformed("incomplete header"))?;
    let head = crate::http::parse_head(&raw[..end])?;
    let body = &raw[end + 4..];
    match head.status {
        200 => {}
        301 | 302 | 303 | 307 | 308 => {
            let location = head
                .header("location")
                .ok_or_else(|| malformed("redirect"))?;
            return Ok(Response::Redirect(location.to_string()));
        }
        code => return Err(Error::new(ErrorKind::Other, format!("HTTP {code}"))),
    }
    match head.framing(false)? {
        Framing::Chunked => dechunk(body).map(Response::Body),
        Framing::Length(length) => {
            let length = usize::try_from(length)
                .ok()
                .filter(|length| *length <= MAX_RESPONSE)
                .ok_or_else(too_large)?;
            body.get(..length)
                .map(|body| Response::Body(body.to_vec()))
                .ok_or_else(|| malformed("truncated body"))
        }
        Framing::Empty | Framing::UntilClose => Ok(Response::Body(body.to_vec())),
    }
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    loop {
        let line_end = find(body, b"\r\n").ok_or_else(|| malformed("chunk"))?;
        let line = std::str::from_utf8(&body[..line_end]).map_err(|_| malformed("chunk"))?;
        let size = line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16).map_err(|_| malformed("chunk size"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(data);
        }
        if data.len() + size > MAX_RESPONSE {
            return Err(too_large());
        }
        let chunk = body
            .get(..size)
            .ok_or_else(|| malformed("truncated chunk"))?;
        data.extend_from_slice(chunk);
        body = body
            .get(size..)
            .and_then(|rest| rest.strip_prefix(b"\r\n"))
            .ok_or_else(|| malformed("chunk end"))?;
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// Connections

fn tls_config() -> Result<Arc<ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Verifier::new(Arc::clone(&provider)).map_err(tls_error)?;
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

async fn connect(config: Arc<ClientConfig>, host: String) -> Result<TlsStream<TcpStream>> {
    let tcp = TcpStream::connect((host.as_str(), HTTPS_PORT))
        .await
        .map_err(|error| connection_error(&host, &error))?;
    let name = ServerName::try_from(host.clone()).map_err(|_| invalid_address(&host))?;
    TlsConnector::from(config)
        .connect(name, tcp)
        .await
        .map_err(|error| connection_error(&host, &error))
}

// Errors

fn connection_error(host: &str, error: &io::Error) -> Error {
    Error::new(ErrorKind::ConnectionFailed, format!("{host}: {error}"))
}

fn tls_error(error: rustls::Error) -> Error {
    Error::new(ErrorKind::Other, error.to_string())
}

fn invalid_address(address: &str) -> Error {
    Error::new(ErrorKind::Other, format!("address not allowed: {address}"))
}

fn malformed(part: &str) -> Error {
    Error::new(ErrorKind::Other, format!("invalid HTTP response: {part}"))
}

fn too_large() -> Error {
    Error::new(ErrorKind::Other, "response too large")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    const USER_AGENT: &str = "Hoplix/1.2.3";

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap_or_else(|| panic!("{text} did not parse"))
    }

    fn body(raw: &str) -> Vec<u8> {
        match parse_response(raw.as_bytes()).expect("parsing failed") {
            Response::Body(body) => body,
            Response::Redirect(location) => panic!("unexpected redirect to {location}"),
        }
    }

    fn server(response: Vec<u8>, requests: Arc<std::sync::Mutex<Vec<String>>>) -> DuplexStream {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while find(&request, b"\r\n\r\n").is_none() {
                match server.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(count) => request.extend_from_slice(&buffer[..count]),
                }
            }
            requests
                .lock()
                .expect("the request list is poisoned")
                .push(String::from_utf8_lossy(&request).into_owned());
            let _ = server.write_all(&response).await;
        });
        client
    }

    // Addresses

    #[test]
    fn only_github_hosts_are_allowed() {
        assert_eq!(
            url("https://api.github.com/repos/a/b/releases?per_page=10"),
            Url {
                host: "api.github.com".to_string(),
                path: "/repos/a/b/releases?per_page=10".to_string(),
            }
        );
        assert_eq!(url("https://GitHub.com").path, "/");
        assert_eq!(url("https://github.com?x=1#top").path, "/?x=1");
        for text in [
            "http://api.github.com/",
            "https://evilgithub.com/",
            "https://github.com.evil.example/",
            "https://user@github.com/",
            "https://github.com:8443/",
            "https://.github.com/",
            "https://example.com/",
            "https://github.com/a b",
        ] {
            assert_eq!(Url::parse(text), None, "{text} was allowed");
        }
    }

    #[test]
    fn redirects_resolve_relative_and_absolute_locations() {
        let base = url("https://api.github.com/repos/a/b/releases");
        assert_eq!(
            base.resolve("/repositories/1/releases"),
            Some(url("https://api.github.com/repositories/1/releases"))
        );
        assert_eq!(
            base.resolve("https://github.com/a/b"),
            Some(url("https://github.com/a/b"))
        );
        assert_eq!(base.resolve("//example.com/x"), None);
        assert_eq!(base.resolve("https://example.com/x"), None);
        assert_eq!(base.resolve("relative"), None);
    }

    #[test]
    fn requests_send_only_the_needed_headers() {
        let text = request_text(&url("https://api.github.com/x?y=1"), USER_AGENT);
        assert_eq!(
            text,
            "GET /x?y=1 HTTP/1.1\r\nHost: api.github.com\r\nUser-Agent: Hoplix/1.2.3\r\n\
             Connection: close\r\n\r\n"
        );
    }

    // Responses

    #[test]
    fn content_length_limits_the_body() {
        assert_eq!(
            body("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello world"),
            b"hello"
        );
        assert_eq!(
            body("HTTP/1.0 200 OK\r\nServer: x\r\n\r\nuntil end"),
            b"until end"
        );
        let error = parse_response(b"HTTP/1.1 200 OK\r\ncontent-length: 50\r\n\r\nshort")
            .expect_err("a truncated body was accepted");
        assert!(error.detail().contains("truncated"));
    }

    #[test]
    fn chunked_bodies_are_decoded() {
        assert_eq!(
            body(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n\
                 5\r\nhello\r\n7;name=x\r\n, world\r\nA\r\n and more!\r\n0\r\nTrailer: x\r\n\r\n"
            ),
            b"hello, world and more!"
        );
        for raw in [
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nhello\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloXX0\r\n\r\n",
        ] {
            assert!(
                parse_response(raw.as_bytes()).is_err(),
                "{raw:?} was accepted"
            );
        }
    }

    #[test]
    fn redirects_and_bad_statuses_are_reported() {
        assert_eq!(
            parse_response(b"HTTP/1.1 301 Moved\r\nLocation: /new\r\n\r\n").expect("failed"),
            Response::Redirect("/new".to_string())
        );
        assert!(parse_response(b"HTTP/1.1 302 Found\r\n\r\n").is_err());
        let error = parse_response(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .expect_err("403 was accepted");
        assert_eq!(error.detail(), "HTTP 403");
        for raw in [
            "garbage",
            "SPDY/3 200 OK\r\n\r\n",
            "HTTP/1.1 abc OK\r\n\r\n",
        ] {
            assert!(
                parse_response(raw.as_bytes()).is_err(),
                "{raw:?} was accepted"
            );
        }
    }

    #[test]
    fn large_responses_are_refused() {
        let declared = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            MAX_RESPONSE + 1
        );
        let error = parse_response(declared.as_bytes()).expect_err("accepted");
        assert_eq!(error.detail(), "response too large");
        let chunk = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
            MAX_RESPONSE + 1
        );
        let error = parse_response(chunk.as_bytes()).expect_err("accepted");
        assert_eq!(error.detail(), "response too large");
    }

    // Exchanges

    #[tokio::test]
    async fn redirects_are_followed_to_github_hosts_only() {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hosts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut responses = vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]".to_vec(),
            b"HTTP/1.1 307 Temporary\r\nLocation: https://github.com/b\r\n\r\n".to_vec(),
            b"HTTP/1.1 301 Moved\r\nLocation: /a?x=1\r\n\r\n".to_vec(),
        ];
        let body = get_with("https://api.github.com/start", USER_AGENT, |host| {
            hosts.lock().expect("poisoned").push(host);
            let response = responses.pop().expect("too many connections");
            let stream = server(response, Arc::clone(&requests));
            async move { Ok(stream) }
        })
        .await
        .expect("the request failed");
        assert_eq!(body, b"[]");
        assert_eq!(
            *hosts.lock().expect("poisoned"),
            ["api.github.com", "api.github.com", "github.com"]
        );
        let requests = requests.lock().expect("poisoned");
        assert!(requests[0].starts_with("GET /start HTTP/1.1\r\nHost: api.github.com\r\n"));
        assert!(requests[1].starts_with("GET /a?x=1 HTTP/1.1\r\n"));
        assert!(requests[2].starts_with("GET /b HTTP/1.1\r\nHost: github.com\r\n"));
        assert!(requests[2].contains("User-Agent: Hoplix/1.2.3\r\n"));
    }

    #[tokio::test]
    async fn foreign_and_endless_redirects_fail() {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let error = get_with("https://api.github.com/", USER_AGENT, |_| {
            let stream = server(
                b"HTTP/1.1 302 Found\r\nLocation: https://example.com/\r\n\r\n".to_vec(),
                Arc::clone(&requests),
            );
            async move { Ok(stream) }
        })
        .await
        .expect_err("a foreign redirect was followed");
        assert!(error.detail().contains("example.com"));

        let mut connections = 0;
        let error = get_with("https://api.github.com/", USER_AGENT, |_| {
            connections += 1;
            let stream = server(
                b"HTTP/1.1 302 Found\r\nLocation: /again\r\n\r\n".to_vec(),
                Arc::clone(&requests),
            );
            async move { Ok(stream) }
        })
        .await
        .expect_err("endless redirects were followed");
        assert_eq!(error.detail(), "too many redirects");
        assert_eq!(connections, MAX_REDIRECTS + 1);
    }

    #[tokio::test]
    async fn streamed_responses_over_the_limit_fail() {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut response = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
        response.resize(MAX_RESPONSE + 100, b'x');
        let error = get_with("https://api.github.com/", USER_AGENT, |_| {
            let stream = server(response.clone(), Arc::clone(&requests));
            async move { Ok(stream) }
        })
        .await
        .expect_err("a large response was accepted");
        assert_eq!(error.detail(), "response too large");
    }
}
