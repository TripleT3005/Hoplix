use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::time::SystemTime;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::{Entry, Error, ErrorKind, HashAlgorithm, Result};

// Types

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type Reader = Box<dyn AsyncRead + Send + Unpin>;
pub type Writer = Box<dyn AsyncWrite + Send + Unpin>;

// Interface

pub trait FileSystem: Debug + Send + Sync {
    fn label(&self) -> Option<String> {
        None
    }

    fn is_remote(&self) -> bool {
        self.label().is_some()
    }

    fn home(&self) -> BoxFuture<'_, Result<String>>;

    fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>>;

    fn metadata(&self, path: &str) -> BoxFuture<'_, Result<Entry>>;

    fn create_directory(&self, path: &str) -> BoxFuture<'_, Result<()>>;

    fn remove_file(&self, path: &str) -> BoxFuture<'_, Result<()>>;

    fn remove_directory(&self, path: &str) -> BoxFuture<'_, Result<()>>;

    fn rename(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>>;

    fn open_read(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Reader>>;

    fn open_write(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Writer>>;

    fn set_modified(&self, path: &str, _modified: SystemTime) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(async move { Err(Error::new(ErrorKind::Other, format!("{path}: unsupported"))) })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn checksum_algorithms(&self) -> BoxFuture<'_, Vec<HashAlgorithm>> {
        Box::pin(async { Vec::new() })
    }

    fn checksum(&self, path: &str, _algorithm: HashAlgorithm) -> BoxFuture<'_, Result<Vec<u8>>> {
        let error = crate::checksum::unsupported(path);
        Box::pin(async move { Err(error) })
    }

    fn join(&self, base: &str, name: &str) -> String;

    fn parent(&self, path: &str) -> Option<String>;

    fn file_name<'a>(&self, path: &'a str) -> &'a str {
        path.trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(path)
    }
}

// Operations

pub async fn remove_recursive(file_system: &dyn FileSystem, path: &str) -> Result<()> {
    let root = file_system.metadata(path).await?;
    if !root.is_directory() || root.is_link {
        return file_system.remove_file(path).await;
    }
    let mut pending = vec![path.to_string()];
    let mut directories = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in file_system.list(&directory).await? {
            let child = file_system.join(&directory, &entry.name);
            if entry.is_directory() && !entry.is_link {
                pending.push(child);
            } else {
                file_system.remove_file(&child).await?;
            }
        }
        directories.push(directory);
    }
    for directory in directories.iter().rev() {
        file_system.remove_directory(directory).await?;
    }
    Ok(())
}

pub async fn copy_file(
    source: &dyn FileSystem,
    from: &str,
    target: &dyn FileSystem,
    to: &str,
    resume: bool,
) -> Result<u64> {
    let offset = if resume {
        match target.metadata(to).await {
            Ok(entry) => entry.size.unwrap_or(0),
            Err(error) if error.kind() == ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        }
    } else {
        0
    };
    let total = source.metadata(from).await?.size.unwrap_or(0);
    if resume && offset >= total && offset > 0 {
        return Ok(0);
    }
    let mut reader = source.open_read(from, offset).await?;
    let mut writer = target.open_write(to, offset).await?;
    let copied = tokio::io::copy(&mut reader, &mut writer)
        .await
        .map_err(|error| Error::from_io(&error, to))?;
    writer
        .shutdown()
        .await
        .map_err(|error| Error::from_io(&error, to))?;
    Ok(copied)
}

pub fn is_valid_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalFileSystem;

    #[tokio::test]
    async fn files_copy_and_resume() {
        let directory = std::env::temp_dir().join(format!("hoplix-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("creating failed");
        let source = directory.join("source.bin").to_string_lossy().into_owned();
        let target = directory.join("target.bin").to_string_lossy().into_owned();
        std::fs::write(&source, b"0123456789").expect("writing failed");

        let local = LocalFileSystem;
        assert_eq!(
            copy_file(&local, &source, &local, &target, false).await,
            Ok(10)
        );
        std::fs::write(&target, b"0123").expect("truncating failed");
        assert_eq!(
            copy_file(&local, &source, &local, &target, true).await,
            Ok(6)
        );
        assert_eq!(
            std::fs::read(&target).expect("reading failed"),
            b"0123456789"
        );
        assert_eq!(
            copy_file(&local, &source, &local, &target, true).await,
            Ok(0)
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn names_reject_separators_and_dots() {
        assert!(is_valid_name("report.pdf"));
        assert!(is_valid_name(" spaced name "));
        for name in ["", "  ", ".", "..", "a/b", "a\\b", "nul\0"] {
            assert!(!is_valid_name(name), "{name:?}");
        }
    }
}
