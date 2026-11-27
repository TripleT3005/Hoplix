use std::fs;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tokio::io::AsyncSeekExt;

use crate::file_system::{Reader, Writer};
use crate::{BoxFuture, Entry, EntryKind, Error, ErrorKind, FileSystem, Properties, Result};

// Types

#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFileSystem;

// Interface

impl FileSystem for LocalFileSystem {
    fn home(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async {
            std::env::home_dir()
                .or_else(|| std::env::current_dir().ok())
                .map(|path| path.to_string_lossy().into_owned())
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "home"))
        })
    }

    fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
        let path = path.to_string();
        Box::pin(blocking(move || read_directory(&path)))
    }

    fn metadata(&self, path: &str) -> BoxFuture<'_, Result<Entry>> {
        let path = path.to_string();
        Box::pin(blocking(move || {
            let target = PathBuf::from(&path);
            let link =
                fs::symlink_metadata(&target).map_err(|error| Error::from_io(&error, &path))?;
            let name = target
                .file_name()
                .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
            Ok(entry_from(
                name,
                link.file_type().is_symlink(),
                fs::metadata(&target).ok(),
            ))
        }))
    }

    fn create_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(blocking(move || {
            fs::create_dir(&path).map_err(|error| Error::from_io(&error, &path))
        }))
    }

    fn remove_file(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(blocking(move || {
            let link =
                fs::symlink_metadata(&path).map_err(|error| Error::from_io(&error, &path))?;
            let result =
                if link.file_type().is_symlink() && fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
                    fs::remove_dir(&path)
                } else {
                    fs::remove_file(&path)
                };
            result.map_err(|error| Error::from_io(&error, &path))
        }))
    }

    fn remove_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(blocking(move || {
            fs::remove_dir(&path).map_err(|error| Error::from_io(&error, &path))
        }))
    }

    fn rename(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        let (from, to) = (from.to_string(), to.to_string());
        Box::pin(blocking(move || {
            if fs::symlink_metadata(&to).is_ok() {
                return Err(Error::new(ErrorKind::AlreadyExists, to));
            }
            fs::rename(&from, &to).map_err(|error| Error::from_io(&error, &from))
        }))
    }

    fn open_read(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Reader>> {
        let path = path.to_string();
        Box::pin(async move {
            let mut file = tokio::fs::File::open(&path)
                .await
                .map_err(|error| Error::from_io(&error, &path))?;
            if offset > 0 {
                file.seek(SeekFrom::Start(offset))
                    .await
                    .map_err(|error| Error::from_io(&error, &path))?;
            }
            Ok(Box::new(file) as Reader)
        })
    }

    fn open_write(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Writer>> {
        let path = path.to_string();
        Box::pin(async move {
            let map = |error: std::io::Error| Error::from_io(&error, &path);
            let file = if offset == 0 {
                tokio::fs::File::create(&path).await.map_err(map)?
            } else {
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .await
                    .map_err(map)?;
                file.set_len(offset).await.map_err(map)?;
                file.seek(SeekFrom::Start(offset)).await.map_err(map)?;
                file
            };
            Ok(Box::new(file) as Writer)
        })
    }

    fn set_modified(&self, path: &str, modified: SystemTime) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(blocking(move || {
            fs::File::options()
                .write(true)
                .open(&path)
                .and_then(|file| file.set_modified(modified))
                .map_err(|error| Error::from_io(&error, &path))
        }))
    }

    fn properties(&self, path: &str) -> BoxFuture<'_, Result<Properties>> {
        let path = path.to_string();
        Box::pin(blocking(move || read_properties(&path)))
    }

    fn can_set_permissions(&self) -> bool {
        cfg!(unix)
    }

    fn set_permissions(&self, path: &str, mode: u32) -> BoxFuture<'_, Result<()>> {
        let path = path.to_string();
        Box::pin(blocking(move || change_permissions(&path, mode)))
    }

    fn join(&self, base: &str, name: &str) -> String {
        Path::new(base).join(name).to_string_lossy().into_owned()
    }

    fn parent(&self, path: &str) -> Option<String> {
        Path::new(path)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(|parent| parent.to_string_lossy().into_owned())
    }
}

// Reading

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| Error::new(ErrorKind::Other, error.to_string()))?
}

fn read_directory(path: &str) -> Result<Vec<Entry>> {
    let directory = PathBuf::from(path);
    let metadata = fs::metadata(&directory).map_err(|error| Error::from_io(&error, path))?;
    if !metadata.is_dir() {
        return Err(Error::new(ErrorKind::NotADirectory, path));
    }
    let reader = fs::read_dir(&directory).map_err(|error| Error::from_io(&error, path))?;
    let entries = reader
        .filter_map(std::result::Result::ok)
        .map(|item| {
            let name = item.file_name().to_string_lossy().into_owned();
            let is_link = item.file_type().is_ok_and(|kind| kind.is_symlink());
            entry_from(name, is_link, fs::metadata(item.path()).ok())
        })
        .collect();
    Ok(entries)
}

fn read_properties(path: &str) -> Result<Properties> {
    let target = PathBuf::from(path);
    let link = fs::symlink_metadata(&target).map_err(|error| Error::from_io(&error, path))?;
    let metadata = fs::metadata(&target).unwrap_or_else(|_| link.clone());
    let name = target.file_name().map_or_else(
        || path.to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut properties = Properties::from_entry(entry_from(
        name,
        link.file_type().is_symlink(),
        Some(metadata.clone()),
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        properties.permissions = Some(metadata.mode() & crate::PERMISSION_BITS);
        properties.owner = Some(metadata.uid().to_string());
        properties.group = Some(metadata.gid().to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const READ_ONLY: u32 = 0x1;
        const HIDDEN: u32 = 0x2;
        let attributes = metadata.file_attributes();
        properties.attributes = Some(crate::WindowsAttributes {
            read_only: attributes & READ_ONLY != 0,
            hidden: attributes & HIDDEN != 0,
        });
    }
    Ok(properties)
}

#[cfg(unix)]
fn change_permissions(path: &str, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(mode & crate::PERMISSION_BITS),
    )
    .map_err(|error| Error::from_io(&error, path))
}

#[cfg(not(unix))]
fn change_permissions(path: &str, _mode: u32) -> Result<()> {
    Err(Error::new(ErrorKind::Unsupported, path))
}

#[cfg(unix)]
fn unix_details(metadata: Option<&fs::Metadata>) -> (Option<u32>, Option<String>) {
    use std::os::unix::fs::MetadataExt;
    metadata.map_or((None, None), |metadata| {
        (
            Some(metadata.mode() & crate::PERMISSION_BITS),
            Some(metadata.uid().to_string()),
        )
    })
}

#[cfg(not(unix))]
fn unix_details(_metadata: Option<&fs::Metadata>) -> (Option<u32>, Option<String>) {
    (None, None)
}

fn entry_from(name: String, is_link: bool, metadata: Option<fs::Metadata>) -> Entry {
    let is_directory = metadata.as_ref().is_some_and(fs::Metadata::is_dir);
    let (permissions, owner) = unix_details(metadata.as_ref());
    Entry {
        name,
        kind: if is_directory {
            EntryKind::Directory
        } else {
            EntryKind::File
        },
        size: metadata
            .as_ref()
            .filter(|_| !is_directory)
            .map(fs::Metadata::len),
        modified: metadata.and_then(|metadata| metadata.modified().ok()),
        is_link,
        permissions,
        owner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remove_recursive;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temporary_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("creating the test directory failed");
        directory
    }

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    // Listing

    #[tokio::test]
    async fn lists_files_and_directories() {
        let directory = temporary_directory("list");
        fs::create_dir(directory.join("folder")).expect("creating a folder failed");
        fs::write(directory.join("note.txt"), b"hello").expect("writing a file failed");

        let mut entries = LocalFileSystem
            .list(&text(&directory))
            .await
            .expect("listing failed");
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "folder");
        assert!(entries[0].is_directory());
        assert_eq!(entries[0].size, None);
        assert_eq!(entries[1].name, "note.txt");
        assert_eq!(entries[1].size, Some(5));
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test]
    async fn reports_missing_paths_and_files() {
        let directory = temporary_directory("errors");
        let missing = text(&directory.join("missing"));
        let error = LocalFileSystem
            .list(&missing)
            .await
            .expect_err("missing path listed");
        assert_eq!(error.kind(), ErrorKind::NotFound);

        let file = directory.join("file.txt");
        fs::write(&file, b"x").expect("writing a file failed");
        let error = LocalFileSystem
            .list(&text(&file))
            .await
            .expect_err("file listed");
        assert_eq!(error.kind(), ErrorKind::NotADirectory);
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Operations

    #[tokio::test]
    async fn creates_renames_and_removes() {
        let file_system = LocalFileSystem;
        let directory = temporary_directory("operations");
        let folder = text(&directory.join("folder"));
        file_system
            .create_directory(&folder)
            .await
            .expect("creating failed");
        let error = file_system
            .create_directory(&folder)
            .await
            .expect_err("created twice");
        assert_eq!(error.kind(), ErrorKind::AlreadyExists);

        let renamed = text(&directory.join("renamed"));
        file_system
            .rename(&folder, &renamed)
            .await
            .expect("renaming failed");
        let metadata = file_system
            .metadata(&renamed)
            .await
            .expect("metadata failed");
        assert_eq!(metadata.name, "renamed");
        assert!(metadata.is_directory());

        fs::write(directory.join("taken.txt"), b"x").expect("writing failed");
        let error = file_system
            .rename(&renamed, &text(&directory.join("taken.txt")))
            .await
            .expect_err("overwrote a file");
        assert_eq!(error.kind(), ErrorKind::AlreadyExists);

        fs::create_dir_all(directory.join("renamed/a/b")).expect("creating failed");
        fs::write(directory.join("renamed/a/b/deep.txt"), b"x").expect("writing failed");
        fs::write(directory.join("renamed/top.txt"), b"x").expect("writing failed");
        let error = file_system
            .remove_directory(&renamed)
            .await
            .expect_err("removed a full folder");
        assert_eq!(error.kind(), ErrorKind::DirectoryNotEmpty);
        remove_recursive(&file_system, &renamed)
            .await
            .expect("removing failed");
        assert!(!directory.join("renamed").exists());

        let file = text(&directory.join("taken.txt"));
        remove_recursive(&file_system, &file)
            .await
            .expect("removing a file failed");
        assert!(!directory.join("taken.txt").exists());
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test]
    async fn reads_and_writes_with_offsets() {
        let file_system = LocalFileSystem;
        let directory = temporary_directory("streams");
        let path = text(&directory.join("data.bin"));

        let mut writer = file_system
            .open_write(&path, 0)
            .await
            .expect("opening failed");
        writer
            .write_all(b"hello world")
            .await
            .expect("writing failed");
        writer.shutdown().await.expect("closing failed");

        let mut writer = file_system
            .open_write(&path, 6)
            .await
            .expect("resuming failed");
        writer.write_all(b"there").await.expect("writing failed");
        writer.shutdown().await.expect("closing failed");
        assert_eq!(fs::read(&path).expect("reading failed"), b"hello there");

        let mut reader = file_system
            .open_read(&path, 6)
            .await
            .expect("opening failed");
        let mut rest = String::new();
        reader
            .read_to_string(&mut rest)
            .await
            .expect("reading failed");
        assert_eq!(rest, "there");

        let mut writer = file_system
            .open_write(&path, 0)
            .await
            .expect("truncating failed");
        writer.write_all(b"new").await.expect("writing failed");
        writer.shutdown().await.expect("closing failed");
        assert_eq!(fs::read(&path).expect("reading failed"), b"new");
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Properties

    #[tokio::test]
    async fn properties_describe_the_platform_details() {
        let directory = temporary_directory("properties");
        let file = directory.join("file.txt");
        fs::write(&file, b"abc").expect("writing failed");
        let properties = LocalFileSystem
            .properties(&text(&file))
            .await
            .expect("properties failed");
        assert_eq!(properties.entry.name, "file.txt");
        assert_eq!(properties.entry.size, Some(3));
        if cfg!(windows) {
            assert_eq!(
                properties.attributes,
                Some(crate::WindowsAttributes::default())
            );
            assert_eq!(properties.permissions, None);
            let error = LocalFileSystem
                .set_permissions(&text(&file), 0o600)
                .await
                .expect_err("changed permissions on Windows");
            assert_eq!(error.kind(), ErrorKind::Unsupported);
            assert!(!LocalFileSystem.can_set_permissions());
        } else {
            assert!(properties.permissions.is_some());
            assert!(LocalFileSystem.can_set_permissions());
        }
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Paths

    #[test]
    fn joins_and_splits_paths() {
        let file_system = LocalFileSystem;
        let base = text(&std::env::temp_dir());
        let joined = file_system.join(&base, "child");
        assert_eq!(
            file_system.parent(&joined).as_deref(),
            Some(base.trim_end_matches(['/', '\\']))
        );
        assert_eq!(file_system.file_name(&joined), "child");
        assert_eq!(
            file_system.parent(if cfg!(windows) { "C:\\" } else { "/" }),
            None
        );
    }
}
