mod keyring;
mod vault;

use std::path::{Path, PathBuf};

pub use keyring::Keyring;
pub use vault::{DEFAULT_ITERATIONS, VAULT_FILE, Vault};

use crate::{Error, ErrorKind, Result};

// Constants

const SITE_PREFIX: &str = "site/";
const PROXY_PREFIX: &str = "proxy/";
const JUMP_PREFIX: &str = "jump/";

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecretBackend {
    #[default]
    Keyring,
    Vault,
}

#[derive(Debug, Clone)]
pub enum SecretStore {
    Keyring(Keyring),
    Vault(Vault),
}

#[derive(Debug, Clone)]
pub struct Moved<T> {
    pub target: T,
    pub leftovers: Vec<String>,
}

// Backends

impl SecretBackend {
    pub const ALL: [Self; 2] = [Self::Keyring, Self::Vault];

    pub fn detect(directory: &Path) -> Self {
        if Vault::exists(directory) {
            Self::Vault
        } else {
            Self::Keyring
        }
    }
}

// Names

pub fn secret_name(site_id: &str) -> String {
    format!("{SITE_PREFIX}{site_id}")
}

pub fn proxy_secret_name(site_id: &str) -> String {
    format!("{PROXY_PREFIX}{site_id}")
}

pub fn jump_secret_name(site_id: &str, hop: usize) -> String {
    format!("{JUMP_PREFIX}{site_id}/{hop}")
}

pub fn site_id_of(name: &str) -> Option<&str> {
    if let Some(site_id) = name.strip_prefix(SITE_PREFIX) {
        return Some(site_id);
    }
    if let Some(site_id) = name.strip_prefix(PROXY_PREFIX) {
        return Some(site_id);
    }
    name.strip_prefix(JUMP_PREFIX)
        .and_then(|rest| rest.rsplit_once('/'))
        .filter(|(_, hop)| hop.parse::<usize>().is_ok())
        .map(|(site_id, _)| site_id)
}

// Stores

impl SecretStore {
    pub fn backend(&self) -> SecretBackend {
        match self {
            Self::Keyring(_) => SecretBackend::Keyring,
            Self::Vault(_) => SecretBackend::Vault,
        }
    }

    pub async fn get(&self, name: &str) -> Result<Option<String>> {
        match self {
            Self::Keyring(keyring) => keyring.get(name).await,
            Self::Vault(vault) => vault.get(name),
        }
    }

    pub async fn set(&mut self, name: &str, secret: &str) -> Result<()> {
        match self {
            Self::Keyring(keyring) => keyring.set(name, secret).await,
            Self::Vault(vault) => vault.set(name, secret),
        }
    }

    pub async fn delete(&mut self, name: &str) -> Result<()> {
        match self {
            Self::Keyring(keyring) => keyring.delete(name).await,
            Self::Vault(vault) => vault.delete(name).map(|_| ()),
        }
    }
}

// Vault access

pub async fn create_vault(directory: PathBuf, password: String) -> Result<Vault> {
    tokio::task::spawn_blocking(move || Vault::create(&directory, &password))
        .await
        .map_err(|error| Error::new(ErrorKind::Other, error.to_string()))?
}

pub async fn unlock_vault(directory: PathBuf, password: String) -> Result<Vault> {
    tokio::task::spawn_blocking(move || Vault::unlock(&directory, &password))
        .await
        .map_err(|error| Error::new(ErrorKind::Other, error.to_string()))?
}

pub async fn change_master_password(mut vault: Vault, password: String) -> Result<Vault> {
    tokio::task::spawn_blocking(move || vault.change_password(&password).map(|()| vault))
        .await
        .map_err(|error| Error::new(ErrorKind::Other, error.to_string()))?
}

// Moving secrets

pub async fn move_to_vault(
    keyring: &Keyring,
    mut vault: Vault,
    names: &[String],
) -> Result<Moved<Vault>> {
    let mut moved = Vec::new();
    for name in names {
        if let Some(secret) = keyring.get(name).await? {
            vault.set(name, &secret)?;
            moved.push(name.clone());
        }
    }
    let mut leftovers = Vec::new();
    for name in moved {
        if keyring.delete(&name).await.is_err() {
            leftovers.push(name);
        }
    }
    Ok(Moved {
        target: vault,
        leftovers,
    })
}

pub async fn move_to_keyring(vault: Vault, keyring: &Keyring) -> Result<Moved<()>> {
    let names = vault.names();
    for name in &names {
        if let Some(secret) = vault.get(name)? {
            keyring.set(name, &secret).await?;
        }
    }
    vault.destroy()?;
    Ok(Moved {
        target: (),
        leftovers: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_password;

    #[test]
    fn names_link_secrets_to_sites() {
        assert_eq!(secret_name("abc123"), "site/abc123");
        assert_eq!(site_id_of("site/abc123"), Some("abc123"));
        assert_eq!(site_id_of("other"), None);
    }

    #[test]
    fn proxy_and_jump_secrets_belong_to_their_site() {
        assert_eq!(proxy_secret_name("abc123"), "proxy/abc123");
        assert_eq!(jump_secret_name("abc123", 0), "jump/abc123/0");
        assert_eq!(site_id_of(&proxy_secret_name("abc123")), Some("abc123"));
        assert_eq!(site_id_of(&jump_secret_name("abc123", 1)), Some("abc123"));
        assert_eq!(site_id_of("jump/abc123"), None);
        assert_eq!(site_id_of("jump/abc123/x"), None);
    }

    #[test]
    fn the_backend_follows_the_vault_file() {
        let directory = std::env::temp_dir().join(format!("hoplix-backend-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("creating failed");
        assert_eq!(SecretBackend::detect(&directory), SecretBackend::Keyring);
        let vault =
            Vault::create_with_iterations(&directory, &test_password(), 1_000).expect("created");
        assert_eq!(SecretBackend::detect(&directory), SecretBackend::Vault);
        assert_eq!(SecretStore::Vault(vault).backend(), SecretBackend::Vault);
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test]
    async fn vault_stores_behave_like_keyrings() {
        let directory = std::env::temp_dir().join(format!("hoplix-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("creating failed");
        let vault =
            Vault::create_with_iterations(&directory, &test_password(), 1_000).expect("created");
        let mut store = SecretStore::Vault(vault);
        store.set("site/a", "alpha").await.expect("stored");
        assert_eq!(
            store.get("site/a").await.expect("read").as_deref(),
            Some("alpha")
        );
        store.delete("site/a").await.expect("deleted");
        assert_eq!(store.get("site/a").await.expect("read"), None);
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "writes to the system keyring"]
    async fn secrets_move_between_keyring_and_vault() {
        let directory = std::env::temp_dir().join(format!("hoplix-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("creating failed");
        let keyring = Keyring::with_service(&format!("Hoplix-test-{}", std::process::id()));
        keyring.set("site/a", "alpha").await.expect("stored");

        let vault =
            Vault::create_with_iterations(&directory, &test_password(), 1_000).expect("created");
        let names = vec!["site/a".to_string(), "site/missing".to_string()];
        let moved = move_to_vault(&keyring, vault, &names).await.expect("moved");
        assert!(moved.leftovers.is_empty());
        assert_eq!(keyring.get("site/a").await.expect("read"), None);
        assert_eq!(
            moved.target.get("site/a").expect("read").as_deref(),
            Some("alpha")
        );

        move_to_keyring(moved.target, &keyring)
            .await
            .expect("moved back");
        assert!(!Vault::exists(&directory));
        assert_eq!(
            keyring.get("site/a").await.expect("read").as_deref(),
            Some("alpha")
        );
        keyring.delete("site/a").await.expect("cleaned up");
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
