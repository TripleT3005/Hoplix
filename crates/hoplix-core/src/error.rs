use std::fmt;
use std::io;

use crate::connection::HostAddress;

// Types

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    NotFound,
    PermissionDenied,
    NotADirectory,
    AlreadyExists,
    DirectoryNotEmpty,
    ConnectionFailed,
    AuthenticationFailed,
    InvalidKey,
    UnknownHostKey,
    HostKeyChanged,
    UntrustedCertificate,
    CertificateChanged,
    WrongMasterPassword,
    VaultLocked,
    SecretStorage,
    ChecksumMismatch,
    Unsupported,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    detail: String,
    hop: Option<String>,
    address: Option<Box<HostAddress>>,
}

// Construction

impl Error {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            hop: None,
            address: None,
        }
    }

    pub fn at_hop(mut self, hop: impl Into<String>) -> Self {
        self.hop = Some(hop.into());
        self
    }

    pub fn at_address(mut self, host: impl Into<String>, port: u16) -> Self {
        self.address = Some(Box::new(HostAddress::new(host, port)));
        self
    }

    pub fn from_io(error: &io::Error, path: &str) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::NotFound => ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
            io::ErrorKind::NotADirectory => ErrorKind::NotADirectory,
            io::ErrorKind::AlreadyExists => ErrorKind::AlreadyExists,
            io::ErrorKind::DirectoryNotEmpty => ErrorKind::DirectoryNotEmpty,
            _ => ErrorKind::Other,
        };
        let detail = if kind == ErrorKind::Other {
            format!("{path}: {error}")
        } else {
            path.to_string()
        };
        Self::new(kind, detail)
    }

    // Accessors

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn hop(&self) -> Option<&str> {
        self.hop.as_deref()
    }

    pub fn address(&self) -> Option<&HostAddress> {
        self.address.as_deref()
    }
}

// Traits

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_keep_the_path() {
        let error = Error::from_io(&io::Error::from(io::ErrorKind::NotFound), "/missing");
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(error.detail(), "/missing");
    }

    #[test]
    fn other_io_errors_include_the_reason() {
        let error = Error::from_io(&io::Error::other("disk on fire"), "/data");
        assert_eq!(error.kind(), ErrorKind::Other);
        assert_eq!(error.detail(), "/data: disk on fire");
    }

    #[test]
    fn errors_can_name_the_hop_that_failed() {
        let error = Error::new(ErrorKind::UnknownHostKey, "SHA256:abc");
        assert_eq!(error.hop(), None);
        let error = error.at_hop("jump@bastion.test");
        assert_eq!(error.hop(), Some("jump@bastion.test"));
        assert_eq!(error.detail(), "SHA256:abc");
    }

    #[test]
    fn errors_can_name_the_address_that_was_checked() {
        let error = Error::new(ErrorKind::UnknownHostKey, "SHA256:abc");
        assert_eq!(error.address(), None);
        let error = error.at_address("bastion.test", 2222);
        assert_eq!(
            error.address(),
            Some(&HostAddress::new("bastion.test", 2222))
        );
        assert_eq!(error.detail(), "SHA256:abc");
        assert_eq!(error.to_string(), "UnknownHostKey: SHA256:abc");
    }
}
