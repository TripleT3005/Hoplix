use std::path::{Path, PathBuf};

// Constants

const CONFIG_OVERRIDE: &str = "HOPLIX_CONFIG_DIR";
pub const PORTABLE_MARKER: &str = "hoplix.portable";
pub const PORTABLE_CONFIG_DIR: &str = "config";
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
    resolve_config_dir(
        std::env::var_os(CONFIG_OVERRIDE).map(PathBuf::from),
        portable_dir(),
        system_config_dir(),
    )
}

pub fn resolve_config_dir(
    override_dir: Option<PathBuf>,
    portable_dir: Option<PathBuf>,
    system_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    override_dir
        .filter(|directory| !directory.as_os_str().is_empty())
        .or_else(|| portable_dir.map(|directory| directory.join(PORTABLE_CONFIG_DIR)))
        .or(system_dir)
}

pub fn is_portable() -> bool {
    portable_dir().is_some()
}

pub fn portable_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .filter(|directory| has_portable_marker(directory))
}

pub fn has_portable_marker(directory: &Path) -> bool {
    directory.join(PORTABLE_MARKER).is_file()
}

fn system_config_dir() -> Option<PathBuf> {
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

    #[test]
    fn override_wins_over_portable_and_system() {
        let resolved = resolve_config_dir(
            Some(PathBuf::from("override")),
            Some(PathBuf::from("app")),
            Some(PathBuf::from("system")),
        );
        assert_eq!(resolved, Some(PathBuf::from("override")));
    }

    #[test]
    fn portable_folder_wins_over_system() {
        let resolved = resolve_config_dir(
            None,
            Some(PathBuf::from("app")),
            Some(PathBuf::from("system")),
        );
        assert_eq!(resolved, Some(Path::new("app").join(PORTABLE_CONFIG_DIR)));
    }

    #[test]
    fn empty_override_is_ignored() {
        let resolved =
            resolve_config_dir(Some(PathBuf::new()), None, Some(PathBuf::from("system")));
        assert_eq!(resolved, Some(PathBuf::from("system")));
    }

    #[test]
    fn system_folder_is_the_default() {
        let resolved = resolve_config_dir(None, None, Some(PathBuf::from("system")));
        assert_eq!(resolved, Some(PathBuf::from("system")));
    }

    #[test]
    fn marker_file_enables_portable_mode() {
        let directory =
            std::env::temp_dir().join(format!("hoplix-portable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        assert!(!has_portable_marker(&directory));
        std::fs::create_dir(directory.join(PORTABLE_MARKER)).unwrap();
        assert!(!has_portable_marker(&directory));
        std::fs::remove_dir(directory.join(PORTABLE_MARKER)).unwrap();
        std::fs::write(directory.join(PORTABLE_MARKER), "").unwrap();
        assert!(has_portable_marker(&directory));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
