use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::{Error, ErrorKind, Result};

// Constants

pub const SERVICE: &str = "Hoplix";

// Types

type Memory = Arc<Mutex<HashMap<String, String>>>;

#[derive(Debug, Clone)]
pub struct Keyring {
    service: String,
    memory: Option<Memory>,
}

impl PartialEq for Keyring {
    fn eq(&self, other: &Self) -> bool {
        self.service == other.service
            && match (&self.memory, &other.memory) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for Keyring {}

impl Default for Keyring {
    fn default() -> Self {
        Self::with_service(SERVICE)
    }
}

// Interface

impl Keyring {
    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.to_string(),
            memory: None,
        }
    }

    pub fn in_memory() -> Self {
        Self {
            service: SERVICE.to_string(),
            memory: Some(Memory::default()),
        }
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub async fn get(&self, name: &str) -> Result<Option<String>> {
        match &self.memory {
            Some(memory) => Ok(lock(memory).get(name).cloned()),
            None => platform::get(&self.service, name).await,
        }
    }

    pub async fn set(&self, name: &str, secret: &str) -> Result<()> {
        match &self.memory {
            Some(memory) => {
                lock(memory).insert(name.to_string(), secret.to_string());
                Ok(())
            }
            None => platform::set(&self.service, name, secret).await,
        }
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        match &self.memory {
            Some(memory) => {
                lock(memory).remove(name);
                Ok(())
            }
            None => platform::delete(&self.service, name).await,
        }
    }
}

fn lock(memory: &Memory) -> MutexGuard<'_, HashMap<String, String>> {
    memory.lock().unwrap_or_else(PoisonError::into_inner)
}

fn storage_error(detail: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::SecretStorage, detail.to_string())
}

// Windows

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };

    use super::storage_error;
    use crate::Result;

    const MAX_BLOB_SIZE: usize = 5 * 512;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn target(service: &str, name: &str) -> Vec<u16> {
        wide(&format!("{service}:{name}"))
    }

    async fn blocking<T: Send + 'static>(
        work: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(storage_error)?
    }

    pub async fn get(service: &str, name: &str) -> Result<Option<String>> {
        let target = target(service, name);
        blocking(move || {
            let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
            let found =
                unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut credential) };
            if found == 0 {
                let code = unsafe { GetLastError() };
                return if code == ERROR_NOT_FOUND {
                    Ok(None)
                } else {
                    Err(storage_error(std::io::Error::from_raw_os_error(
                        code.cast_signed(),
                    )))
                };
            }
            let secret = unsafe {
                let entry = &*credential;
                let length = usize::try_from(entry.CredentialBlobSize).unwrap_or(0);
                let bytes = if entry.CredentialBlob.is_null() || length == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(entry.CredentialBlob, length).to_vec()
                };
                CredFree(credential.cast());
                bytes
            };
            String::from_utf8(secret)
                .map(Some)
                .map_err(|_| storage_error("the saved secret is not valid text"))
        })
        .await
    }

    pub async fn set(service: &str, name: &str, secret: &str) -> Result<()> {
        if secret.len() > MAX_BLOB_SIZE {
            return Err(storage_error("the secret is too long"));
        }
        let mut target = target(service, name);
        let mut user = wide(service);
        let mut blob = secret.as_bytes().to_vec();
        blocking(move || {
            let credential = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: target.as_mut_ptr(),
                CredentialBlobSize: u32::try_from(blob.len()).unwrap_or(0),
                CredentialBlob: blob.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                UserName: user.as_mut_ptr(),
                ..CREDENTIALW::default()
            };
            let written = unsafe { CredWriteW(&raw const credential, 0) };
            blob.fill(0);
            if written == 0 {
                let code = unsafe { GetLastError() };
                return Err(storage_error(std::io::Error::from_raw_os_error(
                    code.cast_signed(),
                )));
            }
            Ok(())
        })
        .await
    }

    pub async fn delete(service: &str, name: &str) -> Result<()> {
        let target = target(service, name);
        blocking(move || {
            let deleted = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
            if deleted == 0 {
                let code = unsafe { GetLastError() };
                if code != ERROR_NOT_FOUND {
                    return Err(storage_error(std::io::Error::from_raw_os_error(
                        code.cast_signed(),
                    )));
                }
            }
            Ok(())
        })
        .await
    }
}

// Linux

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::HashMap;

    use secret_service::{EncryptionType, SecretService};

    use super::storage_error;
    use crate::Result;

    const APPLICATION_ATTRIBUTE: &str = "application";
    const NAME_ATTRIBUTE: &str = "name";
    const CONTENT_TYPE: &str = "text/plain";

    fn attributes<'a>(service: &'a str, name: &'a str) -> HashMap<&'a str, &'a str> {
        HashMap::from([(APPLICATION_ATTRIBUTE, service), (NAME_ATTRIBUTE, name)])
    }

    async fn connect() -> Result<SecretService<'static>> {
        SecretService::connect(EncryptionType::Dh)
            .await
            .map_err(storage_error)
    }

    pub async fn get(service: &str, name: &str) -> Result<Option<String>> {
        let connection = connect().await?;
        let collection = connection
            .get_default_collection()
            .await
            .map_err(storage_error)?;
        collection.ensure_unlocked().await.map_err(storage_error)?;
        let items = collection
            .search_items(attributes(service, name))
            .await
            .map_err(storage_error)?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        item.ensure_unlocked().await.map_err(storage_error)?;
        let secret = item.get_secret().await.map_err(storage_error)?;
        String::from_utf8(secret)
            .map(Some)
            .map_err(|_| storage_error("the saved secret is not valid text"))
    }

    pub async fn set(service: &str, name: &str, secret: &str) -> Result<()> {
        let connection = connect().await?;
        let collection = connection
            .get_default_collection()
            .await
            .map_err(storage_error)?;
        collection.ensure_unlocked().await.map_err(storage_error)?;
        collection
            .create_item(
                &format!("{service}: {name}"),
                attributes(service, name),
                secret.as_bytes(),
                true,
                CONTENT_TYPE,
            )
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    pub async fn delete(service: &str, name: &str) -> Result<()> {
        let connection = connect().await?;
        let collection = connection
            .get_default_collection()
            .await
            .map_err(storage_error)?;
        collection.ensure_unlocked().await.map_err(storage_error)?;
        for item in collection
            .search_items(attributes(service, name))
            .await
            .map_err(storage_error)?
        {
            item.delete().await.map_err(storage_error)?;
        }
        Ok(())
    }
}

// Other systems

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use super::storage_error;
    use crate::Result;

    const UNSUPPORTED: &str = "no system keyring is supported on this system";

    pub async fn get(_service: &str, _name: &str) -> Result<Option<String>> {
        Err(storage_error(UNSUPPORTED))
    }

    pub async fn set(_service: &str, _name: &str, _secret: &str) -> Result<()> {
        Err(storage_error(UNSUPPORTED))
    }

    pub async fn delete(_service: &str, _name: &str) -> Result<()> {
        Err(storage_error(UNSUPPORTED))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_service_is_hoplix() {
        assert_eq!(Keyring::default().service(), "Hoplix");
        assert_eq!(
            Keyring::with_service("Hoplix-test").service(),
            "Hoplix-test"
        );
    }

    #[tokio::test]
    async fn memory_keyrings_stay_apart_from_the_system() {
        let keyring = Keyring::in_memory();
        let copy = keyring.clone();
        assert_ne!(keyring, Keyring::in_memory());
        assert_ne!(keyring, Keyring::default());
        assert_eq!(keyring, copy);
        keyring.set("site/a", "alpha").await.expect("saving failed");
        assert_eq!(
            copy.get("site/a").await.expect("reading failed").as_deref(),
            Some("alpha")
        );
        copy.delete("site/a").await.expect("deleting failed");
        assert_eq!(keyring.get("site/a").await.expect("reading failed"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "writes to the system keyring"]
    async fn secrets_round_trip_through_the_system_keyring() {
        let keyring = Keyring::with_service(&format!("Hoplix-test-{}", std::process::id()));
        let name = "site/roundtrip";
        keyring.delete(name).await.expect("cleaning up failed");
        assert_eq!(keyring.get(name).await.expect("reading failed"), None);
        keyring.set(name, "pässwörd").await.expect("saving failed");
        keyring
            .set(name, "pässwörd 2")
            .await
            .expect("replacing failed");
        assert_eq!(
            keyring.get(name).await.expect("reading failed").as_deref(),
            Some("pässwörd 2")
        );
        keyring.delete(name).await.expect("deleting failed");
        keyring.delete(name).await.expect("deleting twice failed");
        assert_eq!(keyring.get(name).await.expect("reading failed"), None);
    }
}
