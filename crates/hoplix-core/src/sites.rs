use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ring::rand::{SecureRandom, SystemRandom};

use crate::key_value::{self, Section};
use crate::options::{self, ConnectionOptions, FileNameEncoding, FtpMode};
use crate::{
    Bookmarks, ConnectionParams, Protocol, Route, S3Options, SiteJump, SiteProxy, jump_secret_name,
    proxy_secret_name, secret_name,
};

// Constants

pub const SITES_FILE: &str = "sites.conf";
const SITE_SECTION: &str = "site";
const ID_BYTES: usize = 8;
pub const MAX_SITE_TRANSFERS: usize = 8;

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SiteAuth {
    #[default]
    Password,
    KeyFile,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Site {
    pub id: String,
    pub name: String,
    pub folder: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SiteAuth,
    pub key_path: String,
    pub remote_path: String,
    pub local_path: String,
    pub notes: String,
    pub save_secrets: bool,
    pub options: ConnectionOptions,
    pub max_transfers: Option<usize>,
    pub jump: Option<SiteJump>,
    pub proxy: Option<SiteProxy>,
    pub bookmarks: Bookmarks,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SiteList {
    pub folders: Vec<String>,
    pub sites: Vec<Site>,
}

// Sites

impl Site {
    pub fn new(name: &str) -> Self {
        Self {
            id: new_id(),
            name: name.to_string(),
            port: Protocol::default().default_port(),
            ..Self::default()
        }
    }

    pub fn display_path(&self) -> String {
        if self.folder.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.folder, self.name)
        }
    }

    pub fn address(&self) -> String {
        let port = if self.port == self.protocol.default_port() {
            String::new()
        } else {
            format!(":{}", self.port)
        };
        let path = if self.remote_path.starts_with('/') {
            self.remote_path.clone()
        } else if self.remote_path.is_empty() {
            String::new()
        } else {
            format!("/~/{}", self.remote_path)
        };
        if self.protocol.is_s3() {
            let path = if self.remote_path.starts_with('/') || self.remote_path.is_empty() {
                self.remote_path.clone()
            } else {
                format!("/{}", self.remote_path)
            };
            return format!("s3://{}{path}", self.options.s3.bucket);
        }
        format!("{}://{}{port}{path}", self.protocol.scheme(), self.host)
    }

    pub fn uses_key_file(&self) -> bool {
        self.protocol.supports_key_files() && self.auth == SiteAuth::KeyFile
    }

    pub fn uses_agent(&self) -> bool {
        self.protocol.supports_key_files() && self.auth == SiteAuth::Agent
    }

    pub fn effective_auth(&self) -> SiteAuth {
        if self.protocol.supports_key_files() {
            self.auth
        } else {
            SiteAuth::Password
        }
    }

    pub fn active_jump(&self) -> Option<&SiteJump> {
        self.jump
            .as_ref()
            .filter(|_| self.protocol.supports_key_files())
    }

    pub fn to_params(&self, secret: Option<String>) -> ConnectionParams {
        ConnectionParams {
            protocol: self.protocol,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            authentication: self.effective_auth().authentication(&self.key_path, secret),
        }
    }

    pub fn route(&self, jump_secret: Option<String>, proxy_secret: Option<String>) -> Route {
        Route {
            proxy: self
                .proxy
                .as_ref()
                .map(|proxy| proxy.to_proxy(proxy_secret)),
            jump_hosts: self
                .active_jump()
                .map(|jump| jump.to_jump_host(jump_secret))
                .into_iter()
                .collect(),
        }
    }

    pub fn saved_secret_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if self.save_secrets {
            names.push(secret_name(&self.id));
        }
        if self
            .jump
            .as_ref()
            .is_some_and(|jump| jump.save_secret && jump.uses_secret())
        {
            names.push(jump_secret_name(&self.id, 0));
        }
        if self
            .proxy
            .as_ref()
            .is_some_and(|proxy| proxy.save_secret && proxy.uses_secret())
        {
            names.push(proxy_secret_name(&self.id));
        }
        names
    }

    // Serialization

    fn from_section(section: &Section) -> Option<Self> {
        let id = section.get("id").filter(|id| is_valid_id(id))?.to_string();
        let protocol = section
            .get("protocol")
            .and_then(|scheme| Protocol::ALL.into_iter().find(|p| p.scheme() == scheme))
            .unwrap_or_default();
        let text = |key: &str| section.get(key).unwrap_or_default().to_string();
        let seconds = |key: &str| options::seconds_from_text(section.get(key).unwrap_or_default());
        let defaults = ConnectionOptions::default();
        let options = ConnectionOptions {
            connect_timeout: seconds("connect_timeout").unwrap_or_default(),
            keepalive_interval: seconds("keepalive_interval").unwrap_or_default(),
            ftp_mode: section
                .get("ftp_mode")
                .and_then(FtpMode::from_key)
                .unwrap_or_default(),
            ftp_external_ip: section
                .get("ftp_external_ip")
                .and_then(|value| value.parse().ok())
                .unwrap_or(defaults.ftp_external_ip),
            ftp_encoding: section
                .get("ftp_encoding")
                .and_then(FileNameEncoding::from_key)
                .unwrap_or_default(),
            s3: if protocol.is_s3() {
                S3Options::from_section(section)
            } else {
                S3Options::default()
            },
        };
        Some(Self {
            id,
            name: text("name"),
            folder: normalize_folder(&text("folder")),
            protocol,
            host: text("host"),
            port: section
                .get("port")
                .and_then(|port| port.parse().ok())
                .filter(|port| *port > 0)
                .unwrap_or(protocol.default_port()),
            username: text("username"),
            auth: SiteAuth::from_key(section.get("auth").unwrap_or_default()),
            key_path: text("key_path"),
            remote_path: text("remote_path"),
            local_path: text("local_path"),
            notes: text("notes"),
            save_secrets: section.get("save_secrets") == Some("true"),
            options,
            max_transfers: section
                .get("max_transfers")
                .and_then(|value| value.parse().ok())
                .filter(|count| (1..=MAX_SITE_TRANSFERS).contains(count)),
            jump: SiteJump::from_section(section),
            proxy: SiteProxy::from_section(section),
            bookmarks: Bookmarks::from_section(section),
        })
    }

    fn to_text(&self) -> String {
        let mut pairs = vec![
            ("id", self.id.clone()),
            ("name", self.name.clone()),
            ("folder", self.folder.clone()),
            ("protocol", self.protocol.scheme().to_string()),
            ("host", self.host.clone()),
            ("port", self.port.to_string()),
            ("username", self.username.clone()),
            ("auth", self.auth.key().to_string()),
            ("key_path", self.key_path.clone()),
            ("remote_path", self.remote_path.clone()),
            ("local_path", self.local_path.clone()),
            ("notes", self.notes.clone()),
            ("save_secrets", self.save_secrets.to_string()),
            (
                "connect_timeout",
                options::seconds_text(self.options.connect_timeout),
            ),
            (
                "keepalive_interval",
                options::seconds_text(self.options.keepalive_interval),
            ),
            ("ftp_mode", self.options.ftp_mode.key().to_string()),
            ("ftp_external_ip", self.options.ftp_external_ip.to_string()),
            ("ftp_encoding", self.options.ftp_encoding.key().to_string()),
            (
                "max_transfers",
                self.max_transfers
                    .map(|count| count.to_string())
                    .unwrap_or_default(),
            ),
        ];
        if self.protocol.is_s3() {
            pairs.extend(self.options.s3.pairs());
        }
        if let Some(jump) = &self.jump {
            pairs.extend(jump.pairs());
        }
        if let Some(proxy) = &self.proxy {
            pairs.extend(proxy.pairs());
        }
        pairs.extend(self.bookmarks.pairs());
        format!(
            "
[{SITE_SECTION}]
{}",
            key_value::write(&pairs)
        )
    }
}

// Site lists

impl SiteList {
    pub fn parse(text: &str) -> Self {
        let mut list = Self::default();
        for section in key_value::parse_sections(text) {
            if section.name.is_empty() {
                list.folders.extend(
                    section
                        .pairs
                        .iter()
                        .filter(|(key, _)| key == "folder")
                        .map(|(_, value)| normalize_folder(value))
                        .filter(|folder| !folder.is_empty()),
                );
            } else if section.name == SITE_SECTION
                && let Some(site) = Site::from_section(&section)
                && !list.sites.iter().any(|existing| existing.id == site.id)
            {
                list.sites.push(site);
            }
        }
        list.folders.sort();
        list.folders.dedup();
        list
    }

    pub fn to_text(&self) -> String {
        let mut text = String::from("# Hoplix sites\n");
        let folders: Vec<(&str, String)> = self
            .folders
            .iter()
            .map(|folder| ("folder", folder.clone()))
            .collect();
        text.push_str(&key_value::write(&folders));
        for site in &self.sites {
            text.push_str(&site.to_text());
        }
        text
    }

    // Storage

    pub fn load(directory: &Path) -> io::Result<Self> {
        match fs::read_to_string(directory.join(SITES_FILE)) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, directory: &Path) -> io::Result<()> {
        write_atomically(&directory.join(SITES_FILE), self.to_text().as_bytes())
    }

    // Folders

    pub fn all_folders(&self) -> Vec<String> {
        let mut folders: Vec<String> = self
            .folders
            .iter()
            .chain(self.sites.iter().map(|site| &site.folder))
            .filter(|folder| !folder.is_empty())
            .flat_map(|folder| {
                let parts: Vec<&str> = folder.split('/').collect();
                (1..=parts.len())
                    .map(|count| parts[..count].join("/"))
                    .collect::<Vec<_>>()
            })
            .collect();
        folders.sort_by(|a, b| {
            a.split('/')
                .zip(b.split('/'))
                .map(|(x, y)| crate::compare_names(x, y))
                .find(|ordering| ordering.is_ne())
                .unwrap_or_else(|| a.split('/').count().cmp(&b.split('/').count()))
        });
        folders.dedup();
        folders
    }

    pub fn add_folder(&mut self, folder: &str) -> Option<String> {
        let folder = normalize_folder(folder);
        if folder.is_empty() {
            return None;
        }
        if !self.folders.contains(&folder) {
            self.folders.push(folder.clone());
            self.folders.sort();
        }
        Some(folder)
    }

    pub fn remove_folder(&mut self, folder: &str) -> Vec<Site> {
        let inside =
            |candidate: &str| candidate == folder || candidate.starts_with(&format!("{folder}/"));
        self.folders.retain(|candidate| !inside(candidate));
        let (removed, kept) = std::mem::take(&mut self.sites)
            .into_iter()
            .partition(|site| inside(&site.folder));
        self.sites = kept;
        removed
    }

    // Sites

    pub fn get(&self, id: &str) -> Option<&Site> {
        self.sites.iter().find(|site| site.id == id)
    }

    pub fn upsert(&mut self, mut site: Site) {
        site.folder = normalize_folder(&site.folder);
        match self
            .sites
            .iter_mut()
            .find(|existing| existing.id == site.id)
        {
            Some(existing) => *existing = site,
            None => self.sites.push(site),
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<Site> {
        let index = self.sites.iter().position(|site| site.id == id)?;
        Some(self.sites.remove(index))
    }

    pub fn duplicate(&mut self, id: &str, name: &str) -> Option<Site> {
        let mut copy = self.get(id)?.clone();
        copy.id = new_id();
        copy.name = name.to_string();
        copy.save_secrets = false;
        if let Some(jump) = &mut copy.jump {
            jump.save_secret = false;
        }
        if let Some(proxy) = &mut copy.proxy {
            proxy.save_secret = false;
        }
        self.sites.push(copy.clone());
        Some(copy)
    }

    pub fn find(&self, reference: &str) -> Result<&Site, Vec<&Site>> {
        let reference = reference.trim();
        if let Some(site) = self.get(reference) {
            return Ok(site);
        }
        let matches: Vec<&Site> = self
            .sites
            .iter()
            .filter(|site| {
                site.name.eq_ignore_ascii_case(reference)
                    || site.display_path().eq_ignore_ascii_case(reference)
            })
            .collect();
        match matches.as_slice() {
            [site] => Ok(site),
            _ => Err(matches),
        }
    }

    pub fn sorted(&self) -> Vec<&Site> {
        let mut sites: Vec<&Site> = self.sites.iter().collect();
        sites.sort_by(|a, b| {
            crate::compare_names(&a.folder, &b.folder)
                .then_with(|| crate::compare_names(&a.name, &b.name))
        });
        sites
    }
}

// Helpers

pub fn normalize_folder(folder: &str) -> String {
    folder
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

pub fn new_id() -> String {
    let mut bytes = [0u8; ID_BYTES];
    if SystemRandom::new().fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        bytes.copy_from_slice(&nanos.to_le_bytes()[..ID_BYTES]);
    }
    key_value::to_hex(&bytes)
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub(crate) fn write_atomically(path: &Path, content: &[u8]) -> io::Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, content)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_password;
    use crate::{Authentication, JumpHost, Proxy, ProxyKind};

    fn site(name: &str, folder: &str) -> Site {
        Site {
            folder: folder.to_string(),
            host: "files.example.test".to_string(),
            username: "demo".to_string(),
            ..Site::new(name)
        }
    }

    // Serialization

    #[test]
    fn site_lists_round_trip_without_secrets() {
        let mut list = SiteList::default();
        list.add_folder(" Work / Empty ");
        let mut web = site("Web server", "Work/Clients");
        web.protocol = Protocol::FtpsExplicit;
        web.port = 2121;
        web.remote_path = "/var/www".to_string();
        web.local_path = "C:\\Sites".to_string();
        web.notes = "line one\nline two = with equals".to_string();
        web.save_secrets = true;
        web.bookmarks.add("Logs", "/var/log/app");
        web.bookmarks.add("Uploads", "/var/www/uploads");
        let mut backup = site("Backup", "");
        backup.auth = SiteAuth::KeyFile;
        backup.key_path = "C:\\keys\\id_ed25519".to_string();
        list.upsert(web.clone());
        list.upsert(backup.clone());

        let text = list.to_text();
        assert!(!text.contains("password ="));
        let parsed = SiteList::parse(&text);
        assert_eq!(parsed.folders, ["Work/Empty"]);
        assert_eq!(parsed.get(&web.id), Some(&web));
        assert_eq!(parsed.get(&backup.id), Some(&backup));
    }

    #[test]
    fn broken_entries_are_skipped_and_defaults_apply() {
        let parsed = SiteList::parse(
            "[site]\nname = no id\n[site]\nid = ok1\nprotocol = gopher\nport = 0\n[site]\nid = ok1\nname = duplicate\n[other]\nid = x\n",
        );
        assert_eq!(parsed.sites.len(), 1);
        let site = &parsed.sites[0];
        assert_eq!(site.protocol, Protocol::Sftp);
        assert_eq!(site.port, 22);
        assert!(!site.save_secrets);
    }

    #[test]
    fn connection_options_round_trip() {
        let mut list = SiteList::default();
        let mut ftp = site("Legacy", "");
        ftp.protocol = Protocol::Ftp;
        ftp.options = ConnectionOptions {
            connect_timeout: Some(std::time::Duration::from_secs(45)),
            keepalive_interval: Some(std::time::Duration::from_secs(25)),
            ftp_mode: FtpMode::Active,
            ftp_external_ip: false,
            ftp_encoding: FileNameEncoding::Windows1252,
            s3: S3Options::default(),
        };
        ftp.max_transfers = Some(1);
        list.upsert(ftp.clone());
        let parsed = SiteList::parse(&list.to_text());
        assert_eq!(parsed.get(&ftp.id), Some(&ftp));
        assert!(!list.to_text().contains("s3_bucket"));
    }

    #[test]
    fn s3_sites_keep_bucket_and_endpoint_settings() {
        let mut list = SiteList::default();
        let mut s3 = site("Storage", "");
        s3.protocol = Protocol::S3;
        s3.host = "s3.example.test".to_string();
        s3.port = 9000;
        s3.username = "demo-access".to_string();
        s3.remote_path = "/backups".to_string();
        s3.options.s3 = S3Options {
            bucket: "demo-bucket".to_string(),
            region: "eu-central-1".to_string(),
            path_style: true,
            plain_http: true,
            session_token: None,
        };
        list.upsert(s3.clone());
        let parsed = SiteList::parse(&list.to_text());
        assert_eq!(parsed.get(&s3.id), Some(&s3));
        assert_eq!(s3.address(), "s3://demo-bucket/backups");
    }

    #[test]
    fn old_files_get_the_default_options() {
        let parsed = SiteList::parse(
            "# Hoplix sites
[site]
id = old1
name = Old
protocol = ftp
host = h
",
        );
        let site = &parsed.sites[0];
        assert_eq!(site.options, ConnectionOptions::default());
        assert!(site.options.ftp_external_ip);
        assert_eq!(site.max_transfers, None);
        let broken = SiteList::parse(
            "[site]
id = b1
connect_timeout = soon
keepalive_interval = 99999
ftp_mode = sideways
ftp_external_ip = maybe
ftp_encoding = ebcdic
max_transfers = 0
",
        );
        assert_eq!(broken.sites[0].options, ConnectionOptions::default());
        assert_eq!(broken.sites[0].max_transfers, None);
    }

    #[test]
    fn jump_hosts_and_proxies_round_trip_without_secrets() {
        let mut list = SiteList::default();
        let mut inner = site("Inner", "");
        inner.auth = SiteAuth::Agent;
        inner.save_secrets = false;
        inner.jump = Some(SiteJump {
            host: "bastion.test".to_string(),
            port: 2222,
            username: "hop".to_string(),
            auth: SiteAuth::KeyFile,
            key_path: "C:\\keys\\hop".to_string(),
            save_secret: true,
        });
        inner.proxy = Some(SiteProxy {
            kind: ProxyKind::Http,
            host: "proxy.test".to_string(),
            port: 3128,
            username: "me".to_string(),
            save_secret: true,
        });
        list.upsert(inner.clone());
        let text = list.to_text();
        assert!(text.contains("auth = agent"));
        assert!(text.contains("jump_host = bastion.test"));
        assert!(text.contains("proxy = http"));
        let parsed = SiteList::parse(&text);
        assert_eq!(parsed.get(&inner.id), Some(&inner));
        assert_eq!(
            inner.saved_secret_names(),
            [jump_secret_name(&inner.id, 0), proxy_secret_name(&inner.id)]
        );

        let route = inner.route(Some("jump".to_string()), Some("proxy".to_string()));
        assert_eq!(
            route.jump_hosts,
            [JumpHost {
                host: "bastion.test".to_string(),
                port: 2222,
                username: "hop".to_string(),
                authentication: Authentication::KeyFile {
                    path: PathBuf::from("C:\\keys\\hop"),
                    passphrase: Some("jump".to_string()),
                },
            }]
        );
        assert_eq!(
            route.proxy.as_ref().map(Proxy::label),
            Some("http://proxy.test:3128".to_string())
        );
        assert_eq!(inner.to_params(None).authentication, Authentication::Agent);

        inner.protocol = Protocol::Ftp;
        let route = inner.route(None, None);
        assert!(route.jump_hosts.is_empty());
        assert!(route.proxy.is_some());
        assert!(matches!(
            inner.to_params(None).authentication,
            Authentication::Password(_)
        ));

        list.upsert(inner.clone());
        let copy = list.duplicate(&inner.id, "Copy").expect("copied");
        assert!(copy.saved_secret_names().is_empty());
    }

    #[test]
    fn old_files_have_no_route() {
        let parsed = SiteList::parse("[site]\nid = old2\nhost = h\nauth = key\n");
        let site = &parsed.sites[0];
        assert_eq!(site.jump, None);
        assert_eq!(site.proxy, None);
        assert_eq!(site.auth, SiteAuth::KeyFile);
        assert!(site.route(None, None).is_direct());
    }

    // Folders

    #[test]
    fn folders_include_parents_and_sort_naturally() {
        let mut list = SiteList::default();
        list.upsert(site("a", "Work/Clients 10"));
        list.upsert(site("b", "Work/Clients 9"));
        list.add_folder("Home");
        assert_eq!(
            list.all_folders(),
            ["Home", "Work", "Work/Clients 9", "Work/Clients 10"]
        );
        assert_eq!(list.add_folder(" / "), None);
    }

    #[test]
    fn removing_a_folder_removes_its_sites() {
        let mut list = SiteList::default();
        list.add_folder("Work/Old");
        list.upsert(site("inside", "Work/Old"));
        list.upsert(site("deeper", "Work/Old/Archive"));
        list.upsert(site("sibling", "Work/Older"));
        let removed = list.remove_folder("Work/Old");
        assert_eq!(removed.len(), 2);
        assert_eq!(list.sites.len(), 1);
        assert!(list.folders.is_empty());
    }

    // Lookup

    #[test]
    fn sites_are_found_by_id_name_or_path() {
        let mut list = SiteList::default();
        let web = site("Web", "Work");
        list.upsert(web.clone());
        list.upsert(site("Web", "Home"));
        list.upsert(site("Mail", ""));
        assert_eq!(list.find(&web.id).map(|s| s.id.clone()), Ok(web.id.clone()));
        assert_eq!(
            list.find("work/web").map(|s| s.id.clone()),
            Ok(web.id.clone())
        );
        assert!(list.find("mail").is_ok());
        assert_eq!(list.find("Web").map(|_| ()).unwrap_err().len(), 2);
        assert!(list.find("nothing").unwrap_err().is_empty());
    }

    #[test]
    fn duplicates_get_new_ids_and_no_saved_secrets() {
        let mut list = SiteList::default();
        let mut original = site("Web", "");
        original.save_secrets = true;
        list.upsert(original.clone());
        let copy = list
            .duplicate(&original.id, "Web (copy)")
            .expect("site exists");
        assert_ne!(copy.id, original.id);
        assert!(!copy.save_secrets);
        assert_eq!(copy.host, original.host);
        assert_eq!(list.sites.len(), 2);
        assert_eq!(
            list.remove(&copy.id).map(|s| s.name),
            Some("Web (copy)".to_string())
        );
    }

    // Connections

    #[test]
    fn sites_build_params_and_addresses() {
        let mut web = site("Web", "");
        web.remote_path = "www".to_string();
        assert_eq!(web.address(), "sftp://files.example.test/~/www");
        let password = test_password();
        assert_eq!(
            web.to_params(Some(password.clone())).authentication,
            Authentication::Password(password)
        );
        web.auth = SiteAuth::KeyFile;
        web.key_path = "id".to_string();
        web.port = 2222;
        web.remote_path = "/srv".to_string();
        assert_eq!(web.address(), "sftp://files.example.test:2222/srv");
        assert_eq!(
            web.to_params(Some(String::new())).authentication,
            Authentication::KeyFile {
                path: PathBuf::from("id"),
                passphrase: None
            }
        );
        web.protocol = Protocol::Ftp;
        assert!(!web.uses_key_file());
    }

    #[test]
    fn ids_are_random_hex() {
        let (a, b) = (new_id(), new_id());
        assert_eq!(a.len(), 16);
        assert_ne!(a, b);
        assert!(is_valid_id(&a));
        assert!(!is_valid_id("a b"));
    }
}
