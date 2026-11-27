use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::is_valid_name;
use crate::remote_shell::io_error;

// Constants

const MAX_LINE: usize = 64 * 1024;
const MAX_DEPTH: usize = 64;
const BUFFER_SIZE: usize = 32 * 1024;
const FILE_MODE: u32 = 0o644;
const DIRECTORY_MODE: u32 = 0o755;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Record {
    File { mode: u32, size: u64, name: String },
    Directory { mode: u32, name: String },
    End,
    Time { modified: u64, accessed: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreeSummary {
    pub files: u64,
    pub directories: u64,
    pub bytes: u64,
}

pub(crate) struct ScpDownload<S> {
    stream: S,
    remaining: u64,
    state: DownloadState,
    scratch: Box<[u8]>,
    message: Vec<u8>,
}

enum DownloadState {
    Body,
    Trailer,
    Message,
    Ack,
    Flush,
    Done,
}

type Finish = Pin<Box<dyn std::future::Future<Output = io::Result<()>> + Send>>;

pub(crate) struct ScpUpload<S> {
    state: UploadState<S>,
}

enum UploadState<S> {
    Streaming { stream: S, remaining: u64 },
    Finishing(Finish),
    Done,
}

// Records

fn protocol_error(text: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, text.into())
}

fn parse_mode(text: &str) -> io::Result<u32> {
    if text.len() != 4 || !text.bytes().all(|byte| (b'0'..=b'7').contains(&byte)) {
        return Err(protocol_error(format!("bad mode {text}")));
    }
    u32::from_str_radix(text, 8).map_err(|_| protocol_error("bad mode"))
}

fn parse_number(text: &str) -> io::Result<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(protocol_error(format!("bad number {text}")));
    }
    text.parse().map_err(|_| protocol_error("bad number"))
}

pub(crate) fn parse_record(line: &str) -> io::Result<Record> {
    let mut characters = line.chars();
    let kind = characters
        .next()
        .ok_or_else(|| protocol_error("empty record"))?;
    let rest = characters.as_str();
    match kind {
        'C' | 'D' => {
            let mut parts = rest.splitn(3, ' ');
            let mode = parse_mode(parts.next().unwrap_or_default())?;
            let size = parse_number(parts.next().unwrap_or_default())?;
            let name = parts.next().unwrap_or_default().to_string();
            if name.is_empty() || name == "." || name == ".." || name.contains('/') {
                return Err(protocol_error(format!("unexpected file name {name:?}")));
            }
            Ok(if kind == 'C' {
                Record::File { mode, size, name }
            } else {
                Record::Directory { mode, name }
            })
        }
        'E' if rest.is_empty() => Ok(Record::End),
        'T' => {
            let parts: Vec<&str> = rest.split(' ').collect();
            let [modified, modified_micros, accessed, accessed_micros] = parts.as_slice() else {
                return Err(protocol_error("bad time record"));
            };
            parse_number(modified_micros)?;
            parse_number(accessed_micros)?;
            Ok(Record::Time {
                modified: parse_number(modified)?,
                accessed: parse_number(accessed)?,
            })
        }
        _ => Err(protocol_error(format!("unexpected record {line:?}"))),
    }
}

impl Record {
    pub fn line(&self) -> String {
        match self {
            Self::File { mode, size, name } => format!("C{mode:04o} {size} {name}\n"),
            Self::Directory { mode, name } => format!("D{mode:04o} 0 {name}\n"),
            Self::End => "E\n".to_string(),
            Self::Time { modified, accessed } => format!("T{modified} 0 {accessed} 0\n"),
        }
    }
}

pub(crate) fn wire_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if matches!(character, '\n' | '\r' | '/') {
                '_'
            } else {
                character
            }
        })
        .collect();
    if is_valid_name(&cleaned) {
        cleaned
    } else {
        "file".to_string()
    }
}

// Lines and acknowledgements

async fn read_byte<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Option<u8>> {
    let mut byte = [0u8; 1];
    match stream.read(&mut byte).await? {
        0 => Ok(None),
        _ => Ok(Some(byte[0])),
    }
}

async fn read_line<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<String> {
    let mut line = Vec::new();
    loop {
        let byte = read_byte(stream)
            .await?
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        if byte == b'\n' {
            break;
        }
        if line.len() >= MAX_LINE {
            return Err(protocol_error("line too long"));
        }
        line.push(byte);
    }
    String::from_utf8(line).map_err(|_| protocol_error("record is not UTF-8"))
}

async fn remote_message<S: AsyncRead + Unpin>(stream: &mut S) -> io::Error {
    match read_line(stream).await {
        Ok(message) => io_error(&message),
        Err(error) => error,
    }
}

pub(crate) async fn read_ack<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<()> {
    match read_byte(stream).await? {
        Some(0) => Ok(()),
        Some(1 | 2) => Err(remote_message(stream).await),
        Some(other) => {
            let rest = read_line(stream).await.unwrap_or_default();
            Err(protocol_error(format!(
                "unexpected answer {}{rest}",
                char::from(other)
            )))
        }
        None => Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
    }
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(bytes).await?;
    stream.flush().await
}

async fn ack<S: AsyncWrite + Unpin>(stream: &mut S) -> io::Result<()> {
    send(stream, &[0]).await
}

async fn send_record<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    record: &Record,
) -> io::Result<()> {
    send(stream, record.line().as_bytes()).await?;
    read_ack(stream).await
}

async fn next_record<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Option<Record>> {
    match read_byte(stream).await? {
        None => Ok(None),
        Some(1 | 2) => Err(remote_message(stream).await),
        Some(first) => {
            let rest = read_line(stream).await?;
            parse_record(&format!("{}{rest}", char::from(first))).map(Some)
        }
    }
}

// Single files

pub(crate) async fn start_download<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
) -> io::Result<(ScpDownload<S>, Option<u64>)> {
    ack(&mut stream).await?;
    let mut modified = None;
    loop {
        match next_record(&mut stream).await? {
            Some(Record::Time { modified: time, .. }) => {
                modified = Some(time);
                ack(&mut stream).await?;
            }
            Some(Record::File { size, .. }) => {
                ack(&mut stream).await?;
                let download = ScpDownload {
                    stream,
                    remaining: size,
                    state: DownloadState::Body,
                    scratch: vec![0; BUFFER_SIZE].into_boxed_slice(),
                    message: Vec::new(),
                };
                return Ok((download, modified));
            }
            Some(Record::Directory { .. }) => {
                return Err(io::Error::other("Is a directory"));
            }
            Some(Record::End) => return Err(protocol_error("unexpected end record")),
            None => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for ScpDownload<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            match this.state {
                DownloadState::Body => {
                    if this.remaining == 0 {
                        this.state = DownloadState::Trailer;
                        continue;
                    }
                    if buffer.remaining() == 0 {
                        return Poll::Ready(Ok(()));
                    }
                    let wanted = usize::try_from(this.remaining)
                        .unwrap_or(usize::MAX)
                        .min(buffer.remaining())
                        .min(this.scratch.len());
                    let mut scratch = ReadBuf::new(&mut this.scratch[..wanted]);
                    ready!(Pin::new(&mut this.stream).poll_read(context, &mut scratch))?;
                    let read = scratch.filled().len();
                    if read == 0 {
                        return Poll::Ready(Err(io::Error::from(io::ErrorKind::UnexpectedEof)));
                    }
                    buffer.put_slice(scratch.filled());
                    this.remaining -= read as u64;
                    return Poll::Ready(Ok(()));
                }
                DownloadState::Trailer | DownloadState::Message => {
                    let mut byte = [0u8; 1];
                    let mut scratch = ReadBuf::new(&mut byte);
                    ready!(Pin::new(&mut this.stream).poll_read(context, &mut scratch))?;
                    let Some(&value) = scratch.filled().first() else {
                        return Poll::Ready(Err(io::Error::from(io::ErrorKind::UnexpectedEof)));
                    };
                    let trailer = matches!(this.state, DownloadState::Trailer);
                    match (trailer, value) {
                        (true, 0) => this.state = DownloadState::Ack,
                        (true, 1 | 2) => this.state = DownloadState::Message,
                        (true, _) => {
                            return Poll::Ready(Err(protocol_error("missing end of file")));
                        }
                        (_, b'\n') => {
                            let message = String::from_utf8_lossy(&this.message).into_owned();
                            this.state = DownloadState::Done;
                            return Poll::Ready(Err(io_error(&message)));
                        }
                        _ if this.message.len() < MAX_LINE => this.message.push(value),
                        _ => return Poll::Ready(Err(protocol_error("line too long"))),
                    }
                }
                DownloadState::Ack => {
                    let written = ready!(Pin::new(&mut this.stream).poll_write(context, &[0]))?;
                    if written == 1 {
                        this.state = DownloadState::Flush;
                    }
                }
                DownloadState::Flush => {
                    ready!(Pin::new(&mut this.stream).poll_flush(context))?;
                    this.state = DownloadState::Done;
                }
                DownloadState::Done => return Poll::Ready(Ok(())),
            }
        }
    }
}

pub(crate) async fn start_upload<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    name: &str,
    size: u64,
    modified: Option<u64>,
) -> io::Result<ScpUpload<S>> {
    read_ack(&mut stream).await?;
    if let Some(modified) = modified {
        let record = Record::Time {
            modified,
            accessed: modified,
        };
        send_record(&mut stream, &record).await?;
    }
    let record = Record::File {
        mode: FILE_MODE,
        size,
        name: wire_name(name),
    };
    send_record(&mut stream, &record).await?;
    Ok(ScpUpload {
        state: UploadState::Streaming {
            stream,
            remaining: size,
        },
    })
}

pub(crate) async fn finish_upload<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
) -> io::Result<()> {
    ack(&mut stream).await?;
    read_ack(&mut stream).await?;
    stream.shutdown().await
}

fn size_changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "the file size changed during the copy",
    )
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> AsyncWrite for ScpUpload<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let UploadState::Streaming { stream, remaining } = &mut this.state else {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        };
        if data.len() as u64 > *remaining {
            return Poll::Ready(Err(size_changed()));
        }
        let written = ready!(Pin::new(stream).poll_write(context, data))?;
        *remaining -= written as u64;
        Poll::Ready(Ok(written))
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().state {
            UploadState::Streaming { stream, .. } => Pin::new(stream).poll_flush(context),
            UploadState::Finishing(_) | UploadState::Done => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            match &mut this.state {
                UploadState::Streaming { remaining, .. } => {
                    if *remaining != 0 {
                        this.state = UploadState::Done;
                        return Poll::Ready(Err(size_changed()));
                    }
                    let UploadState::Streaming { stream, .. } =
                        std::mem::replace(&mut this.state, UploadState::Done)
                    else {
                        continue;
                    };
                    this.state = UploadState::Finishing(Box::pin(finish_upload(stream)));
                }
                UploadState::Finishing(finish) => {
                    let result = ready!(finish.as_mut().poll(context));
                    this.state = UploadState::Done;
                    return Poll::Ready(result);
                }
                UploadState::Done => return Poll::Ready(Ok(())),
            }
        }
    }
}

// Trees

fn local_time(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn seconds_of(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

pub(crate) async fn receive_tree<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    root: &Path,
) -> io::Result<TreeSummary> {
    let mut summary = TreeSummary::default();
    let mut directories: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut time = None;
    ack(stream).await?;
    while let Some(record) = next_record(stream).await? {
        let current = directories
            .last()
            .cloned()
            .unwrap_or_else(|| root.to_path_buf());
        match record {
            Record::Time { modified, .. } => {
                time = Some(modified);
                ack(stream).await?;
            }
            Record::Directory { name, .. } => {
                if directories.len() > MAX_DEPTH {
                    return Err(protocol_error("folders nested too deeply"));
                }
                let path = current.join(local_target(&name)?);
                tokio::fs::create_dir_all(&path).await?;
                directories.push(path);
                summary.directories += 1;
                time = None;
                ack(stream).await?;
            }
            Record::End => {
                if directories.len() <= 1 {
                    return Err(protocol_error("unexpected end record"));
                }
                directories.pop();
                ack(stream).await?;
            }
            Record::File { size, name, .. } => {
                let path = current.join(local_target(&name)?);
                let mut file = tokio::fs::File::create(&path).await?;
                ack(stream).await?;
                let copied = tokio::io::copy(&mut (&mut *stream).take(size), &mut file).await?;
                if copied != size {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
                file.flush().await?;
                if let Some(modified) = time.take() {
                    let file = file.into_std().await;
                    file.set_modified(local_time(modified))?;
                }
                read_ack(stream).await?;
                ack(stream).await?;
                summary.files += 1;
                summary.bytes += size;
            }
        }
    }
    if directories.len() > 1 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    Ok(summary)
}

fn local_target(name: &str) -> io::Result<&str> {
    if is_valid_name(name) && !name.contains(['\n', '\r', ':']) {
        Ok(name)
    } else {
        Err(protocol_error(format!("unusable file name {name:?}")))
    }
}

fn local_mode(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        if metadata.is_dir() {
            DIRECTORY_MODE
        } else {
            FILE_MODE
        }
    }
}

fn local_name(path: &Path) -> io::Result<String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| protocol_error(format!("{}: unusable name", path.display())))?;
    if name.contains(['\n', '\r']) || !is_valid_name(name) {
        return Err(protocol_error(format!("{}: unusable name", path.display())));
    }
    Ok(name.to_string())
}

enum Work {
    Enter(PathBuf, usize),
    Leave,
}

pub(crate) async fn send_tree<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    local: &Path,
    preserve: bool,
) -> io::Result<TreeSummary> {
    let mut summary = TreeSummary::default();
    read_ack(stream).await?;
    let mut work = vec![Work::Enter(local.to_path_buf(), 0)];
    while let Some(item) = work.pop() {
        let (path, depth) = match item {
            Work::Leave => {
                send_record(stream, &Record::End).await?;
                continue;
            }
            Work::Enter(path, depth) => (path, depth),
        };
        let metadata = tokio::fs::metadata(&path).await?;
        let name = local_name(&path)?;
        if preserve && let Some(modified) = metadata.modified().ok().and_then(seconds_of) {
            let accessed = metadata
                .accessed()
                .ok()
                .and_then(seconds_of)
                .unwrap_or(modified);
            send_record(stream, &Record::Time { modified, accessed }).await?;
        }
        if metadata.is_dir() {
            if depth >= MAX_DEPTH {
                return Err(protocol_error("folders nested too deeply"));
            }
            let mode = if cfg!(unix) {
                local_mode(&metadata)
            } else {
                DIRECTORY_MODE
            };
            send_record(stream, &Record::Directory { mode, name }).await?;
            summary.directories += 1;
            work.push(Work::Leave);
            let mut children = Vec::new();
            let mut entries = tokio::fs::read_dir(&path).await?;
            while let Some(entry) = entries.next_entry().await? {
                children.push(entry.path());
            }
            children.sort();
            for child in children.into_iter().rev() {
                work.push(Work::Enter(child, depth + 1));
            }
        } else {
            let size = metadata.len();
            let record = Record::File {
                mode: local_mode(&metadata),
                size,
                name,
            };
            send_record(stream, &record).await?;
            let file = tokio::fs::File::open(&path).await?;
            let copied = tokio::io::copy(&mut file.take(size), stream).await?;
            if copied != size {
                return Err(size_changed());
            }
            ack(stream).await?;
            read_ack(stream).await?;
            summary.files += 1;
            summary.bytes += size;
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{DuplexStream, duplex};

    // Records

    #[test]
    fn records_parse_and_format() {
        let cases = [
            (
                "C0644 12 a b.txt",
                Record::File {
                    mode: 0o644,
                    size: 12,
                    name: "a b.txt".to_string(),
                },
            ),
            (
                "D0755 0 Grüße",
                Record::Directory {
                    mode: 0o755,
                    name: "Grüße".to_string(),
                },
            ),
            ("E", Record::End),
            (
                "T1773579600 0 1773576000 0",
                Record::Time {
                    modified: 1_773_579_600,
                    accessed: 1_773_576_000,
                },
            ),
        ];
        for (line, record) in cases {
            assert_eq!(parse_record(line).expect("parsing failed"), record);
            assert_eq!(record.line(), format!("{line}\n"));
        }
        for line in [
            "",
            "C644 1 x",
            "C0644 -1 x",
            "C0644 1 a/b",
            "C0644 1 ..",
            "C0644 1 ",
            "D0755 0 .",
            "E extra",
            "T1 0 2",
            "X",
        ] {
            assert!(parse_record(line).is_err(), "{line:?}");
        }
        assert!(parse_record("C0644 1 a\\M-C").is_ok());
        assert!(local_target("a\\M-C").is_err());
        assert!(local_target("../x").is_err());
        assert_eq!(local_target("Grüße 'x'").ok(), Some("Grüße 'x'"));
        assert_eq!(wire_name("a\nb/c"), "a_b_c");
        assert_eq!(wire_name(".."), "file");
        assert_eq!(wire_name("it's"), "it's");
    }

    // Mock server

    async fn server_reads_byte(server: &mut DuplexStream) -> u8 {
        read_byte(server)
            .await
            .expect("read failed")
            .expect("closed")
    }

    async fn expect_ack(server: &mut DuplexStream) {
        assert_eq!(server_reads_byte(server).await, 0);
    }

    #[tokio::test]
    async fn downloads_follow_the_handshake() {
        let (client, mut server) = duplex(64);
        let peer = tokio::spawn(async move {
            expect_ack(&mut server).await;
            server
                .write_all(b"T1773579600 0 1773579600 0\n")
                .await
                .unwrap();
            expect_ack(&mut server).await;
            server.write_all(b"C0640 11 hello.txt\n").await.unwrap();
            expect_ack(&mut server).await;
            server.write_all(b"hello world\0").await.unwrap();
            expect_ack(&mut server).await;
        });
        let (mut download, modified) = start_download(client).await.expect("start failed");
        assert_eq!(modified, Some(1_773_579_600));
        let mut data = Vec::new();
        download.read_to_end(&mut data).await.expect("read failed");
        assert_eq!(data, b"hello world");
        peer.await.expect("server failed");
    }

    #[tokio::test]
    async fn download_errors_carry_the_message() {
        let (client, mut server) = duplex(64);
        tokio::spawn(async move {
            expect_ack(&mut server).await;
            server
                .write_all(b"\x01scp: /x: No such file or directory\n")
                .await
                .unwrap();
        });
        let error = start_download(client).await.err().expect("started");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);

        let (client, mut server) = duplex(64);
        tokio::spawn(async move {
            expect_ack(&mut server).await;
            server.write_all(b"C0644 4 x\n").await.unwrap();
            expect_ack(&mut server).await;
            server.write_all(b"ab\x02scp: read error\n").await.unwrap();
        });
        let (mut download, _) = start_download(client).await.expect("start failed");
        let mut data = Vec::new();
        assert!(download.read_to_end(&mut data).await.is_err());

        let (client, mut server) = duplex(64);
        tokio::spawn(async move {
            expect_ack(&mut server).await;
            server
                .write_all(b"C0644 2 x\nab\x02disk failed\n")
                .await
                .unwrap();
            let _ = server_reads_byte(&mut server).await;
        });
        let (mut download, _) = start_download(client).await.expect("start failed");
        let error = download
            .read_to_end(&mut Vec::new())
            .await
            .expect_err("read");
        assert_eq!(error.to_string(), "disk failed");

        let (client, mut server) = duplex(64);
        tokio::spawn(async move {
            expect_ack(&mut server).await;
            server.write_all(b"D0755 0 folder\n").await.unwrap();
        });
        assert!(start_download(client).await.is_err());
    }

    #[tokio::test]
    async fn uploads_announce_the_size_and_wait_for_acks() {
        let (client, mut server) = duplex(64);
        let peer = tokio::spawn(async move {
            server.write_all(&[0]).await.unwrap();
            let time = read_line(&mut server).await.unwrap();
            assert_eq!(time, "T1773579600 0 1773579600 0");
            server.write_all(&[0]).await.unwrap();
            let header = read_line(&mut server).await.unwrap();
            assert_eq!(header, "C0644 5 it's_x");
            server.write_all(&[0]).await.unwrap();
            let mut body = [0u8; 6];
            server.read_exact(&mut body).await.unwrap();
            assert_eq!(&body, b"hello\0");
            server.write_all(&[0]).await.unwrap();
            assert_eq!(read_byte(&mut server).await.unwrap(), None);
        });
        let mut upload = start_upload(client, "it's\nx", 5, Some(1_773_579_600))
            .await
            .expect("start failed");
        upload.write_all(b"hel").await.expect("write failed");
        upload.write_all(b"lo").await.expect("write failed");
        upload.shutdown().await.expect("finish failed");
        peer.await.expect("server failed");
    }

    #[tokio::test]
    async fn uploads_reject_wrong_sizes_and_server_errors() {
        let (client, mut server) = duplex(64);
        tokio::spawn(async move {
            server.write_all(&[0]).await.unwrap();
            let _ = read_line(&mut server).await;
            server.write_all(&[0]).await.unwrap();
            let mut sink = Vec::new();
            let _ = server.read_to_end(&mut sink).await;
        });
        let mut upload = start_upload(client, "x", 2, None)
            .await
            .expect("start failed");
        assert!(upload.write_all(b"abc").await.is_err());
        upload.write_all(b"a").await.expect("write failed");
        assert!(upload.shutdown().await.is_err());

        let (client, mut server) = duplex(64);
        tokio::spawn(async move {
            server.write_all(&[0]).await.unwrap();
            let _ = read_line(&mut server).await;
            server
                .write_all(b"\x01scp: /ro/x: Permission denied\n")
                .await
                .unwrap();
        });
        let error = start_upload(client, "x", 2, None)
            .await
            .err()
            .expect("started");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        let (client, mut server) = duplex(64);
        tokio::spawn(async move {
            server.write_all(b"\x07?").await.unwrap();
        });
        assert!(start_upload(client, "x", 0, None).await.is_err());
    }

    // Trees

    fn temporary(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-scp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("creating failed");
        directory
    }

    #[tokio::test]
    async fn trees_round_trip_with_folders_and_times() {
        let source = temporary("tree-source");
        let tree = source.join("top folder");
        std::fs::create_dir_all(tree.join("sub dir")).expect("creating failed");
        std::fs::write(tree.join("a'b $c.txt"), b"first").expect("writing failed");
        std::fs::write(tree.join("sub dir").join("Grüße.bin"), b"").expect("writing failed");
        std::fs::write(tree.join("z.txt"), vec![7u8; 100_000]).expect("writing failed");
        let time = local_time(1_773_579_600);
        std::fs::File::options()
            .write(true)
            .open(tree.join("z.txt"))
            .and_then(|file| file.set_modified(time))
            .expect("setting the time failed");

        let (mut client, mut server) = duplex(4096);
        let target = temporary("tree-target");
        let receiving = target.clone();
        let receiver = tokio::spawn(async move { receive_tree(&mut server, &receiving).await });
        let sent = send_tree(&mut client, &tree, true)
            .await
            .expect("sending failed");
        drop(client);
        let received = receiver
            .await
            .expect("join failed")
            .expect("receiving failed");
        assert_eq!(sent, received);
        assert_eq!(
            sent,
            TreeSummary {
                files: 3,
                directories: 2,
                bytes: 100_005,
            }
        );
        let copy = target.join("top folder");
        assert_eq!(
            std::fs::read(copy.join("a'b $c.txt")).expect("missing"),
            b"first"
        );
        assert!(copy.join("sub dir").join("Grüße.bin").is_file());
        let metadata = std::fs::metadata(copy.join("z.txt")).expect("missing");
        assert_eq!(metadata.len(), 100_000);
        assert_eq!(metadata.modified().expect("no time"), time);
        std::fs::remove_dir_all(source).expect("cleaning up failed");
        std::fs::remove_dir_all(target).expect("cleaning up failed");
    }

    #[tokio::test]
    async fn received_trees_refuse_escaping_names() {
        let target = temporary("tree-evil");
        let (mut client, mut server) = duplex(256);
        tokio::spawn(async move {
            expect_ack(&mut server).await;
            server.write_all(b"C0644 1 ../evil\n").await.unwrap();
        });
        assert!(receive_tree(&mut client, &target).await.is_err());
        assert!(!target.parent().expect("no parent").join("evil").exists());

        let (mut client, mut server) = duplex(256);
        tokio::spawn(async move {
            expect_ack(&mut server).await;
            server.write_all(b"E\n").await.unwrap();
        });
        assert!(receive_tree(&mut client, &target).await.is_err());

        let (mut client, mut server) = duplex(256);
        tokio::spawn(async move {
            expect_ack(&mut server).await;
            server.write_all(b"D0755 0 open\n").await.unwrap();
            expect_ack(&mut server).await;
        });
        assert!(receive_tree(&mut client, &target).await.is_err());
        std::fs::remove_dir_all(target).expect("cleaning up failed");
    }
}
