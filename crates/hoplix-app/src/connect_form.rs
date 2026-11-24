use std::path::PathBuf;

use hoplix_core::{Authentication, ConnectionParams, Error, Protocol, Site, SiteAuth};

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMethod {
    #[default]
    Password,
    KeyFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormError {
    MissingHost,
    InvalidPort,
    MissingUsername,
    MissingKey,
}

#[derive(Debug, Clone)]
pub enum Field {
    Protocol(Protocol),
    Host(String),
    Port(String),
    Username(String),
    Method(AuthMethod),
    Password(String),
    KeyPath(String),
    Passphrase(String),
    SavePassword(bool),
}

#[derive(Debug, Clone, Default)]
pub struct ConnectForm {
    pub protocol: Protocol,
    pub host: String,
    pub port: String,
    pub username: String,
    pub method: AuthMethod,
    pub password: String,
    pub key_path: String,
    pub passphrase: String,
    pub connecting: bool,
    pub error: Option<Error>,
    pub invalid: Option<FormError>,
    pub site: Option<String>,
    pub save_password: bool,
    pub secret_from_store: bool,
}

impl AuthMethod {
    pub const ALL: [Self; 2] = [Self::Password, Self::KeyFile];
}

impl FormError {
    pub fn key(self) -> &'static str {
        match self {
            Self::MissingHost => "connect.invalid.host",
            Self::InvalidPort => "connect.invalid.port",
            Self::MissingUsername => "connect.invalid.username",
            Self::MissingKey => "connect.invalid.key",
        }
    }
}

// Editing

impl ConnectForm {
    pub fn apply(&mut self, field: Field) {
        match field {
            Field::Protocol(protocol) => {
                self.protocol = protocol;
                if !protocol.supports_key_files() {
                    self.method = AuthMethod::Password;
                }
            }
            Field::Host(host) => self.host = host,
            Field::Port(port) => {
                if port.chars().all(|c| c.is_ascii_digit()) {
                    self.port = port;
                }
            }
            Field::Username(username) => self.username = username,
            Field::Method(method) => self.method = method,
            Field::Password(password) => {
                self.password = password;
                self.secret_from_store = false;
            }
            Field::KeyPath(path) => self.key_path = path,
            Field::Passphrase(passphrase) => {
                self.passphrase = passphrase;
                self.secret_from_store = false;
            }
            Field::SavePassword(save) => self.save_password = save,
        }
        self.invalid = None;
    }

    pub fn edit(&mut self, field: Field) {
        let target = self.target();
        self.apply(field);
        if self.site.is_some() && self.target() != target {
            self.site = None;
            self.save_password = false;
            self.secret_from_store = false;
        }
    }

    fn target(&self) -> (Protocol, String, String, String, AuthMethod, String) {
        (
            self.protocol,
            self.host.trim().to_string(),
            self.port.clone(),
            self.username.trim().to_string(),
            self.effective_method(),
            self.key_path.trim().to_string(),
        )
    }

    pub fn fill_from_site(&mut self, site: &Site) {
        *self = Self {
            protocol: site.protocol,
            host: site.host.clone(),
            port: if site.port == site.protocol.default_port() {
                String::new()
            } else {
                site.port.to_string()
            },
            username: site.username.clone(),
            method: if site.uses_key_file() {
                AuthMethod::KeyFile
            } else {
                AuthMethod::Password
            },
            key_path: site.key_path.clone(),
            site: Some(site.id.clone()),
            save_password: site.save_secrets,
            ..Self::default()
        };
    }

    pub fn set_secret(&mut self, secret: String) {
        match self.effective_method() {
            AuthMethod::Password => self.password = secret,
            AuthMethod::KeyFile => self.passphrase = secret,
        }
    }

    pub fn secret(&self) -> &str {
        match self.effective_method() {
            AuthMethod::Password => &self.password,
            AuthMethod::KeyFile => &self.passphrase,
        }
    }

    pub fn effective_method(&self) -> AuthMethod {
        if self.protocol.supports_key_files() {
            self.method
        } else {
            AuthMethod::Password
        }
    }

    pub fn site_auth(&self) -> SiteAuth {
        match self.effective_method() {
            AuthMethod::Password => SiteAuth::Password,
            AuthMethod::KeyFile => SiteAuth::KeyFile,
        }
    }

    pub fn clear_secrets(&mut self) {
        self.password.clear();
        self.passphrase.clear();
    }

    // Validation

    pub fn to_params(&self) -> Result<ConnectionParams, FormError> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err(FormError::MissingHost);
        }
        let port = if self.port.trim().is_empty() {
            self.protocol.default_port()
        } else {
            self.port
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or(FormError::InvalidPort)?
        };
        let username = self.username.trim();
        if username.is_empty() {
            return Err(FormError::MissingUsername);
        }
        let authentication = match self.effective_method() {
            AuthMethod::Password => Authentication::Password(self.password.clone()),
            AuthMethod::KeyFile => {
                let path = self.key_path.trim().trim_matches('"');
                if path.is_empty() {
                    return Err(FormError::MissingKey);
                }
                Authentication::KeyFile {
                    path: PathBuf::from(path),
                    passphrase: Some(self.passphrase.clone()).filter(|value| !value.is_empty()),
                }
            }
        };
        Ok(ConnectionParams {
            protocol: self.protocol,
            host: host.to_string(),
            port,
            username: username.to_string(),
            authentication,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> ConnectForm {
        let mut form = ConnectForm::default();
        form.apply(Field::Host(" files.example.test ".to_string()));
        form.apply(Field::Username("demo".to_string()));
        form.apply(Field::Password("secret".to_string()));
        form
    }

    #[test]
    fn valid_forms_become_params_with_default_port() {
        let params = filled().to_params().expect("form is valid");
        assert_eq!(params.host, "files.example.test");
        assert_eq!(params.port, 22);
        assert_eq!(
            params.authentication,
            Authentication::Password("secret".to_string())
        );
    }

    #[test]
    fn missing_or_invalid_fields_are_reported() {
        assert_eq!(
            ConnectForm::default().to_params(),
            Err(FormError::MissingHost)
        );

        let mut form = filled();
        form.apply(Field::Port("70000".to_string()));
        assert_eq!(form.to_params(), Err(FormError::InvalidPort));
        form.apply(Field::Port("0".to_string()));
        assert_eq!(form.to_params(), Err(FormError::InvalidPort));
        form.apply(Field::Port("2222".to_string()));
        assert_eq!(form.to_params().map(|params| params.port), Ok(2222));

        form.apply(Field::Username(" ".to_string()));
        assert_eq!(form.to_params(), Err(FormError::MissingUsername));
    }

    #[test]
    fn ftp_always_signs_in_with_a_password() {
        let mut form = filled();
        form.apply(Field::Method(AuthMethod::KeyFile));
        form.apply(Field::Protocol(Protocol::FtpsExplicit));
        assert_eq!(form.method, AuthMethod::Password);
        let params = form.to_params().expect("form is valid");
        assert_eq!(params.port, 21);
        assert_eq!(
            params.authentication,
            Authentication::Password("secret".to_string())
        );
        form.apply(Field::Protocol(Protocol::FtpsImplicit));
        assert_eq!(form.to_params().map(|params| params.port), Ok(990));
    }

    #[test]
    fn sites_fill_the_form_and_pick_the_secret_field() {
        let mut site = Site::new("Web");
        site.host = "files.example.test".to_string();
        site.username = "demo".to_string();
        site.port = 2222;
        site.auth = SiteAuth::KeyFile;
        site.key_path = "id".to_string();
        site.save_secrets = true;
        let mut form = filled();
        form.fill_from_site(&site);
        assert_eq!(form.password, "");
        assert_eq!(form.port, "2222");
        assert_eq!(form.site.as_deref(), Some(site.id.as_str()));
        assert!(form.save_password);
        form.set_secret("phrase".to_string());
        assert_eq!(form.passphrase, "phrase");
        assert_eq!(form.secret(), "phrase");
        assert_eq!(form.site_auth(), SiteAuth::KeyFile);

        site.protocol = Protocol::Ftp;
        site.port = 21;
        form.fill_from_site(&site);
        assert_eq!(form.port, "");
        form.set_secret("pw".to_string());
        assert_eq!(form.password, "pw");
        assert_eq!(form.site_auth(), SiteAuth::Password);
    }

    #[test]
    fn changing_the_server_detaches_the_site() {
        let mut site = Site::new("Web");
        site.host = "files.example.test".to_string();
        site.username = "demo".to_string();
        site.save_secrets = true;
        let mut form = ConnectForm::default();
        form.fill_from_site(&site);
        form.edit(Field::Password("new".to_string()));
        form.edit(Field::Protocol(Protocol::Sftp));
        assert_eq!(form.site.as_deref(), Some(site.id.as_str()));
        assert!(form.save_password);

        form.edit(Field::Protocol(Protocol::Ftp));
        assert_eq!(form.site, None);
        assert!(!form.save_password);

        form.fill_from_site(&site);
        form.edit(Field::Host("other.example.test".to_string()));
        assert_eq!(form.site, None);
    }

    #[test]
    fn port_accepts_digits_only() {
        let mut form = filled();
        form.apply(Field::Port("22a".to_string()));
        assert_eq!(form.port, "");
    }

    #[test]
    fn key_files_need_a_path_and_keep_the_passphrase() {
        let mut form = filled();
        form.apply(Field::Method(AuthMethod::KeyFile));
        assert_eq!(form.to_params(), Err(FormError::MissingKey));
        form.apply(Field::KeyPath("\"C:\\keys\\id_ed25519\"".to_string()));
        form.apply(Field::Passphrase("phrase".to_string()));
        assert_eq!(
            form.to_params().map(|params| params.authentication),
            Ok(Authentication::KeyFile {
                path: PathBuf::from("C:\\keys\\id_ed25519"),
                passphrase: Some("phrase".to_string()),
            })
        );
        form.clear_secrets();
        assert!(form.password.is_empty() && form.passphrase.is_empty());
    }
}
