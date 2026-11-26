use std::sync::Arc;

use crate::search::Walk;
use crate::{Entry, FileSystem, Result};

// Constants

pub const PERMISSION_BITS: u32 = 0o7777;
const ACCESS_LETTERS: [char; 3] = ['r', 'w', 'x'];

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowsAttributes {
    pub read_only: bool,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Properties {
    pub entry: Entry,
    pub permissions: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub attributes: Option<WindowsAttributes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionChange {
    pub mode: u32,
    pub recursive: bool,
    pub files: bool,
    pub folders: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FolderSize {
    pub bytes: u64,
    pub files: u64,
    pub folders: u64,
    pub unreadable: u64,
}

// Properties

impl Properties {
    pub fn from_entry(entry: Entry) -> Self {
        Self {
            entry,
            permissions: None,
            owner: None,
            group: None,
            attributes: None,
        }
    }
}

impl PermissionChange {
    pub fn single(mode: u32) -> Self {
        Self {
            mode,
            recursive: false,
            files: true,
            folders: true,
        }
    }

    fn applies_to(&self, entry: &Entry, is_root: bool) -> bool {
        if is_root && (!self.recursive || !entry.is_directory()) {
            return true;
        }
        if entry.is_directory() {
            self.folders
        } else {
            self.files
        }
    }
}

// Permission text

pub fn permission_text(mode: u32) -> String {
    let mut text = String::with_capacity(9);
    for group in 0..3 {
        let shift = 6 - group * 3;
        for (bit, letter) in ACCESS_LETTERS.iter().enumerate() {
            let set = mode >> (shift + 2 - bit as u32) & 1 == 1;
            let special = bit == 2 && mode >> (11 - group) & 1 == 1;
            text.push(match (set, special) {
                (true, false) => *letter,
                (false, false) => '-',
                (true, true) if group == 2 => 't',
                (false, true) if group == 2 => 'T',
                (true, true) => 's',
                (false, true) => 'S',
            });
        }
    }
    text
}

pub fn parse_permission_text(text: &str) -> Option<u32> {
    let characters: Vec<char> = text.chars().collect();
    if characters.len() != 9 {
        return None;
    }
    let mut mode = 0;
    for (index, character) in characters.iter().enumerate() {
        let group = index / 3;
        let bit = index % 3;
        let value = 1 << (8 - index);
        let special = 1 << (11 - group);
        match (bit, character) {
            (_, '-') => {}
            (0, 'r') | (1, 'w') | (2, 'x') => mode |= value,
            (2, 's') if group < 2 => mode |= value | special,
            (2, 'S') if group < 2 => mode |= special,
            (2, 't') if group == 2 => mode |= value | special,
            (2, 'T') if group == 2 => mode |= special,
            _ => return None,
        }
    }
    Some(mode)
}

pub fn parse_mode(text: &str) -> Option<u32> {
    let text = text.trim();
    if !(3..=4).contains(&text.len()) || !text.bytes().all(|byte| (b'0'..=b'7').contains(&byte)) {
        return None;
    }
    u32::from_str_radix(text, 8).ok()
}

pub fn mode_text(mode: u32) -> String {
    if mode & 0o7000 == 0 {
        format!("{:03o}", mode & 0o777)
    } else {
        format!("{:04o}", mode & PERMISSION_BITS)
    }
}

// Operations

pub async fn set_permissions_with(
    file_system: Arc<dyn FileSystem>,
    path: &str,
    change: PermissionChange,
) -> Result<usize> {
    let root = file_system.metadata(path).await?;
    let mut changed = 0;
    if change.applies_to(&root, true) {
        file_system.set_permissions(path, change.mode).await?;
        changed += 1;
    }
    if !change.recursive || !root.is_directory() || root.is_link {
        return Ok(changed);
    }
    let mut walk = Walk::new(Arc::clone(&file_system), path, None);
    while let Some(visited) = walk.next().await {
        for entry in visited.result? {
            if entry.is_link || !change.applies_to(&entry, false) {
                continue;
            }
            let child = file_system.join(&visited.directory, &entry.name);
            file_system.set_permissions(&child, change.mode).await?;
            changed += 1;
        }
    }
    Ok(changed)
}

pub async fn folder_size(file_system: Arc<dyn FileSystem>, path: &str) -> FolderSize {
    let mut size = FolderSize::default();
    let mut walk = Walk::new(file_system, path, None);
    while let Some(visited) = walk.next().await {
        let Ok(entries) = visited.result else {
            size.unreadable += 1;
            continue;
        };
        for entry in entries {
            if entry.is_directory() {
                size.folders += 1;
            } else {
                size.files += 1;
                size.bytes += entry.size.unwrap_or(0);
            }
        }
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalFileSystem;

    // Permission text

    #[test]
    fn modes_format_as_letters() {
        assert_eq!(permission_text(0o755), "rwxr-xr-x");
        assert_eq!(permission_text(0o640), "rw-r-----");
        assert_eq!(permission_text(0o4755), "rwsr-xr-x");
        assert_eq!(permission_text(0o2644), "rw-r-Sr--");
        assert_eq!(permission_text(0o1777), "rwxrwxrwt");
        assert_eq!(permission_text(0o1776), "rwxrwxrwT");
    }

    #[test]
    fn letters_parse_back_to_modes() {
        for mode in [0o755, 0o640, 0o4755, 0o2644, 0o1777, 0o1776, 0, 0o777] {
            assert_eq!(parse_permission_text(&permission_text(mode)), Some(mode));
        }
        assert_eq!(parse_permission_text("rwxr-xr-"), None);
        assert_eq!(parse_permission_text("rwxr-xr-q"), None);
        assert_eq!(parse_permission_text("wrxr-xr-x"), None);
    }

    #[test]
    fn octal_modes_parse_and_format() {
        assert_eq!(parse_mode("755"), Some(0o755));
        assert_eq!(parse_mode(" 0644 "), Some(0o644));
        assert_eq!(parse_mode("4755"), Some(0o4755));
        for wrong in ["", "75", "758", "12345", "rwx"] {
            assert_eq!(parse_mode(wrong), None, "{wrong:?}");
        }
        assert_eq!(mode_text(0o644), "644");
        assert_eq!(mode_text(0o4755), "4755");
        assert_eq!(mode_text(0o100_644), "644");
    }

    // Changes

    #[test]
    fn changes_pick_files_and_folders() {
        let file = Entry {
            name: "a".to_string(),
            kind: crate::EntryKind::File,
            size: Some(1),
            modified: None,
            is_link: false,
        };
        let folder = Entry {
            kind: crate::EntryKind::Directory,
            ..file.clone()
        };
        let files_only = PermissionChange {
            mode: 0o644,
            recursive: true,
            files: true,
            folders: false,
        };
        assert!(files_only.applies_to(&file, false));
        assert!(!files_only.applies_to(&folder, false));
        assert!(!files_only.applies_to(&folder, true));
        assert!(files_only.applies_to(&file, true));
        assert!(PermissionChange::single(0o700).applies_to(&folder, true));
    }

    #[tokio::test]
    async fn folder_sizes_add_up_every_level() {
        let directory = std::env::temp_dir().join(format!("hoplix-size-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("a/b")).expect("creating failed");
        std::fs::write(directory.join("top.txt"), b"12345").expect("writing failed");
        std::fs::write(directory.join("a/one.txt"), b"1").expect("writing failed");
        std::fs::write(directory.join("a/b/two.txt"), b"12").expect("writing failed");
        let size = folder_size(Arc::new(LocalFileSystem), &directory.to_string_lossy()).await;
        assert_eq!(
            size,
            FolderSize {
                bytes: 8,
                files: 3,
                folders: 2,
                unreadable: 0,
            }
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_permissions_change_recursively() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!("hoplix-chmod-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("sub")).expect("creating failed");
        std::fs::write(directory.join("sub/file.txt"), b"x").expect("writing failed");
        let mode = |path: &std::path::Path| {
            std::fs::metadata(path)
                .expect("metadata failed")
                .permissions()
                .mode()
                & 0o777
        };
        let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
        let root = directory.to_string_lossy().into_owned();
        let changed = set_permissions_with(
            Arc::clone(&file_system),
            &root,
            PermissionChange {
                mode: 0o600,
                recursive: true,
                files: true,
                folders: false,
            },
        )
        .await
        .expect("changing failed");
        assert_eq!(changed, 1);
        assert_eq!(mode(&directory.join("sub/file.txt")), 0o600);
        assert_ne!(mode(&directory.join("sub")), 0o600);
        let properties = file_system
            .properties(&directory.join("sub/file.txt").to_string_lossy())
            .await
            .expect("properties failed");
        assert_eq!(properties.permissions, Some(0o600));
        assert!(properties.owner.is_some());
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
