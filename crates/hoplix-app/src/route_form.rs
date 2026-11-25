use hoplix_core::{JumpHost, ProxyKind, SiteAuth, SiteJump, SiteProxy};

use crate::connect_form::{AuthMethod, FormError};

// Types

#[derive(Debug, Clone)]
pub enum JumpField {
    Enabled(bool),
    Host(String),
    Port(String),
    Username(String),
    Method(AuthMethod),
    Password(String),
    KeyPath(String),
    Passphrase(String),
    SavePassword(bool),
}

#[derive(Debug, Clone)]
pub enum ProxyField {
    Kind(Option<ProxyKind>),
    Host(String),
    Port(String),
    Username(String),
    Password(String),
    SavePassword(bool),
}

#[derive(Debug, Clone, Default)]
pub struct JumpForm {
    pub enabled: bool,
    pub host: String,
    pub port: String,
    pub username: String,
    pub method: AuthMethod,
    pub password: String,
    pub key_path: String,
    pub passphrase: String,
    pub save_password: bool,
    pub secret_from_store: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ProxyForm {
    pub kind: Option<ProxyKind>,
    pub host: String,
    pub port: String,
    pub username: String,
    pub password: String,
    pub save_password: bool,
    pub secret_from_store: bool,
}

// Jump hosts

impl JumpForm {
    pub fn apply(&mut self, field: JumpField) {
        match field {
            JumpField::Enabled(enabled) => self.enabled = enabled,
            JumpField::Host(host) => self.host = host,
            JumpField::Port(port) => {
                if port.chars().all(|c| c.is_ascii_digit()) {
                    self.port = port;
                }
            }
            JumpField::Username(username) => self.username = username,
            JumpField::Method(method) => self.method = method,
            JumpField::Password(password) => {
                self.password = password;
                self.secret_from_store = false;
            }
            JumpField::KeyPath(path) => self.key_path = path,
            JumpField::Passphrase(passphrase) => {
                self.passphrase = passphrase;
                self.secret_from_store = false;
            }
            JumpField::SavePassword(save) => self.save_password = save,
        }
    }

    pub fn fill(jump: Option<&SiteJump>) -> Self {
        let Some(jump) = jump else {
            return Self::default();
        };
        Self {
            enabled: true,
            host: jump.host.clone(),
            port: if jump.port == JumpHost::DEFAULT_PORT {
                String::new()
            } else {
                jump.port.to_string()
            },
            username: jump.username.clone(),
            method: AuthMethod::from_site_auth(jump.auth),
            key_path: jump.key_path.clone(),
            save_password: jump.save_secret,
            ..Self::default()
        }
    }

    pub fn secret(&self) -> &str {
        match self.method {
            AuthMethod::Password => &self.password,
            AuthMethod::KeyFile => &self.passphrase,
            AuthMethod::Agent => "",
        }
    }

    pub fn set_secret(&mut self, secret: String) {
        match self.method {
            AuthMethod::Password => self.password = secret,
            AuthMethod::KeyFile => self.passphrase = secret,
            AuthMethod::Agent => {}
        }
        self.secret_from_store = true;
    }

    pub fn uses_secret(&self) -> bool {
        self.method != AuthMethod::Agent
    }

    pub fn clear_secrets(&mut self) {
        self.password.clear();
        self.passphrase.clear();
    }

    pub fn to_site_jump(&self) -> Result<SiteJump, FormError> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err(FormError::MissingJumpHost);
        }
        let port =
            parse_port(&self.port, JumpHost::DEFAULT_PORT).ok_or(FormError::InvalidJumpPort)?;
        let username = self.username.trim();
        if username.is_empty() {
            return Err(FormError::MissingJumpUsername);
        }
        let key_path = self.key_path.trim().trim_matches('"');
        if self.method == AuthMethod::KeyFile && key_path.is_empty() {
            return Err(FormError::MissingJumpKey);
        }
        Ok(SiteJump {
            host: host.to_string(),
            port,
            username: username.to_string(),
            auth: self.method.site_auth(),
            key_path: if self.method == AuthMethod::KeyFile {
                key_path.to_string()
            } else {
                String::new()
            },
            save_secret: self.save_password && self.uses_secret(),
        })
    }
}

// Proxies

impl ProxyForm {
    pub fn apply(&mut self, field: ProxyField) {
        match field {
            ProxyField::Kind(kind) => self.kind = kind,
            ProxyField::Host(host) => self.host = host,
            ProxyField::Port(port) => {
                if port.chars().all(|c| c.is_ascii_digit()) {
                    self.port = port;
                }
            }
            ProxyField::Username(username) => self.username = username,
            ProxyField::Password(password) => {
                self.password = password;
                self.secret_from_store = false;
            }
            ProxyField::SavePassword(save) => self.save_password = save,
        }
    }

    pub fn fill(proxy: Option<&SiteProxy>) -> Self {
        let Some(proxy) = proxy else {
            return Self::default();
        };
        Self {
            kind: Some(proxy.kind),
            host: proxy.host.clone(),
            port: if proxy.port == proxy.kind.default_port() {
                String::new()
            } else {
                proxy.port.to_string()
            },
            username: proxy.username.clone(),
            save_password: proxy.save_secret,
            ..Self::default()
        }
    }

    pub fn set_secret(&mut self, secret: String) {
        self.password = secret;
        self.secret_from_store = true;
    }

    pub fn uses_secret(&self) -> bool {
        !self.username.trim().is_empty()
    }

    pub fn to_site_proxy(&self) -> Result<Option<SiteProxy>, FormError> {
        let Some(kind) = self.kind else {
            return Ok(None);
        };
        let host = self.host.trim();
        if host.is_empty() {
            return Err(FormError::MissingProxyHost);
        }
        let port =
            parse_port(&self.port, kind.default_port()).ok_or(FormError::InvalidProxyPort)?;
        Ok(Some(SiteProxy {
            kind,
            host: host.to_string(),
            port,
            username: self.username.trim().to_string(),
            save_secret: self.save_password && self.uses_secret(),
        }))
    }
}

// Sign-in methods

impl AuthMethod {
    pub fn from_site_auth(auth: SiteAuth) -> Self {
        match auth {
            SiteAuth::Password => Self::Password,
            SiteAuth::KeyFile => Self::KeyFile,
            SiteAuth::Agent => Self::Agent,
        }
    }

    pub fn site_auth(self) -> SiteAuth {
        match self {
            Self::Password => SiteAuth::Password,
            Self::KeyFile => SiteAuth::KeyFile,
            Self::Agent => SiteAuth::Agent,
        }
    }
}

// Helpers

pub fn parse_port(text: &str, default: u16) -> Option<u16> {
    match text.trim() {
        "" => Some(default),
        text => text.parse::<u16>().ok().filter(|port| *port > 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_tests::test_password;

    #[test]
    fn jump_hosts_are_validated() {
        let mut form = JumpForm {
            enabled: true,
            ..JumpForm::default()
        };
        assert_eq!(form.to_site_jump(), Err(FormError::MissingJumpHost));
        form.apply(JumpField::Host(" bastion.test ".to_string()));
        form.apply(JumpField::Port("0".to_string()));
        assert_eq!(form.to_site_jump(), Err(FormError::InvalidJumpPort));
        form.apply(JumpField::Port("x".to_string()));
        assert_eq!(form.port, "0");
        form.apply(JumpField::Port(String::new()));
        assert_eq!(form.to_site_jump(), Err(FormError::MissingJumpUsername));
        form.apply(JumpField::Username("hop".to_string()));
        form.apply(JumpField::Method(AuthMethod::KeyFile));
        assert_eq!(form.to_site_jump(), Err(FormError::MissingJumpKey));
        form.apply(JumpField::KeyPath("\"C:\\keys\\hop\"".to_string()));
        form.apply(JumpField::SavePassword(true));
        let jump = form.to_site_jump().expect("valid");
        assert_eq!(jump.host, "bastion.test");
        assert_eq!(jump.port, 22);
        assert_eq!(jump.key_path, "C:\\keys\\hop");
        assert!(jump.save_secret);
        assert_eq!(JumpForm::fill(Some(&jump)).key_path, "C:\\keys\\hop");

        form.apply(JumpField::Method(AuthMethod::Agent));
        let jump = form.to_site_jump().expect("valid");
        assert_eq!(jump.auth, SiteAuth::Agent);
        assert!(!jump.save_secret);
        assert_eq!(jump.key_path, "");
    }

    #[test]
    fn jump_secrets_follow_the_method() {
        let mut form = JumpForm::default();
        let password = test_password();
        form.set_secret(password.clone());
        assert_eq!(form.secret(), password);
        assert!(form.secret_from_store);
        form.apply(JumpField::Password(test_password()));
        assert!(!form.secret_from_store);
        form.apply(JumpField::Method(AuthMethod::Agent));
        assert_eq!(form.secret(), "");
        form.clear_secrets();
        assert!(form.password.is_empty());
    }

    #[test]
    fn proxies_get_their_default_port() {
        let mut form = ProxyForm::default();
        assert_eq!(form.to_site_proxy(), Ok(None));
        form.apply(ProxyField::Kind(Some(ProxyKind::Http)));
        assert_eq!(form.to_site_proxy(), Err(FormError::MissingProxyHost));
        form.apply(ProxyField::Host("proxy.test".to_string()));
        let proxy = form.to_site_proxy().expect("valid").expect("proxy");
        assert_eq!(proxy.port, 8080);
        assert!(!proxy.uses_secret());
        form.apply(ProxyField::Port("70000".to_string()));
        assert_eq!(form.to_site_proxy(), Err(FormError::InvalidProxyPort));
        form.apply(ProxyField::Port("3128".to_string()));
        form.apply(ProxyField::Username(" me ".to_string()));
        form.apply(ProxyField::SavePassword(true));
        let proxy = form.to_site_proxy().expect("valid").expect("proxy");
        assert_eq!(proxy.username, "me");
        assert!(proxy.save_secret);
        let filled = ProxyForm::fill(Some(&proxy));
        assert_eq!(filled.port, "3128");
        assert_eq!(filled.kind, Some(ProxyKind::Http));
        assert!(filled.password.is_empty());
    }
}
