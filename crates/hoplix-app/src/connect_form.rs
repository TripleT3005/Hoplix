use std::path::PathBuf;

use hoplix_core::{
    Authentication, ConnectionParams, Error, Protocol, RecentConnection, Route, Site, SiteAuth,
    SiteJump, SiteProxy,
};

use crate::route_form::{JumpField, JumpForm, ProxyField, ProxyForm, parse_port};

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMethod {
    #[default]
    Password,
    KeyFile,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormError {
    MissingHost,
    InvalidPort,
    MissingUsername,
    MissingKey,
    MissingJumpHost,
    InvalidJumpPort,
    MissingJumpUsername,
    MissingJumpKey,
    MissingProxyHost,
    InvalidProxyPort,
}

#[derive(Debug, Clone)]
pub enum Field {
    Protocol(Protocol),
    Host(String),
    Port(String),
    BasePath(String),
    Username(String),
    Method(AuthMethod),
    Password(String),
    KeyPath(String),
    Passphrase(String),
    SavePassword(bool),
    ToggleAdvanced,
    Jump(JumpField),
    Proxy(ProxyField),
}

#[derive(Debug, Clone, Default)]
pub struct ConnectForm {
    pub protocol: Protocol,
    pub host: String,
    pub port: String,
    pub base_path: String,
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
    pub advanced_open: bool,
    pub jump: JumpForm,
    pub proxy: ProxyForm,
}

impl AuthMethod {
    pub const ALL: [Self; 3] = [Self::Password, Self::KeyFile, Self::Agent];
}

impl FormError {
    pub fn key(self) -> &'static str {
        match self {
            Self::MissingHost => "connect.invalid.host",
            Self::InvalidPort => "connect.invalid.port",
            Self::MissingUsername => "connect.invalid.username",
            Self::MissingKey => "connect.invalid.key",
            Self::MissingJumpHost => "connect.invalid.jump_host",
            Self::InvalidJumpPort => "connect.invalid.jump_port",
            Self::MissingJumpUsername => "connect.invalid.jump_username",
            Self::MissingJumpKey => "connect.invalid.jump_key",
            Self::MissingProxyHost => "connect.invalid.proxy_host",
            Self::InvalidProxyPort => "connect.invalid.proxy_port",
        }
    }

    pub fn is_route(self) -> bool {
        !matches!(
            self,
            Self::MissingHost | Self::InvalidPort | Self::MissingUsername | Self::MissingKey
        )
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
            Field::BasePath(path) => self.base_path = path,
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
            Field::ToggleAdvanced => self.advanced_open = !self.advanced_open,
            Field::Jump(field) => self.jump.apply(field),
            Field::Proxy(field) => self.proxy.apply(field),
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
            self.jump.save_password = false;
            self.jump.secret_from_store = false;
            self.proxy.save_password = false;
            self.proxy.secret_from_store = false;
        }
    }

    fn target(&self) -> (Protocol, String, String, String, AuthMethod, String, String) {
        (
            self.protocol,
            self.host.trim().to_string(),
            self.port.clone(),
            self.username.trim().to_string(),
            self.effective_method(),
            self.key_path.trim().to_string(),
            self.effective_base_path(),
        )
    }

    pub fn effective_base_path(&self) -> String {
        if self.protocol.is_webdav() {
            self.base_path.trim().to_string()
        } else {
            String::new()
        }
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
            base_path: site.options.base_path.clone(),
            username: site.username.clone(),
            method: AuthMethod::from_site_auth(site.effective_auth()),
            key_path: site.key_path.clone(),
            site: Some(site.id.clone()),
            save_password: site.save_secrets,
            ..Self::default()
        };
        self.fill_route(site.jump.as_ref(), site.proxy.as_ref());
    }

    pub fn fill_from_recent(&mut self, recent: &RecentConnection) {
        *self = Self {
            protocol: recent.protocol,
            host: recent.host.clone(),
            port: if recent.port == recent.protocol.default_port() {
                String::new()
            } else {
                recent.port.to_string()
            },
            base_path: recent.base_path.clone(),
            username: recent.username.clone(),
            method: if recent.protocol.supports_key_files() {
                AuthMethod::from_site_auth(recent.auth)
            } else {
                AuthMethod::Password
            },
            key_path: recent.key_path.clone(),
            ..Self::default()
        };
        self.fill_route(recent.jump.as_ref(), recent.proxy.as_ref());
    }

    fn fill_route(&mut self, jump: Option<&SiteJump>, proxy: Option<&SiteProxy>) {
        self.jump = JumpForm::fill(jump);
        self.proxy = ProxyForm::fill(proxy);
        self.advanced_open = self.has_route();
    }

    pub fn set_secret(&mut self, secret: String) {
        match self.effective_method() {
            AuthMethod::Password => self.password = secret,
            AuthMethod::KeyFile => self.passphrase = secret,
            AuthMethod::Agent => {}
        }
    }

    pub fn secret(&self) -> &str {
        match self.effective_method() {
            AuthMethod::Password => &self.password,
            AuthMethod::KeyFile => &self.passphrase,
            AuthMethod::Agent => "",
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
        self.effective_method().site_auth()
    }

    pub fn uses_secret(&self) -> bool {
        self.effective_method() != AuthMethod::Agent
    }

    pub fn clear_secrets(&mut self) {
        self.password.clear();
        self.passphrase.clear();
        self.jump.clear_secrets();
        self.proxy.password.clear();
    }

    // Routes

    pub fn uses_jump(&self) -> bool {
        self.jump.enabled && self.protocol.supports_key_files()
    }

    pub fn has_route(&self) -> bool {
        self.uses_jump() || self.proxy.kind.is_some()
    }

    pub fn jump_settings(&self) -> Result<Option<SiteJump>, FormError> {
        if self.uses_jump() {
            self.jump.to_site_jump().map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn proxy_settings(&self) -> Result<Option<SiteProxy>, FormError> {
        self.proxy.to_site_proxy()
    }

    pub fn to_route(&self) -> Result<Route, FormError> {
        let jump = self.jump_settings()?;
        let proxy = self.proxy_settings()?;
        Ok(Route {
            proxy: proxy.map(|proxy| proxy.to_proxy(Some(self.proxy.password.clone()))),
            jump_hosts: jump
                .map(|jump| jump.to_jump_host(Some(self.jump.secret().to_string())))
                .into_iter()
                .collect(),
        })
    }

    pub fn ready_to_connect(&self) -> bool {
        let main = self.effective_method() != AuthMethod::Password || !self.password.is_empty();
        let jump = !self.uses_jump()
            || self.jump.method != AuthMethod::Password
            || !self.jump.password.is_empty();
        let proxy = self.proxy.kind.is_none()
            || !self.proxy.uses_secret()
            || !self.proxy.password.is_empty();
        main && jump && proxy
    }

    // Validation

    pub fn to_params(&self) -> Result<ConnectionParams, FormError> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err(FormError::MissingHost);
        }
        let port =
            parse_port(&self.port, self.protocol.default_port()).ok_or(FormError::InvalidPort)?;
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
            AuthMethod::Agent => Authentication::Agent,
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
    use crate::ui_tests::test_password;

    fn filled(password: &str) -> ConnectForm {
        let mut form = ConnectForm::default();
        form.apply(Field::Host(" files.example.test ".to_string()));
        form.apply(Field::Username("demo".to_string()));
        form.apply(Field::Password(password.to_string()));
        form
    }

    #[test]
    fn valid_forms_become_params_with_default_port() {
        let password = test_password();
        let params = filled(&password).to_params().expect("form is valid");
        assert_eq!(params.host, "files.example.test");
        assert_eq!(params.port, 22);
        assert_eq!(params.authentication, Authentication::Password(password));
    }

    #[test]
    fn missing_or_invalid_fields_are_reported() {
        assert_eq!(
            ConnectForm::default().to_params(),
            Err(FormError::MissingHost)
        );

        let mut form = filled(&test_password());
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
        let password = test_password();
        let mut form = filled(&password);
        form.apply(Field::Method(AuthMethod::KeyFile));
        form.apply(Field::Protocol(Protocol::FtpsExplicit));
        assert_eq!(form.method, AuthMethod::Password);
        let params = form.to_params().expect("form is valid");
        assert_eq!(params.port, 21);
        assert_eq!(params.authentication, Authentication::Password(password));
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
        let mut form = filled(&test_password());
        form.fill_from_site(&site);
        assert_eq!(form.password, "");
        assert_eq!(form.port, "2222");
        assert_eq!(form.site.as_deref(), Some(site.id.as_str()));
        assert!(form.save_password);
        let passphrase = test_password();
        form.set_secret(passphrase.clone());
        assert_eq!(form.passphrase, passphrase);
        assert_eq!(form.secret(), passphrase);
        assert_eq!(form.site_auth(), SiteAuth::KeyFile);

        site.protocol = Protocol::Ftp;
        site.port = 21;
        form.fill_from_site(&site);
        assert_eq!(form.port, "");
        let password = test_password();
        form.set_secret(password.clone());
        assert_eq!(form.password, password);
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
        form.edit(Field::Password(test_password()));
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
    fn agents_and_routes_become_params() {
        use crate::route_form::{JumpField, ProxyField};
        use hoplix_core::ProxyKind;

        let empty = String::new();
        let mut form = filled(&empty);
        form.apply(Field::Method(AuthMethod::Agent));
        assert_eq!(
            form.to_params().map(|params| params.authentication),
            Ok(Authentication::Agent)
        );
        assert!(form.ready_to_connect());
        assert!(!form.uses_secret());
        assert_eq!(form.to_route().map(|route| route.is_direct()), Ok(true));

        form.apply(Field::Jump(JumpField::Enabled(true)));
        form.apply(Field::Jump(JumpField::Host("bastion.test".to_string())));
        form.apply(Field::Jump(JumpField::Username("hop".to_string())));
        assert!(!form.ready_to_connect());
        let password = test_password();
        form.apply(Field::Jump(JumpField::Password(password.clone())));
        form.apply(Field::Proxy(ProxyField::Kind(Some(ProxyKind::Http))));
        form.apply(Field::Proxy(ProxyField::Host("proxy.test".to_string())));
        form.apply(Field::Proxy(ProxyField::Username("me".to_string())));
        assert!(!form.ready_to_connect());
        form.apply(Field::Proxy(ProxyField::Password(password.clone())));
        assert!(form.ready_to_connect());
        let route = form.to_route().expect("valid");
        assert_eq!(
            route.jump_hosts[0].authentication,
            Authentication::Password(password.clone())
        );
        assert_eq!(
            route
                .proxy
                .and_then(|proxy| proxy.credentials)
                .map(|c| c.password),
            Some(password)
        );

        form.apply(Field::Protocol(Protocol::Ftp));
        assert_eq!(form.effective_method(), AuthMethod::Password);
        assert!(form.to_route().expect("valid").jump_hosts.is_empty());
        form.clear_secrets();
        assert!(form.jump.password.is_empty() && form.proxy.password.is_empty());
        form.apply(Field::Proxy(ProxyField::Port("0".to_string())));
        assert_eq!(
            form.to_route().map(|_| ()),
            Err(FormError::InvalidProxyPort)
        );
        assert!(FormError::InvalidProxyPort.is_route());
        assert!(!FormError::MissingHost.is_route());
    }

    #[test]
    fn port_accepts_digits_only() {
        let mut form = filled(&test_password());
        form.apply(Field::Port("22a".to_string()));
        assert_eq!(form.port, "");
    }

    #[test]
    fn key_files_need_a_path_and_keep_the_passphrase() {
        let mut form = filled(&test_password());
        form.apply(Field::Method(AuthMethod::KeyFile));
        assert_eq!(form.to_params(), Err(FormError::MissingKey));
        form.apply(Field::KeyPath("\"C:\\keys\\id_ed25519\"".to_string()));
        let passphrase = test_password();
        form.apply(Field::Passphrase(passphrase.clone()));
        assert_eq!(
            form.to_params().map(|params| params.authentication),
            Ok(Authentication::KeyFile {
                path: PathBuf::from("C:\\keys\\id_ed25519"),
                passphrase: Some(passphrase),
            })
        );
        form.clear_secrets();
        assert!(form.password.is_empty() && form.passphrase.is_empty());
    }
}
