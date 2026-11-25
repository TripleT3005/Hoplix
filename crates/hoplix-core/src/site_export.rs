use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU32;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::key_value::{self, from_hex, to_hex};
use crate::secrets::vault::{self, CIPHER, KDF, SALT_LENGTH};
use crate::{DEFAULT_ITERATIONS, SecretSlot, Site, SiteList, new_id, normalize_folder};

// Constants

pub const EXPORT_EXTENSION: &str = "hoplix-sites";
pub const EXPORT_FILE_NAME: &str = "hoplix-sites.hoplix-sites";
const HEADER: &str = "# Hoplix site list";
const FORMAT: &str = "hoplix-sites";
const VERSION: u32 = 1;
const SECRET_SECTION: &str = "secret";
const ROUTE_SECRET_SECTION: &str = "route-secret";
const CHECK_NAME: &str = "export-check";
const CHECK_TEXT: &[u8] = b"hoplix-sites";
const DATA_NAME: &str = "export-data";

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportScope {
    All,
    Folder(String),
    Site(String),
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct SiteExport {
    pub list: SiteList,
    pub secrets: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedExport {
    iterations: NonZeroU32,
    salt: Vec<u8>,
    check: Vec<u8>,
    data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportFile {
    Plain(SiteExport),
    Encrypted(SealedExport),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportError {
    NotASiteList,
    NewerVersion,
    WrongPassword,
    Damaged,
    EmptyPassword,
    Encryption,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ImportSummary {
    pub folder: String,
    pub sites: usize,
    pub secrets: usize,
    pub stored: Vec<(String, String)>,
}

// Selection

impl SiteExport {
    pub fn select(list: &SiteList, scope: &ExportScope) -> SiteList {
        match scope {
            ExportScope::All => list.clone(),
            ExportScope::Site(id) => SiteList {
                folders: Vec::new(),
                sites: list
                    .get(id)
                    .map(|site| Site {
                        folder: String::new(),
                        ..site.clone()
                    })
                    .into_iter()
                    .collect(),
            },
            ExportScope::Folder(folder) => {
                let folder = normalize_folder(folder);
                let parent = folder
                    .rsplit_once('/')
                    .map(|(parent, _)| format!("{parent}/"))
                    .unwrap_or_default();
                let inside = |candidate: &str| {
                    candidate == folder || candidate.starts_with(&format!("{folder}/"))
                };
                let relative = |candidate: &str| {
                    candidate
                        .strip_prefix(&parent)
                        .unwrap_or(candidate)
                        .to_string()
                };
                let mut folders: Vec<String> = list
                    .folders
                    .iter()
                    .filter(|candidate| inside(candidate))
                    .map(|candidate| relative(candidate))
                    .collect();
                folders.push(relative(&folder));
                folders.sort();
                folders.dedup();
                SiteList {
                    folders,
                    sites: list
                        .sites
                        .iter()
                        .filter(|site| inside(&site.folder))
                        .map(|site| Site {
                            folder: relative(&site.folder),
                            ..site.clone()
                        })
                        .collect(),
                }
            }
        }
    }

    pub fn site_count(&self) -> usize {
        self.list.sites.len()
    }

    // Writing

    pub fn to_plain_text(&self) -> String {
        let mut text = header(false);
        text.push_str(&body_of(&self.list));
        text
    }

    pub fn to_encrypted_text(
        &self,
        password: &str,
        iterations: u32,
    ) -> Result<String, ExportError> {
        if password.is_empty() {
            return Err(ExportError::EmptyPassword);
        }
        let iterations = NonZeroU32::new(iterations).ok_or(ExportError::Encryption)?;
        let salt = vault::random_bytes::<SALT_LENGTH>()
            .map_err(|_| ExportError::Encryption)?
            .to_vec();
        let key = vault::derive_key(password, &salt, iterations);
        let check =
            vault::seal(&key, CHECK_NAME, CHECK_TEXT).map_err(|_| ExportError::Encryption)?;
        let data = vault::seal(&key, DATA_NAME, self.inner_text().as_bytes())
            .map_err(|_| ExportError::Encryption)?;
        let mut text = header(true);
        text.push_str(&key_value::write(&[
            ("kdf", KDF.to_string()),
            ("cipher", CIPHER.to_string()),
            ("iterations", iterations.to_string()),
            ("salt", to_hex(&salt)),
            ("check", to_hex(&check)),
            ("data", to_hex(&data)),
        ]));
        Ok(text)
    }

    fn inner_text(&self) -> String {
        let mut text = body_of(&self.list);
        for (name, secret) in &self.secrets {
            let Some((site, slot)) = SecretSlot::parse(name) else {
                continue;
            };
            if self.list.get(site).is_none() || secret.is_empty() {
                continue;
            }
            let section = match slot {
                SecretSlot::Site => format!(
                    "\n[{SECRET_SECTION}]\n{}",
                    key_value::write(&[("site", site.to_string()), ("value", secret.clone())])
                ),
                _ => format!(
                    "\n[{ROUTE_SECRET_SECTION}]\n{}",
                    key_value::write(&[
                        ("site", site.to_string()),
                        ("slot", slot.key()),
                        ("value", secret.clone()),
                    ])
                ),
            };
            text.push_str(&section);
        }
        text
    }

    fn from_inner_text(text: &str) -> Self {
        let list = SiteList::parse(text);
        let secrets = key_value::parse_sections(text)
            .into_iter()
            .filter_map(|section| {
                let slot = match section.name.as_str() {
                    SECRET_SECTION => SecretSlot::Site,
                    ROUTE_SECRET_SECTION => SecretSlot::from_key(section.get("slot")?)
                        .filter(|slot| *slot != SecretSlot::Site)?,
                    _ => return None,
                };
                let site = section.get("site")?;
                let value = section.get("value").filter(|value| !value.is_empty())?;
                list.get(site).map(|_| (slot.name(site), value.to_string()))
            })
            .collect();
        Self { list, secrets }
    }

    // Importing

    pub fn import_into(self, list: &mut SiteList, folder: &str) -> ImportSummary {
        let base = normalize_folder(folder);
        let place = |relative: &str| match (base.is_empty(), relative.is_empty()) {
            (true, _) => relative.to_string(),
            (false, true) => base.clone(),
            (false, false) => format!("{base}/{relative}"),
        };
        if !base.is_empty() {
            list.add_folder(&base);
        }
        for exported in &self.list.folders {
            list.add_folder(&place(exported));
        }
        let mut stored = Vec::new();
        let mut count = 0;
        for mut site in self.list.sites {
            let secrets: Vec<(SecretSlot, String)> = self
                .secrets
                .iter()
                .filter_map(|(name, secret)| match SecretSlot::parse(name) {
                    Some((id, slot)) if id == site.id => Some((slot, secret.clone())),
                    _ => None,
                })
                .collect();
            let has = |wanted: SecretSlot| secrets.iter().any(|(slot, _)| *slot == wanted);
            site.id = new_id();
            site.folder = normalize_folder(&place(&site.folder));
            site.name = free_name(list, &site.folder, &site.name);
            site.save_secrets = has(SecretSlot::Site);
            if let Some(jump) = &mut site.jump {
                jump.save_secret = has(SecretSlot::Jump(0));
            }
            if let Some(proxy) = &mut site.proxy {
                proxy.save_secret = has(SecretSlot::Proxy);
            }
            for (slot, secret) in secrets {
                stored.push((slot.name(&site.id), secret));
            }
            list.upsert(site);
            count += 1;
        }
        ImportSummary {
            folder: base,
            sites: count,
            secrets: stored.len(),
            stored,
        }
    }
}

impl fmt::Debug for SiteExport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SiteExport")
            .field("list", &self.list)
            .field("secrets", &self.secrets.len())
            .finish()
    }
}

impl fmt::Debug for ImportSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportSummary")
            .field("folder", &self.folder)
            .field("sites", &self.sites)
            .field("secrets", &self.secrets)
            .finish_non_exhaustive()
    }
}

// Reading

impl ExportFile {
    pub fn parse(text: &str) -> Result<Self, ExportError> {
        let sections = key_value::parse_sections(text);
        let top = sections.first().ok_or(ExportError::NotASiteList)?;
        if top.get("format") != Some(FORMAT) {
            return Err(ExportError::NotASiteList);
        }
        let version: u32 = top
            .get("version")
            .and_then(|version| version.parse().ok())
            .ok_or(ExportError::NotASiteList)?;
        if version > VERSION {
            return Err(ExportError::NewerVersion);
        }
        if top.get("encrypted") != Some("true") {
            let mut export = SiteExport::from_inner_text(text);
            export.secrets.clear();
            return Ok(Self::Plain(export));
        }
        if top.get("kdf") != Some(KDF) || top.get("cipher") != Some(CIPHER) {
            return Err(ExportError::Damaged);
        }
        let hex = |key: &str| top.get(key).and_then(from_hex).ok_or(ExportError::Damaged);
        let iterations = top
            .get("iterations")
            .and_then(|value| value.parse().ok())
            .and_then(NonZeroU32::new)
            .ok_or(ExportError::Damaged)?;
        Ok(Self::Encrypted(SealedExport {
            iterations,
            salt: hex("salt")?,
            check: hex("check")?,
            data: hex("data")?,
        }))
    }

    pub fn is_encrypted(&self) -> bool {
        matches!(self, Self::Encrypted(_))
    }
}

impl SealedExport {
    pub fn open(&self, password: &str) -> Result<SiteExport, ExportError> {
        let key = vault::derive_key(password, &self.salt, self.iterations);
        match vault::open(&key, CHECK_NAME, &self.check) {
            Some(text) if text == CHECK_TEXT => {}
            _ => return Err(ExportError::WrongPassword),
        }
        let plain = vault::open(&key, DATA_NAME, &self.data).ok_or(ExportError::Damaged)?;
        let text = String::from_utf8(plain).map_err(|_| ExportError::Damaged)?;
        Ok(SiteExport::from_inner_text(&text))
    }
}

// Background work

pub async fn encrypt_export(export: SiteExport, password: String) -> Result<String, ExportError> {
    tokio::task::spawn_blocking(move || export.to_encrypted_text(&password, DEFAULT_ITERATIONS))
        .await
        .map_err(|_| ExportError::Encryption)?
}

pub async fn decrypt_export(
    sealed: SealedExport,
    password: String,
) -> Result<SiteExport, ExportError> {
    tokio::task::spawn_blocking(move || sealed.open(&password))
        .await
        .map_err(|_| ExportError::Damaged)?
}

// Helpers

fn header(encrypted: bool) -> String {
    format!(
        "{HEADER}\n{}",
        key_value::write(&[
            ("format", FORMAT.to_string()),
            ("version", VERSION.to_string()),
            ("encrypted", encrypted.to_string()),
        ])
    )
}

fn body_of(list: &SiteList) -> String {
    let text = list.to_text();
    text.strip_prefix("# Hoplix sites\n")
        .unwrap_or(&text)
        .to_string()
}

fn free_name(list: &SiteList, folder: &str, name: &str) -> String {
    let taken = |candidate: &str| {
        list.sites
            .iter()
            .any(|site| site.folder == folder && site.name.eq_ignore_ascii_case(candidate))
    };
    if !taken(name) {
        return name.to_string();
    }
    (2..)
        .map(|number| format!("{name} ({number})"))
        .find(|candidate| !taken(candidate))
        .unwrap_or_else(|| name.to_string())
}

pub fn utc_date(time: SystemTime) -> String {
    let days = time
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() / 86_400)
        .unwrap_or_default();
    let days = i64::try_from(days).unwrap_or_default() + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_password;
    use crate::{
        FtpMode, Protocol, ProxyKind, SiteAuth, SiteJump, SiteProxy, jump_secret_name,
        proxy_secret_name, secret_name,
    };
    use std::time::Duration;

    const FAST: u32 = 1_000;

    fn site(name: &str, folder: &str) -> Site {
        Site {
            folder: folder.to_string(),
            host: "files.example.test".to_string(),
            username: "demo".to_string(),
            ..Site::new(name)
        }
    }

    fn sample() -> (SiteList, Site, Site) {
        let mut list = SiteList::default();
        let mut web = site("Web", "Work/Clients");
        web.protocol = Protocol::Ftp;
        web.port = 21;
        web.options.ftp_mode = FtpMode::Active;
        web.max_transfers = Some(2);
        web.save_secrets = true;
        web.notes = "first\nsecond".to_string();
        let backup = site("Backup", "Home");
        list.upsert(web.clone());
        list.upsert(backup.clone());
        list.add_folder("Work/Clients/Empty");
        (list, web, backup)
    }

    #[test]
    fn plain_exports_round_trip_without_secrets() {
        let (list, web, _) = sample();
        let password = test_password();
        let export = SiteExport {
            list: list.clone(),
            secrets: BTreeMap::from([(secret_name(&web.id), password.clone())]),
        };
        let text = export.to_plain_text();
        assert!(text.starts_with(HEADER));
        assert!(text.contains("encrypted = false"));
        assert!(!text.contains(&password));
        let ExportFile::Plain(parsed) = ExportFile::parse(&text).expect("parsed") else {
            panic!("not a plain export");
        };
        assert_eq!(parsed.list, list);
        assert!(parsed.secrets.is_empty());
    }

    #[test]
    fn encrypted_exports_round_trip_with_secrets() {
        let (list, web, _) = sample();
        let secret = test_password();
        let password = test_password();
        let export = SiteExport {
            list: list.clone(),
            secrets: BTreeMap::from([(secret_name(&web.id), secret.clone())]),
        };
        let text = export
            .to_encrypted_text(&password, FAST)
            .expect("encrypted");
        assert!(!text.contains(&secret));
        assert!(!text.contains("files.example.test"));
        let file = ExportFile::parse(&text).expect("parsed");
        assert!(file.is_encrypted());
        let ExportFile::Encrypted(sealed) = file else {
            panic!("not encrypted");
        };
        let opened = sealed.open(&password).expect("opened");
        assert_eq!(opened.list, list);
        assert_eq!(opened.secrets.get(&secret_name(&web.id)), Some(&secret));
        assert_eq!(
            sealed.open(&test_password()).map(|_| ()),
            Err(ExportError::WrongPassword)
        );
        assert_eq!(
            export.to_encrypted_text("", FAST).map(|_| ()),
            Err(ExportError::EmptyPassword)
        );
        let again = export
            .to_encrypted_text(&password, FAST)
            .expect("encrypted");
        assert_ne!(again, text, "every export needs a fresh salt and nonce");
    }

    #[test]
    fn tampered_and_foreign_files_are_rejected() {
        let (list, _, _) = sample();
        let password = test_password();
        let export = SiteExport {
            list,
            secrets: BTreeMap::new(),
        };
        let text = export
            .to_encrypted_text(&password, FAST)
            .expect("encrypted");
        let data_line = text
            .lines()
            .find(|line| line.starts_with("data = "))
            .expect("data line");
        let mut bytes = data_line.as_bytes().to_vec();
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'0' { b'1' } else { b'0' };
        let tampered = text.replace(data_line, &String::from_utf8(bytes).expect("text"));
        let ExportFile::Encrypted(sealed) = ExportFile::parse(&tampered).expect("parsed") else {
            panic!("not encrypted");
        };
        assert_eq!(
            sealed.open(&password).map(|_| ()),
            Err(ExportError::Damaged)
        );

        let broken = text.replace("salt = ", "salt = zz");
        assert_eq!(
            ExportFile::parse(&broken).map(|_| ()),
            Err(ExportError::Damaged)
        );
        assert_eq!(
            ExportFile::parse("# Hoplix sites\n[site]\nid = a\n").map(|_| ()),
            Err(ExportError::NotASiteList)
        );
        assert_eq!(
            ExportFile::parse("format = hoplix-sites\nversion = 2\n").map(|_| ()),
            Err(ExportError::NewerVersion)
        );
        let plain_with_secret =
            "format = hoplix-sites\nversion = 1\n[site]\nid = a1\n[secret]\nsite = a1\nvalue = x\n";
        let ExportFile::Plain(plain) = ExportFile::parse(plain_with_secret).expect("parsed") else {
            panic!("not plain");
        };
        assert!(plain.secrets.is_empty());
    }

    #[test]
    fn selections_export_folders_relative_to_their_parent() {
        let (list, web, backup) = sample();
        let folder = SiteExport::select(&list, &ExportScope::Folder("Work/Clients".to_string()));
        assert_eq!(folder.sites.len(), 1);
        assert_eq!(folder.sites[0].folder, "Clients");
        assert_eq!(folder.folders, ["Clients", "Clients/Empty"]);
        let single = SiteExport::select(&list, &ExportScope::Site(backup.id.clone()));
        assert_eq!(single.sites.len(), 1);
        assert_eq!(single.sites[0].folder, "");
        assert_eq!(single.sites[0].id, backup.id);
        assert_eq!(SiteExport::select(&list, &ExportScope::All), list);
        let top = SiteExport::select(&list, &ExportScope::Folder("Work".to_string()));
        assert_eq!(
            top.get(&web.id).map(|s| s.folder.as_str()),
            Some("Work/Clients")
        );
    }

    #[test]
    fn imports_get_new_ids_free_names_and_their_own_folder() {
        let (exported, web, _) = sample();
        let secret = test_password();
        let export = SiteExport {
            list: SiteExport::select(&exported, &ExportScope::Folder("Work".to_string())),
            secrets: BTreeMap::from([(secret_name(&web.id), secret.clone())]),
        };
        let mut target = SiteList::default();
        let mut existing = site("Web", "Imported 2026-11-25/Work/Clients");
        existing.save_secrets = false;
        target.upsert(existing.clone());

        let summary = export
            .clone()
            .import_into(&mut target, "Imported 2026-11-25");
        assert_eq!(summary.sites, 1);
        assert_eq!(summary.secrets, 1);
        assert_eq!(summary.folder, "Imported 2026-11-25");
        let imported = target
            .sites
            .iter()
            .find(|candidate| candidate.id != existing.id)
            .expect("imported site");
        assert_ne!(imported.id, web.id);
        assert_eq!(imported.name, "Web (2)");
        assert_eq!(imported.folder, "Imported 2026-11-25/Work/Clients");
        assert!(imported.save_secrets);
        assert_eq!(imported.options.ftp_mode, FtpMode::Active);
        assert_eq!(imported.max_transfers, Some(2));
        assert_eq!(summary.stored, [(secret_name(&imported.id), secret)]);
        assert!(
            target
                .all_folders()
                .contains(&"Imported 2026-11-25/Work/Clients/Empty".to_string())
        );

        let again = export.import_into(&mut target, "Imported 2026-11-25");
        assert_eq!(again.sites, 1);
        assert!(
            target
                .sites
                .iter()
                .any(|candidate| candidate.name == "Web (3)")
        );

        let plain = SiteExport {
            list: SiteExport::select(&exported, &ExportScope::All),
            secrets: BTreeMap::new(),
        };
        let mut root = SiteList::default();
        let summary = plain.import_into(&mut root, "");
        assert_eq!(summary.secrets, 0);
        assert!(root.sites.iter().all(|candidate| !candidate.save_secrets));
        assert!(
            root.sites
                .iter()
                .any(|candidate| candidate.folder == "Home")
        );
    }

    #[test]
    fn route_secrets_travel_with_their_site() {
        let mut list = SiteList::default();
        let mut inner = site("Inner", "");
        inner.auth = SiteAuth::Agent;
        inner.jump = Some(SiteJump {
            host: "bastion.test".to_string(),
            port: 22,
            username: "hop".to_string(),
            auth: SiteAuth::Password,
            key_path: String::new(),
            save_secret: true,
        });
        inner.proxy = Some(SiteProxy {
            kind: ProxyKind::Socks5,
            host: "proxy.test".to_string(),
            port: 1080,
            username: "me".to_string(),
            save_secret: true,
        });
        list.upsert(inner.clone());
        let jump_password = test_password();
        let proxy_password = test_password();
        let export = SiteExport {
            list: list.clone(),
            secrets: BTreeMap::from([
                (jump_secret_name(&inner.id, 0), jump_password.clone()),
                (proxy_secret_name(&inner.id), proxy_password.clone()),
            ]),
        };
        let password = test_password();
        let text = export
            .to_encrypted_text(&password, FAST)
            .expect("encrypted");
        let ExportFile::Encrypted(sealed) = ExportFile::parse(&text).expect("parsed") else {
            panic!("not encrypted");
        };
        let opened = sealed.open(&password).expect("opened");
        assert_eq!(opened.list, list);
        assert_eq!(opened.secrets, export.secrets);
        assert!(export.inner_text().contains("[route-secret]"));
        assert!(!export.inner_text().contains("[secret]"));

        let mut target = SiteList::default();
        let summary = opened.import_into(&mut target, "");
        let imported = &target.sites[0];
        assert!(!imported.save_secrets);
        assert!(imported.jump.as_ref().expect("jump").save_secret);
        assert!(imported.proxy.as_ref().expect("proxy").save_secret);
        assert_eq!(summary.secrets, 2);
        assert_eq!(
            summary.stored,
            [
                (jump_secret_name(&imported.id, 0), jump_password),
                (proxy_secret_name(&imported.id), proxy_password),
            ]
        );
        assert_eq!(
            imported.saved_secret_names(),
            [
                jump_secret_name(&imported.id, 0),
                proxy_secret_name(&imported.id)
            ]
        );
    }

    #[test]
    fn dates_are_formatted_in_utc() {
        assert_eq!(utc_date(UNIX_EPOCH), "1970-01-01");
        assert_eq!(
            utc_date(UNIX_EPOCH + Duration::from_secs(1_795_000_000)),
            "2026-11-18"
        );
    }

    #[test]
    fn debug_output_hides_secrets() {
        let secret = test_password();
        let export = SiteExport {
            list: SiteList::default(),
            secrets: BTreeMap::from([("a".to_string(), secret.clone())]),
        };
        assert!(!format!("{export:?}").contains(&secret));
    }
}
