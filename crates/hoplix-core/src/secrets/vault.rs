use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::pbkdf2;
use ring::rand::{self, SystemRandom};

use crate::key_value::{self, from_hex, to_hex};
use crate::sites::write_atomically;
use crate::{Error, ErrorKind, Result};

// Constants

pub const VAULT_FILE: &str = "secrets.vault";
pub const DEFAULT_ITERATIONS: u32 = 600_000;
const FORMAT: &str = "hoplix-vault-1";
const KDF: &str = "pbkdf2-hmac-sha256";
const CIPHER: &str = "aes-256-gcm";
const SALT_LENGTH: usize = 32;
const KEY_LENGTH: usize = 32;
const CHECK_NAME: &str = "check";
const CHECK_TEXT: &[u8] = b"hoplix";
const ENTRY_PREFIX: &str = "entry.";

// Types

#[derive(Clone)]
pub struct Vault {
    path: PathBuf,
    iterations: NonZeroU32,
    salt: Vec<u8>,
    key: [u8; KEY_LENGTH],
    check: Vec<u8>,
    entries: BTreeMap<String, Vec<u8>>,
}

struct VaultFile {
    iterations: NonZeroU32,
    salt: Vec<u8>,
    check: Vec<u8>,
    entries: BTreeMap<String, Vec<u8>>,
}

// Lifecycle

impl Vault {
    pub fn path_in(directory: &Path) -> PathBuf {
        directory.join(VAULT_FILE)
    }

    pub fn exists(directory: &Path) -> bool {
        Self::path_in(directory).is_file()
    }

    pub fn create(directory: &Path, password: &str) -> Result<Self> {
        Self::create_with_iterations(directory, password, DEFAULT_ITERATIONS)
    }

    pub fn create_with_iterations(
        directory: &Path,
        password: &str,
        iterations: u32,
    ) -> Result<Self> {
        if password.is_empty() {
            return Err(Error::new(ErrorKind::WrongMasterPassword, "empty"));
        }
        let path = Self::path_in(directory);
        if path.exists() {
            return Err(Error::new(ErrorKind::AlreadyExists, path.to_string_lossy()));
        }
        let iterations = NonZeroU32::new(iterations)
            .ok_or_else(|| Error::new(ErrorKind::Other, "iterations"))?;
        let salt = random_bytes::<SALT_LENGTH>()?.to_vec();
        let key = derive_key(password, &salt, iterations);
        let check = seal(&key, CHECK_NAME, CHECK_TEXT)?;
        let vault = Self {
            path,
            iterations,
            salt,
            key,
            check,
            entries: BTreeMap::new(),
        };
        vault.save()?;
        Ok(vault)
    }

    pub fn unlock(directory: &Path, password: &str) -> Result<Self> {
        let path = Self::path_in(directory);
        let text = fs::read_to_string(&path)
            .map_err(|error| Error::from_io(&error, &path.to_string_lossy()))?;
        let file = VaultFile::parse(&text).ok_or_else(|| damaged(&path))?;
        let key = derive_key(password, &file.salt, file.iterations);
        match open(&key, CHECK_NAME, &file.check) {
            Some(text) if text == CHECK_TEXT => {}
            _ => return Err(Error::new(ErrorKind::WrongMasterPassword, "")),
        }
        Ok(Self {
            path,
            iterations: file.iterations,
            salt: file.salt,
            key,
            check: file.check,
            entries: file.entries,
        })
    }

    pub fn destroy(self) -> Result<()> {
        fs::remove_file(&self.path)
            .or_else(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(|error| Error::from_io(&error, &self.path.to_string_lossy()))
    }

    // Entries

    pub fn names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Result<Option<String>> {
        let Some(sealed) = self.entries.get(name) else {
            return Ok(None);
        };
        let plain = open(&self.key, name, sealed).ok_or_else(|| damaged(&self.path))?;
        String::from_utf8(plain)
            .map(Some)
            .map_err(|_| damaged(&self.path))
    }

    pub fn set(&mut self, name: &str, secret: &str) -> Result<()> {
        if !is_valid_name(name) {
            return Err(Error::new(
                ErrorKind::Other,
                format!("invalid name: {name}"),
            ));
        }
        let sealed = seal(&self.key, name, secret.as_bytes())?;
        self.entries.insert(name.to_string(), sealed);
        self.save()
    }

    pub fn delete(&mut self, name: &str) -> Result<bool> {
        if self.entries.remove(name).is_none() {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) -> Result<usize> {
        let before = self.entries.len();
        self.entries.retain(|name, _| keep(name));
        let removed = before - self.entries.len();
        if removed > 0 {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn change_password(&mut self, password: &str) -> Result<()> {
        if password.is_empty() {
            return Err(Error::new(ErrorKind::WrongMasterPassword, "empty"));
        }
        let mut plain = Vec::with_capacity(self.entries.len());
        for name in self.names() {
            let secret = self.get(&name)?.unwrap_or_default();
            plain.push((name, secret));
        }
        let salt = random_bytes::<SALT_LENGTH>()?.to_vec();
        let key = derive_key(password, &salt, self.iterations);
        let mut entries = BTreeMap::new();
        for (name, secret) in plain {
            entries.insert(name.clone(), seal(&key, &name, secret.as_bytes())?);
        }
        self.check = seal(&key, CHECK_NAME, CHECK_TEXT)?;
        self.salt = salt;
        self.key = key;
        self.entries = entries;
        self.save()
    }

    // Storage

    fn save(&self) -> Result<()> {
        let file = VaultFile {
            iterations: self.iterations,
            salt: self.salt.clone(),
            check: self.check.clone(),
            entries: self.entries.clone(),
        };
        write_atomically(&self.path, file.to_text().as_bytes())
            .map_err(|error| Error::from_io(&error, &self.path.to_string_lossy()))
    }
}

impl fmt::Debug for Vault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Vault")
            .field("path", &self.path)
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

// File format

impl VaultFile {
    fn parse(text: &str) -> Option<Self> {
        let pairs = key_value::parse(text);
        let value = |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        if value("format")? != FORMAT || value("kdf")? != KDF || value("cipher")? != CIPHER {
            return None;
        }
        let iterations = NonZeroU32::new(value("iterations")?.parse().ok()?)?;
        let salt = from_hex(value("salt")?).filter(|salt| !salt.is_empty())?;
        let check = from_hex(value("check")?)?;
        let mut entries = BTreeMap::new();
        for (key, value) in &pairs {
            if let Some(name) = key.strip_prefix(ENTRY_PREFIX) {
                entries.insert(name.to_string(), from_hex(value)?);
            }
        }
        Some(Self {
            iterations,
            salt,
            check,
            entries,
        })
    }

    fn to_text(&self) -> String {
        let mut pairs = vec![
            ("format", FORMAT.to_string()),
            ("kdf", KDF.to_string()),
            ("cipher", CIPHER.to_string()),
            ("iterations", self.iterations.to_string()),
            ("salt", to_hex(&self.salt)),
            ("check", to_hex(&self.check)),
        ];
        let entries: Vec<(String, String)> = self
            .entries
            .iter()
            .map(|(name, sealed)| (format!("{ENTRY_PREFIX}{name}"), to_hex(sealed)))
            .collect();
        pairs.extend(
            entries
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone())),
        );
        format!(
            "# Hoplix encrypted secrets. Do not edit.\n{}",
            key_value::write(&pairs)
        )
    }
}

// Cryptography

fn derive_key(password: &str, salt: &[u8], iterations: NonZeroU32) -> [u8; KEY_LENGTH] {
    let mut key = [0u8; KEY_LENGTH];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        password.as_bytes(),
        &mut key,
    );
    key
}

fn cipher(key: &[u8; KEY_LENGTH]) -> Result<LessSafeKey> {
    UnboundKey::new(&AES_256_GCM, key)
        .map(LessSafeKey::new)
        .map_err(|_| Error::new(ErrorKind::Other, "cipher"))
}

fn associated_data(name: &str) -> Vec<u8> {
    format!("{FORMAT}:{name}").into_bytes()
}

fn seal(key: &[u8; KEY_LENGTH], name: &str, plain: &[u8]) -> Result<Vec<u8>> {
    let nonce_bytes = random_bytes::<NONCE_LEN>()?;
    let mut data = plain.to_vec();
    cipher(key)?
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::from(associated_data(name)),
            &mut data,
        )
        .map_err(|_| Error::new(ErrorKind::Other, "encryption failed"))?;
    let mut sealed = nonce_bytes.to_vec();
    sealed.extend_from_slice(&data);
    Ok(sealed)
}

fn open(key: &[u8; KEY_LENGTH], name: &str, sealed: &[u8]) -> Option<Vec<u8>> {
    let (nonce, data) = sealed.split_at_checked(NONCE_LEN)?;
    let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
    let mut data = data.to_vec();
    let plain = cipher(key)
        .ok()?
        .open_in_place(nonce, Aad::from(associated_data(name)), &mut data)
        .ok()?;
    Some(plain.to_vec())
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    rand::generate::<[u8; N]>(&SystemRandom::new())
        .map(|random| random.expose())
        .map_err(|_| Error::new(ErrorKind::Other, "no random numbers available"))
}

// Helpers

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != CHECK_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
}

fn damaged(path: &Path) -> Error {
    Error::new(
        ErrorKind::Other,
        format!("{}: the file is damaged", path.to_string_lossy()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: u32 = 1_000;

    fn temporary_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-vault-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("creating the test directory failed");
        directory
    }

    // Round trip

    #[test]
    fn secrets_survive_locking_and_unlocking() {
        let directory = temporary_directory("round-trip");
        let mut vault =
            Vault::create_with_iterations(&directory, "correct horse", FAST).expect("created");
        vault.set("site/abc", "pässwörd 🔑").expect("stored");
        vault.set("site/def", "other").expect("stored");
        assert!(vault.delete("site/def").expect("deleted"));
        assert!(!vault.delete("site/def").expect("deleted twice"));

        let text = fs::read_to_string(Vault::path_in(&directory)).expect("file exists");
        assert!(!text.contains("pässwörd") && !text.contains("correct horse"));
        assert!(text.contains("iterations = 1000"));

        let unlocked = Vault::unlock(&directory, "correct horse").expect("unlocked");
        assert_eq!(
            unlocked.get("site/abc").expect("readable").as_deref(),
            Some("pässwörd 🔑")
        );
        assert_eq!(unlocked.get("site/def").expect("readable"), None);
        assert_eq!(unlocked.names(), ["site/abc"]);
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn wrong_passwords_are_refused() {
        let directory = temporary_directory("wrong");
        Vault::create_with_iterations(&directory, "secret", FAST).expect("created");
        let error = Vault::unlock(&directory, "Secret").expect_err("unlocked");
        assert_eq!(error.kind(), ErrorKind::WrongMasterPassword);
        assert!(Vault::create_with_iterations(&directory, "again", FAST).is_err());
        assert_eq!(
            Vault::unlock(&temporary_directory("missing"), "x")
                .expect_err("opened")
                .kind(),
            ErrorKind::NotFound
        );
        assert!(Vault::create_with_iterations(&temporary_directory("empty"), "", FAST).is_err());
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Tampering

    #[test]
    fn tampered_entries_are_detected() {
        let directory = temporary_directory("tamper");
        let mut vault = Vault::create_with_iterations(&directory, "pw", FAST).expect("created");
        vault.set("site/a", "alpha").expect("stored");
        vault.set("site/b", "beta").expect("stored");
        let path = Vault::path_in(&directory);
        let original = fs::read_to_string(&path).expect("file exists");

        let line = |name: &str| {
            original
                .lines()
                .find(|line| line.starts_with(&format!("entry.{name} =")))
                .expect("entry exists")
                .to_string()
        };
        let value_a = line("site/a")
            .split_once(" = ")
            .expect("pair")
            .1
            .to_string();
        let value_b = line("site/b")
            .split_once(" = ")
            .expect("pair")
            .1
            .to_string();
        let swapped = original
            .replace(&value_a, "SWAP")
            .replace(&value_b, &value_a)
            .replace("SWAP", &value_b);
        fs::write(&path, swapped).expect("writing failed");
        let vault = Vault::unlock(&directory, "pw").expect("header intact");
        assert!(vault.get("site/a").is_err());

        let mut flipped = value_a.clone().into_bytes();
        let last = flipped.len() - 1;
        flipped[last] = if flipped[last] == b'0' { b'1' } else { b'0' };
        let flipped = String::from_utf8(flipped).expect("ascii");
        fs::write(&path, original.replace(&value_a, &flipped)).expect("writing failed");
        let vault = Vault::unlock(&directory, "pw").expect("header intact");
        assert!(vault.get("site/a").is_err());
        assert_eq!(
            vault.get("site/b").expect("intact").as_deref(),
            Some("beta")
        );

        fs::write(
            &path,
            original.replace("iterations = 1000", "iterations = 999"),
        )
        .expect("writing failed");
        assert_eq!(
            Vault::unlock(&directory, "pw").expect_err("opened").kind(),
            ErrorKind::WrongMasterPassword
        );
        fs::write(&path, "format = something-else\n").expect("writing failed");
        assert_eq!(
            Vault::unlock(&directory, "pw").expect_err("opened").kind(),
            ErrorKind::Other
        );
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Maintenance

    #[test]
    fn changing_the_password_keeps_the_secrets() {
        let directory = temporary_directory("change");
        let mut vault = Vault::create_with_iterations(&directory, "old", FAST).expect("created");
        vault.set("site/a", "alpha").expect("stored");
        vault.change_password("new").expect("changed");
        assert_eq!(
            Vault::unlock(&directory, "old")
                .expect_err("old works")
                .kind(),
            ErrorKind::WrongMasterPassword
        );
        let mut vault = Vault::unlock(&directory, "new").expect("new works");
        assert_eq!(
            vault.get("site/a").expect("readable").as_deref(),
            Some("alpha")
        );
        assert!(vault.change_password("").is_err());

        vault.set("site/b", "beta").expect("stored");
        assert_eq!(vault.retain(|name| name == "site/b").expect("pruned"), 1);
        assert_eq!(vault.names(), ["site/b"]);
        assert!(vault.set("check", "x").is_err());
        assert!(vault.set("bad name", "x").is_err());

        let debug = format!("{vault:?}");
        assert!(!debug.contains("beta"));
        vault.destroy().expect("removed");
        assert!(!Vault::exists(&directory));
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
