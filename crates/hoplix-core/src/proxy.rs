use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{Error, ErrorKind, Result};

// Constants

const SOCKS_VERSION: u8 = 5;
const SOCKS_NO_AUTHENTICATION: u8 = 0x00;
const SOCKS_PASSWORD_AUTHENTICATION: u8 = 0x02;
const SOCKS_NO_ACCEPTABLE_METHOD: u8 = 0xff;
const SOCKS_PASSWORD_VERSION: u8 = 1;
const SOCKS_CONNECT: u8 = 1;
const SOCKS_IPV4: u8 = 1;
const SOCKS_DOMAIN: u8 = 3;
const SOCKS_IPV6: u8 = 4;
const MAX_HTTP_HEADER: usize = 16 * 1024;

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyKind {
    #[default]
    Socks5,
    Http,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProxyCredentials {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub credentials: Option<ProxyCredentials>,
}

// Kinds

impl ProxyKind {
    pub const ALL: [Self; 2] = [Self::Socks5, Self::Http];

    pub fn default_port(self) -> u16 {
        match self {
            Self::Socks5 => 1080,
            Self::Http => 8080,
        }
    }

    pub fn scheme(self) -> &'static str {
        match self {
            Self::Socks5 => "socks5",
            Self::Http => "http",
        }
    }
}

// Proxies

impl Proxy {
    pub fn label(&self) -> String {
        if self.host.contains(':') {
            format!("{}://[{}]:{}", self.kind.scheme(), self.host, self.port)
        } else {
            format!("{}://{}:{}", self.kind.scheme(), self.host, self.port)
        }
    }

    pub async fn connect(&self, host: &str, port: u16) -> Result<TcpStream> {
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(|error| self.error(ErrorKind::ConnectionFailed, &error.to_string()))?;
        let _ = stream.set_nodelay(true);
        match self.kind {
            ProxyKind::Socks5 => {
                socks5_handshake(&mut stream, host, port, self.credentials.as_ref()).await
            }
            ProxyKind::Http => {
                http_connect_handshake(&mut stream, host, port, self.credentials.as_ref()).await
            }
        }
        .map_err(|error| self.error(error.kind(), error.detail()))?;
        Ok(stream)
    }

    fn error(&self, kind: ErrorKind, detail: &str) -> Error {
        Error::new(kind, format!("proxy {}: {detail}", self.label())).at_hop(self.label())
    }
}

pub(crate) async fn open_tcp(proxy: Option<&Proxy>, host: &str, port: u16) -> Result<TcpStream> {
    match proxy {
        Some(proxy) => proxy.connect(host, port).await,
        None => TcpStream::connect((host, port))
            .await
            .map_err(|error| Error::new(ErrorKind::ConnectionFailed, format!("{host}: {error}"))),
    }
}

impl fmt::Debug for ProxyCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProxyCredentials")
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

// SOCKS5

pub(crate) async fn socks5_handshake<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    credentials: Option<&ProxyCredentials>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request = socks5_connect_request(host, port)?;
    let greeting: &[u8] = if credentials.is_some() {
        &[
            SOCKS_VERSION,
            2,
            SOCKS_NO_AUTHENTICATION,
            SOCKS_PASSWORD_AUTHENTICATION,
        ]
    } else {
        &[SOCKS_VERSION, 1, SOCKS_NO_AUTHENTICATION]
    };
    stream.write_all(greeting).await.map_err(io_error)?;
    let mut choice = [0u8; 2];
    stream.read_exact(&mut choice).await.map_err(io_error)?;
    if choice[0] != SOCKS_VERSION {
        return Err(protocol_error("not a SOCKS5 proxy"));
    }
    match (choice[1], credentials) {
        (SOCKS_NO_AUTHENTICATION, _) => {}
        (SOCKS_PASSWORD_AUTHENTICATION, Some(credentials)) => {
            socks5_authenticate(stream, credentials).await?;
        }
        (SOCKS_NO_ACCEPTABLE_METHOD | SOCKS_PASSWORD_AUTHENTICATION, None) => {
            return Err(Error::new(
                ErrorKind::AuthenticationFailed,
                "the proxy needs a user name and password",
            ));
        }
        _ => return Err(protocol_error("no supported sign-in method")),
    }
    stream.write_all(&request).await.map_err(io_error)?;
    let mut reply = [0u8; 4];
    stream.read_exact(&mut reply).await.map_err(io_error)?;
    if reply[0] != SOCKS_VERSION {
        return Err(protocol_error("invalid reply"));
    }
    if reply[1] != 0 {
        return Err(Error::new(
            ErrorKind::ConnectionFailed,
            socks5_reply_text(reply[1]),
        ));
    }
    let skip = match reply[3] {
        SOCKS_IPV4 => 4,
        SOCKS_IPV6 => 16,
        SOCKS_DOMAIN => {
            let mut length = [0u8; 1];
            stream.read_exact(&mut length).await.map_err(io_error)?;
            usize::from(length[0])
        }
        _ => return Err(protocol_error("invalid address type in reply")),
    };
    let mut bound = vec![0u8; skip + 2];
    stream.read_exact(&mut bound).await.map_err(io_error)?;
    Ok(())
}

async fn socks5_authenticate<S>(stream: &mut S, credentials: &ProxyCredentials) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let username = credentials.username.as_bytes();
    let password = credentials.password.as_bytes();
    let (Ok(username_length), Ok(password_length)) =
        (u8::try_from(username.len()), u8::try_from(password.len()))
    else {
        return Err(Error::new(
            ErrorKind::AuthenticationFailed,
            "user name and password may have at most 255 bytes each",
        ));
    };
    let mut message = Vec::with_capacity(3 + username.len() + password.len());
    message.push(SOCKS_PASSWORD_VERSION);
    message.push(username_length);
    message.extend_from_slice(username);
    message.push(password_length);
    message.extend_from_slice(password);
    stream.write_all(&message).await.map_err(io_error)?;
    let mut status = [0u8; 2];
    stream.read_exact(&mut status).await.map_err(io_error)?;
    if status[1] == 0 {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::AuthenticationFailed,
            "the proxy rejected the user name or password",
        ))
    }
}

fn socks5_connect_request(host: &str, port: u16) -> Result<Vec<u8>> {
    let mut request = vec![SOCKS_VERSION, SOCKS_CONNECT, 0];
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(address) = bare.parse::<Ipv4Addr>() {
        request.push(SOCKS_IPV4);
        request.extend_from_slice(&address.octets());
    } else if let Ok(address) = bare.parse::<Ipv6Addr>() {
        request.push(SOCKS_IPV6);
        request.extend_from_slice(&address.octets());
    } else {
        let length = u8::try_from(host.len())
            .ok()
            .filter(|length| *length > 0)
            .ok_or_else(|| Error::new(ErrorKind::ConnectionFailed, "invalid host name"))?;
        request.push(SOCKS_DOMAIN);
        request.push(length);
        request.extend_from_slice(host.as_bytes());
    }
    request.extend_from_slice(&port.to_be_bytes());
    Ok(request)
}

fn socks5_reply_text(code: u8) -> String {
    match code {
        1 => "general failure".to_string(),
        2 => "connection not allowed by the proxy rules".to_string(),
        3 => "network unreachable".to_string(),
        4 => "host unreachable".to_string(),
        5 => "connection refused".to_string(),
        6 => "TTL expired".to_string(),
        7 => "command not supported".to_string(),
        8 => "address type not supported".to_string(),
        other => format!("error {other}"),
    }
}

// HTTP CONNECT

pub(crate) async fn http_connect_handshake<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    credentials: Option<&ProxyCredentials>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if host.is_empty()
        || host
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(Error::new(ErrorKind::ConnectionFailed, "invalid host name"));
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let authority = if bare.contains(':') {
        format!("[{bare}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(credentials) = credentials {
        if credentials.username.contains(':')
            || credentials
                .username
                .chars()
                .chain(credentials.password.chars())
                .any(char::is_control)
        {
            return Err(Error::new(
                ErrorKind::AuthenticationFailed,
                "the user name may not contain a colon or control characters",
            ));
        }
        let token = base64(format!("{}:{}", credentials.username, credentials.password).as_bytes());
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(io_error)?;
    let header = read_http_header(stream).await?;
    let status = http_status(&header)?;
    match status {
        200..=299 => Ok(()),
        401 | 407 => Err(Error::new(
            ErrorKind::AuthenticationFailed,
            if credentials.is_some() {
                "the proxy rejected the user name or password"
            } else {
                "the proxy needs a user name and password"
            },
        )),
        _ => Err(Error::new(
            ErrorKind::ConnectionFailed,
            format!("the proxy answered with status {status}"),
        )),
    }
}

async fn read_http_header<S>(stream: &mut S) -> Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= MAX_HTTP_HEADER {
            return Err(protocol_error("the proxy response is too long"));
        }
        let read = stream.read(&mut byte).await.map_err(io_error)?;
        if read == 0 {
            return Err(protocol_error("the proxy closed the connection"));
        }
        header.push(byte[0]);
    }
    Ok(header)
}

fn http_status(header: &[u8]) -> Result<u16> {
    let text = std::str::from_utf8(header).map_err(|_| protocol_error("invalid response"))?;
    let line = text.lines().next().unwrap_or_default();
    let mut parts = line.split(' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(protocol_error("invalid response"));
    }
    parts
        .next()
        .filter(|code| code.len() == 3)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| protocol_error("invalid response"))
}

pub(crate) fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let value = u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (value >> (18 - 6 * index)) & 0x3f;
                output.push(char::from(ALPHABET[sextet as usize]));
            } else {
                output.push('=');
            }
        }
    }
    output
}

// Errors

fn io_error(error: std::io::Error) -> Error {
    Error::new(ErrorKind::ConnectionFailed, error.to_string())
}

fn protocol_error(detail: &str) -> Error {
    Error::new(ErrorKind::ConnectionFailed, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_password;
    use tokio::io::{DuplexStream, duplex};

    fn credentials() -> ProxyCredentials {
        ProxyCredentials {
            username: "tester".to_string(),
            password: test_password(),
        }
    }

    async fn read_bytes(server: &mut DuplexStream, count: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; count];
        server.read_exact(&mut buffer).await.expect("readable");
        buffer
    }

    async fn read_request(server: &mut DuplexStream) -> String {
        String::from_utf8(read_http_header(server).await.expect("header")).expect("utf8")
    }

    async fn socks_reply(server: &mut DuplexStream, code: u8) {
        server
            .write_all(&[SOCKS_VERSION, code, 0, SOCKS_IPV4, 10, 0, 0, 1, 0x1f, 0x90])
            .await
            .expect("writable");
    }

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(
            base64(b"Aladdin:open sesame"),
            "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
    }

    #[test]
    fn labels_and_debug_output_hide_secrets() {
        let credentials = credentials();
        let password = credentials.password.clone();
        let proxy = Proxy {
            kind: ProxyKind::Socks5,
            host: "::1".to_string(),
            port: 1080,
            credentials: Some(credentials),
        };
        assert_eq!(proxy.label(), "socks5://[::1]:1080");
        let output = format!("{proxy:?}");
        assert!(!output.contains(&password));
        assert!(output.contains("tester"));
        assert_eq!(ProxyKind::Http.default_port(), 8080);
        assert_eq!(ProxyKind::ALL.len(), 2);
    }

    #[tokio::test]
    async fn socks5_connects_without_authentication_by_domain() {
        let (mut client, mut server) = duplex(1024);
        let serve = async {
            assert_eq!(read_bytes(&mut server, 3).await, [5, 1, 0]);
            server.write_all(&[5, 0]).await.expect("writable");
            let mut expected = vec![5, 1, 0, 3, 12];
            expected.extend_from_slice(b"example.test");
            expected.extend_from_slice(&22u16.to_be_bytes());
            assert_eq!(read_bytes(&mut server, expected.len()).await, expected);
            socks_reply(&mut server, 0).await;
            server.write_all(b"SSH-2.0").await.expect("writable");
        };
        let (result, ()) = tokio::join!(
            socks5_handshake(&mut client, "example.test", 22, None),
            serve
        );
        result.expect("connected");
        assert_eq!(read_bytes(&mut client, 7).await, b"SSH-2.0".to_vec());
    }

    #[tokio::test]
    async fn socks5_signs_in_with_user_and_password() {
        let credentials = credentials();
        let password = credentials.password.clone();
        let (mut client, mut server) = duplex(1024);
        let serve = async {
            assert_eq!(read_bytes(&mut server, 4).await, [5, 2, 0, 2]);
            server.write_all(&[5, 2]).await.expect("writable");
            let mut expected = vec![1, 6];
            expected.extend_from_slice(b"tester");
            expected.push(u8::try_from(password.len()).expect("short"));
            expected.extend_from_slice(password.as_bytes());
            assert_eq!(read_bytes(&mut server, expected.len()).await, expected);
            server.write_all(&[1, 0]).await.expect("writable");
            let mut expected = vec![5, 1, 0, 1, 192, 0, 2, 7];
            expected.extend_from_slice(&21u16.to_be_bytes());
            assert_eq!(read_bytes(&mut server, expected.len()).await, expected);
            socks_reply(&mut server, 0).await;
        };
        let (result, ()) = tokio::join!(
            socks5_handshake(&mut client, "192.0.2.7", 21, Some(&credentials)),
            serve
        );
        result.expect("connected");
    }

    #[tokio::test]
    async fn socks5_sends_ipv6_addresses() {
        let (mut client, mut server) = duplex(1024);
        let serve = async {
            read_bytes(&mut server, 3).await;
            server.write_all(&[5, 0]).await.expect("writable");
            let mut expected = vec![5, 1, 0, 4];
            expected.extend_from_slice(&"2001:db8::5".parse::<Ipv6Addr>().unwrap().octets());
            expected.extend_from_slice(&990u16.to_be_bytes());
            assert_eq!(read_bytes(&mut server, expected.len()).await, expected);
            let mut reply = vec![5, 0, 0, 4];
            reply.extend_from_slice(&[0u8; 18]);
            server.write_all(&reply).await.expect("writable");
        };
        let (result, ()) = tokio::join!(
            socks5_handshake(&mut client, "[2001:db8::5]", 990, None),
            serve
        );
        result.expect("connected");
    }

    #[tokio::test]
    async fn socks5_reports_rejections() {
        let (mut client, mut server) = duplex(1024);
        let serve = async {
            read_bytes(&mut server, 3).await;
            server.write_all(&[5, 0]).await.expect("writable");
            read_bytes(&mut server, 5 + 12 + 2).await;
            socks_reply(&mut server, 5).await;
        };
        let (result, ()) = tokio::join!(
            socks5_handshake(&mut client, "example.test", 22, None),
            serve
        );
        let error = result.expect_err("refused");
        assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
        assert_eq!(error.detail(), "connection refused");
    }

    #[tokio::test]
    async fn socks5_reports_missing_and_wrong_credentials() {
        let (mut client, mut server) = duplex(1024);
        let serve = async {
            read_bytes(&mut server, 3).await;
            server.write_all(&[5, 0xff]).await.expect("writable");
        };
        let (result, ()) = tokio::join!(
            socks5_handshake(&mut client, "example.test", 22, None),
            serve
        );
        assert_eq!(
            result.expect_err("refused").kind(),
            ErrorKind::AuthenticationFailed
        );

        let credentials = credentials();
        let (mut client, mut server) = duplex(1024);
        let serve = async {
            read_bytes(&mut server, 4).await;
            server.write_all(&[5, 2]).await.expect("writable");
            read_bytes(&mut server, 3 + 6 + credentials.password.len()).await;
            server.write_all(&[1, 1]).await.expect("writable");
        };
        let (result, ()) = tokio::join!(
            socks5_handshake(&mut client, "example.test", 22, Some(&credentials)),
            serve
        );
        assert_eq!(
            result.expect_err("rejected").kind(),
            ErrorKind::AuthenticationFailed
        );
    }

    #[tokio::test]
    async fn socks5_rejects_other_servers() {
        let (mut client, mut server) = duplex(1024);
        let serve = async {
            read_bytes(&mut server, 3).await;
            server.write_all(b"HTTP/1.1 400").await.expect("writable");
        };
        let (result, ()) = tokio::join!(
            socks5_handshake(&mut client, "example.test", 22, None),
            serve
        );
        assert_eq!(result.expect_err("invalid").detail(), "not a SOCKS5 proxy");
        let long = "a".repeat(256);
        let (mut client, _server) = duplex(1024);
        assert!(
            socks5_handshake(&mut client, &long, 22, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn http_connect_accepts_200_and_keeps_the_tunnel_data() {
        let (mut client, mut server) = duplex(4096);
        let serve = async {
            let request = read_request(&mut server).await;
            assert_eq!(
                request,
                "CONNECT example.test:22 HTTP/1.1\r\nHost: example.test:22\r\n\r\n"
            );
            server
                .write_all(b"HTTP/1.1 200 Connection established\r\nVia: test\r\n\r\nSSH-2.0")
                .await
                .expect("writable");
        };
        let (result, ()) = tokio::join!(
            http_connect_handshake(&mut client, "example.test", 22, None),
            serve
        );
        result.expect("connected");
        assert_eq!(read_bytes(&mut client, 7).await, b"SSH-2.0".to_vec());
    }

    #[tokio::test]
    async fn http_connect_sends_basic_credentials_and_brackets_ipv6() {
        let credentials = credentials();
        let token = base64(format!("tester:{}", credentials.password).as_bytes());
        let (mut client, mut server) = duplex(4096);
        let serve = async {
            let request = read_request(&mut server).await;
            assert_eq!(
                request,
                format!(
                    "CONNECT [::1]:21 HTTP/1.1\r\nHost: [::1]:21\r\nProxy-Authorization: Basic {token}\r\n\r\n"
                )
            );
            server
                .write_all(b"HTTP/1.0 200 OK\r\n\r\n")
                .await
                .expect("writable");
        };
        let (result, ()) = tokio::join!(
            http_connect_handshake(&mut client, "::1", 21, Some(&credentials)),
            serve
        );
        result.expect("connected");
    }

    #[tokio::test]
    async fn http_connect_reports_407_and_other_statuses() {
        for (status, kind) in [
            (
                "407 Proxy Authentication Required",
                ErrorKind::AuthenticationFailed,
            ),
            ("401 Unauthorized", ErrorKind::AuthenticationFailed),
            ("403 Forbidden", ErrorKind::ConnectionFailed),
        ] {
            let (mut client, mut server) = duplex(4096);
            let serve = async {
                read_request(&mut server).await;
                server
                    .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
                    .await
                    .expect("writable");
            };
            let (result, ()) = tokio::join!(
                http_connect_handshake(&mut client, "example.test", 22, None),
                serve
            );
            assert_eq!(result.expect_err("rejected").kind(), kind);
        }
    }

    #[tokio::test]
    async fn http_connect_rejects_malformed_responses() {
        for response in [
            &b"SSH-2.0-OpenSSH\r\n\r\n"[..],
            b"HTTP/1.1 abc\r\n\r\n",
            b"HTTP/1.1 200",
        ] {
            let (mut client, mut server) = duplex(4096);
            let serve = async {
                read_request(&mut server).await;
                server.write_all(response).await.expect("writable");
                drop(server);
            };
            let (result, ()) = tokio::join!(
                http_connect_handshake(&mut client, "example.test", 22, None),
                serve
            );
            assert_eq!(
                result.expect_err("malformed").kind(),
                ErrorKind::ConnectionFailed
            );
        }
        let (mut client, _server) = duplex(4096);
        let injected = http_connect_handshake(&mut client, "host\r\nX: y", 22, None).await;
        assert!(injected.is_err());
    }
}
