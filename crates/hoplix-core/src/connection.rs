use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::{FileSystem, FtpFileSystem, Result, SftpFileSystem, TrustStore};

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    Sftp,
    Ftp,
    FtpsExplicit,
    FtpsImplicit,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Authentication {
    Password(String),
    KeyFile {
        path: PathBuf,
        passphrase: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionParams {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub authentication: Authentication,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyTrust {
    pub fingerprint: String,
    pub remember: bool,
}

impl Protocol {
    pub const ALL: [Self; 4] = [
        Self::Sftp,
        Self::Ftp,
        Self::FtpsExplicit,
        Self::FtpsImplicit,
    ];

    pub fn default_port(self) -> u16 {
        match self {
            Self::Sftp => 22,
            Self::Ftp | Self::FtpsExplicit => 21,
            Self::FtpsImplicit => 990,
        }
    }

    pub fn scheme(self) -> &'static str {
        match self {
            Self::Sftp => "sftp",
            Self::Ftp => "ftp",
            Self::FtpsExplicit => "ftpes",
            Self::FtpsImplicit => "ftps",
        }
    }

    pub fn is_encrypted(self) -> bool {
        self != Self::Ftp
    }

    pub fn supports_key_files(self) -> bool {
        self == Self::Sftp
    }

    pub fn uses_tls(self) -> bool {
        matches!(self, Self::FtpsExplicit | Self::FtpsImplicit)
    }
}

impl ConnectionParams {
    pub fn label(&self) -> String {
        if self.port == self.protocol.default_port() {
            format!("{}@{}", self.username, self.host)
        } else {
            format!("{}@{}:{}", self.username, self.host, self.port)
        }
    }
}

impl fmt::Debug for Authentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password(_) => formatter.write_str("Password(***)"),
            Self::KeyFile { path, passphrase } => formatter
                .debug_struct("KeyFile")
                .field("path", path)
                .field("passphrase", &passphrase.as_ref().map(|_| "***"))
                .finish(),
        }
    }
}

// Connecting

pub async fn connect(
    params: &ConnectionParams,
    trust_store: &TrustStore,
    trust: Option<HostKeyTrust>,
) -> Result<Arc<dyn FileSystem>> {
    match params.protocol {
        Protocol::Sftp => {
            let file_system =
                SftpFileSystem::connect(params, &trust_store.known_hosts, trust).await?;
            Ok(Arc::new(file_system))
        }
        Protocol::Ftp | Protocol::FtpsExplicit | Protocol::FtpsImplicit => {
            let file_system =
                FtpFileSystem::connect(params, &trust_store.certificates, trust).await?;
            Ok(Arc::new(file_system))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_password;

    fn params(port: u16, authentication: Authentication) -> ConnectionParams {
        ConnectionParams {
            protocol: Protocol::Sftp,
            host: "example.test".to_string(),
            port,
            username: "demo".to_string(),
            authentication,
        }
    }

    #[test]
    fn labels_hide_the_default_port() {
        let password = Authentication::Password(test_password());
        assert_eq!(params(22, password.clone()).label(), "demo@example.test");
        assert_eq!(params(2222, password).label(), "demo@example.test:2222");
    }

    #[test]
    fn protocols_know_ports_and_schemes() {
        let ports: Vec<u16> = Protocol::ALL.iter().map(|p| p.default_port()).collect();
        assert_eq!(ports, [22, 21, 21, 990]);
        let schemes: Vec<&str> = Protocol::ALL.iter().map(|p| p.scheme()).collect();
        assert_eq!(schemes, ["sftp", "ftp", "ftpes", "ftps"]);
        assert!(!Protocol::Ftp.is_encrypted());
        assert!(Protocol::FtpsImplicit.is_encrypted() && Protocol::FtpsImplicit.uses_tls());
        assert!(!Protocol::Sftp.uses_tls() && Protocol::Sftp.supports_key_files());
        assert!(!Protocol::FtpsExplicit.supports_key_files());
    }

    #[test]
    fn debug_output_hides_secrets() {
        let password = test_password();
        let output = format!(
            "{:?}",
            params(22, Authentication::Password(password.clone()))
        );
        assert!(!output.contains(&password));
        let passphrase = test_password();
        let output = format!(
            "{:?}",
            Authentication::KeyFile {
                path: PathBuf::from("id"),
                passphrase: Some(passphrase.clone())
            }
        );
        assert!(!output.contains(&passphrase));
    }
}
