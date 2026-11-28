use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, ready};
use std::time::SystemTime;

use russh::ChannelMsg;
use russh::ChannelStream;
use russh::client::Msg;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, OnceCell};

use crate::connection::{ConnectionParams, HostKeyTrust};
use crate::file_system::{Reader, Writer};
use crate::keyboard_interactive::SharedPrompter;
use crate::remote_shell::{
    CommandOutput, LOCALE, Listed, entry_argument, list_command, parse_listing, rename_command,
    simple_command, stat_command, touch_command,
};
use crate::route::Route;
use crate::scp_protocol::{
    self, ScpDownload, ScpUpload, TreeSummary, start_download, start_upload,
};
use crate::sftp::{file_name_posix, join_posix, parent_posix};
use crate::ssh::{SshLink, SshTarget, auto_reconnect, connection_error};
use crate::{
    BoxFuture, ConnectionOptions, Entry, Error, ErrorKind, FileSystem, HashAlgorithm,
    PERMISSION_BITS, Properties, Result,
};

// Constants

pub const SCP_NO_RESUME: &str = "SCP cannot resume transfers";
const MAX_OUTPUT: usize = 64 * 1024 * 1024;
const HASH_PROGRAMS: [(HashAlgorithm, &str); 3] = [
    (HashAlgorithm::Sha256, "sha256sum"),
    (HashAlgorithm::Sha1, "sha1sum"),
    (HashAlgorithm::Md5, "md5sum"),
];

static SPOOL_COUNTER: AtomicU64 = AtomicU64::new(0);

// Types

pub struct ScpFileSystem {
    target: SshTarget,
    connection: AsyncMutex<Arc<SshLink>>,
    label: String,
    hashes: OnceCell<Vec<HashAlgorithm>>,
}

struct SpoolUpload {
    file: Option<tokio::fs::File>,
    spool: PathBuf,
    link: Arc<SshLink>,
    path: String,
    host: String,
    finishing: Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send>>>,
}

// Connecting

impl ScpFileSystem {
    pub async fn connect_prompting(
        params: &ConnectionParams,
        route: &Route,
        options: &ConnectionOptions,
        known_hosts: &Path,
        trusted: &[HostKeyTrust],
        prompter: Option<SharedPrompter>,
    ) -> Result<Self> {
        let (target, link) =
            SshTarget::connect(params, route, options, known_hosts, trusted, prompter).await?;
        Ok(Self {
            target,
            connection: AsyncMutex::new(Arc::new(link)),
            label: params.label(),
            hashes: OnceCell::new(),
        })
    }

    async fn current(&self) -> Result<Arc<SshLink>> {
        let mut slot = self.connection.lock().await;
        if slot.is_closed() {
            if !auto_reconnect() {
                return Err(self.target.closed_error());
            }
            *slot = Arc::new(self.target.reconnect().await?);
        }
        Ok(Arc::clone(&slot))
    }

    async fn reconnect(&self, stale: &Arc<SshLink>) -> Result<Arc<SshLink>> {
        let mut slot = self.connection.lock().await;
        if Arc::ptr_eq(&slot, stale) {
            *slot = Arc::new(self.target.reconnect().await?);
        }
        Ok(Arc::clone(&slot))
    }

    async fn run<T, F, Fut>(&self, operation: F) -> Result<T>
    where
        F: Fn(Arc<SshLink>) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let link = self.current().await?;
        match operation(Arc::clone(&link)).await {
            Err(error) if error.kind() == ErrorKind::ConnectionFailed && auto_reconnect() => {
                let link = self.reconnect(&link).await?;
                operation(link).await
            }
            result => result,
        }
    }

    fn host(&self) -> String {
        self.target.params.host.clone()
    }

    async fn command(&self, command: String, path: &str) -> Result<CommandOutput> {
        let host = self.host();
        let path = path.to_string();
        self.run(move |link| {
            let (command, host, path) = (command.clone(), host.clone(), path.clone());
            async move { execute(&link, &command, &host).await?.check(&path) }
        })
        .await
    }

    async fn listed(&self, path: &str) -> Result<(Listed, Option<Listed>)> {
        let output = self.command(stat_command(path, false), path).await?;
        let listed = parse_listing(&output.stdout)
            .into_iter()
            .next()
            .ok_or_else(|| Error::new(ErrorKind::Other, format!("{path}: unreadable listing")))?;
        if !listed.is_link() {
            return Ok((listed, None));
        }
        let followed = self
            .command(stat_command(path, true), path)
            .await
            .ok()
            .and_then(|output| parse_listing(&output.stdout).into_iter().next());
        Ok((listed, followed))
    }

    pub async fn download_tree(&self, remote: &str, local_parent: &Path) -> Result<TreeSummary> {
        let command = format!("scp -r -p -f -- {}", entry_argument(remote));
        let link = self.current().await?;
        let mut stream = open_stream(&link, &command, &self.host()).await?;
        scp_protocol::receive_tree(&mut stream, local_parent)
            .await
            .map_err(|error| Error::from_io(&error, remote))
    }

    pub async fn upload_tree(&self, local: &Path, remote_parent: &str) -> Result<TreeSummary> {
        let command = format!("scp -r -p -t -- {}", entry_argument(remote_parent));
        let link = self.current().await?;
        let mut stream = open_stream(&link, &command, &self.host()).await?;
        let summary = scp_protocol::send_tree(&mut stream, local, true)
            .await
            .map_err(|error| Error::from_io(&error, remote_parent))?;
        let _ = stream.shutdown().await;
        Ok(summary)
    }
}

// Channels

async fn execute(link: &SshLink, command: &str, host: &str) -> Result<CommandOutput> {
    let mut channel = link
        .handle
        .channel_open_session()
        .await
        .map_err(|error| connection_error(host, &error))?;
    channel
        .exec(true, command)
        .await
        .map_err(|error| connection_error(host, &error))?;
    let mut output = CommandOutput::default();
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => output.stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, ext: 1 } => output.stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => output.status = Some(exit_status),
            ChannelMsg::Failure => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("{host}: the server refused to run a command"),
                ));
            }
            _ => {}
        }
        if output.stdout.len() + output.stderr.len() > MAX_OUTPUT {
            return Err(Error::new(
                ErrorKind::Other,
                format!("{host}: too much output"),
            ));
        }
    }
    Ok(output)
}

async fn open_stream(link: &SshLink, command: &str, host: &str) -> Result<ChannelStream<Msg>> {
    let channel = link
        .handle
        .channel_open_session()
        .await
        .map_err(|error| connection_error(host, &error))?;
    channel
        .exec(true, command)
        .await
        .map_err(|error| connection_error(host, &error))?;
    Ok(channel.into_stream())
}

fn no_resume(path: &str) -> Error {
    Error::new(ErrorKind::Unsupported, format!("{path}: {SCP_NO_RESUME}"))
}

fn download_command(path: &str) -> String {
    format!("scp -f -- {}", entry_argument(path))
}

fn upload_command(path: &str) -> String {
    format!("scp -t -- {}", entry_argument(path))
}

async fn open_upload(
    link: &SshLink,
    path: &str,
    size: u64,
    host: &str,
) -> Result<ScpUpload<ChannelStream<Msg>>> {
    let stream = open_stream(link, &upload_command(path), host).await?;
    start_upload(stream, file_name_posix(path), size, None)
        .await
        .map_err(|error| Error::from_io(&error, path))
}

// Spooling

impl SpoolUpload {
    async fn create(link: Arc<SshLink>, path: &str, host: String) -> Result<Self> {
        let spool = std::env::temp_dir().join(format!(
            "hoplix-scp-{}-{}.part",
            std::process::id(),
            SPOOL_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let file = tokio::fs::File::create(&spool)
            .await
            .map_err(|error| Error::from_io(&error, &spool.to_string_lossy()))?;
        Ok(Self {
            file: Some(file),
            spool,
            link,
            path: path.to_string(),
            host,
            finishing: None,
        })
    }

    fn finish(&mut self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send>> {
        let file = self.file.take();
        let spool = self.spool.clone();
        let link = Arc::clone(&self.link);
        let path = self.path.clone();
        let host = self.host.clone();
        Box::pin(async move {
            if let Some(mut file) = file {
                file.flush().await?;
            }
            let mut file = tokio::fs::File::open(&spool).await?;
            let size = file.metadata().await?.len();
            let mut upload = open_upload(&link, &path, size, &host)
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            tokio::io::copy(&mut file, &mut upload).await?;
            upload.shutdown().await
        })
    }
}

impl AsyncWrite for SpoolUpload {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut().file.as_mut() {
            Some(file) => Pin::new(file).poll_write(context, data),
            None => Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut().file.as_mut() {
            Some(file) => Pin::new(file).poll_flush(context),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.finishing.is_none() {
            if this.file.is_none() {
                return Poll::Ready(Ok(()));
            }
            this.finishing = Some(this.finish());
        }
        let Some(finishing) = this.finishing.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        let result = ready!(finishing.as_mut().poll(context));
        this.finishing = None;
        let _ = std::fs::remove_file(&this.spool);
        Poll::Ready(result)
    }
}

impl Drop for SpoolUpload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.spool);
    }
}

// Checksums

fn parse_hash(output: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(output).ok()?;
    let hex = text.trim_start_matches('\\').split_whitespace().next()?;
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(hex.get(index..index + 2)?, 16).ok())
        .collect()
}

fn hash_probe_command() -> String {
    let programs: Vec<&str> = HASH_PROGRAMS.iter().map(|(_, program)| *program).collect();
    format!(
        "for program in {}; do command -v \"$program\" >/dev/null 2>&1 && echo \"$program\"; done",
        programs.join(" ")
    )
}

fn available_hashes(output: &[u8]) -> Vec<HashAlgorithm> {
    let found: Vec<&str> = std::str::from_utf8(output)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .collect();
    HASH_PROGRAMS
        .iter()
        .filter(|(_, program)| found.contains(program))
        .map(|(algorithm, _)| *algorithm)
        .collect()
}

// Interface

impl FileSystem for ScpFileSystem {
    fn label(&self) -> Option<String> {
        Some(self.label.clone())
    }

    fn url(&self, path: &str) -> Option<String> {
        Some(self.target.params.url(path))
    }

    fn can_read_from_offset(&self) -> bool {
        false
    }

    fn can_append(&self) -> bool {
        false
    }

    fn home(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let output = self.command(format!("{LOCALE}pwd"), ".").await?;
            let home = String::from_utf8_lossy(&output.stdout)
                .trim_end_matches(['\n', '\r'])
                .to_string();
            if home.starts_with('/') {
                Ok(home)
            } else {
                Ok("/".to_string())
            }
        })
    }

    fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
        let path = path.to_string();
        Box::pin(async move {
            let output = self.command(list_command(&path, false), &path).await?;
            let listed = parse_listing(&output.stdout);
            let followed = if listed.iter().any(Listed::is_link) {
                let host = self.host();
                let command = list_command(&path, true);
                let output = self
                    .run(move |link| {
                        let (command, host) = (command.clone(), host.clone());
                        async move { execute(&link, &command, &host).await }
                    })
                    .await?;
                parse_listing(&output.stdout)
            } else {
                Vec::new()
            };
            Ok(listed
                .iter()
                .map(|item| {
                    let target = item
                        .is_link()
                        .then(|| followed.iter().find(|other| other.name == item.name))
                        .flatten()
                        .filter(|other| !other.is_link());
                    item.entry(target, &item.name)
                })
                .collect())
        })
    }

    fn metadata(&self, path: &str) -> BoxFuture<'_, Result<Entry>> {
        let path = path.to_string();
        Box::pin(async move {
            let (listed, followed) = self.listed(&path).await?;
            Ok(listed.entry(followed.as_ref(), file_name_posix(&path)))
        })
    }

    fn create_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            self.command(simple_command("mkdir", &path), &path)
                .await
                .map(drop)
        })
    }

    fn remove_file(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            let (listed, _) = self.listed(&path).await?;
            if listed.kind == 'd' {
                return Err(Error::new(
                    ErrorKind::Other,
                    format!("{path}: Is a directory"),
                ));
            }
            self.command(simple_command("rm", &path), &path)
                .await
                .map(drop)
        })
    }

    fn remove_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            self.command(simple_command("rmdir", &path), &path)
                .await
                .map(drop)
        })
    }

    fn rename(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        let (from, to) = (from.to_string(), to.to_string());
        Box::pin(async move {
            match self.command(rename_command(&from, &to), &from).await {
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    Err(Error::new(ErrorKind::AlreadyExists, to))
                }
                result => result.map(drop),
            }
        })
    }

    fn open_read(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Reader>> {
        let path = path.to_string();
        Box::pin(async move {
            if offset > 0 {
                return Err(no_resume(&path));
            }
            let host = self.host();
            self.run(move |link| {
                let (path, host) = (path.clone(), host.clone());
                async move {
                    let stream = open_stream(&link, &download_command(&path), &host).await?;
                    let (download, _): (ScpDownload<_>, _) = start_download(stream)
                        .await
                        .map_err(|error| Error::from_io(&error, &path))?;
                    Ok(Box::new(download) as Reader)
                }
            })
            .await
        })
    }

    fn open_write(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Writer>> {
        let path = path.to_string();
        Box::pin(async move {
            if offset > 0 {
                return Err(no_resume(&path));
            }
            let link = self.current().await?;
            let spool = SpoolUpload::create(link, &path, self.host()).await?;
            Ok(Box::new(spool) as Writer)
        })
    }

    fn open_write_sized(
        &self,
        path: &str,
        offset: u64,
        size: u64,
    ) -> BoxFuture<'_, Result<Writer>> {
        let path = path.to_string();
        Box::pin(async move {
            if offset > 0 {
                return Err(no_resume(&path));
            }
            let host = self.host();
            self.run(move |link| {
                let (path, host) = (path.clone(), host.clone());
                async move {
                    let upload = open_upload(&link, &path, size, &host).await?;
                    Ok(Box::new(upload) as Writer)
                }
            })
            .await
        })
    }

    fn set_modified(&self, path: &str, modified: SystemTime) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            let command = touch_command(&path, modified)
                .ok_or_else(|| Error::new(ErrorKind::Other, format!("{path}: time")))?;
            self.command(command, &path).await.map(drop)
        })
    }

    fn properties(&self, path: &str) -> BoxFuture<'_, Result<Properties>> {
        let path = path.to_string();
        Box::pin(async move {
            let (listed, followed) = self.listed(&path).await?;
            Ok(listed.properties(followed.as_ref(), file_name_posix(&path)))
        })
    }

    fn can_set_permissions(&self) -> bool {
        true
    }

    fn set_permissions(&self, path: &str, mode: u32) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move {
            let command = format!(
                "{LOCALE}chmod {:04o} -- {}",
                mode & PERMISSION_BITS,
                entry_argument(&path)
            );
            self.command(command, &path).await.map(drop)
        })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let link = Arc::clone(&*self.connection.lock().await);
            link.disconnect().await;
        })
    }

    fn checksum_algorithms(&self) -> BoxFuture<'_, Vec<HashAlgorithm>> {
        Box::pin(async {
            self.hashes
                .get_or_init(|| async {
                    self.command(hash_probe_command(), ".")
                        .await
                        .map(|output| available_hashes(&output.stdout))
                        .unwrap_or_default()
                })
                .await
                .clone()
        })
    }

    fn checksum(&self, path: &str, algorithm: HashAlgorithm) -> BoxFuture<'_, Result<Vec<u8>>> {
        let path = path.to_string();
        Box::pin(async move {
            let Some((_, program)) = HASH_PROGRAMS
                .iter()
                .find(|(candidate, _)| *candidate == algorithm)
            else {
                return Err(crate::checksum::unsupported(&path));
            };
            if !self.checksum_algorithms().await.contains(&algorithm) {
                return Err(crate::checksum::unsupported(&path));
            }
            let command = format!("{LOCALE}{program} -b -- {}", entry_argument(&path));
            let output = self.command(command, &path).await?;
            parse_hash(&output.stdout).ok_or_else(|| crate::checksum::unsupported(&path))
        })
    }

    fn join(&self, base: &str, name: &str) -> String {
        join_posix(base, name)
    }

    fn parent(&self, path: &str) -> Option<String> {
        parent_posix(path)
    }

    fn file_name<'a>(&self, path: &'a str) -> &'a str {
        file_name_posix(path)
    }
}

impl fmt::Debug for ScpFileSystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScpFileSystem")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_shell::quote;

    #[test]
    fn transfer_commands_quote_paths() {
        assert_eq!(
            download_command("/srv/a 'b'"),
            "scp -f -- '/srv/a '\\''b'\\'''"
        );
        assert_eq!(upload_command("/srv/x/"), "scp -t -- '/srv/x'");
        assert!(hash_probe_command().starts_with("for program in sha256sum sha1sum md5sum;"));
        assert_eq!(quote("-x"), "'-x'");
    }

    #[test]
    fn hash_output_is_parsed() {
        assert_eq!(parse_hash(b"0a1B *file name\n"), Some(vec![0x0a, 0x1b]));
        assert_eq!(parse_hash(b"\\0aff *odd\\nname\n"), Some(vec![0x0a, 0xff]));
        assert_eq!(parse_hash(b"abc file"), None);
        assert_eq!(parse_hash(b"zz file"), None);
        assert_eq!(
            available_hashes(b"md5sum\nsha256sum\n"),
            [HashAlgorithm::Sha256, HashAlgorithm::Md5]
        );
        assert_eq!(no_resume("/x").kind(), ErrorKind::Unsupported);
    }
}
