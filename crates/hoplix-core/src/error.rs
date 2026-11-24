use std::fmt;
use std::io;

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
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    detail: String,
}

// Construction

impl Error {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
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
}
