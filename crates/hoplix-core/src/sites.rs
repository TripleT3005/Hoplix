use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ring::rand::{SecureRandom, SystemRandom};

use crate::key_value::{self, Section};
use crate::{Authentication, ConnectionParams, Protocol};

// Constants

pub const SITES_FILE: &str = "sites.conf";
const SITE_SECTION: &str = "site";
const ID_BYTES: usize = 8;

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SiteAuth {
    #[default]
    Password,
    KeyFile,
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

    pub fn url(&self) -> String {
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
        format!(
            "{}://{}@{}{port}{path}",
            self.protocol.scheme(),
            self.username,
            self.host
        )
    }

    pub fn uses_key_file(&self) -> bool {
        self.protocol.supports_key_files() && self.auth == SiteAuth::KeyFile
    }

    pub fn to_params(&self, secret: Option<String>) -> ConnectionParams {
        let authentication = if self.uses_key_file() {
            Authentication::KeyFile {
                path: PathBuf::from(&self.key_path),
                passphrase: secret.filter(|value| !value.is_empty()),
            }
        } else {
            Authentication::Password(secret.unwrap_or_default())
        };
        ConnectionParams {
            protocol: self.protocol,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            authentication,
        }
    }

    // Serialization

    fn from_section(section: &Section) -> Option<Self> {
        let id = section.get("id").filter(|id| is_valid_id(id))?.to_string();
        let protocol = section
            .get("protocol")
            .and_then(|scheme| Protocol::ALL.into_iter().find(|p| p.scheme() == scheme))
            .unwrap_or_default();
        let text = |key: &str| section.get(key).unwrap_or_default().to_string();
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
            auth: if section.get("auth") == Some("key") {
                SiteAuth::KeyFile
            } else {
                SiteAuth::Password
            },
            key_path: text("key_path"),
            remote_path: text("remote_path"),
            local_path: text("local_path"),
            notes: text("notes"),
            save_secrets: section.get("save_secrets") == Some("true"),
        })
    }

    fn to_text(&self) -> String {
        let auth = match self.auth {
            SiteAuth::Password => "password",
            SiteAuth::KeyFile => "key",
        };
        format!(
            "\n[{SITE_SECTION}]\n{}",
            key_value::write(&[
                ("id", self.id.clone()),
                ("name", self.name.clone()),
                ("folder", self.folder.clone()),
                ("protocol", self.protocol.scheme().to_string()),
                ("host", self.host.clone()),
                ("port", self.port.to_string()),
                ("username", self.username.clone()),
                ("auth", auth.to_string()),
                ("key_path", self.key_path.clone()),
                ("remote_path", self.remote_path.clone()),
                ("local_path", self.local_path.clone()),
                ("notes", self.notes.clone()),
                ("save_secrets", self.save_secrets.to_string()),
            ])
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
    fn sites_build_params_and_urls() {
        let mut web = site("Web", "");
        web.remote_path = "www".to_string();
        assert_eq!(web.url(), "sftp://demo@files.example.test/~/www");
        let password = test_password();
        assert_eq!(
            web.to_params(Some(password.clone())).authentication,
            Authentication::Password(password)
        );
        web.auth = SiteAuth::KeyFile;
        web.key_path = "id".to_string();
        web.port = 2222;
        web.remote_path = "/srv".to_string();
        assert_eq!(web.url(), "sftp://demo@files.example.test:2222/srv");
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
