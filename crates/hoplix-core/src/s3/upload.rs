use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncReadExt, AsyncWrite};

use super::client::{Call, Client, Part};
use crate::{BoxFuture, Error, ErrorKind, HashAlgorithm, Result};

// Constants

pub(crate) const PART_SIZE: usize = 8 * 1024 * 1024;
pub(crate) const MIN_PART: u64 = 5 * 1024 * 1024;
const MAX_COPY_PART: u64 = 1024 * 1024 * 1024;
const PARTS_PER_STEP: u32 = 1000;
const MAX_GROWTH: u32 = 4;
const MAX_PARTS: u32 = 10_000;

// Types

struct Uploader {
    client: Arc<Client>,
    key: String,
    upload: Option<String>,
    parts: Vec<Part>,
    shared: Arc<Mutex<Option<String>>>,
}

type Step = BoxFuture<'static, (Uploader, Result<()>)>;

pub(crate) struct ObjectWriter {
    client: Arc<Client>,
    key: String,
    uploader: Option<Uploader>,
    buffer: Vec<u8>,
    busy: Option<Step>,
    closing: bool,
    finished: bool,
    failure: Option<(io::ErrorKind, String)>,
    shared: Arc<Mutex<Option<String>>>,
}

// Uploads

impl Uploader {
    fn part_size(&self) -> usize {
        let step = u32::try_from(self.parts.len())
            .unwrap_or(u32::MAX)
            .saturating_div(PARTS_PER_STEP)
            .min(MAX_GROWTH);
        PART_SIZE << step
    }

    fn next_number(&self) -> Result<u32> {
        let number = u32::try_from(self.parts.len() + 1).unwrap_or(u32::MAX);
        if number > MAX_PARTS {
            return Err(Error::new(
                ErrorKind::Other,
                format!("/{}: too many parts", self.key),
            ));
        }
        Ok(number)
    }

    async fn start(&mut self) -> Result<String> {
        if let Some(upload) = &self.upload {
            return Ok(upload.clone());
        }
        let upload = self.client.create_upload(&self.key).await?;
        if let Ok(mut shared) = self.shared.lock() {
            *shared = Some(upload.clone());
        }
        self.upload = Some(upload.clone());
        Ok(upload)
    }

    async fn send_part(&mut self, data: Vec<u8>) -> Result<()> {
        let upload = self.start().await?;
        let number = self.next_number()?;
        let expected = HashAlgorithm::Md5.digest(&data);
        let part = self
            .client
            .upload_part(&self.key, &upload, number, data)
            .await?;
        check_etag(&self.key, part.comparable.as_deref(), &expected)?;
        self.parts.push(Part {
            number,
            etag: part.etag,
            comparable: None,
        });
        Ok(())
    }

    async fn finish(&mut self, data: Vec<u8>) -> Result<()> {
        if self.upload.is_none() {
            let expected = HashAlgorithm::Md5.digest(&data);
            let etag = self.client.put(&self.key, data).await?;
            return check_etag(&self.key, etag.as_deref(), &expected);
        }
        if !data.is_empty() || self.parts.is_empty() {
            self.send_part(data).await?;
        }
        let upload = self.start().await?;
        self.client
            .complete_upload(&self.key, &upload, &self.parts)
            .await?;
        if let Ok(mut shared) = self.shared.lock() {
            *shared = None;
        }
        Ok(())
    }

    async fn copy_range(&mut self, source: &str, length: u64) -> Result<()> {
        let upload = self.start().await?;
        let pieces = length.div_ceil(MAX_COPY_PART).max(1);
        let size = length.div_ceil(pieces);
        let mut start = 0;
        while start < length {
            let end = (start + size).min(length) - 1;
            let number = self.next_number()?;
            let part = self
                .client
                .copy_part(&self.key, &upload, number, source, (start, end))
                .await?;
            self.parts.push(part);
            start = end + 1;
        }
        Ok(())
    }
}

pub(crate) async fn copy_large(client: Arc<Client>, from: &str, to: &str, size: u64) -> Result<()> {
    let mut uploader = Uploader {
        client: Arc::clone(&client),
        key: to.to_string(),
        upload: None,
        parts: Vec::new(),
        shared: Arc::new(Mutex::new(None)),
    };
    let result = async {
        uploader.copy_range(from, size).await?;
        let upload = uploader.start().await?;
        client.complete_upload(to, &upload, &uploader.parts).await
    }
    .await;
    if result.is_err()
        && let Some(upload) = &uploader.upload
    {
        let _ = client.abort_upload(to, upload).await;
    }
    result
}

fn check_etag(key: &str, etag: Option<&str>, expected: &[u8]) -> Result<()> {
    match etag.and_then(etag_digest) {
        Some(digest) if digest != expected => {
            Err(Error::new(ErrorKind::ChecksumMismatch, format!("/{key}")))
        }
        _ => Ok(()),
    }
}

pub(crate) fn etag_digest(etag: &str) -> Option<Vec<u8>> {
    let hex = etag.trim().trim_matches('"');
    if hex.len() != 32 {
        return None;
    }
    crate::key_value::from_hex(&hex.to_ascii_lowercase())
}

// Writers

impl ObjectWriter {
    pub(crate) async fn open(client: Arc<Client>, key: &str, offset: u64) -> Result<Self> {
        let shared = Arc::new(Mutex::new(None));
        let mut uploader = Uploader {
            client: Arc::clone(&client),
            key: key.to_string(),
            upload: None,
            parts: Vec::new(),
            shared: Arc::clone(&shared),
        };
        let mut buffer = Vec::new();
        if offset > 0 {
            let existing = client
                .head(key)
                .await?
                .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("/{key}")))?;
            if existing.size < offset {
                return Err(Error::new(
                    ErrorKind::Other,
                    format!("/{key}: cannot continue after the end of the object"),
                ));
            }
            if offset < MIN_PART {
                buffer = read_prefix(&client, key, offset).await?;
            } else if let Err(error) = uploader.copy_range(key, offset).await {
                if let Some(upload) = &uploader.upload {
                    let _ = client.abort_upload(key, upload).await;
                }
                return Err(error);
            }
        }
        Ok(Self {
            client,
            key: key.to_string(),
            uploader: Some(uploader),
            buffer,
            busy: None,
            closing: false,
            finished: false,
            failure: None,
            shared,
        })
    }

    fn failed(&self) -> io::Error {
        match &self.failure {
            Some((kind, message)) => io::Error::new(*kind, message.clone()),
            None => io::Error::other("the upload was closed"),
        }
    }

    fn poll_busy(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Some(step) = &mut self.busy {
            let (uploader, result) = ready!(step.as_mut().poll(context));
            self.busy = None;
            self.uploader = Some(uploader);
            if let Err(error) = result {
                let kind = match error.kind() {
                    ErrorKind::NotFound => io::ErrorKind::NotFound,
                    ErrorKind::PermissionDenied => io::ErrorKind::PermissionDenied,
                    ErrorKind::ConnectionFailed => io::ErrorKind::ConnectionAborted,
                    _ => io::ErrorKind::Other,
                };
                self.failure = Some((kind, error.detail().to_string()));
                self.closing = false;
            } else if self.closing {
                self.finished = true;
            }
        }
        if self.failure.is_some() {
            return Poll::Ready(Err(self.failed()));
        }
        Poll::Ready(Ok(()))
    }

    fn begin(&mut self, finish: bool, data: Vec<u8>) {
        let Some(mut uploader) = self.uploader.take() else {
            return;
        };
        self.closing = finish;
        self.busy = Some(Box::pin(async move {
            let result = if finish {
                uploader.finish(data).await
            } else {
                uploader.send_part(data).await
            };
            (uploader, result)
        }));
    }

    fn part_size(&self) -> usize {
        self.uploader
            .as_ref()
            .map_or(PART_SIZE, Uploader::part_size)
    }
}

async fn read_prefix(client: &Client, key: &str, length: u64) -> Result<Vec<u8>> {
    let call = Call::new("GET", Some(key)).header("Range", &format!("bytes=0-{}", length - 1));
    let (_, body) = client.open(&call).await?;
    let mut data = Vec::new();
    body.take(length)
        .read_to_end(&mut data)
        .await
        .map_err(|error| Error::from_io(&error, &format!("/{key}")))?;
    if data.len() as u64 != length {
        return Err(Error::new(
            ErrorKind::Other,
            format!("/{key}: the object changed while continuing the upload"),
        ));
    }
    Ok(data)
}

impl AsyncWrite for ObjectWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        loop {
            ready!(this.poll_busy(context))?;
            if this.finished || this.closing {
                return Poll::Ready(Err(this.failed()));
            }
            let size = this.part_size();
            if this.buffer.len() >= size {
                let rest = this.buffer.split_off(size);
                let part = std::mem::replace(&mut this.buffer, rest);
                this.begin(false, part);
                continue;
            }
            let count = data.len().min(size - this.buffer.len());
            this.buffer.extend_from_slice(&data[..count]);
            return Poll::Ready(Ok(count));
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().poll_busy(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            ready!(this.poll_busy(context))?;
            if this.finished {
                return Poll::Ready(Ok(()));
            }
            let size = this.part_size();
            let multipart = this
                .uploader
                .as_ref()
                .is_some_and(|uploader| uploader.upload.is_some());
            if this.buffer.len() > size || (multipart && this.buffer.len() == size) {
                let rest = this.buffer.split_off(size);
                let part = std::mem::replace(&mut this.buffer, rest);
                this.begin(false, part);
                continue;
            }
            let data = std::mem::take(&mut this.buffer);
            this.begin(true, data);
        }
    }
}

impl Drop for ObjectWriter {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let upload = self.shared.lock().ok().and_then(|mut shared| shared.take());
        if let (Some(upload), Ok(runtime)) = (upload, tokio::runtime::Handle::try_current()) {
            let client = Arc::clone(&self.client);
            let key = self.key.clone();
            runtime.spawn(async move {
                let _ = client.abort_upload(&key, &upload).await;
            });
        }
    }
}
