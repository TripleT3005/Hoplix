use std::sync::Arc;

use crate::connection::{Authentication, ConnectionParams, HostKeyTrust, Protocol};
use crate::proxy::Proxy;
use crate::{
    ConnectionOptions, Error, ErrorKind, FileSystem, FtpFileSystem, Result, S3FileSystem,
    SftpFileSystem, TrustStore, WebDavFileSystem,
};

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpHost {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub authentication: Authentication,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Route {
    pub proxy: Option<Proxy>,
    pub jump_hosts: Vec<JumpHost>,
}

// Jump hosts

impl JumpHost {
    pub const DEFAULT_PORT: u16 = 22;

    pub fn label(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == Self::DEFAULT_PORT {
            format!("{}@{host}", self.username)
        } else {
            format!("{}@{host}:{}", self.username, self.port)
        }
    }
}

// Routes

impl Route {
    pub fn is_direct(&self) -> bool {
        self.proxy.is_none() && self.jump_hosts.is_empty()
    }
}

// Connecting

pub async fn connect_via(
    params: &ConnectionParams,
    route: &Route,
    trust_store: &TrustStore,
    trusted: &[HostKeyTrust],
) -> Result<Arc<dyn FileSystem>> {
    connect_via_with(
        params,
        route,
        &ConnectionOptions::default(),
        trust_store,
        trusted,
    )
    .await
}

pub async fn connect_via_with(
    params: &ConnectionParams,
    route: &Route,
    options: &ConnectionOptions,
    trust_store: &TrustStore,
    trusted: &[HostKeyTrust],
) -> Result<Arc<dyn FileSystem>> {
    match params.protocol {
        Protocol::Sftp => {
            let file_system = SftpFileSystem::connect_via_with(
                params,
                route,
                options,
                &trust_store.known_hosts,
                trusted,
            )
            .await?;
            Ok(Arc::new(file_system))
        }
        Protocol::Ftp | Protocol::FtpsExplicit | Protocol::FtpsImplicit => {
            if !route.jump_hosts.is_empty() {
                return Err(Error::new(
                    ErrorKind::Other,
                    "jump hosts only work with SFTP",
                ));
            }
            let file_system = FtpFileSystem::connect_via_with(
                params,
                route.proxy.as_ref(),
                options,
                &trust_store.certificates,
                trusted
                    .iter()
                    .rev()
                    .find(|trust| trust.address.is(&params.host, params.port))
                    .cloned(),
            )
            .await?;
            Ok(Arc::new(file_system))
        }
        Protocol::WebDav | Protocol::WebDavs => {
            if !route.jump_hosts.is_empty() {
                return Err(Error::new(
                    ErrorKind::Other,
                    "jump hosts only work with SFTP",
                ));
            }
            let file_system = WebDavFileSystem::connect(
                params,
                route.proxy.as_ref(),
                options,
                &trust_store.certificates,
                trusted
                    .iter()
                    .rev()
                    .find(|trust| trust.address.is(&params.host, params.port))
                    .cloned(),
            )
            .await?;
            Ok(Arc::new(file_system))
        }
        Protocol::S3 => {
            if !route.jump_hosts.is_empty() {
                return Err(Error::new(
                    ErrorKind::Other,
                    "jump hosts only work with SFTP",
                ));
            }
            let host = if params.host.trim().is_empty() {
                options.s3.default_endpoint()
            } else {
                params.host.clone()
            };
            let file_system = S3FileSystem::connect(
                params,
                route.proxy.as_ref(),
                options,
                &trust_store.certificates,
                trusted
                    .iter()
                    .rev()
                    .find(|trust| trust.address.port == params.port)
                    .filter(|trust| trust.address.host.ends_with(&host))
                    .cloned(),
            )
            .await?;
            Ok(Arc::new(file_system))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::{ProxyCredentials, ProxyKind};
    use crate::test_support::test_password;

    fn jump(port: u16, authentication: Authentication) -> JumpHost {
        JumpHost {
            host: "bastion.test".to_string(),
            port,
            username: "hop".to_string(),
            authentication,
        }
    }

    #[test]
    fn jump_host_labels_hide_the_default_port() {
        assert_eq!(jump(22, Authentication::Agent).label(), "hop@bastion.test");
        assert_eq!(
            jump(2222, Authentication::Agent).label(),
            "hop@bastion.test:2222"
        );
        let mut ipv6 = jump(22, Authentication::Agent);
        ipv6.host = "fe80::1".to_string();
        assert_eq!(ipv6.label(), "hop@[fe80::1]");
    }

    #[test]
    fn routes_hide_all_secrets_in_debug_output() {
        let jump_password = test_password();
        let proxy_password = test_password();
        let route = Route {
            proxy: Some(Proxy {
                kind: ProxyKind::Http,
                host: "proxy.test".to_string(),
                port: 3128,
                credentials: Some(ProxyCredentials {
                    username: "user".to_string(),
                    password: proxy_password.clone(),
                }),
            }),
            jump_hosts: vec![jump(22, Authentication::Password(jump_password.clone()))],
        };
        let output = format!("{route:?}");
        assert!(!output.contains(&jump_password));
        assert!(!output.contains(&proxy_password));
        assert!(output.contains("bastion.test") && output.contains("proxy.test"));
        assert!(!route.is_direct());
        assert!(Route::default().is_direct());
    }

    #[tokio::test]
    async fn ftp_refuses_jump_hosts() {
        let params = ConnectionParams {
            protocol: Protocol::Ftp,
            host: "127.0.0.1".to_string(),
            port: 9,
            username: "demo".to_string(),
            authentication: Authentication::Password(test_password()),
        };
        let route = Route {
            proxy: None,
            jump_hosts: vec![jump(22, Authentication::Agent)],
        };
        let directory = std::env::temp_dir();
        let trust_store = TrustStore {
            known_hosts: directory.join("hoplix-route-known-hosts"),
            certificates: directory.join("hoplix-route-certificates"),
        };
        let error = connect_via(&params, &route, &trust_store, &[])
            .await
            .expect_err("refused");
        assert_eq!(error.detail(), "jump hosts only work with SFTP");
    }
}
