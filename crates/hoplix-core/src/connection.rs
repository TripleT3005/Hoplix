use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::{
    ConnectionOptions, Error, ErrorKind, FileSystem, Result, Route, TrustStore, connect_via_with,
};

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    Sftp,
    Ftp,
    FtpsExplicit,
    FtpsImplicit,
    Scp,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Authentication {
    Password(String),
    KeyFile {
        path: PathBuf,
        passphrase: Option<String>,
    },
    Agent,
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
pub struct HostAddress {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyTrust {
    pub address: HostAddress,
    pub fingerprint: String,
    pub remember: bool,
}

impl HostAddress {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }

    pub fn is(&self, host: &str, port: u16) -> bool {
        self.port == port && self.host.eq_ignore_ascii_case(host)
    }
}

impl HostKeyTrust {
    pub fn for_error(error: &Error, remember: bool) -> Option<Self> {
        if !matches!(
            error.kind(),
            ErrorKind::UnknownHostKey | ErrorKind::UntrustedCertificate
        ) {
            return None;
        }
        Some(Self {
            address: error.address()?.clone(),
            fingerprint: error.detail().to_string(),
            remember,
        })
    }

    pub fn applies_to(&self, host: &str, port: u16, fingerprint: &str) -> bool {
        self.address.is(host, port) && self.fingerprint == fingerprint
    }

    pub fn answers(&self, error: &Error) -> bool {
        error
            .address()
            .is_some_and(|address| self.applies_to(&address.host, address.port, error.detail()))
    }
}

impl Protocol {
    pub const ALL: [Self; 5] = [
        Self::Sftp,
        Self::Ftp,
        Self::FtpsExplicit,
        Self::FtpsImplicit,
        Self::Scp,
    ];

    pub fn default_port(self) -> u16 {
        match self {
            Self::Sftp | Self::Scp => 22,
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
            Self::Scp => "scp",
        }
    }

    pub fn is_encrypted(self) -> bool {
        self != Self::Ftp
    }

    pub fn supports_key_files(self) -> bool {
        self.uses_ssh()
    }

    pub fn uses_ssh(self) -> bool {
        matches!(self, Self::Sftp | Self::Scp)
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

    pub fn url(&self, path: &str) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let port = if self.port == self.protocol.default_port() {
            String::new()
        } else {
            format!(":{}", self.port)
        };
        let path = if path.starts_with('/') {
            encode_url_path(path)
        } else {
            format!("/{}", encode_url_path(path))
        };
        format!(
            "{}://{}@{host}{port}{path}",
            self.protocol.scheme(),
            percent_encode(&self.username, is_user_byte)
        )
    }
}

// URLs

pub fn encode_url_path(path: &str) -> String {
    percent_encode(path, |byte| {
        is_user_byte(byte) || matches!(byte, b'/' | b':' | b'@')
    })
}

fn is_user_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
        )
}

fn percent_encode(text: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if keep(byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
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
            Self::Agent => formatter.write_str("Agent"),
        }
    }
}

// Connecting

pub async fn connect(
    params: &ConnectionParams,
    trust_store: &TrustStore,
    trust: Option<HostKeyTrust>,
) -> Result<Arc<dyn FileSystem>> {
    connect_with(params, &ConnectionOptions::default(), trust_store, trust).await
}

pub async fn connect_with(
    params: &ConnectionParams,
    options: &ConnectionOptions,
    trust_store: &TrustStore,
    trust: Option<HostKeyTrust>,
) -> Result<Arc<dyn FileSystem>> {
    let trusted: Vec<HostKeyTrust> = trust.into_iter().collect();
    connect_via_with(params, &Route::default(), options, trust_store, &trusted).await
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
    fn urls_name_user_host_and_path_but_no_secret() {
        let secret = test_password();
        let sftp = params(22, Authentication::Password(secret.clone()));
        assert_eq!(
            sftp.url("/var/www/a b#1.txt"),
            "sftp://demo@example.test/var/www/a%20b%231.txt"
        );
        let mut ftp = params(2121, Authentication::Password(secret.clone()));
        ftp.protocol = Protocol::FtpsImplicit;
        ftp.username = "web:admin@site".to_string();
        ftp.host = "::1".to_string();
        let url = ftp.url("Grüße");
        assert_eq!(url, "ftps://web%3Aadmin%40site@[::1]:2121/Gr%C3%BC%C3%9Fe");
        assert!(!url.contains(&secret) && !sftp.url("/").contains(&secret));
        assert_eq!(encode_url_path("/a:b@c/~d"), "/a:b@c/~d");
    }

    #[test]
    fn protocols_know_ports_and_schemes() {
        let ports: Vec<u16> = Protocol::ALL.iter().map(|p| p.default_port()).collect();
        assert_eq!(ports, [22, 21, 21, 990, 22]);
        let schemes: Vec<&str> = Protocol::ALL.iter().map(|p| p.scheme()).collect();
        assert_eq!(schemes, ["sftp", "ftp", "ftpes", "ftps", "scp"]);
        assert!(Protocol::Scp.uses_ssh() && Protocol::Scp.supports_key_files());
        assert!(Protocol::Scp.is_encrypted() && !Protocol::Scp.uses_tls());
        assert!(!Protocol::Ftp.is_encrypted());
        assert!(Protocol::FtpsImplicit.is_encrypted() && Protocol::FtpsImplicit.uses_tls());
        assert!(!Protocol::Sftp.uses_tls() && Protocol::Sftp.supports_key_files());
        assert!(!Protocol::FtpsExplicit.supports_key_files());
    }

    #[test]
    fn trusts_are_built_from_errors_and_bound_to_their_address() {
        let error = Error::new(ErrorKind::UnknownHostKey, "SHA256:abc").at_address("Jump.test", 22);
        let trust = HostKeyTrust::for_error(&error, true).expect("no trust");
        assert_eq!(
            trust,
            HostKeyTrust {
                address: HostAddress::new("Jump.test", 22),
                fingerprint: "SHA256:abc".to_string(),
                remember: true,
            }
        );
        assert!(trust.answers(&error));
        assert!(trust.applies_to("jump.TEST", 22, "SHA256:abc"));
        assert!(!trust.applies_to("jump.test", 2222, "SHA256:abc"));
        assert!(!trust.applies_to("target.test", 22, "SHA256:abc"));
        assert!(!trust.applies_to("jump.test", 22, "SHA256:other"));
        let target =
            Error::new(ErrorKind::UnknownHostKey, "SHA256:abc").at_address("target.test", 22);
        assert!(!trust.answers(&target));

        let unbound = Error::new(ErrorKind::UnknownHostKey, "SHA256:abc");
        assert_eq!(HostKeyTrust::for_error(&unbound, false), None);
        let failed = Error::new(ErrorKind::ConnectionFailed, "x").at_address("jump.test", 22);
        assert_eq!(HostKeyTrust::for_error(&failed, false), None);
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
