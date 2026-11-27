use hoplix_core::FileSystem;
use iced::Task;

use crate::app::{Hoplix, Message, Tab};

// Constants

#[cfg(windows)]
pub const LINE_BREAK: &str = "\r\n";
#[cfg(not(windows))]
pub const LINE_BREAK: &str = "\n";

// Formatting

pub fn location(file_system: &dyn FileSystem, path: &str) -> String {
    file_system.url(path).unwrap_or_else(|| path.to_string())
}

pub fn selection_paths(tab: &Tab) -> Vec<String> {
    let file_system = tab.file_system.as_ref();
    let entries = tab.selected_entries();
    if entries.is_empty() {
        return vec![location(file_system, &tab.path)];
    }
    entries
        .iter()
        .map(|entry| location(file_system, &file_system.join(&tab.path, &entry.name)))
        .collect()
}

// Clipboard

impl Hoplix {
    pub(crate) fn copy_paths(&mut self) -> Task<Message> {
        let Some(paths) = self
            .focused_tab()
            .filter(|tab| !tab.path.is_empty())
            .map(selection_paths)
        else {
            return Task::none();
        };
        self.notice = Some(if paths.len() == 1 {
            self.strings.get("copy_path.copied.one").to_string()
        } else {
            self.strings.format(
                "copy_path.copied.other",
                &[("count", &paths.len().to_string())],
            )
        });
        iced::clipboard::write(paths.join(LINE_BREAK))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Pane;
    use crate::settings::Settings;
    use hoplix_core::{
        Authentication, ConnectionParams, Entry, EntryKind, LocalFileSystem, Protocol,
    };

    #[derive(Debug)]
    struct Server {
        params: ConnectionParams,
    }

    impl FileSystem for Server {
        fn label(&self) -> Option<String> {
            Some(self.params.label())
        }

        fn url(&self, path: &str) -> Option<String> {
            Some(self.params.url(path))
        }

        fn home(&self) -> hoplix_core::BoxFuture<'_, hoplix_core::Result<String>> {
            LocalFileSystem.home()
        }

        fn list(&self, path: &str) -> hoplix_core::BoxFuture<'_, hoplix_core::Result<Vec<Entry>>> {
            LocalFileSystem.list(path)
        }

        fn metadata(&self, path: &str) -> hoplix_core::BoxFuture<'_, hoplix_core::Result<Entry>> {
            LocalFileSystem.metadata(path)
        }

        fn create_directory(
            &self,
            path: &str,
        ) -> hoplix_core::BoxFuture<'_, hoplix_core::Result<()>> {
            LocalFileSystem.create_directory(path)
        }

        fn remove_file(&self, path: &str) -> hoplix_core::BoxFuture<'_, hoplix_core::Result<()>> {
            LocalFileSystem.remove_file(path)
        }

        fn remove_directory(
            &self,
            path: &str,
        ) -> hoplix_core::BoxFuture<'_, hoplix_core::Result<()>> {
            LocalFileSystem.remove_directory(path)
        }

        fn rename(
            &self,
            from: &str,
            to: &str,
        ) -> hoplix_core::BoxFuture<'_, hoplix_core::Result<()>> {
            LocalFileSystem.rename(from, to)
        }

        fn open_read(
            &self,
            path: &str,
            offset: u64,
        ) -> hoplix_core::BoxFuture<'_, hoplix_core::Result<hoplix_core::Reader>> {
            LocalFileSystem.open_read(path, offset)
        }

        fn open_write(
            &self,
            path: &str,
            offset: u64,
        ) -> hoplix_core::BoxFuture<'_, hoplix_core::Result<hoplix_core::Writer>> {
            LocalFileSystem.open_write(path, offset)
        }

        fn join(&self, base: &str, name: &str) -> String {
            format!("{}/{name}", base.trim_end_matches('/'))
        }

        fn parent(&self, _path: &str) -> Option<String> {
            None
        }
    }

    fn file(name: &str) -> Entry {
        Entry {
            name: name.to_string(),
            kind: EntryKind::File,
            size: Some(1),
            modified: None,
            is_link: false,
            permissions: None,
            owner: None,
        }
    }

    #[test]
    fn remote_paths_become_urls_without_secrets() {
        let secret = crate::ui_tests::test_password();
        let server = std::sync::Arc::new(Server {
            params: ConnectionParams {
                protocol: Protocol::Sftp,
                host: "files.example.test".to_string(),
                port: 2222,
                username: "demo".to_string(),
                authentication: Authentication::Password(secret.clone()),
            },
        });
        let mut app = Hoplix::with_settings(Settings::default(), None);
        app.panes.push(Pane::default());
        let _ = app.open_tab(0, server, None, None);
        let tab = &mut app.panes[0].tabs[0];
        tab.path = "/var/www".to_string();
        assert_eq!(
            selection_paths(tab),
            ["sftp://demo@files.example.test:2222/var/www"]
        );
        tab.entries = vec![file("index.html"), file("my page.html")];
        tab.selection = vec![0, 1];
        let paths = selection_paths(tab);
        assert_eq!(
            paths,
            [
                "sftp://demo@files.example.test:2222/var/www/index.html",
                "sftp://demo@files.example.test:2222/var/www/my%20page.html",
            ]
        );
        assert!(paths.iter().all(|path| !path.contains(&secret)));
    }

    #[test]
    fn local_paths_stay_plain() {
        let mut app = Hoplix::with_settings(Settings::default(), None);
        app.panes.push(Pane::default());
        let _ = app.open_local_tab(0, None);
        let tab = &mut app.panes[0].tabs[0];
        let folder = std::env::temp_dir().to_string_lossy().into_owned();
        tab.path.clone_from(&folder);
        tab.entries = vec![file("a b.txt")];
        tab.selection = vec![0];
        assert_eq!(
            selection_paths(tab),
            [LocalFileSystem.join(&folder, "a b.txt")]
        );
    }
}
