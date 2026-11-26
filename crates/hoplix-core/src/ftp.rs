use std::fmt;
use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustls::ClientConfig;
use suppaftp::list::{File, ListParser};
use suppaftp::tokio::{
    AsyncRustlsConnector, AsyncRustlsFtpStream, AsyncRustlsStream, TransferStream,
};
use suppaftp::types::FileType;
use suppaftp::{Encoding, FtpError, Mode, Status};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, MutexGuard};
use tokio_rustls::TlsConnector;

use crate::connection::{Authentication, ConnectionParams, HostKeyTrust, Protocol};
use crate::file_system::{Reader, Writer};
use crate::options::{ConnectionOptions, FileNameEncoding, FtpMode};
use crate::proxy::Proxy;
use crate::sftp::{join_posix, parent_posix};
use crate::{
    BoxFuture, Entry, EntryKind, Error, ErrorKind, FileSystem, Properties, Result, certificates,
    decode_windows_1252, encode_windows_1252, mode_text, parse_mode, parse_permission_text,
};

// Constants

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const KEEPALIVE_AFTER: Duration = Duration::from_secs(60);
const MAX_IDLE_CONNECTIONS: usize = 2;

// Types

type Client = AsyncRustlsFtpStream;
type Transfer = TransferStream<AsyncRustlsStream>;
type Finishing =
    Pin<Box<dyn Future<Output = (Box<Client>, std::result::Result<(), FtpError>)> + Send>>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Capabilities {
    mlsd: bool,
    mlst: bool,
    rest_stream: bool,
    epsv: bool,
    utf8: bool,
    mfmt: bool,
}

struct Session {
    params: ConnectionParams,
    options: ConnectionOptions,
    tls: Option<Arc<ClientConfig>>,
    rejection: Arc<StdMutex<Option<Error>>>,
    proxy: Option<Proxy>,
}

struct Control {
    client: Client,
    last_used: Instant,
    broken: bool,
}

pub struct FtpFileSystem {
    session: Session,
    capabilities: Capabilities,
    control: Arc<Mutex<Control>>,
    idle: Arc<StdMutex<Vec<Client>>>,
    label: String,
}

#[derive(Debug, Clone, Copy)]
struct Windows1252;

enum TransferState {
    Open {
        client: Box<Client>,
        stream: Transfer,
    },
    Finishing(Finishing),
    Done,
}

struct FtpTransfer {
    state: TransferState,
    idle: Arc<StdMutex<Vec<Client>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct UnixDetails {
    name: String,
    permissions: Option<u32>,
    owner: Option<String>,
    group: Option<String>,
}

// Connecting

impl FtpFileSystem {
    pub async fn connect(
        params: &ConnectionParams,
        certificates: &Path,
        trust: Option<HostKeyTrust>,
    ) -> Result<Self> {
        Self::connect_via(params, None, certificates, trust).await
    }

    pub async fn connect_via(
        params: &ConnectionParams,
        proxy: Option<&Proxy>,
        certificates: &Path,
        trust: Option<HostKeyTrust>,
    ) -> Result<Self> {
        Self::connect_via_with(
            params,
            proxy,
            &ConnectionOptions::default(),
            certificates,
            trust,
        )
        .await
    }

    pub async fn connect_via_with(
        params: &ConnectionParams,
        proxy: Option<&Proxy>,
        options: &ConnectionOptions,
        certificates: &Path,
        trust: Option<HostKeyTrust>,
    ) -> Result<Self> {
        ensure_passive_mode(proxy, options.ftp_mode == FtpMode::Active)?;
        let rejection = Arc::new(StdMutex::new(None));
        let tls = if params.protocol.uses_tls() {
            Some(certificates::client_config(
                &params.host,
                params.port,
                certificates,
                trust,
                Arc::clone(&rejection),
            )?)
        } else {
            None
        };
        let session = Session {
            params: params.clone(),
            options: options.clone(),
            tls,
            rejection,
            proxy: proxy.cloned(),
        };
        let (client, capabilities) = session.open().await?;
        let control = Arc::new(Mutex::new(Control {
            client,
            last_used: Instant::now(),
            broken: false,
        }));
        if let Some(interval) = options.keepalive() {
            keep_alive(Arc::downgrade(&control), interval);
        }
        Ok(Self {
            session,
            capabilities,
            control,
            idle: Arc::new(StdMutex::new(Vec::new())),
            label: params.label(),
        })
    }

    async fn control(&self) -> Result<MutexGuard<'_, Control>> {
        let mut control = self.control.lock().await;
        let alive = !control.broken
            && (control.last_used.elapsed() < KEEPALIVE_AFTER
                || control.client.noop().await.is_ok());
        if !alive {
            control.client = self.session.open().await?.0;
            control.broken = false;
        }
        control.last_used = Instant::now();
        Ok(control)
    }

    async fn transfer_client(&self) -> Result<Client> {
        loop {
            let pooled = self.idle.lock().ok().and_then(|mut idle| idle.pop());
            let Some(mut client) = pooled else {
                return Ok(self.session.open().await?.0);
            };
            if client.noop().await.is_ok() {
                return Ok(client);
            }
        }
    }
}

impl Session {
    async fn open(&self) -> Result<(Client, Capabilities)> {
        tokio::time::timeout(self.options.timeout_or(CONNECT_TIMEOUT), self.establish())
            .await
            .map_err(|_| Error::new(ErrorKind::ConnectionFailed, self.params.host.clone()))?
    }

    async fn establish(&self) -> Result<(Client, Capabilities)> {
        let params = &self.params;
        let password = match &params.authentication {
            Authentication::Password(password) => password.clone(),
            Authentication::KeyFile { path, .. } => {
                return Err(Error::new(
                    ErrorKind::InvalidKey,
                    format!("{}: FTP does not support key files", path.display()),
                ));
            }
            Authentication::Agent => {
                return Err(Error::new(
                    ErrorKind::AuthenticationFailed,
                    format!("{}: FTP does not support SSH agents", params.label()),
                ));
            }
        };
        if let Some(proxy) = &self.proxy {
            return self
                .finish(self.connect_proxied(proxy).await?, &password)
                .await;
        }
        let address = (params.host.as_str(), params.port);
        let connected = match (params.protocol, &self.tls) {
            (Protocol::FtpsImplicit, Some(config)) => {
                match Client::connect_secure_implicit(address, connector(config), &params.host)
                    .await
                {
                    Ok(mut client) => protect_data(&mut client).await.map(|()| client),
                    Err(error) => Err(error),
                }
            }
            (Protocol::FtpsExplicit, Some(config)) => match Client::connect(address).await {
                Ok(client) => client.into_secure(connector(config), &params.host).await,
                Err(error) => Err(error),
            },
            _ => Client::connect(address).await,
        };
        let client = connected.map_err(|error| self.connect_error(&error))?;
        self.finish(client, &password).await
    }

    async fn connect_proxied(&self, proxy: &Proxy) -> Result<Client> {
        let params = &self.params;
        let stream = proxy.connect(&params.host, params.port).await?;
        let connected = match (params.protocol, &self.tls) {
            (Protocol::FtpsImplicit, Some(config)) => {
                let relay = relay(stream).await?;
                match Client::connect_secure_implicit(relay, connector(config), &params.host).await
                {
                    Ok(mut client) => protect_data(&mut client).await.map(|()| client),
                    Err(error) => Err(error),
                }
            }
            (Protocol::FtpsExplicit, Some(config)) => {
                match Client::connect_with_stream(stream).await {
                    Ok(client) => client.into_secure(connector(config), &params.host).await,
                    Err(error) => Err(error),
                }
            }
            _ => Client::connect_with_stream(stream).await,
        };
        let client = connected.map_err(|error| self.connect_error(&error))?;
        let proxy = proxy.clone();
        let host = params.host.clone();
        Ok(client.passive_stream_builder(move |address| {
            let proxy = proxy.clone();
            let host = host.clone();
            Box::pin(async move {
                proxy
                    .connect(&host, address.port())
                    .await
                    .map_err(|error| FtpError::ConnectionError(io::Error::other(error.to_string())))
            })
        }))
    }

    async fn finish(&self, mut client: Client, password: &str) -> Result<(Client, Capabilities)> {
        let params = &self.params;
        let encoding = self.options.ftp_encoding;
        if encoding == FileNameEncoding::Windows1252 {
            client.set_encoding(Windows1252).await;
        }
        match client.login(params.username.as_str(), password).await {
            Ok(()) => {}
            Err(FtpError::UnexpectedResponse(response))
                if response.status == Status::NotLoggedIn =>
            {
                return Err(Error::new(ErrorKind::AuthenticationFailed, params.label()));
            }
            Err(error) => return Err(self.connect_error(&error)),
        }
        let capabilities = capabilities(
            client
                .feat()
                .await
                .map(|features| features.into_keys().collect::<Vec<_>>())
                .unwrap_or_default()
                .iter()
                .map(String::as_str),
        );
        if capabilities.utf8 && encoding == FileNameEncoding::Utf8 {
            let _ = client.opts("UTF8", Some("ON")).await;
        }
        client
            .transfer_type(FileType::Binary)
            .await
            .map_err(|error| ftp_error(&error, &params.host))?;
        let client = match data_mode(&self.options, capabilities) {
            Mode::Active => client.active_mode(self.options.timeout_or(CONNECT_TIMEOUT)),
            mode => {
                client.set_mode(mode);
                client.set_passive_nat_workaround(self.options.ftp_external_ip);
                client
            }
        };
        Ok((client, capabilities))
    }

    fn connect_error(&self, error: &FtpError) -> Error {
        let rejected = self.rejection.lock().ok().and_then(|mut slot| slot.take());
        rejected.unwrap_or_else(|| {
            Error::new(
                ErrorKind::ConnectionFailed,
                format!("{}: {error}", self.params.host),
            )
        })
    }
}

fn data_mode(options: &ConnectionOptions, capabilities: Capabilities) -> Mode {
    match options.ftp_mode {
        FtpMode::Active => Mode::Active,
        FtpMode::Passive if capabilities.epsv && options.ftp_external_ip => Mode::ExtendedPassive,
        FtpMode::Passive => Mode::Passive,
    }
}

fn keep_alive(control: Weak<Mutex<Control>>, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(control) = control.upgrade() else {
                break;
            };
            let Ok(mut control) = control.try_lock() else {
                continue;
            };
            if control.broken || control.last_used.elapsed() < interval {
                continue;
            }
            if control.client.noop().await.is_ok() {
                control.last_used = Instant::now();
            } else {
                control.broken = true;
            }
        }
    });
}

async fn protect_data(client: &mut Client) -> std::result::Result<(), FtpError> {
    client
        .custom_command("PBSZ 0", &[Status::CommandOk])
        .await?;
    client
        .custom_command("PROT P", &[Status::CommandOk])
        .await?;
    Ok(())
}

async fn relay(stream: TcpStream) -> Result<std::net::SocketAddr> {
    let relay_error = |error: io::Error| Error::new(ErrorKind::ConnectionFailed, error.to_string());
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(relay_error)?;
    let address = listener.local_addr().map_err(relay_error)?;
    tokio::spawn(async move {
        let mut upstream = stream;
        if let Ok(Ok((mut local, _))) =
            tokio::time::timeout(CONNECT_TIMEOUT, listener.accept()).await
        {
            drop(listener);
            let _ = tokio::io::copy_bidirectional(&mut local, &mut upstream).await;
        }
    });
    Ok(address)
}

pub fn ensure_passive_mode(proxy: Option<&Proxy>, active: bool) -> Result<()> {
    match proxy {
        Some(proxy) if active => Err(Error::new(
            ErrorKind::ConnectionFailed,
            format!(
                "active FTP mode does not work through the proxy {}, use passive mode",
                proxy.label()
            ),
        )),
        _ => Ok(()),
    }
}

fn connector(config: &Arc<ClientConfig>) -> AsyncRustlsConnector {
    AsyncRustlsConnector::from(TlsConnector::from(Arc::clone(config)))
}

fn capabilities<'a>(features: impl Iterator<Item = &'a str>) -> Capabilities {
    let mut capabilities = Capabilities::default();
    for feature in features {
        match feature.to_ascii_uppercase().as_str() {
            "MLSD" | "MLST" => {
                capabilities.mlsd = true;
                capabilities.mlst = true;
            }
            "REST" => capabilities.rest_stream = true,
            "EPSV" => capabilities.epsv = true,
            "UTF8" => capabilities.utf8 = true,
            "MFMT" => capabilities.mfmt = true,
            _ => {}
        }
    }
    capabilities
}

// Interface

impl FileSystem for FtpFileSystem {
    fn label(&self) -> Option<String> {
        Some(self.label.clone())
    }

    fn home(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async {
            let mut control = self.control().await?;
            let result = control.client.pwd().await;
            control.check(result, "/")
        })
    }

    fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
        let path = path.to_string();
        Box::pin(async move {
            let mut control = self.control().await?;
            let changed = control.client.cwd(&path).await;
            if let Err(error) = changed {
                let error = control.check::<()>(Err(error), &path).unwrap_err();
                return Err(control.refine_missing(error, &path).await);
            }
            let result = if self.capabilities.mlsd {
                control.client.mlsd(None).await
            } else {
                control.client.list(None).await
            };
            let lines = control.check(result, &path)?;
            let mut entries: Vec<Entry> = lines
                .iter()
                .filter_map(|line| parse_line(line, self.capabilities.mlsd))
                .collect();
            for entry in entries.iter_mut().filter(|entry| entry.is_link) {
                let target = join_posix(&path, &entry.name);
                if control.client.cwd(&target).await.is_ok() {
                    entry.kind = EntryKind::Directory;
                    entry.size = None;
                }
            }
            Ok(entries)
        })
    }

    fn metadata(&self, path: &str) -> BoxFuture<'_, Result<Entry>> {
        let path = path.to_string();
        Box::pin(async move {
            let name = self.file_name(&path).to_string();
            let mut control = self.control().await?;
            if self.capabilities.mlst {
                let result = control.client.mlst(Some(&path)).await;
                let line = control.check(result, &path)?;
                return ListParser::parse_mlst(&line)
                    .map(|file| entry_from(name, &file))
                    .map_err(|error| Error::new(ErrorKind::Other, format!("{path}: {error}")));
            }
            if control.client.cwd(&path).await.is_ok() {
                return Ok(directory_entry(name));
            }
            let result = control.client.size(&path).await;
            let size = control.check(result, &path)?;
            Ok(Entry {
                name,
                kind: EntryKind::File,
                size: Some(size as u64),
                modified: None,
                is_link: false,
            })
        })
    }

    fn create_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            if self.metadata(&path).await.is_ok() {
                return Err(Error::new(ErrorKind::AlreadyExists, path));
            }
            let mut control = self.control().await?;
            let result = control.client.mkdir(&path).await;
            control.check(result, &path)
        })
    }

    fn remove_file(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            let mut control = self.control().await?;
            let result = control.client.rm(&path).await;
            control.check(result, &path)
        })
    }

    fn remove_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            let result = {
                let mut control = self.control().await?;
                let result = control.client.rmdir(&path).await;
                control.check(result, &path)
            };
            match result {
                Err(error)
                    if error.kind() != ErrorKind::ConnectionFailed
                        && self
                            .list(&path)
                            .await
                            .is_ok_and(|entries| !entries.is_empty()) =>
                {
                    Err(Error::new(ErrorKind::DirectoryNotEmpty, path))
                }
                other => other,
            }
        })
    }

    fn rename(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        let (from, to) = (from.to_string(), to.to_string());
        Box::pin(async move {
            if self.metadata(&to).await.is_ok() {
                return Err(Error::new(ErrorKind::AlreadyExists, to));
            }
            let mut control = self.control().await?;
            let result = control.client.rename(&from, &to).await;
            control.check(result, &from)
        })
    }

    fn open_read(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Reader>> {
        let path = path.to_string();
        Box::pin(async move {
            let mut client = self.transfer_client().await?;
            if offset > 0 {
                let offset = usize::try_from(offset)
                    .map_err(|_| Error::new(ErrorKind::Other, format!("{path}: offset")))?;
                let _ = client.resume_transfer(offset).await;
            }
            let stream = client
                .retr_as_stream(&path)
                .await
                .map_err(|error| ftp_error(&error, &path))?;
            Ok(Box::new(FtpTransfer::new(client, stream, Arc::clone(&self.idle))) as Reader)
        })
    }

    fn open_write(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Writer>> {
        let path = path.to_string();
        Box::pin(async move {
            let mut client = self.transfer_client().await?;
            let stream = if offset == 0 {
                client.put_with_stream(&path).await
            } else if self.capabilities.rest_stream {
                let offset = usize::try_from(offset)
                    .map_err(|_| Error::new(ErrorKind::Other, format!("{path}: offset")))?;
                let _ = client.resume_transfer(offset).await;
                client.put_with_stream(&path).await
            } else {
                client.append_with_stream(&path).await
            }
            .map_err(|error| ftp_error(&error, &path))?;
            Ok(Box::new(FtpTransfer::new(client, stream, Arc::clone(&self.idle))) as Writer)
        })
    }

    fn set_modified(&self, path: &str, modified: SystemTime) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            let stamp = timestamp(modified)
                .ok_or_else(|| Error::new(ErrorKind::Other, format!("{path}: time")))?;
            let command = if self.capabilities.mfmt {
                "MFMT"
            } else {
                "MDTM"
            };
            let mut control = self.control().await?;
            let result = control
                .client
                .custom_command(format!("{command} {stamp} {path}"), &[Status::File])
                .await;
            control.check(result, &path).map(|_| ())
        })
    }

    fn properties(&self, path: &str) -> BoxFuture<'_, Result<Properties>> {
        let path = path.to_string();
        Box::pin(async move {
            let mut properties = Properties::from_entry(self.metadata(&path).await?);
            let mut details = UnixDetails::default();
            if self.capabilities.mlst {
                let mut control = self.control().await?;
                let result = control.client.mlst(Some(&path)).await;
                if let Ok(line) = control.check(result, &path) {
                    details = machine_details(&line);
                }
            }
            if details.permissions.is_none()
                && let Some(parent) = self.parent(&path)
            {
                let name = self.file_name(&path).to_string();
                let mut control = self.control().await?;
                if control.client.cwd(&parent).await.is_ok() {
                    let result = control.client.list(None).await;
                    if let Some(found) = control.check(result, &parent).ok().and_then(|lines| {
                        lines
                            .iter()
                            .filter_map(|line| list_details(line))
                            .find(|found| found.name == name)
                    }) {
                        details = found;
                    }
                }
            }
            properties.permissions = details.permissions;
            properties.owner = details.owner;
            properties.group = details.group;
            Ok(properties)
        })
    }

    fn can_set_permissions(&self) -> bool {
        true
    }

    fn set_permissions(&self, path: &str, mode: u32) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            let mut control = self.control().await?;
            let result = control
                .client
                .site(format!("CHMOD {} {path}", mode_text(mode)))
                .await;
            match result {
                Err(FtpError::UnexpectedResponse(response))
                    if matches!(response.status.code(), 500 | 502 | 504) =>
                {
                    Err(Error::new(
                        ErrorKind::Unsupported,
                        format!(
                            "{path}: {}",
                            response.as_string().unwrap_or_default().trim()
                        ),
                    ))
                }
                other => control.check(other, &path).map(|_| ()),
            }
        })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let idle: Vec<Client> = self
                .idle
                .lock()
                .map(|mut idle| idle.drain(..).collect())
                .unwrap_or_default();
            for mut client in idle {
                let _ = client.quit().await;
            }
            let _ = self.control.lock().await.client.quit().await;
        })
    }

    fn join(&self, base: &str, name: &str) -> String {
        join_posix(base, name)
    }

    fn parent(&self, path: &str) -> Option<String> {
        parent_posix(path)
    }

    fn file_name<'a>(&self, path: &'a str) -> &'a str {
        path.trim_end_matches('/')
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or(path)
    }
}

impl Encoding for Windows1252 {
    fn decode(&self, bytes: &[u8]) -> String {
        decode_windows_1252(bytes)
    }

    fn encode(&self, text: &str) -> io::Result<Vec<u8>> {
        encode_windows_1252(text).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "not representable in Windows-1252",
            )
        })
    }
}

impl fmt::Debug for FtpFileSystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FtpFileSystem")
            .field("label", &self.label)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

// Control connection

impl Control {
    fn check<T>(&mut self, result: std::result::Result<T, FtpError>, path: &str) -> Result<T> {
        result.map_err(|error| {
            if matches!(error, FtpError::ConnectionError(_) | FtpError::BadResponse)
                && !is_unencodable(&error)
            {
                self.broken = true;
            }
            ftp_error(&error, path)
        })
    }

    async fn refine_missing(&mut self, error: Error, path: &str) -> Error {
        if error.kind() == ErrorKind::NotFound && self.client.size(path).await.is_ok() {
            return Error::new(ErrorKind::NotADirectory, path);
        }
        error
    }
}

// Transfers

impl FtpTransfer {
    fn new(client: Client, stream: Transfer, idle: Arc<StdMutex<Vec<Client>>>) -> Self {
        Self {
            state: TransferState::Open {
                client: Box::new(client),
                stream,
            },
            idle,
        }
    }

    fn begin_finish(&mut self) {
        if !matches!(self.state, TransferState::Open { .. }) {
            return;
        }
        if let TransferState::Open { client, stream } =
            std::mem::replace(&mut self.state, TransferState::Done)
        {
            self.state = TransferState::Finishing(Box::pin(async move {
                let result = stream.finish().await;
                (client, result)
            }));
        }
    }

    fn poll_finish(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.begin_finish();
        let TransferState::Finishing(finishing) = &mut self.state else {
            return Poll::Ready(Ok(()));
        };
        let (client, result) = ready!(finishing.as_mut().poll(context));
        self.state = TransferState::Done;
        match result {
            Ok(()) => {
                if let Ok(mut idle) = self.idle.lock()
                    && idle.len() < MAX_IDLE_CONNECTIONS
                {
                    idle.push(*client);
                }
                Poll::Ready(Ok(()))
            }
            Err(error) => Poll::Ready(Err(io::Error::other(error.to_string()))),
        }
    }
}

impl AsyncRead for FtpTransfer {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let TransferState::Open { stream, .. } = &mut this.state {
            let before = buffer.filled().len();
            ready!(Pin::new(stream).poll_read(context, buffer))?;
            if buffer.filled().len() > before || buffer.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
        }
        this.poll_finish(context)
    }
}

impl AsyncWrite for FtpTransfer {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().state {
            TransferState::Open { stream, .. } => Pin::new(stream).poll_write(context, data),
            _ => Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().state {
            TransferState::Open { stream, .. } => Pin::new(stream).poll_flush(context),
            _ => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let TransferState::Open { stream, .. } = &mut this.state {
            ready!(Pin::new(stream).poll_flush(context))?;
        }
        this.poll_finish(context)
    }
}

// Listings

fn parse_line(line: &str, machine_readable: bool) -> Option<Entry> {
    let file = if machine_readable {
        let facts = line
            .split("; ")
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if facts.contains("type=cdir") || facts.contains("type=pdir") {
            return None;
        }
        ListParser::parse_mlsd(line).ok()?
    } else {
        ListParser::parse_posix(line)
            .or_else(|_| ListParser::parse_dos(line))
            .ok()?
    };
    let name = file.name().to_string();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(entry_from(name, &file))
}

fn machine_details(line: &str) -> UnixDetails {
    let line = line.strip_prefix(' ').unwrap_or(line);
    let (facts, name) = match line.find("; ") {
        Some(end) => (&line[..=end], &line[end + 2..]),
        None => ("", line.trim()),
    };
    let mut details = UnixDetails {
        name: name.to_string(),
        ..UnixDetails::default()
    };
    let (mut uid, mut gid) = (None, None);
    for fact in facts.split(';') {
        let Some((key, value)) = fact.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "unix.mode" => details.permissions = parse_mode(value),
            "unix.owner" | "unix.ownername" => details.owner = Some(value.to_string()),
            "unix.group" | "unix.groupname" => details.group = Some(value.to_string()),
            "unix.uid" => uid = Some(value.to_string()),
            "unix.gid" => gid = Some(value.to_string()),
            _ => {}
        }
    }
    details.owner = details.owner.or(uid);
    details.group = details.group.or(gid);
    details
}

fn list_details(line: &str) -> Option<UnixDetails> {
    let file = ListParser::parse_posix(line).ok()?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    Some(UnixDetails {
        name: file.name().to_string(),
        permissions: fields
            .first()
            .and_then(|field| field.get(1..10))
            .and_then(parse_permission_text),
        owner: fields.get(2).map(ToString::to_string),
        group: fields.get(3).map(ToString::to_string),
    })
}

fn entry_from(name: String, file: &File) -> Entry {
    if file.is_directory() {
        let mut entry = directory_entry(name);
        entry.modified = Some(file.modified());
        return entry;
    }
    Entry {
        name,
        kind: EntryKind::File,
        size: (!file.is_symlink()).then_some(file.size() as u64),
        modified: Some(file.modified()),
        is_link: file.is_symlink(),
    }
}

fn directory_entry(name: String) -> Entry {
    Entry {
        name,
        kind: EntryKind::Directory,
        size: None,
        modified: None,
        is_link: false,
    }
}

// Times

fn timestamp(time: SystemTime) -> Option<String> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let days = i64::try_from(seconds / 86_400).ok()?;
    let rest = seconds % 86_400;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Some(format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    ))
}

// Errors

fn is_unencodable(error: &FtpError) -> bool {
    matches!(error, FtpError::ConnectionError(source) if source.kind() == io::ErrorKind::InvalidInput)
}

fn ftp_error(error: &FtpError, path: &str) -> Error {
    if is_unencodable(error) {
        return Error::new(
            ErrorKind::Other,
            format!("{path}: the name cannot be written in the file name encoding of this site"),
        );
    }
    match error {
        FtpError::ConnectionError(_) | FtpError::BadResponse => {
            Error::new(ErrorKind::ConnectionFailed, format!("{path}: {error}"))
        }
        FtpError::UnexpectedResponse(response) => {
            let text = response
                .as_string()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let kind = match response.status.code() {
                421 | 425 | 426 => ErrorKind::ConnectionFailed,
                530 => ErrorKind::PermissionDenied,
                550 if text.contains("permission") || text.contains("denied") => {
                    ErrorKind::PermissionDenied
                }
                550 if text.contains("exists") => ErrorKind::AlreadyExists,
                550 => ErrorKind::NotFound,
                553 => ErrorKind::PermissionDenied,
                _ => ErrorKind::Other,
            };
            if kind == ErrorKind::Other || kind == ErrorKind::ConnectionFailed {
                Error::new(
                    kind,
                    format!(
                        "{path}: {}",
                        response.as_string().unwrap_or_default().trim()
                    ),
                )
            } else {
                Error::new(kind, path)
            }
        }
        _ => Error::new(ErrorKind::Other, format!("{path}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_password;
    use suppaftp::types::Response;

    fn names(
        lines: &[&str],
        machine_readable: bool,
    ) -> Vec<(String, EntryKind, Option<u64>, bool)> {
        lines
            .iter()
            .filter_map(|line| parse_line(line, machine_readable))
            .map(|entry| (entry.name, entry.kind, entry.size, entry.is_link))
            .collect()
    }

    // Proxies

    #[test]
    fn active_mode_is_refused_through_a_proxy() {
        let proxy = Proxy {
            kind: crate::ProxyKind::Socks5,
            host: "proxy.test".to_string(),
            port: 1080,
            credentials: None,
        };
        assert!(ensure_passive_mode(None, true).is_ok());
        assert!(ensure_passive_mode(Some(&proxy), false).is_ok());
        let error = ensure_passive_mode(Some(&proxy), true).expect_err("refused");
        assert!(error.detail().contains("socks5://proxy.test:1080"));
    }

    #[tokio::test]
    async fn the_relay_forwards_one_connection() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let server = TcpListener::bind("127.0.0.1:0").await.expect("bound");
        let upstream = TcpStream::connect(server.local_addr().expect("address"))
            .await
            .expect("connected");
        let (mut remote, _) = server.accept().await.expect("accepted");
        let address = relay(upstream).await.expect("relay");
        let mut local = TcpStream::connect(address).await.expect("connected");
        local.write_all(b"hello").await.expect("written");
        let mut received = [0u8; 5];
        remote.read_exact(&mut received).await.expect("read");
        assert_eq!(&received, b"hello");
        remote.write_all(b"world").await.expect("written");
        local.read_exact(&mut received).await.expect("read");
        assert_eq!(&received, b"world");
        assert!(TcpStream::connect(address).await.is_err());
    }

    // Listings

    #[test]
    fn unix_listings_are_parsed() {
        let lines = [
            "total 24",
            "drwxr-xr-x    2 1000     1000         4096 Mar 15 12:00 photos",
            "-rw-r--r--    1 1000     1000       250000 Mar 03  2025 report 2026.pdf",
            "lrwxrwxrwx    1 0        0              11 Mar 15 12:00 latest -> photos",
            "drwxr-xr-x    3 0        0            4096 Mar 15 12:00 .",
            "drwxr-xr-x    3 0        0            4096 Mar 15 12:00 ..",
        ];
        assert_eq!(
            names(&lines, false),
            [
                ("photos".to_string(), EntryKind::Directory, None, false),
                (
                    "report 2026.pdf".to_string(),
                    EntryKind::File,
                    Some(250_000),
                    false
                ),
                ("latest".to_string(), EntryKind::File, None, true),
            ]
        );
    }

    #[test]
    fn listings_give_permissions_and_owners() {
        let details =
            list_details("-rwsr-x---    1 demo     www-data      250 Mar 03  2025 report 2026.pdf")
                .expect("not parsed");
        assert_eq!(
            details,
            UnixDetails {
                name: "report 2026.pdf".to_string(),
                permissions: Some(0o4750),
                owner: Some("demo".to_string()),
                group: Some("www-data".to_string()),
            }
        );
        assert_eq!(
            list_details("11-24-26  09:15AM       <DIR>          Backups"),
            None
        );
        let machine = machine_details(
            " type=file;size=5;UNIX.mode=0640;UNIX.uid=1000;UNIX.gid=1000;UNIX.ownername=ftp; a b.txt",
        );
        assert_eq!(
            machine,
            UnixDetails {
                name: "a b.txt".to_string(),
                permissions: Some(0o640),
                owner: Some("ftp".to_string()),
                group: Some("1000".to_string()),
            }
        );
        assert_eq!(
            machine_details("type=dir;modify=20250101000000; pub").permissions,
            None
        );
    }

    #[test]
    fn windows_listings_are_parsed() {
        let lines = [
            "11-24-26  09:15AM       <DIR>          Backups",
            "03-03-25  04:30PM                 1536 notes.txt",
        ];
        assert_eq!(
            names(&lines, false),
            [
                ("Backups".to_string(), EntryKind::Directory, None, false),
                ("notes.txt".to_string(), EntryKind::File, Some(1536), false),
            ]
        );
    }

    #[test]
    fn machine_listings_skip_the_current_and_parent_folder() {
        let lines = [
            "type=cdir;modify=20260315120000;perm=flcdmpe; /home/demo",
            "type=pdir;modify=20260315120000;perm=flcdmpe; /home",
            "type=dir;modify=20260315120000;perm=flcdmpe; Projects",
            "type=file;size=642;modify=20260315120000;perm=adfrw; deploy; v2.sh",
        ];
        assert_eq!(
            names(&lines, true),
            [
                ("Projects".to_string(), EntryKind::Directory, None, false),
                (
                    "deploy; v2.sh".to_string(),
                    EntryKind::File,
                    Some(642),
                    false
                ),
            ]
        );
    }

    // Capabilities

    #[test]
    fn features_enable_capabilities() {
        let enabled = capabilities(["MLST", "rest", "EPSV", "UTF8", "SIZE", "MFMT"].into_iter());
        assert!(
            enabled.mlsd
                && enabled.mlst
                && enabled.rest_stream
                && enabled.epsv
                && enabled.utf8
                && enabled.mfmt
        );
        assert_eq!(capabilities(["SIZE"].into_iter()), Capabilities::default());
    }

    #[test]
    fn times_format_as_utc_timestamps() {
        let at = |seconds: u64| timestamp(UNIX_EPOCH + Duration::from_secs(seconds));
        assert_eq!(at(0).as_deref(), Some("19700101000000"));
        assert_eq!(at(951_782_400).as_deref(), Some("20000229000000"));
        assert_eq!(at(1_795_000_000).as_deref(), Some("20261118110640"));
        assert_eq!(
            timestamp(UNIX_EPOCH - Duration::from_secs(1)).as_deref(),
            None
        );
    }

    // Errors

    #[test]
    fn replies_map_to_error_kinds() {
        let reply = |code: u32, text: &str| {
            FtpError::UnexpectedResponse(Response::new(
                Status::from(code),
                format!("{code} {text}").into_bytes(),
            ))
        };
        let kind = |error: FtpError| ftp_error(&error, "/x").kind();
        assert_eq!(
            kind(reply(550, "No such file or directory")),
            ErrorKind::NotFound
        );
        assert_eq!(
            kind(reply(550, "Permission denied")),
            ErrorKind::PermissionDenied
        );
        assert_eq!(kind(reply(550, "File exists")), ErrorKind::AlreadyExists);
        assert_eq!(
            kind(reply(553, "Could not create file")),
            ErrorKind::PermissionDenied
        );
        assert_eq!(kind(reply(421, "Timeout")), ErrorKind::ConnectionFailed);
        assert_eq!(kind(reply(500, "Unknown command")), ErrorKind::Other);
        assert_eq!(
            kind(FtpError::ConnectionError(io::Error::from(
                io::ErrorKind::ConnectionReset
            ))),
            ErrorKind::ConnectionFailed
        );
    }
    // Transfers

    struct MockServer {
        port: u16,
        logins: Arc<std::sync::atomic::AtomicUsize>,
        received: Arc<StdMutex<Vec<usize>>>,
    }

    fn mock_server(transfer_reply: &'static str) -> MockServer {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binding failed");
        let port = listener.local_addr().expect("no address").port();
        let logins = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(StdMutex::new(Vec::new()));
        let (counter, sizes) = (Arc::clone(&logins), Arc::clone(&received));
        std::thread::spawn(move || {
            for socket in listener.incoming().filter_map(std::result::Result::ok) {
                let (counter, sizes) = (Arc::clone(&counter), Arc::clone(&sizes));
                std::thread::spawn(move || {
                    let mut writer = socket.try_clone().expect("cloning failed");
                    let mut reader = BufReader::new(socket);
                    let mut data_listener = None;
                    let _ = writer.write_all(
                        b"220 ready
",
                    );
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                        let command = line.trim_end().to_ascii_uppercase();
                        let reply = if command.starts_with("USER") {
                            "331 password
"
                            .to_string()
                        } else if command.starts_with("PASS") {
                            counter.fetch_add(1, Ordering::SeqCst);
                            "230 logged in
"
                            .to_string()
                        } else if command.starts_with("FEAT") {
                            "211-Features:
 EPSV
211 End
"
                            .to_string()
                        } else if command.starts_with("EPSV") {
                            let listener =
                                std::net::TcpListener::bind("127.0.0.1:0").expect("binding failed");
                            let port = listener.local_addr().expect("no address").port();
                            data_listener = Some(listener);
                            format!(
                                "229 passive (|||{port}|)
"
                            )
                        } else if command.starts_with("STOR") {
                            let Some(listener) = data_listener.take() else {
                                break;
                            };
                            let (mut data, _) = listener.accept().expect("accepting failed");
                            let _ = writer.write_all(
                                b"150 send
",
                            );
                            let mut content = Vec::new();
                            let _ = data.read_to_end(&mut content);
                            drop(data);
                            sizes.lock().expect("poisoned").push(content.len());
                            std::thread::sleep(Duration::from_millis(200));
                            transfer_reply.to_string()
                        } else if command.starts_with("QUIT") {
                            let _ = writer.write_all(
                                b"221 bye
",
                            );
                            break;
                        } else {
                            "200 ok
"
                            .to_string()
                        };
                        let _ = writer.write_all(reply.as_bytes());
                        line.clear();
                    }
                });
            }
        });
        MockServer {
            port,
            logins,
            received,
        }
    }

    async fn upload(server: &MockServer, uploads: usize) -> Vec<Result<()>> {
        use tokio::io::AsyncWriteExt;

        let params = ConnectionParams {
            protocol: Protocol::Ftp,
            host: "127.0.0.1".to_string(),
            port: server.port,
            username: "test".to_string(),
            authentication: Authentication::Password(test_password()),
        };
        let certificates = std::env::temp_dir().join("hoplix-mock-certificates");
        let file_system = FtpFileSystem::connect(&params, &certificates, None)
            .await
            .expect("connecting failed");
        let mut results = Vec::new();
        for index in 0..uploads {
            let path = format!("/upload-{index}.bin");
            let mut writer = file_system
                .open_write(&path, 0)
                .await
                .expect("opening failed");
            writer
                .write_all(&vec![7; 256 * 1024])
                .await
                .expect("writing failed");
            results.push(
                writer
                    .shutdown()
                    .await
                    .map_err(|error| Error::from_io(&error, &path)),
            );
        }
        file_system.close().await;
        results
    }

    #[tokio::test]
    async fn finished_uploads_report_the_server_reply_and_reuse_connections() {
        let server = mock_server(
            "226 done
",
        );
        let results = upload(&server, 2).await;
        assert!(
            results.iter().all(std::result::Result::is_ok),
            "{results:?}"
        );
        assert_eq!(*server.received.lock().expect("poisoned"), [256 * 1024; 2]);
        assert_eq!(
            server.logins.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "uploads did not reuse the transfer connection"
        );
    }

    #[tokio::test]
    async fn failed_uploads_are_reported_even_when_the_reply_is_late() {
        let server = mock_server(
            "426 Failure reading network stream.
",
        );
        let results = upload(&server, 1).await;
        assert!(results[0].is_err(), "{results:?}");
    }

    // Connection options

    struct OptionServer {
        port: u16,
        commands: Arc<StdMutex<Vec<Vec<u8>>>>,
    }

    impl OptionServer {
        fn commands(&self) -> Vec<String> {
            self.commands
                .lock()
                .expect("poisoned")
                .iter()
                .map(|line| String::from_utf8_lossy(line).trim_end().to_string())
                .collect()
        }

        fn raw(&self, prefix: &str) -> Option<Vec<u8>> {
            self.commands
                .lock()
                .expect("poisoned")
                .iter()
                .find(|line| line.starts_with(prefix.as_bytes()))
                .cloned()
        }
    }

    fn option_server(
        listing: &'static [u8],
        epsv: bool,
        reported: [u8; 4],
        bind: [u8; 4],
    ) -> OptionServer {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").expect("binding failed");
        let port = listener.local_addr().expect("no address").port();
        let commands = Arc::new(StdMutex::new(Vec::new()));
        let log = Arc::clone(&commands);
        std::thread::spawn(move || {
            for socket in listener.incoming().filter_map(std::result::Result::ok) {
                let log = Arc::clone(&log);
                std::thread::spawn(move || {
                    let mut writer = socket.try_clone().expect("cloning failed");
                    let mut reader = BufReader::new(socket);
                    let mut passive: Option<TcpListener> = None;
                    let mut active: Option<SocketAddr> = None;
                    let _ = writer.write_all(b"220 ready\r\n");
                    let mut line = Vec::new();
                    while reader
                        .read_until(b'\n', &mut line)
                        .is_ok_and(|read| read > 0)
                    {
                        log.lock().expect("poisoned").push(line.clone());
                        let text = String::from_utf8_lossy(&line).trim_end().to_string();
                        let verb = text
                            .split(' ')
                            .next()
                            .unwrap_or_default()
                            .to_ascii_uppercase();
                        let argument = text
                            .split_once(' ')
                            .map(|(_, rest)| rest.to_string())
                            .unwrap_or_default();
                        let reply = match verb.as_str() {
                            "USER" => "331 password\r\n".to_string(),
                            "PASS" => "230 logged in\r\n".to_string(),
                            "FEAT" if epsv => {
                                "211-Features:\r\n EPSV\r\n UTF8\r\n211 End\r\n".to_string()
                            }
                            "FEAT" => "211-Features:\r\n UTF8\r\n211 End\r\n".to_string(),
                            "PWD" => "257 \"/\"\r\n".to_string(),
                            "CWD" => "250 ok\r\n".to_string(),
                            "EPSV" => {
                                let data = TcpListener::bind("127.0.0.1:0").expect("binding");
                                let data_port = data.local_addr().expect("address").port();
                                passive = Some(data);
                                format!("229 passive (|||{data_port}|)\r\n")
                            }
                            "PASV" => {
                                let data =
                                    TcpListener::bind((Ipv4Addr::from(bind), 0)).expect("binding");
                                let data_port = data.local_addr().expect("address").port();
                                passive = Some(data);
                                let [a, b, c, d] = reported;
                                format!(
                                    "227 passive ({a},{b},{c},{d},{},{})\r\n",
                                    data_port / 256,
                                    data_port % 256
                                )
                            }
                            "PORT" => {
                                let numbers: Vec<u8> = argument
                                    .split(',')
                                    .filter_map(|part| part.trim().parse().ok())
                                    .collect();
                                if let [a, b, c, d, high, low] = numbers[..] {
                                    let data_port = u16::from(high) * 256 + u16::from(low);
                                    active = Some(SocketAddr::from((
                                        Ipv4Addr::new(a, b, c, d),
                                        data_port,
                                    )));
                                }
                                "200 port ok\r\n".to_string()
                            }
                            "LIST" | "NLST" => {
                                let _ = writer.write_all(b"150 listing\r\n");
                                let data = match passive.take() {
                                    Some(listener) => listener.accept().ok().map(|(data, _)| data),
                                    None => active
                                        .take()
                                        .and_then(|address| TcpStream::connect(address).ok()),
                                };
                                if let Some(mut data) = data {
                                    let _ = data.write_all(listing);
                                }
                                "226 done\r\n".to_string()
                            }
                            "DELE" => "250 deleted\r\n".to_string(),
                            "SITE" if argument.contains("refused") => {
                                "550 Permission denied.\r\n".to_string()
                            }
                            "SITE" if argument.contains("unknown") => {
                                "500 Unknown SITE command.\r\n".to_string()
                            }
                            "SITE" => "200 SITE CHMOD command ok.\r\n".to_string(),
                            "QUIT" => {
                                let _ = writer.write_all(b"221 bye\r\n");
                                break;
                            }
                            _ => "200 ok\r\n".to_string(),
                        };
                        let _ = writer.write_all(reply.as_bytes());
                        line.clear();
                    }
                });
            }
        });
        OptionServer { port, commands }
    }

    async fn connect_to(server: &OptionServer, options: &ConnectionOptions) -> FtpFileSystem {
        let params = ConnectionParams {
            protocol: Protocol::Ftp,
            host: "127.0.0.1".to_string(),
            port: server.port,
            username: "test".to_string(),
            authentication: Authentication::Password(test_password()),
        };
        let certificates = std::env::temp_dir().join("hoplix-mock-certificates");
        FtpFileSystem::connect_via_with(&params, None, options, &certificates, None)
            .await
            .expect("connecting failed")
    }

    const LOOPBACK: [u8; 4] = [127, 0, 0, 1];
    const LISTING: &[u8] =
        b"-rw-r--r--    1 1000     1000            5 Mar 15 12:00 caf\xe9 \x80.txt\r\n";

    #[tokio::test]
    async fn permissions_are_changed_with_site_chmod() {
        let server = option_server(LISTING, true, LOOPBACK, LOOPBACK);
        let file_system = connect_to(&server, &ConnectionOptions::default()).await;
        assert!(file_system.can_set_permissions());
        file_system
            .set_permissions("/www/index.html", 0o644)
            .await
            .expect("changing failed");
        let error = file_system
            .set_permissions("/refused", 0o4755)
            .await
            .expect_err("refusal ignored");
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        let error = file_system
            .set_permissions("/unknown", 0o600)
            .await
            .expect_err("unknown command ignored");
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert!(error.detail().contains("Unknown SITE command"), "{error:?}");
        file_system.close().await;
        let commands = server.commands();
        for expected in [
            "SITE CHMOD 644 /www/index.html",
            "SITE CHMOD 4755 /refused",
            "SITE CHMOD 600 /unknown",
        ] {
            assert!(commands.iter().any(|line| line == expected), "{commands:?}");
        }
    }

    #[tokio::test]
    async fn properties_read_the_listing_of_the_parent() {
        let server = option_server(LISTING_WITH_OWNER, false, LOOPBACK, LOOPBACK);
        let file_system = connect_to(&server, &ConnectionOptions::default()).await;
        let properties = file_system
            .properties("/data/notes.txt")
            .await
            .expect("properties failed");
        assert_eq!(properties.permissions, Some(0o640));
        assert_eq!(properties.owner.as_deref(), Some("demo"));
        assert_eq!(properties.group.as_deref(), Some("staff"));
        file_system.close().await;
    }

    const LISTING_WITH_OWNER: &[u8] =
        b"-rw-r-----    1 demo     staff           5 Mar 15 12:00 notes.txt\r\n";

    #[tokio::test]
    async fn active_mode_sends_port_and_receives_the_listing() {
        let server = option_server(LISTING, true, LOOPBACK, LOOPBACK);
        let options = ConnectionOptions {
            ftp_mode: FtpMode::Active,
            ..ConnectionOptions::default()
        };
        let file_system = connect_to(&server, &options).await;
        let entries = file_system.list("/").await.expect("listing failed");
        assert_eq!(entries.len(), 1);
        file_system.close().await;
        let commands = server.commands();
        assert!(
            commands
                .iter()
                .any(|line| line.starts_with("PORT 127,0,0,1,")),
            "{commands:?}"
        );
        assert!(!commands.iter().any(|line| line == "EPSV" || line == "PASV"));
    }

    #[tokio::test]
    async fn passive_mode_uses_the_server_address_unless_told_otherwise() {
        let private = [10, 255, 255, 1];
        let server = option_server(LISTING, true, private, LOOPBACK);
        let file_system = connect_to(&server, &ConnectionOptions::default()).await;
        assert_eq!(
            file_system.list("/").await.expect("listing failed").len(),
            1
        );
        file_system.close().await;
        assert!(server.commands().iter().any(|line| line == "EPSV"));

        let server = option_server(LISTING, false, private, LOOPBACK);
        let file_system = connect_to(&server, &ConnectionOptions::default()).await;
        assert_eq!(
            file_system.list("/").await.expect("listing failed").len(),
            1
        );
        file_system.close().await;
        assert!(server.commands().iter().any(|line| line == "PASV"));

        let options = ConnectionOptions {
            ftp_external_ip: false,
            ..ConnectionOptions::default()
        };
        let other = [127, 0, 0, 2];
        let server = option_server(LISTING, true, other, other);
        let file_system = connect_to(&server, &options).await;
        assert_eq!(
            file_system.list("/").await.expect("listing failed").len(),
            1
        );
        file_system.close().await;
        let commands = server.commands();
        assert!(commands.iter().any(|line| line == "PASV"), "{commands:?}");
        assert!(!commands.iter().any(|line| line == "EPSV"));
    }

    #[test]
    fn data_modes_follow_the_options() {
        let with_epsv = Capabilities {
            epsv: true,
            ..Capabilities::default()
        };
        let default = ConnectionOptions::default();
        assert_eq!(data_mode(&default, with_epsv), Mode::ExtendedPassive);
        assert_eq!(data_mode(&default, Capabilities::default()), Mode::Passive);
        let reported = ConnectionOptions {
            ftp_external_ip: false,
            ..ConnectionOptions::default()
        };
        assert_eq!(data_mode(&reported, with_epsv), Mode::Passive);
        let active = ConnectionOptions {
            ftp_mode: FtpMode::Active,
            ..ConnectionOptions::default()
        };
        assert_eq!(data_mode(&active, with_epsv), Mode::Active);
    }

    #[tokio::test]
    async fn windows_1252_names_are_decoded_and_sent_as_bytes() {
        let server = option_server(LISTING, true, LOOPBACK, LOOPBACK);
        let options = ConnectionOptions {
            ftp_encoding: FileNameEncoding::Windows1252,
            ..ConnectionOptions::default()
        };
        let file_system = connect_to(&server, &options).await;
        let entries = file_system.list("/").await.expect("listing failed");
        assert_eq!(entries[0].name, "café €.txt");
        file_system
            .remove_file("/café €.txt")
            .await
            .expect("deleting failed");
        let error = file_system
            .remove_file("/日本.txt")
            .await
            .expect_err("unencodable names must fail");
        assert_eq!(error.kind(), ErrorKind::Other);
        assert!(file_system.home().await.is_ok());
        file_system.close().await;
        assert_eq!(
            server.raw("DELE").as_deref(),
            Some(&b"DELE /caf\xe9 \x80.txt\r\n"[..])
        );
        let commands = server.commands();
        assert!(!commands.iter().any(|line| line.starts_with("OPTS UTF8")));
        assert_eq!(
            commands
                .iter()
                .filter(|line| line.starts_with("DELE"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn utf8_stays_the_default() {
        let server = option_server(
            "-rw-r--r--    1 1000     1000            5 Mar 15 12:00 café.txt\r\n".as_bytes(),
            true,
            LOOPBACK,
            LOOPBACK,
        );
        let file_system = connect_to(&server, &ConnectionOptions::default()).await;
        let entries = file_system.list("/").await.expect("listing failed");
        assert_eq!(entries[0].name, "café.txt");
        file_system.close().await;
        assert!(server.commands().iter().any(|line| line == "OPTS UTF8 ON"));
    }

    #[tokio::test]
    async fn idle_connections_send_noop_keepalives() {
        let server = option_server(LISTING, true, LOOPBACK, LOOPBACK);
        let options = ConnectionOptions {
            keepalive_interval: Some(Duration::from_millis(200)),
            ..ConnectionOptions::default()
        };
        let file_system = connect_to(&server, &options).await;
        tokio::time::sleep(Duration::from_millis(900)).await;
        let noops = server
            .commands()
            .iter()
            .filter(|line| *line == "NOOP")
            .count();
        assert!(noops >= 2, "{:?}", server.commands());
        file_system.close().await;

        let server = option_server(LISTING, true, LOOPBACK, LOOPBACK);
        let file_system = connect_to(&server, &ConnectionOptions::default()).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!server.commands().iter().any(|line| line == "NOOP"));
        file_system.close().await;
    }
}
