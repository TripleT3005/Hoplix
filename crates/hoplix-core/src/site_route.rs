use std::path::PathBuf;

use crate::key_value::Section;
use crate::{Authentication, JumpHost, Proxy, ProxyCredentials, ProxyKind, SiteAuth};

// Types

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SiteJump {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SiteAuth,
    pub key_path: String,
    pub save_secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SiteProxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub save_secret: bool,
}

// Sign-in methods

impl SiteAuth {
    pub fn key(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::KeyFile => "key",
            Self::Agent => "agent",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "key" => Self::KeyFile,
            "agent" => Self::Agent,
            _ => Self::Password,
        }
    }

    pub fn authentication(self, key_path: &str, secret: Option<String>) -> Authentication {
        match self {
            Self::Password => Authentication::Password(secret.unwrap_or_default()),
            Self::KeyFile => Authentication::KeyFile {
                path: PathBuf::from(key_path),
                passphrase: secret.filter(|value| !value.is_empty()),
            },
            Self::Agent => Authentication::Agent,
        }
    }
}

// Jump hosts

impl SiteJump {
    pub fn uses_secret(&self) -> bool {
        self.auth != SiteAuth::Agent
    }

    pub fn to_jump_host(&self, secret: Option<String>) -> JumpHost {
        JumpHost {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            authentication: self.auth.authentication(&self.key_path, secret),
        }
    }

    pub fn label(&self) -> String {
        self.to_jump_host(None).label()
    }

    pub(crate) fn from_section(section: &Section) -> Option<Self> {
        let host = section.get("jump_host").filter(|host| !host.is_empty())?;
        let auth = SiteAuth::from_key(section.get("jump_auth").unwrap_or_default());
        Some(Self {
            host: host.to_string(),
            port: port_of(section, "jump_port").unwrap_or(JumpHost::DEFAULT_PORT),
            username: section.get("jump_username").unwrap_or_default().to_string(),
            auth,
            key_path: if auth == SiteAuth::KeyFile {
                section.get("jump_key_path").unwrap_or_default().to_string()
            } else {
                String::new()
            },
            save_secret: section.get("jump_save_secret") == Some("true"),
        })
    }

    pub(crate) fn pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("jump_host", self.host.clone()),
            ("jump_port", self.port.to_string()),
            ("jump_username", self.username.clone()),
            ("jump_auth", self.auth.key().to_string()),
            ("jump_key_path", self.key_path.clone()),
            ("jump_save_secret", self.save_secret.to_string()),
        ]
    }
}

// Proxies

impl SiteProxy {
    pub fn uses_secret(&self) -> bool {
        !self.username.is_empty()
    }

    pub fn to_proxy(&self, secret: Option<String>) -> Proxy {
        Proxy {
            kind: self.kind,
            host: self.host.clone(),
            port: self.port,
            credentials: self.uses_secret().then(|| ProxyCredentials {
                username: self.username.clone(),
                password: secret.unwrap_or_default(),
            }),
        }
    }

    pub fn label(&self) -> String {
        self.to_proxy(None).label()
    }

    pub(crate) fn from_section(section: &Section) -> Option<Self> {
        let kind = section
            .get("proxy")
            .and_then(|scheme| ProxyKind::ALL.into_iter().find(|k| k.scheme() == scheme))?;
        let host = section.get("proxy_host").filter(|host| !host.is_empty())?;
        Some(Self {
            kind,
            host: host.to_string(),
            port: port_of(section, "proxy_port").unwrap_or(kind.default_port()),
            username: section
                .get("proxy_username")
                .unwrap_or_default()
                .to_string(),
            save_secret: section.get("proxy_save_secret") == Some("true"),
        })
    }

    pub(crate) fn pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("proxy", self.kind.scheme().to_string()),
            ("proxy_host", self.host.clone()),
            ("proxy_port", self.port.to_string()),
            ("proxy_username", self.username.clone()),
            ("proxy_save_secret", self.save_secret.to_string()),
        ]
    }
}

// Helpers

fn port_of(section: &Section, key: &str) -> Option<u16> {
    section
        .get(key)
        .and_then(|port| port.parse().ok())
        .filter(|port| *port > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_value;
    use crate::test_support::test_password;

    fn section(text: &str) -> Section {
        key_value::parse_sections(&format!("[site]\n{text}"))
            .pop()
            .expect("one section")
    }

    #[test]
    fn jump_hosts_build_their_sign_in() {
        let mut jump = SiteJump {
            host: "bastion.test".to_string(),
            port: 22,
            username: "hop".to_string(),
            auth: SiteAuth::KeyFile,
            key_path: "C:\\keys\\hop".to_string(),
            save_secret: true,
        };
        let passphrase = test_password();
        assert_eq!(
            jump.to_jump_host(Some(passphrase.clone())).authentication,
            Authentication::KeyFile {
                path: PathBuf::from("C:\\keys\\hop"),
                passphrase: Some(passphrase),
            }
        );
        assert_eq!(jump.label(), "hop@bastion.test");
        assert!(jump.uses_secret());
        jump.auth = SiteAuth::Agent;
        assert!(!jump.uses_secret());
        assert_eq!(
            jump.to_jump_host(Some(test_password())).authentication,
            Authentication::Agent
        );
    }

    #[test]
    fn proxies_only_sign_in_with_a_user_name() {
        let mut proxy = SiteProxy {
            kind: ProxyKind::Http,
            host: "proxy.test".to_string(),
            port: 3128,
            username: String::new(),
            save_secret: false,
        };
        assert_eq!(proxy.to_proxy(Some(test_password())).credentials, None);
        proxy.username = "me".to_string();
        let password = test_password();
        let built = proxy.to_proxy(Some(password.clone()));
        assert_eq!(
            built.credentials,
            Some(ProxyCredentials {
                username: "me".to_string(),
                password,
            })
        );
        assert_eq!(proxy.label(), "http://proxy.test:3128");
    }

    #[test]
    fn broken_route_entries_fall_back_to_defaults() {
        assert_eq!(SiteJump::from_section(&section("jump_host =\n")), None);
        let jump = SiteJump::from_section(&section(
            "jump_host = b\njump_port = x\njump_auth = magic\njump_key_path = k\n",
        ))
        .expect("jump host");
        assert_eq!(jump.port, 22);
        assert_eq!(jump.auth, SiteAuth::Password);
        assert_eq!(jump.key_path, "");
        assert!(!jump.save_secret);
        assert_eq!(
            SiteProxy::from_section(&section("proxy = ftp\nproxy_host = p\n")),
            None
        );
        let proxy =
            SiteProxy::from_section(&section("proxy = socks5\nproxy_host = p\n")).expect("proxy");
        assert_eq!(proxy.port, 1080);
        assert_eq!(proxy.username, "");
    }
}
