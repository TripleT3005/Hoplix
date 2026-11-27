use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::BoxFuture;

// Constants

const MAX_HEAD: usize = 64 * 1024;
const MAX_CHUNK_LINE: usize = 4096;
const READ_SIZE: usize = 64 * 1024;

// Types

pub(crate) trait Duplex: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Duplex for T {}

pub(crate) type Stream = Box<dyn Duplex>;

pub(crate) type Connector = Arc<dyn Fn() -> BoxFuture<'static, crate::Result<Stream>> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub method: &'static str,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framing {
    Length(u64),
    Chunked(ChunkState),
    UntilClose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkState {
    Size,
    Data(u64),
    DataEnd,
    Trailer,
    Done,
}

struct Source {
    stream: Stream,
    buffer: Vec<u8>,
    start: usize,
}

pub(crate) struct Body {
    source: Source,
    framing: Framing,
    reusable: bool,
}

// Requests

impl Request {
    pub(crate) fn head_text(&self) -> String {
        let mut text = format!("{} {} HTTP/1.1\r\n", self.method, self.target);
        for (name, value) in &self.headers {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
        if !self.body.is_empty() || matches!(self.method, "PUT" | "POST") {
            text.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        }
        text.push_str("\r\n");
        text
    }
}

pub(crate) async fn send(stream: &mut Stream, request: &Request) -> io::Result<()> {
    let mut bytes = request.head_text().into_bytes();
    if request.body.len() <= READ_SIZE {
        bytes.extend_from_slice(&request.body);
        stream.write_all(&bytes).await?;
    } else {
        stream.write_all(&bytes).await?;
        stream.write_all(&request.body).await?;
    }
    stream.flush().await
}

// Responses

impl Head {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn has_token(&self, name: &str, token: &str) -> bool {
        self.header(name).is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case(token))
        })
    }

    fn parse(text: &str) -> io::Result<Self> {
        let mut lines = text.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        let mut parts = status_line.splitn(3, ' ');
        let protocol = parts.next().unwrap_or_default();
        if !protocol.starts_with("HTTP/1.") {
            return Err(invalid("status line"));
        }
        let status = parts
            .next()
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| invalid("status code"))?;
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        Ok(Self { status, headers })
    }
}

pub(crate) async fn receive(stream: Stream, head_only: bool) -> io::Result<(Head, Body)> {
    let mut source = Source {
        stream,
        buffer: Vec::new(),
        start: 0,
    };
    let head = loop {
        let head = source.read_head().await?;
        if !(100..200).contains(&head.status) {
            break head;
        }
    };
    let chunked = head.header("transfer-encoding").is_some_and(|value| {
        value
            .rsplit(',')
            .next()
            .is_some_and(|coding| coding.trim().eq_ignore_ascii_case("chunked"))
    });
    let framing = if head_only || matches!(head.status, 204 | 304) {
        Framing::Length(0)
    } else if chunked {
        Framing::Chunked(ChunkState::Size)
    } else if let Some(length) = head.header("content-length") {
        Framing::Length(length.parse().map_err(|_| invalid("content length"))?)
    } else {
        Framing::UntilClose
    };
    let reusable = framing != Framing::UntilClose && !head.has_token("connection", "close");
    Ok((
        head,
        Body {
            source,
            framing,
            reusable,
        },
    ))
}

impl Source {
    fn buffered(&self) -> &[u8] {
        &self.buffer[self.start..]
    }

    fn consume(&mut self, count: usize) {
        self.start += count;
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        }
    }

    fn poll_more(&mut self, context: &mut Context<'_>) -> Poll<io::Result<usize>> {
        if self.start > 0 {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        let mut chunk = [0u8; READ_SIZE];
        let mut read = ReadBuf::new(&mut chunk);
        ready!(Pin::new(&mut self.stream).poll_read(context, &mut read))?;
        let filled = read.filled();
        self.buffer.extend_from_slice(filled);
        Poll::Ready(Ok(filled.len()))
    }

    async fn read_head(&mut self) -> io::Result<Head> {
        loop {
            if let Some(end) = find(self.buffered(), b"\r\n\r\n") {
                let text = std::str::from_utf8(&self.buffered()[..end])
                    .map_err(|_| invalid("header"))?
                    .to_string();
                self.consume(end + 4);
                return Head::parse(&text);
            }
            if self.buffered().len() > MAX_HEAD {
                return Err(invalid("header too large"));
            }
            let read = std::future::poll_fn(|context| self.poll_more(context)).await?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the server closed the connection",
                ));
            }
        }
    }
}

// Bodies

impl Body {
    pub(crate) fn is_done(&self) -> bool {
        matches!(
            self.framing,
            Framing::Length(0) | Framing::Chunked(ChunkState::Done)
        )
    }

    pub(crate) fn into_stream(self) -> Option<Stream> {
        (self.reusable && self.is_done() && self.source.buffered().is_empty())
            .then_some(self.source.stream)
    }

    pub(crate) async fn read_all(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut data = Vec::new();
        let mut chunk = vec![0u8; READ_SIZE];
        loop {
            let read = self.read(&mut chunk).await?;
            if read == 0 {
                return Ok(data);
            }
            data.extend_from_slice(&chunk[..read]);
            if data.len() > limit {
                return Err(invalid("response too large"));
            }
        }
    }

    fn poll_chunked(
        &mut self,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let Framing::Chunked(state) = self.framing else {
                return Poll::Ready(Ok(()));
            };
            let available = self.source.buffered();
            match state {
                ChunkState::Done => return Poll::Ready(Ok(())),
                ChunkState::Size | ChunkState::Trailer => {
                    if let Some(end) = find(available, b"\r\n") {
                        let line = std::str::from_utf8(&available[..end])
                            .map_err(|_| invalid("chunk"))?
                            .to_string();
                        self.source.consume(end + 2);
                        self.framing = Framing::Chunked(match state {
                            ChunkState::Size => {
                                let size = line.split(';').next().unwrap_or_default().trim();
                                let size = u64::from_str_radix(size, 16)
                                    .map_err(|_| invalid("chunk size"))?;
                                if size == 0 {
                                    ChunkState::Trailer
                                } else {
                                    ChunkState::Data(size)
                                }
                            }
                            _ if line.is_empty() => ChunkState::Done,
                            _ => ChunkState::Trailer,
                        });
                        continue;
                    }
                    if available.len() > MAX_CHUNK_LINE {
                        return Poll::Ready(Err(invalid("chunk")));
                    }
                }
                ChunkState::DataEnd => {
                    if available.len() >= 2 {
                        if &available[..2] != b"\r\n" {
                            return Poll::Ready(Err(invalid("chunk end")));
                        }
                        self.source.consume(2);
                        self.framing = Framing::Chunked(ChunkState::Size);
                        continue;
                    }
                }
                ChunkState::Data(remaining) => {
                    if !available.is_empty() {
                        let count = available
                            .len()
                            .min(output.remaining())
                            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
                        output.put_slice(&available[..count]);
                        self.source.consume(count);
                        let left = remaining - count as u64;
                        self.framing = Framing::Chunked(if left == 0 {
                            ChunkState::DataEnd
                        } else {
                            ChunkState::Data(left)
                        });
                        return Poll::Ready(Ok(()));
                    }
                }
            }
            if ready!(self.source.poll_more(context))? == 0 {
                return Poll::Ready(Err(truncated()));
            }
        }
    }
}

impl AsyncRead for Body {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        match this.framing {
            Framing::Chunked(_) => this.poll_chunked(context, output),
            Framing::Length(0) => Poll::Ready(Ok(())),
            Framing::Length(remaining) => {
                if this.source.buffered().is_empty() && ready!(this.source.poll_more(context))? == 0
                {
                    return Poll::Ready(Err(truncated()));
                }
                let available = this.source.buffered();
                let count = available
                    .len()
                    .min(output.remaining())
                    .min(usize::try_from(remaining).unwrap_or(usize::MAX));
                output.put_slice(&available[..count]);
                this.source.consume(count);
                this.framing = Framing::Length(remaining - count as u64);
                Poll::Ready(Ok(()))
            }
            Framing::UntilClose => {
                if this.source.buffered().is_empty() && ready!(this.source.poll_more(context))? == 0
                {
                    return Poll::Ready(Ok(()));
                }
                let available = this.source.buffered();
                let count = available.len().min(output.remaining());
                output.put_slice(&available[..count]);
                this.source.consume(count);
                Poll::Ready(Ok(()))
            }
        }
    }
}

// Helpers

pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn invalid(part: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid HTTP response: {part}"),
    )
}

fn truncated() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the server closed the connection early",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn respond(raw: &[u8], head_only: bool) -> (Head, Body) {
        let (client, mut server) = tokio::io::duplex(1024);
        let raw = raw.to_vec();
        tokio::spawn(async move {
            for piece in raw.chunks(7) {
                if server.write_all(piece).await.is_err() {
                    return;
                }
            }
        });
        receive(Box::new(client), head_only)
            .await
            .expect("receiving failed")
    }

    #[test]
    fn requests_carry_their_headers_and_length() {
        let request = Request {
            method: "PUT",
            target: "/bucket/a%20b?uploadId=1".to_string(),
            headers: vec![("Host".to_string(), "s3.example.test".to_string())],
            body: b"data".to_vec(),
        };
        assert_eq!(
            request.head_text(),
            "PUT /bucket/a%20b?uploadId=1 HTTP/1.1\r\nHost: s3.example.test\r\n\
             Content-Length: 4\r\n\r\n"
        );
        let empty = Request {
            method: "GET",
            body: Vec::new(),
            ..request.clone()
        };
        assert!(!empty.head_text().contains("Content-Length"));
        let post = Request {
            method: "POST",
            body: Vec::new(),
            ..request
        };
        assert!(post.head_text().contains("Content-Length: 0\r\n"));
    }

    #[tokio::test]
    async fn length_framed_bodies_stop_at_the_length() {
        let (head, mut body) = respond(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 5\r\nETag: \"x\"\r\n\r\nhello",
            false,
        )
        .await;
        assert_eq!(head.status, 200);
        assert_eq!(head.header("etag"), Some("\"x\""));
        assert_eq!(body.read_all(100).await.expect("reading failed"), b"hello");
        assert!(body.into_stream().is_some());
    }

    #[tokio::test]
    async fn chunked_bodies_are_decoded_while_streaming() {
        let (_, mut body) = respond(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
              5\r\nhello\r\n7;ext=1\r\n, world\r\n0\r\nX-Trailer: 1\r\n\r\n",
            false,
        )
        .await;
        assert_eq!(
            body.read_all(100).await.expect("reading failed"),
            b"hello, world"
        );
        assert!(body.is_done());
    }

    #[tokio::test]
    async fn truncated_and_oversized_bodies_fail() {
        let (_, mut body) = respond(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort", false).await;
        let error = body.read_all(100).await.expect_err("accepted");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);

        let (_, mut body) = respond(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel",
            false,
        )
        .await;
        assert!(body.read_all(100).await.is_err());

        let (_, mut body) = respond(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789", false).await;
        assert!(body.read_all(4).await.is_err());
    }

    #[tokio::test]
    async fn heads_and_closing_connections_are_handled() {
        let (head, mut body) = respond(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 120\r\n\r\n",
            true,
        )
        .await;
        assert_eq!(head.status, 404);
        assert_eq!(body.read_all(10).await.expect("reading failed"), b"");
        assert!(body.into_stream().is_some());

        let (_, mut body) = respond(
            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok",
            false,
        )
        .await;
        assert_eq!(body.read_all(10).await.expect("reading failed"), b"ok");
        assert!(body.into_stream().is_none());

        let (_, mut body) = respond(b"HTTP/1.0 200 OK\r\n\r\nuntil the end", false).await;
        assert_eq!(
            body.read_all(100).await.expect("reading failed"),
            b"until the end"
        );
        assert!(body.into_stream().is_none());
    }

    #[tokio::test]
    async fn broken_heads_are_refused() {
        for raw in [&b"garbage\r\n\r\n"[..], b"HTTP/1.1 abc OK\r\n\r\n", b"HTTP/1.1 200"] {
            let (client, mut server) = tokio::io::duplex(1024);
            let raw = raw.to_vec();
            tokio::spawn(async move {
                let _ = server.write_all(&raw).await;
            });
            assert!(receive(Box::new(client), false).await.is_err());
        }
    }
}
