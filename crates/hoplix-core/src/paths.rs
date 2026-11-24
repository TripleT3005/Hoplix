use std::path::{Path, PathBuf};

// Constants

const CONFIG_OVERRIDE: &str = "HOPLIX_CONFIG_DIR";
const KNOWN_HOSTS_FILE: &str = "known_hosts";
const TRUSTED_CERTIFICATES_FILE: &str = "trusted_certificates";

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustStore {
    pub known_hosts: PathBuf,
    pub certificates: PathBuf,
}

impl TrustStore {
    pub fn in_directory(directory: &Path) -> Self {
        Self {
            known_hosts: directory.join(KNOWN_HOSTS_FILE),
            certificates: directory.join(TRUSTED_CERTIFICATES_FILE),
        }
    }

    pub fn default_location() -> Option<Self> {
        config_dir().map(|directory| Self::in_directory(&directory))
    }
}

// Locations

pub fn config_dir() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os(CONFIG_OVERRIDE).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(directory));
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("Hoplix"));
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".config")))
        .map(|base| base.join("hoplix"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_files_share_one_folder() {
        let store = TrustStore::in_directory(Path::new("config"));
        assert_eq!(store.known_hosts, Path::new("config").join("known_hosts"));
        assert_eq!(
            store.certificates,
            Path::new("config").join("trusted_certificates")
        );
    }
}
