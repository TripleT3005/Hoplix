use std::fs;
use std::io;
use std::path::Path;

use crate::key_value::{self, Section};
use crate::sites::write_atomically;
use crate::{Authentication, ConnectionParams, Protocol, SiteAuth};

// Constants

pub const RECENT_FILE: &str = "recent.conf";
pub const MAX_RECENT: usize = 10;
const ENTRY_SECTION: &str = "connection";

// Types

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecentConnection {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SiteAuth,
    pub key_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecentList {
    pub entries: Vec<RecentConnection>,
}

// Connections

impl RecentConnection {
    pub fn from_params(params: &ConnectionParams) -> Self {
        let (auth, key_path) = match &params.authentication {
            Authentication::KeyFile { path, .. } if params.protocol.supports_key_files() => {
                (SiteAuth::KeyFile, path.to_string_lossy().into_owned())
            }
            _ => (SiteAuth::Password, String::new()),
        };
        Self {
            protocol: params.protocol,
            host: params.host.clone(),
            port: params.port,
            username: params.username.clone(),
            auth,
            key_path,
        }
    }

    pub fn label(&self) -> String {
        let port = if self.port == self.protocol.default_port() {
            String::new()
        } else {
            format!(":{}", self.port)
        };
        format!(
            "{}://{}@{}{port}",
            self.protocol.scheme(),
            self.username,
            self.host
        )
    }

    pub fn uses_key_file(&self) -> bool {
        self.protocol.supports_key_files() && self.auth == SiteAuth::KeyFile
    }

    fn from_section(section: &Section) -> Option<Self> {
        let protocol = section
            .get("protocol")
            .and_then(|scheme| Protocol::ALL.into_iter().find(|p| p.scheme() == scheme))?;
        let host = section.get("host").filter(|host| !host.is_empty())?;
        let auth = if section.get("auth") == Some("key") {
            SiteAuth::KeyFile
        } else {
            SiteAuth::Password
        };
        Some(Self {
            protocol,
            host: host.to_string(),
            port: section
                .get("port")
                .and_then(|port| port.parse().ok())
                .filter(|port| *port > 0)
                .unwrap_or(protocol.default_port()),
            username: section.get("username").unwrap_or_default().to_string(),
            auth,
            key_path: if auth == SiteAuth::KeyFile {
                section.get("key_path").unwrap_or_default().to_string()
            } else {
                String::new()
            },
        })
    }

    fn to_text(&self) -> String {
        let auth = match self.auth {
            SiteAuth::Password => "password",
            SiteAuth::KeyFile => "key",
        };
        format!(
            "\n[{ENTRY_SECTION}]\n{}",
            key_value::write(&[
                ("protocol", self.protocol.scheme().to_string()),
                ("host", self.host.clone()),
                ("port", self.port.to_string()),
                ("username", self.username.clone()),
                ("auth", auth.to_string()),
                ("key_path", self.key_path.clone()),
            ])
        )
    }
}

// Lists

impl RecentList {
    pub fn parse(text: &str) -> Self {
        let mut list = Self::default();
        for section in key_value::parse_sections(text) {
            if section.name == ENTRY_SECTION
                && let Some(entry) = RecentConnection::from_section(&section)
                && !list.entries.contains(&entry)
                && list.entries.len() < MAX_RECENT
            {
                list.entries.push(entry);
            }
        }
        list
    }

    pub fn to_text(&self) -> String {
        let mut text = String::from("# Hoplix recent connections (no passwords)\n");
        for entry in self.entries.iter().take(MAX_RECENT) {
            text.push_str(&entry.to_text());
        }
        text
    }

    pub fn record(&mut self, entry: RecentConnection) {
        self.entries.retain(|existing| *existing != entry);
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_RECENT);
    }

    pub fn remove(&mut self, index: usize) -> Option<RecentConnection> {
        (index < self.entries.len()).then(|| self.entries.remove(index))
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    // Storage

    pub fn load(directory: &Path) -> io::Result<Self> {
        match fs::read_to_string(directory.join(RECENT_FILE)) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, directory: &Path) -> io::Result<()> {
        let path = directory.join(RECENT_FILE);
        if self.entries.is_empty() {
            return match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        write_atomically(&path, self.to_text().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_password;
    use std::path::PathBuf;

    fn params(host: &str, authentication: Authentication) -> ConnectionParams {
        ConnectionParams {
            protocol: Protocol::Sftp,
            host: host.to_string(),
            port: 22,
            username: "demo".to_string(),
            authentication,
        }
    }

    fn temporary_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-recent-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("creating failed");
        directory
    }

    #[test]
    fn the_list_keeps_ten_entries_newest_first_without_duplicates() {
        let mut list = RecentList::default();
        for index in 0..12 {
            let host = format!("host{index}.example.test");
            list.record(RecentConnection::from_params(&params(
                &host,
                Authentication::Password(test_password()),
            )));
        }
        assert_eq!(list.entries.len(), MAX_RECENT);
        assert_eq!(list.entries[0].host, "host11.example.test");
        assert_eq!(list.entries[9].host, "host2.example.test");

        list.record(RecentConnection::from_params(&params(
            "host5.example.test",
            Authentication::Password(test_password()),
        )));
        assert_eq!(list.entries.len(), MAX_RECENT);
        assert_eq!(list.entries[0].host, "host5.example.test");
        assert_eq!(
            list.entries
                .iter()
                .filter(|entry| entry.host == "host5.example.test")
                .count(),
            1
        );
        assert_eq!(
            list.remove(0).map(|entry| entry.host),
            Some("host5.example.test".to_string())
        );
        assert_eq!(list.remove(99), None);
        list.clear();
        assert!(list.entries.is_empty());
    }

    #[test]
    fn files_never_contain_secrets() {
        let directory = temporary_directory("secrets");
        let password = test_password();
        let passphrase = test_password();
        let mut list = RecentList::default();
        list.record(RecentConnection::from_params(&params(
            "files.example.test",
            Authentication::Password(password.clone()),
        )));
        list.record(RecentConnection::from_params(&params(
            "keys.example.test",
            Authentication::KeyFile {
                path: PathBuf::from("C:\\keys\\id_ed25519"),
                passphrase: Some(passphrase.clone()),
            },
        )));
        list.save(&directory).expect("saving failed");
        let text = fs::read_to_string(directory.join(RECENT_FILE)).expect("reading failed");
        assert!(!text.contains(&password));
        assert!(!text.contains(&passphrase));
        let loaded = RecentList::load(&directory).expect("loading failed");
        assert_eq!(loaded, list);
        assert_eq!(loaded.entries[0].key_path, "C:\\keys\\id_ed25519");
        assert!(loaded.entries[0].uses_key_file());
        assert!(!loaded.entries[1].uses_key_file());

        list.clear();
        list.save(&directory).expect("saving failed");
        assert!(!directory.join(RECENT_FILE).exists());
        assert_eq!(
            RecentList::load(&directory).expect("loading failed"),
            RecentList::default()
        );
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn labels_and_broken_entries() {
        let mut entry = RecentConnection::from_params(&params(
            "files.example.test",
            Authentication::Password(test_password()),
        ));
        assert_eq!(entry.label(), "sftp://demo@files.example.test");
        entry.port = 2222;
        assert_eq!(entry.label(), "sftp://demo@files.example.test:2222");
        let parsed = RecentList::parse(
            "[connection]\nprotocol = gopher\nhost = a\n[connection]\nprotocol = ftp\nhost =\n[connection]\nprotocol = ftp\nhost = ok\nport = 0\nauth = key\nkey_path = x\n",
        );
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].port, 21);
        assert!(!parsed.entries[0].uses_key_file());
    }
}
