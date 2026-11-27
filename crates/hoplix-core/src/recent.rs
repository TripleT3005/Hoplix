use std::fs;
use std::io;
use std::path::Path;

use crate::key_value::{self, Section};
use crate::sites::write_atomically;
use crate::{Authentication, ConnectionParams, Protocol, S3Options, SiteAuth, SiteJump, SiteProxy};

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
    pub jump: Option<SiteJump>,
    pub proxy: Option<SiteProxy>,
    pub s3: Box<S3Options>,
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
            Authentication::Agent if params.protocol.supports_key_files() => {
                (SiteAuth::Agent, String::new())
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
            jump: None,
            proxy: None,
            s3: Box::default(),
        }
    }

    pub fn with_s3(mut self, options: &S3Options) -> Self {
        if self.protocol.is_s3() {
            self.s3 = Box::new(S3Options {
                session_token: None,
                ..options.clone()
            });
        }
        self
    }

    pub fn with_route(mut self, jump: Option<SiteJump>, proxy: Option<SiteProxy>) -> Self {
        self.jump = jump
            .filter(|_| self.protocol.supports_key_files())
            .map(|jump| SiteJump {
                save_secret: false,
                ..jump
            });
        self.proxy = proxy.map(|proxy| SiteProxy {
            save_secret: false,
            ..proxy
        });
        self
    }

    pub fn label(&self) -> String {
        let port = if self.port == self.protocol.default_port() {
            String::new()
        } else {
            format!(":{}", self.port)
        };
        if self.protocol.is_s3() {
            return format!("s3://{} @ {}{port}", self.s3.bucket, self.host);
        }
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

    pub fn uses_agent(&self) -> bool {
        self.protocol.supports_key_files() && self.auth == SiteAuth::Agent
    }

    pub(crate) fn from_section(section: &Section) -> Option<Self> {
        let protocol = section
            .get("protocol")
            .and_then(|scheme| Protocol::ALL.into_iter().find(|p| p.scheme() == scheme))?;
        let host = section
            .get("host")
            .filter(|host| !host.is_empty() || protocol.is_s3())?;
        let auth = SiteAuth::from_key(section.get("auth").unwrap_or_default());
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
            jump: SiteJump::from_section(section)
                .filter(|_| protocol.supports_key_files())
                .map(|jump| SiteJump {
                    save_secret: false,
                    ..jump
                }),
            proxy: SiteProxy::from_section(section).map(|proxy| SiteProxy {
                save_secret: false,
                ..proxy
            }),
            s3: if protocol.is_s3() {
                Box::new(S3Options::from_section(section))
            } else {
                Box::default()
            },
        })
    }

    fn to_text(&self) -> String {
        format!("\n[{ENTRY_SECTION}]\n{}", key_value::write(&self.pairs()))
    }

    pub(crate) fn pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = vec![
            ("protocol", self.protocol.scheme().to_string()),
            ("host", self.host.clone()),
            ("port", self.port.to_string()),
            ("username", self.username.clone()),
            ("auth", self.auth.key().to_string()),
            ("key_path", self.key_path.clone()),
        ];
        if self.protocol.is_s3() {
            pairs.extend(self.s3.pairs());
        }
        if let Some(jump) = &self.jump {
            pairs.extend(
                jump.pairs()
                    .into_iter()
                    .filter(|(key, _)| *key != "jump_save_secret"),
            );
        }
        if let Some(proxy) = &self.proxy {
            pairs.extend(
                proxy
                    .pairs()
                    .into_iter()
                    .filter(|(key, _)| *key != "proxy_save_secret"),
            );
        }
        pairs
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
    fn routes_are_remembered_without_secrets() {
        let directory = temporary_directory("routes");
        let mut list = RecentList::default();
        let jump = SiteJump {
            host: "bastion.test".to_string(),
            port: 2222,
            username: "hop".to_string(),
            auth: SiteAuth::KeyFile,
            key_path: "C:\\keys\\hop".to_string(),
            save_secret: true,
        };
        let proxy = SiteProxy {
            kind: crate::ProxyKind::Http,
            host: "proxy.test".to_string(),
            port: 3128,
            username: "me".to_string(),
            save_secret: true,
        };
        let entry = RecentConnection::from_params(&params("inner.test", Authentication::Agent))
            .with_route(Some(jump.clone()), Some(proxy.clone()));
        assert!(entry.uses_agent());
        assert!(!entry.jump.as_ref().expect("jump").save_secret);
        list.record(entry.clone());
        list.save(&directory).expect("saving failed");
        let text = fs::read_to_string(directory.join(RECENT_FILE)).expect("reading failed");
        assert!(!text.contains("save_secret"));
        let loaded = RecentList::load(&directory).expect("loading failed");
        assert_eq!(loaded.entries, [entry]);
        assert_eq!(loaded.entries[0].jump.as_ref().map(|j| j.port), Some(2222));
        assert_eq!(
            loaded.entries[0].proxy.as_ref().map(|p| p.host.as_str()),
            Some("proxy.test")
        );

        let mut ftp = params("ftp.test", Authentication::Password(test_password()));
        ftp.protocol = Protocol::Ftp;
        let entry = RecentConnection::from_params(&ftp).with_route(Some(jump), Some(proxy));
        assert_eq!(entry.jump, None);
        assert!(entry.proxy.is_some());
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
