use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::sync::oneshot;
use tokio_rustls::TlsConnector;

use super::auth::{self, Challenge, Credentials, State};
use super::{BUFFER_SIZE, Body, Connection, Head, Stream, io_error};
use crate::file_system::{Reader, Writer};
use crate::proxy::{self, Proxy};
use crate::{Error, ErrorKind, Result};

// Constants

const MAX_REDIRECTS: usize = 5;
const MAX_AUTH_ROUNDS: usize = 2;
const MAX_IDLE: usize = 4;
const MAX_DRAIN: usize = 256 * 1024;
const CONTINUE_WAIT: Duration = Duration::from_secs(2);
const FINISH_TIMEOUT: Duration = Duration::from_secs(600);
const PIPE_SIZE: usize = 256 * 1024;
const HTTPS_PORT: u16 = 443;
const HTTP_PORT: u16 = 80;

// Types

type Pool = Arc<StdMutex<Vec<Connection>>>;

pub(crate) struct Endpoint {
    pub host: String,
    pub port: u16,
    pub tls: Option<Arc<ClientConfig>>,
    pub proxy: Option<Proxy>,
    pub rejection: Arc<StdMutex<Option<Error>>>,
}

pub(crate) struct Client {
    endpoint: Endpoint,
    credentials: Option<Credentials>,
    allow_basic: bool,
    auth: StdMutex<State>,
    idle: Pool,
    timeout: Duration,
    user_agent: String,
    label: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Request<'a> {
    pub method: &'a str,
    pub target: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: &'a [u8],
}

pub(crate) struct Response {
    pub head: Head,
    connection: Connection,
    body: Body,
    idle: Pool,
    timeout: Duration,
}

pub(crate) struct Upload {
    connection: Connection,
    idle: Pool,
    timeout: Duration,
}

struct BodyReader {
    pipe: DuplexStream,
    result: Option<oneshot::Receiver<io::Result<()>>>,
}

struct BodyWriter {
    pipe: Option<DuplexStream>,
    committed: Arc<AtomicBool>,
    result: Option<oneshot::Receiver<io::Result<()>>>,
}

// Clients

impl Client {
    pub fn new(
        endpoint: Endpoint,
        credentials: Option<Credentials>,
        allow_basic: bool,
        timeout: Duration,
        user_agent: String,
        label: String,
    ) -> Self {
        Self {
            endpoint,
            credentials,
            allow_basic,
            auth: StdMutex::new(State::None),
            idle: Arc::new(StdMutex::new(Vec::new())),
            timeout,
            user_agent,
            label,
        }
    }

    pub fn origin(&self) -> String {
        let scheme = if self.endpoint.tls.is_some() {
            "https"
        } else {
            "http"
        };
        format!("{scheme}://{}", self.authority())
    }

    fn default_port(&self) -> u16 {
        if self.endpoint.tls.is_some() {
            HTTPS_PORT
        } else {
            HTTP_PORT
        }
    }

    fn authority(&self) -> String {
        let host = &self.endpoint.host;
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.clone()
        };
        if self.endpoint.port == self.default_port() {
            host
        } else {
            format!("{host}:{}", self.endpoint.port)
        }
    }

    pub fn close(&self) {
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    // Connections

    async fn open(&self) -> Result<Connection> {
        let endpoint = &self.endpoint;
        let host = endpoint.host.as_str();
        let connect = async {
            let tcp = proxy::open_tcp(endpoint.proxy.as_ref(), host, endpoint.port).await?;
            let _ = tcp.set_nodelay(true);
            let stream: Box<dyn Stream> = match &endpoint.tls {
                Some(config) => {
                    let name = ServerName::try_from(host.to_string()).map_err(|_| {
                        Error::new(ErrorKind::ConnectionFailed, format!("{host}: invalid name"))
                    })?;
                    let tls = TlsConnector::from(Arc::clone(config))
                        .connect(name, tcp)
                        .await
                        .map_err(|error| self.tls_failure(&error))?;
                    Box::new(tls)
                }
                None => Box::new(tcp),
            };
            Ok(Connection::new(stream))
        };
        tokio::time::timeout(self.timeout, connect)
            .await
            .map_err(|_| Error::new(ErrorKind::ConnectionFailed, format!("{host}: timed out")))?
    }

    fn tls_failure(&self, error: &io::Error) -> Error {
        let rejected = self
            .endpoint
            .rejection
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        rejected.unwrap_or_else(|| {
            Error::new(
                ErrorKind::ConnectionFailed,
                format!("{}: {error}", self.endpoint.host),
            )
        })
    }

    async fn connection(&self, fresh: bool) -> Result<(Connection, bool)> {
        if fresh {
            return Ok((self.open().await?, false));
        }
        loop {
            let pooled = self
                .idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop();
            let Some(mut connection) = pooled else {
                return Ok((self.open().await?, false));
            };
            if connection.is_alive().await {
                return Ok((connection, true));
            }
        }
    }

    // Authentication

    fn authorization(&self, method: &str, target: &str) -> Result<Option<String>> {
        let Some(credentials) = &self.credentials else {
            return Ok(None);
        };
        self.auth
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .header(credentials, method, target)
    }

    fn answer(&self, challenges: &[Challenge], authorized: bool) -> bool {
        if self.credentials.is_none() {
            return false;
        }
        let mut state = self.auth.lock().unwrap_or_else(PoisonError::into_inner);
        if authorized {
            return state.refresh(challenges);
        }
        if *state != State::None {
            return true;
        }
        match State::choose(challenges, self.allow_basic) {
            Some(chosen) => {
                *state = chosen;
                true
            }
            None => false,
        }
    }

    fn rejected(&self) -> Error {
        Error::new(ErrorKind::AuthenticationFailed, self.label.clone())
    }

    // Requests

    fn head_text(
        &self,
        method: &str,
        target: &str,
        headers: &[(&'static str, String)],
        authorization: Option<&str>,
    ) -> Result<String> {
        let mut text = format!(
            "{method} {target} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\n",
            self.authority(),
            self.user_agent
        );
        if let Some(authorization) = authorization {
            text.push_str(&format!("Authorization: {authorization}\r\n"));
        }
        for (name, value) in headers {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
        text.push_str("\r\n");
        let invalid = target.bytes().any(|byte| !byte.is_ascii_graphic())
            || headers
                .iter()
                .any(|(_, value)| value.contains(['\r', '\n']));
        if invalid {
            return Err(Error::new(
                ErrorKind::Other,
                format!("invalid request {target}"),
            ));
        }
        Ok(text)
    }

    pub async fn send(&self, request: Request<'_>) -> Result<Response> {
        let mut target = request.target.clone();
        let mut redirects = 0;
        let mut rounds = 0;
        loop {
            let (response, authorized) = self.attempt(&request, &target).await?;
            let head = &response.head;
            if head.status == 401 {
                let challenges = auth::parse_challenges(head.values("www-authenticate"));
                response.discard().await;
                if rounds < MAX_AUTH_ROUNDS && self.answer(&challenges, authorized) {
                    rounds += 1;
                    continue;
                }
                return Err(self.rejected());
            }
            if head.is_redirect() && redirects < MAX_REDIRECTS {
                let location = head.header("location").unwrap_or_default().to_string();
                response.discard().await;
                target = self.same_origin(&location)?;
                redirects += 1;
                continue;
            }
            return Ok(response);
        }
    }

    async fn attempt(&self, request: &Request<'_>, target: &str) -> Result<(Response, bool)> {
        let mut headers = request.headers.clone();
        if !request.body.is_empty() {
            headers.push(("Content-Length", request.body.len().to_string()));
        }
        let mut fresh = false;
        loop {
            let (mut connection, reused) = self.connection(fresh).await?;
            let authorization = self.authorization(request.method, target)?;
            let authorized = authorization.is_some();
            let text =
                self.head_text(request.method, target, &headers, authorization.as_deref())?;
            let exchange = exchange(&mut connection, &text, request.body);
            match tokio::time::timeout(self.timeout, exchange).await {
                Err(_) => return Err(self.timed_out()),
                Ok(Err(_)) if reused => fresh = true,
                Ok(Err(error)) => return Err(error),
                Ok(Ok(head)) => {
                    let body = Body::new(head.framing(request.method == "HEAD")?);
                    let response = Response {
                        head,
                        connection,
                        body,
                        idle: Arc::clone(&self.idle),
                        timeout: self.timeout,
                    };
                    return Ok((response, authorized));
                }
            }
        }
    }

    fn timed_out(&self) -> Error {
        Error::new(
            ErrorKind::ConnectionFailed,
            format!("{}: timed out", self.endpoint.host),
        )
    }

    pub(crate) fn same_origin(&self, location: &str) -> Result<String> {
        let refused = || {
            Error::new(
                ErrorKind::Other,
                format!("redirect to another server refused: {location}"),
            )
        };
        let location = location.trim();
        let location = location.split('#').next().unwrap_or_default();
        let path = if location.starts_with('/') && !location.starts_with("//") {
            location.to_string()
        } else {
            let (scheme, rest) = location.split_once("://").ok_or_else(refused)?;
            let expected = if self.endpoint.tls.is_some() {
                "https"
            } else {
                "http"
            };
            if !scheme.eq_ignore_ascii_case(expected) {
                return Err(refused());
            }
            let split = rest.find('/').unwrap_or(rest.len());
            let (authority, path) = rest.split_at(split);
            let (host, port) =
                split_authority(authority, self.default_port()).ok_or_else(refused)?;
            if !host.eq_ignore_ascii_case(&self.endpoint.host) || port != self.endpoint.port {
                return Err(refused());
            }
            if path.is_empty() {
                "/".to_string()
            } else {
                path.to_string()
            }
        };
        if path.bytes().all(|byte| byte.is_ascii_graphic()) {
            Ok(path)
        } else {
            Err(refused())
        }
    }

    // Uploads

    pub async fn upload(
        &self,
        target: String,
        headers: Vec<(&'static str, String)>,
    ) -> Result<std::result::Result<Upload, Head>> {
        let mut headers = headers;
        headers.push(("Transfer-Encoding", "chunked".to_string()));
        headers.push(("Expect", "100-continue".to_string()));
        let mut target = target;
        let mut redirects = 0;
        let mut rounds = 0;
        let mut fresh = false;
        loop {
            let (mut connection, reused) = self.connection(fresh).await?;
            let authorization = self.authorization("PUT", &target)?;
            let authorized = authorization.is_some();
            let text = self.head_text("PUT", &target, &headers, authorization.as_deref())?;
            let started = tokio::time::timeout(self.timeout, async {
                start_upload(&mut connection, &text).await
            })
            .await
            .map_err(|_| self.timed_out())?;
            let head = match started {
                Err(_) if reused => {
                    fresh = true;
                    continue;
                }
                Err(error) => return Err(error),
                Ok(None) => None,
                Ok(Some(head)) => Some(head),
            };
            fresh = false;
            let Some(head) = head.filter(|head| head.status != 100) else {
                return Ok(Ok(Upload {
                    connection,
                    idle: Arc::clone(&self.idle),
                    timeout: self.timeout,
                }));
            };
            if head.status == 401 {
                let challenges = auth::parse_challenges(head.values("www-authenticate"));
                if rounds < MAX_AUTH_ROUNDS && self.answer(&challenges, authorized) {
                    rounds += 1;
                    continue;
                }
                return Err(self.rejected());
            }
            if !head.is_redirect() || redirects >= MAX_REDIRECTS {
                return Ok(Err(head));
            }
            target = self.same_origin(head.header("location").unwrap_or_default())?;
            redirects += 1;
        }
    }
}

async fn exchange(connection: &mut Connection, head: &str, body: &[u8]) -> Result<Head> {
    connection
        .send(head.as_bytes())
        .await
        .map_err(|error| io_error(&error))?;
    if !body.is_empty() {
        connection
            .send(body)
            .await
            .map_err(|error| io_error(&error))?;
    }
    connection.flush().await.map_err(|error| io_error(&error))?;
    connection.read_head().await
}

async fn start_upload(connection: &mut Connection, head: &str) -> Result<Option<Head>> {
    connection
        .send(head.as_bytes())
        .await
        .map_err(|error| io_error(&error))?;
    connection.flush().await.map_err(|error| io_error(&error))?;
    if connection.answered_within(CONTINUE_WAIT).await {
        connection.read_any_head().await.map(Some)
    } else {
        Ok(None)
    }
}

fn split_authority(authority: &str, default_port: u16) -> Option<(String, u16)> {
    if authority.contains('@') {
        return None;
    }
    let parse = |port: &str| port.parse::<u16>().ok().filter(|port| *port > 0);
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(port) => parse(port)?,
            None if after.is_empty() => default_port,
            None => return None,
        };
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), parse(port)?)),
        None => Some((authority.to_string(), default_port)),
    }
}

// Responses

impl Response {
    pub async fn bytes(mut self, limit: usize) -> Result<Vec<u8>> {
        let read = self.connection.read_to_end(&mut self.body, limit);
        let data = tokio::time::timeout(self.timeout, read)
            .await
            .map_err(|_| Error::new(ErrorKind::ConnectionFailed, "timed out"))??;
        self.release();
        Ok(data)
    }

    pub async fn discard(mut self) {
        let read = self.connection.read_to_end(&mut self.body, MAX_DRAIN);
        if let Ok(Ok(_)) = tokio::time::timeout(self.timeout, read).await {
            self.release();
        }
    }

    fn release(self) {
        if self.head.persistent && self.body.is_reusable() {
            let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
            if idle.len() < MAX_IDLE {
                idle.push(self.connection);
            }
        }
    }

    pub fn into_reader(self, skip: u64) -> Reader {
        let (pipe, mut writer) = tokio::io::duplex(PIPE_SIZE);
        let (sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let result = self.pump(&mut writer, skip).await;
            let _ = sender.send(result);
        });
        Box::new(BodyReader {
            pipe,
            result: Some(receiver),
        })
    }

    async fn pump(mut self, writer: &mut DuplexStream, mut skip: u64) -> io::Result<()> {
        let mut buffer = vec![0; BUFFER_SIZE];
        loop {
            let read = self.connection.read_body(&mut self.body, &mut buffer);
            let read = tokio::time::timeout(self.timeout, read)
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out"))??;
            if read == 0 {
                break;
            }
            let dropped = usize::try_from(skip).unwrap_or(usize::MAX).min(read);
            skip -= dropped as u64;
            writer.write_all(&buffer[dropped..read]).await?;
        }
        if skip > 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        self.release();
        Ok(())
    }
}

impl AsyncRead for BodyReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        ready!(Pin::new(&mut self.pipe).poll_read(context, buffer))?;
        if buffer.filled().len() > before || buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let Some(receiver) = self.result.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        let outcome = ready!(Pin::new(receiver).poll(context));
        self.result = None;
        Poll::Ready(match outcome {
            Ok(result) => result,
            Err(_) => Err(io::Error::other("the download was aborted")),
        })
    }
}

// Uploads

impl Upload {
    async fn write(&mut self, data: &[u8]) -> io::Result<()> {
        tokio::time::timeout(self.timeout, self.connection.send_chunk(data))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out"))?
    }

    async fn finish(mut self) -> Result<Head> {
        let finish = async {
            self.connection
                .finish_chunks()
                .await
                .map_err(|error| io_error(&error))?;
            let head = self.connection.read_head().await?;
            let mut body = Body::new(head.framing(false)?);
            let drained = self.connection.read_to_end(&mut body, MAX_DRAIN).await;
            Ok::<_, Error>((head, body, drained.is_ok()))
        };
        let (head, body, drained) = tokio::time::timeout(FINISH_TIMEOUT, finish)
            .await
            .map_err(|_| Error::new(ErrorKind::ConnectionFailed, "timed out"))??;
        if drained && head.persistent && body.is_reusable() {
            let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
            if idle.len() < MAX_IDLE {
                idle.push(self.connection);
            }
        }
        Ok(head)
    }

    pub fn into_writer(self, check: impl FnOnce(&Head) -> Result<()> + Send + 'static) -> Writer {
        let (pipe, mut reader) = tokio::io::duplex(PIPE_SIZE);
        let (sender, receiver) = oneshot::channel();
        let committed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&committed);
        tokio::spawn(async move {
            let result = self.pump(&mut reader, &flag, check).await;
            let _ = sender.send(result);
        });
        Box::new(BodyWriter {
            pipe: Some(pipe),
            committed,
            result: Some(receiver),
        })
    }

    async fn pump(
        mut self,
        reader: &mut DuplexStream,
        committed: &AtomicBool,
        check: impl FnOnce(&Head) -> Result<()>,
    ) -> io::Result<()> {
        let mut buffer = vec![0; BUFFER_SIZE];
        loop {
            let read = reader.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            self.write(&buffer[..read]).await?;
        }
        if !committed.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "the upload was aborted",
            ));
        }
        let head = self.finish().await.map_err(|error| to_io(&error))?;
        check(&head).map_err(|error| to_io(&error))
    }
}

impl BodyWriter {
    fn failure(&mut self) -> Option<io::Error> {
        let outcome = self.result.as_mut()?.try_recv().ok()?;
        self.result = None;
        outcome.err()
    }
}

impl AsyncWrite for BodyWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let Some(pipe) = self.pipe.as_mut() else {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        };
        match ready!(Pin::new(pipe).poll_write(context, buffer)) {
            Ok(written) => Poll::Ready(Ok(written)),
            Err(error) => Poll::Ready(Err(self.failure().unwrap_or(error))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.pipe.as_mut() {
            Some(pipe) => Pin::new(pipe).poll_flush(context),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.committed.store(true, Ordering::SeqCst);
        self.pipe = None;
        let Some(receiver) = self.result.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        let outcome = ready!(Pin::new(receiver).poll(context));
        self.result = None;
        Poll::Ready(match outcome {
            Ok(result) => result,
            Err(_) => Err(io::Error::other("the upload was aborted")),
        })
    }
}

pub(crate) fn to_io(error: &Error) -> io::Error {
    let kind = match error.kind() {
        ErrorKind::NotFound => io::ErrorKind::NotFound,
        ErrorKind::PermissionDenied => io::ErrorKind::PermissionDenied,
        ErrorKind::AlreadyExists => io::ErrorKind::AlreadyExists,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error.detail().to_string())
}
