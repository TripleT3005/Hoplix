mod auth;
mod client;

use std::io;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufStream};

use crate::{Error, ErrorKind, Result};

pub(crate) use auth::Credentials;
pub(crate) use client::{Client, Endpoint, Request, Response};

// Constants

const MAX_HEAD: usize = 64 * 1024;
const MAX_LINE: u64 = 16 * 1024;
const BUFFER_SIZE: usize = 64 * 1024;

// Types

pub(crate) trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Head {
    pub status: u16,
    pub persistent: bool,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framing {
    Empty,
    Length(u64),
    Chunked,
    UntilClose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Body {
    framing: Framing,
    remaining: u64,
    started: bool,
    finished: bool,
}

pub(crate) struct Connection {
    stream: BufStream<Box<dyn Stream>>,
}

// Heads

pub(crate) fn parse_head(raw: &[u8]) -> Result<Head> {
    let text = String::from_utf8_lossy(raw);
    let mut lines = text.split('\n').map(|line| line.trim_end_matches('\r'));
    let status = lines.next().unwrap_or_default();
    let mut parts = status.splitn(3, ' ');
    let (Some(version), Some(code)) = (parts.next(), parts.next()) else {
        return Err(malformed("status line"));
    };
    let minor = match version {
        "HTTP/1.1" => 1,
        "HTTP/1.0" => 0,
        _ => return Err(malformed("status line")),
    };
    let status = code
        .parse::<u16>()
        .ok()
        .filter(|_| code.len() == 3)
        .ok_or_else(|| malformed("status code"))?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let mut head = Head {
        status,
        persistent: true,
        headers,
    };
    head.persistent = if minor == 0 {
        head.has_token("connection", "keep-alive")
    } else {
        !head.has_token("connection", "close")
    };
    Ok(head)
}

impl Head {
    pub fn header<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        self.values(name).next()
    }

    pub fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn has_token(&self, name: &str, token: &str) -> bool {
        self.values(name)
            .flat_map(|value| value.split(','))
            .any(|part| part.trim().eq_ignore_ascii_case(token))
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn is_redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308)
    }

    pub fn framing(&self, head_request: bool) -> Result<Framing> {
        if head_request || matches!(self.status, 100..=199 | 204 | 304) {
            return Ok(Framing::Empty);
        }
        let chunked = self
            .values("transfer-encoding")
            .flat_map(|value| value.split(','))
            .last()
            .is_some_and(|coding| coding.trim().eq_ignore_ascii_case("chunked"));
        if chunked {
            return Ok(Framing::Chunked);
        }
        match self.header("content-length") {
            Some(length) => length
                .parse::<u64>()
                .map(Framing::Length)
                .map_err(|_| malformed("content length")),
            None => Ok(Framing::UntilClose),
        }
    }
}

// Bodies

impl Body {
    pub fn new(framing: Framing) -> Self {
        let remaining = match framing {
            Framing::Length(length) => length,
            _ => 0,
        };
        Self {
            framing,
            remaining,
            started: false,
            finished: framing == Framing::Empty || framing == Framing::Length(0),
        }
    }

    pub fn is_reusable(&self) -> bool {
        self.finished && self.framing != Framing::UntilClose
    }
}

// Connections

impl Connection {
    pub fn new(stream: Box<dyn Stream>) -> Self {
        Self {
            stream: BufStream::with_capacity(BUFFER_SIZE, BUFFER_SIZE, stream),
        }
    }

    pub async fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes).await
    }

    pub async fn flush(&mut self) -> io::Result<()> {
        self.stream.flush().await
    }

    pub async fn send_chunk(&mut self, data: &[u8]) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.stream
            .write_all(format!("{:x}\r\n", data.len()).as_bytes())
            .await?;
        self.stream.write_all(data).await?;
        self.stream.write_all(b"\r\n").await
    }

    pub async fn finish_chunks(&mut self) -> io::Result<()> {
        self.stream.write_all(b"0\r\n\r\n").await?;
        self.stream.flush().await
    }

    pub async fn is_alive(&mut self) -> bool {
        tokio::time::timeout(Duration::ZERO, self.stream.fill_buf())
            .await
            .is_err()
    }

    pub async fn answered_within(&mut self, wait: Duration) -> bool {
        tokio::time::timeout(wait, self.stream.fill_buf())
            .await
            .is_ok()
    }

    pub async fn read_head(&mut self) -> Result<Head> {
        loop {
            let head = self.read_any_head().await?;
            if !(100..200).contains(&head.status) {
                return Ok(head);
            }
        }
    }

    pub async fn read_any_head(&mut self) -> Result<Head> {
        let mut raw = Vec::new();
        loop {
            let line = self.read_line().await.map_err(|error| io_error(&error))?;
            if line.is_empty() {
                if raw.is_empty() {
                    continue;
                }
                return parse_head(&raw);
            }
            raw.extend_from_slice(&line);
            raw.extend_from_slice(b"\r\n");
            if raw.len() > MAX_HEAD {
                return Err(malformed("header too long"));
            }
        }
    }

    async fn read_line(&mut self) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        (&mut self.stream)
            .take(MAX_LINE)
            .read_until(b'\n', &mut line)
            .await?;
        if line.pop() != Some(b'\n') {
            return Err(if line.len() as u64 >= MAX_LINE - 1 {
                io::Error::new(io::ErrorKind::InvalidData, "line too long")
            } else {
                io::Error::from(io::ErrorKind::UnexpectedEof)
            });
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Ok(line)
    }

    pub async fn read_body(&mut self, body: &mut Body, buffer: &mut [u8]) -> io::Result<usize> {
        if body.finished || buffer.is_empty() {
            return Ok(0);
        }
        match body.framing {
            Framing::Empty => {
                body.finished = true;
                Ok(0)
            }
            Framing::UntilClose => {
                let read = self.stream.read(buffer).await?;
                body.finished = read == 0;
                Ok(read)
            }
            Framing::Length(_) => self.read_counted(body, buffer).await,
            Framing::Chunked => {
                if body.remaining == 0 {
                    if body.started && !self.read_line().await?.is_empty() {
                        return Err(invalid("chunk end"));
                    }
                    let line = self.read_line().await?;
                    let text = String::from_utf8_lossy(&line);
                    let size = text.split(';').next().unwrap_or_default().trim();
                    let size = u64::from_str_radix(size, 16).map_err(|_| invalid("chunk size"))?;
                    body.started = true;
                    if size == 0 {
                        while !self.read_line().await?.is_empty() {}
                        body.finished = true;
                        return Ok(0);
                    }
                    body.remaining = size;
                }
                self.read_counted(body, buffer).await
            }
        }
    }

    async fn read_counted(&mut self, body: &mut Body, buffer: &mut [u8]) -> io::Result<usize> {
        let wanted = usize::try_from(body.remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = self.stream.read(&mut buffer[..wanted]).await?;
        if read == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        body.remaining -= read as u64;
        if body.remaining == 0 && matches!(body.framing, Framing::Length(_)) {
            body.finished = true;
        }
        Ok(read)
    }

    pub async fn read_to_end(&mut self, body: &mut Body, limit: usize) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        let mut buffer = vec![0; BUFFER_SIZE];
        loop {
            let read = self
                .read_body(body, &mut buffer)
                .await
                .map_err(|error| io_error(&error))?;
            if read == 0 {
                return Ok(data);
            }
            if data.len() + read > limit {
                return Err(Error::new(ErrorKind::Other, "response too large"));
            }
            data.extend_from_slice(&buffer[..read]);
        }
    }
}

// Errors

pub(crate) fn io_error(error: &io::Error) -> Error {
    Error::new(ErrorKind::ConnectionFailed, error.to_string())
}

fn malformed(part: &str) -> Error {
    Error::new(ErrorKind::Other, format!("invalid HTTP response: {part}"))
}

fn invalid(part: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid HTTP response: {part}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(data: &[u8]) -> Connection {
        let (client, mut server) = tokio::io::duplex(1024 * 1024);
        let data = data.to_vec();
        tokio::spawn(async move {
            let _ = server.write_all(&data).await;
            let _ = server.shutdown().await;
            let mut sink = Vec::new();
            let _ = server.read_to_end(&mut sink).await;
        });
        Connection::new(Box::new(client))
    }

    async fn read_all(connection: &mut Connection, head_request: bool) -> (Head, Vec<u8>, Body) {
        let head = connection.read_head().await.expect("no head");
        let mut body = Body::new(head.framing(head_request).expect("framing"));
        let data = connection
            .read_to_end(&mut body, 1024)
            .await
            .expect("no body");
        (head, data, body)
    }

    #[test]
    fn heads_name_status_headers_and_persistence() {
        let head = parse_head(
            b"HTTP/1.1 207 Multi-Status\r\nContent-Type: text/xml\r\nDAV: 1\r\nDAV: 2\r\n",
        )
        .expect("valid");
        assert_eq!(head.status, 207);
        assert!(head.persistent && head.is_success());
        assert_eq!(head.header("content-type"), Some("text/xml"));
        assert_eq!(head.values("dav").collect::<Vec<_>>(), ["1", "2"]);
        let head =
            parse_head(b"HTTP/1.1 301 Moved\r\nConnection: Keep-Alive, Close\r\n").expect("valid");
        assert!(!head.persistent && head.is_redirect());
        let head = parse_head(b"HTTP/1.0 200 OK\r\n").expect("valid");
        assert!(!head.persistent);
        let head = parse_head(b"HTTP/1.0 200 OK\r\nConnection: keep-alive\r\n").expect("valid");
        assert!(head.persistent);
        for raw in ["garbage", "HTTP/2 200 OK", "HTTP/1.1 20 OK", "HTTP/1.1 abc"] {
            assert!(parse_head(raw.as_bytes()).is_err(), "{raw}");
        }
    }

    #[test]
    fn framing_follows_the_headers() {
        let head = |raw: &str| parse_head(raw.as_bytes()).expect("valid");
        assert_eq!(
            head("HTTP/1.1 200 OK\r\nContent-Length: 5").framing(false),
            Ok(Framing::Length(5))
        );
        assert_eq!(
            head("HTTP/1.1 200 OK\r\nContent-Length: 5").framing(true),
            Ok(Framing::Empty)
        );
        assert_eq!(
            head("HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\nContent-Length: 5")
                .framing(false),
            Ok(Framing::Chunked)
        );
        assert_eq!(
            head("HTTP/1.1 204 No Content").framing(false),
            Ok(Framing::Empty)
        );
        assert_eq!(
            head("HTTP/1.1 200 OK").framing(false),
            Ok(Framing::UntilClose)
        );
        assert!(
            head("HTTP/1.1 200 OK\r\nContent-Length: x")
                .framing(false)
                .is_err()
        );
    }

    #[tokio::test]
    async fn bodies_are_read_by_length_chunks_and_close() {
        let mut stream = connection(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello\
              HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;x=1\r\nhello\r\n7\r\n, world\r\n0\r\nTrailer: y\r\n\r\n\
              HTTP/1.1 204 No Content\r\n\r\n\
              HTTP/1.0 200 OK\r\n\r\nuntil the end",
        );
        let (head, data, body) = read_all(&mut stream, false).await;
        assert_eq!((head.status, data.as_slice()), (200, b"hello".as_slice()));
        assert!(body.is_reusable());
        let (_, data, body) = read_all(&mut stream, false).await;
        assert_eq!(data, b"hello, world");
        assert!(body.is_reusable());
        let (head, data, _) = read_all(&mut stream, false).await;
        assert_eq!((head.status, data.len()), (204, 0));
        let (head, data, body) = read_all(&mut stream, false).await;
        assert_eq!(data, b"until the end");
        assert!(!head.persistent && body.finished && !body.is_reusable());
    }

    #[tokio::test]
    async fn broken_bodies_fail() {
        for raw in [
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nhello\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloXX0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel",
        ] {
            let mut stream = connection(raw.as_bytes());
            let head = stream.read_head().await.expect("no head");
            let mut body = Body::new(head.framing(false).expect("framing"));
            assert!(stream.read_to_end(&mut body, 1024).await.is_err(), "{raw}");
        }
        let mut stream = connection(b"HTTP/1.1 200 OK\r\nContent-Length: 2048\r\n\r\n");
        let head = stream.read_head().await.expect("no head");
        let mut body = Body::new(head.framing(false).expect("framing"));
        assert!(stream.read_to_end(&mut body, 16).await.is_err());
    }

    #[tokio::test]
    async fn chunks_are_framed_for_uploads() {
        let (client, mut server) = tokio::io::duplex(4096);
        let mut stream = Connection::new(Box::new(client));
        stream.send(b"PUT / HTTP/1.1\r\n\r\n").await.expect("sent");
        stream.send_chunk(b"hello").await.expect("sent");
        stream.send_chunk(b"").await.expect("sent");
        stream.send_chunk(&[b'x'; 20]).await.expect("sent");
        stream.finish_chunks().await.expect("sent");
        drop(stream);
        let mut sent = Vec::new();
        server.read_to_end(&mut sent).await.expect("read");
        assert_eq!(
            String::from_utf8_lossy(&sent),
            format!(
                "PUT / HTTP/1.1\r\n\r\n5\r\nhello\r\n14\r\n{}\r\n0\r\n\r\n",
                "x".repeat(20)
            )
        );
    }

    #[tokio::test]
    async fn idle_connections_notice_a_closed_peer() {
        let (client, server) = tokio::io::duplex(64);
        let mut stream = Connection::new(Box::new(client));
        assert!(stream.is_alive().await);
        assert!(!stream.answered_within(Duration::from_millis(10)).await);
        drop(server);
        assert!(!stream.is_alive().await);
    }
}
