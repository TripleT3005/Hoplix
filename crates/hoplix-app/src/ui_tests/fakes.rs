use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use hoplix_core::{
    AGENT_MISSING, Authentication, BoxFuture, ConnectionOptions, ConnectionParams, Entry, Error,
    ErrorKind, FileSystem, FtpMode, HostKeyTrust, LocalFileSystem, Properties, Protocol, Reader,
    Result, Route, TrustStore, Writer, ensure_passive_mode,
};

use super::test_password;
use crate::app::Connector;
use crate::edits::Opener;
use crate::updates::UpdateSource;

// Credentials

pub fn password() -> &'static str {
    static PASSWORD: OnceLock<String> = OnceLock::new();
    PASSWORD.get_or_init(test_password)
}

// Permissions

pub const FAKE_OWNER: &str = "demo";
pub const FAKE_GROUP: &str = "staff";

pub fn permissions() -> &'static Mutex<Vec<(String, u32)>> {
    static PERMISSIONS: OnceLock<Mutex<Vec<(String, u32)>>> = OnceLock::new();
    PERMISSIONS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn permission_of(path: &str) -> Option<u32> {
    permissions()
        .lock()
        .expect("the permission log is poisoned")
        .iter()
        .rev()
        .find(|(changed, _)| changed == path)
        .map(|(_, mode)| *mode)
}

// Local file system

#[derive(Debug)]
pub struct Folder {
    home: PathBuf,
    label: Option<String>,
    inner: LocalFileSystem,
}

impl Folder {
    pub fn local(home: &Path) -> Arc<dyn FileSystem> {
        Arc::new(Self {
            home: home.to_path_buf(),
            label: None,
            inner: LocalFileSystem,
        })
    }

    pub fn remote(home: &Path, label: String) -> Arc<dyn FileSystem> {
        Arc::new(Self {
            home: home.to_path_buf(),
            label: Some(label),
            inner: LocalFileSystem,
        })
    }
}

impl FileSystem for Folder {
    fn label(&self) -> Option<String> {
        self.label.clone()
    }

    fn home(&self) -> BoxFuture<'_, Result<String>> {
        let home = self.home.to_string_lossy().into_owned();
        Box::pin(async move { Ok(home) })
    }

    fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
        self.inner.list(path)
    }

    fn metadata(&self, path: &str) -> BoxFuture<'_, Result<Entry>> {
        self.inner.metadata(path)
    }

    fn create_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        self.inner.create_directory(path)
    }

    fn remove_file(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        self.inner.remove_file(path)
    }

    fn remove_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
        self.inner.remove_directory(path)
    }

    fn rename(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        self.inner.rename(from, to)
    }

    fn open_read(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Reader>> {
        self.inner.open_read(path, offset)
    }

    fn open_write(&self, path: &str, offset: u64) -> BoxFuture<'_, Result<Writer>> {
        self.inner.open_write(path, offset)
    }

    fn set_modified(&self, path: &str, modified: SystemTime) -> BoxFuture<'_, Result<()>> {
        self.inner.set_modified(path, modified)
    }

    fn properties(&self, path: &str) -> BoxFuture<'_, Result<Properties>> {
        if self.label.is_none() {
            return self.inner.properties(path);
        }
        let path = path.to_string();
        Box::pin(async move {
            let entry = self.inner.metadata(&path).await?;
            let default = if entry.is_directory() { 0o755 } else { 0o644 };
            let mut properties = Properties::from_entry(entry);
            properties.permissions = Some(permission_of(&path).unwrap_or(default));
            properties.owner = Some(FAKE_OWNER.to_string());
            properties.group = Some(FAKE_GROUP.to_string());
            Ok(properties)
        })
    }

    fn can_set_permissions(&self) -> bool {
        self.label.is_some() || self.inner.can_set_permissions()
    }

    fn set_permissions(&self, path: &str, mode: u32) -> BoxFuture<'_, Result<()>> {
        if self.label.is_none() {
            return self.inner.set_permissions(path, mode);
        }
        permissions()
            .lock()
            .expect("the permission log is poisoned")
            .push((path.to_string(), mode));
        Box::pin(async { Ok(()) })
    }

    fn join(&self, base: &str, name: &str) -> String {
        self.inner.join(base, name)
    }

    fn parent(&self, path: &str) -> Option<String> {
        self.inner.parent(path)
    }
}

// Opener

pub fn opener(opened: Arc<Mutex<Vec<(PathBuf, String)>>>) -> Opener {
    Arc::new(move |path, editor| {
        opened
            .lock()
            .expect("the opener list is poisoned")
            .push((path.to_path_buf(), editor.to_string()));
        Ok(())
    })
}

// Updates

pub struct Updates {
    requests: AtomicUsize,
    response: Mutex<Result<String>>,
}

impl Updates {
    pub fn up_to_date() -> Arc<Self> {
        Self::offering(&["0.0.1"])
    }

    pub fn offering(versions: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            requests: AtomicUsize::new(0),
            response: Mutex::new(Ok(releases(versions))),
        })
    }

    pub fn respond(&self, response: Result<String>) {
        *self
            .response
            .lock()
            .expect("the update response is poisoned") = response;
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

pub fn release_page(version: &str) -> String {
    format!("https://github.com/TripleT3005/Hoplix/releases/tag/v{version}")
}

pub fn releases(versions: &[&str]) -> String {
    let items: Vec<String> = versions
        .iter()
        .map(|version| {
            format!(
                r#"{{"html_url": "{}", "tag_name": "v{version}", "draft": false, "prerelease": true}}"#,
                release_page(version)
            )
        })
        .collect();
    format!("[{}]", items.join(", "))
}

pub fn update_source(updates: Arc<Updates>) -> UpdateSource {
    Arc::new(move || {
        updates.requests.fetch_add(1, Ordering::SeqCst);
        let response = updates
            .response
            .lock()
            .expect("the update response is poisoned")
            .clone();
        Box::pin(async move { response })
    })
}

// Server

pub const AGENT_USER: &str = "agent-user";
pub const SHARED_KEY_DOMAIN: &str = ".shared-key.test";

pub struct Logged {
    pub remote: PathBuf,
    pub host: String,
    pub options: ConnectionOptions,
    pub route: Route,
}

pub fn connections() -> &'static Mutex<Vec<Logged>> {
    static CONNECTIONS: OnceLock<Mutex<Vec<Logged>>> = OnceLock::new();
    CONNECTIONS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn options_used_for(host: &str) -> Option<ConnectionOptions> {
    connections()
        .lock()
        .expect("the connection log is poisoned")
        .iter()
        .rev()
        .find(|logged| logged.host == host)
        .map(|logged| logged.options.clone())
}

pub fn route_used_by(remote: &Path) -> Option<Route> {
    connections()
        .lock()
        .expect("the connection log is poisoned")
        .iter()
        .rev()
        .find(|logged| logged.remote == remote)
        .map(|logged| logged.route.clone())
}

pub fn fingerprint(host: &str) -> String {
    if host.ends_with(SHARED_KEY_DOMAIN) {
        format!("SHA256:fake-shared{SHARED_KEY_DOMAIN}")
    } else {
        format!("SHA256:fake-{host}")
    }
}

pub fn connector(remote: PathBuf) -> Connector {
    Arc::new(move |params, route, options, trust_store, trusted| {
        let remote = remote.clone();
        Box::pin(async move { connect(&remote, &params, &route, &options, &trust_store, &trusted) })
    })
}

fn connect(
    remote: &Path,
    params: &ConnectionParams,
    route: &Route,
    options: &ConnectionOptions,
    trust_store: &TrustStore,
    trusted: &[HostKeyTrust],
) -> Result<Arc<dyn FileSystem>> {
    if let Some(proxy) = &route.proxy {
        ensure_passive_mode(Some(proxy), options.ftp_mode == FtpMode::Active)?;
        let failed = |kind: ErrorKind, detail: &str| {
            Error::new(kind, format!("proxy {}: {detail}", proxy.label())).at_hop(proxy.label())
        };
        if proxy.host.starts_with("offline.") {
            return Err(failed(ErrorKind::ConnectionFailed, "connection refused"));
        }
        if let Some(credentials) = &proxy.credentials
            && credentials.password != password()
        {
            return Err(failed(
                ErrorKind::AuthenticationFailed,
                "the user name or password was rejected",
            ));
        }
    }
    for jump in &route.jump_hosts {
        let label = jump.label();
        if jump.host.starts_with("offline.") {
            return Err(Error::new(ErrorKind::ConnectionFailed, jump.host.clone()).at_hop(label));
        }
        check_host_key(
            &trust_store.known_hosts,
            &jump.host,
            jump.port,
            trusted,
            ErrorKind::UnknownHostKey,
            ErrorKind::HostKeyChanged,
        )
        .map_err(|error| error.at_hop(label.clone()))?;
        sign_in(&jump.authentication, &jump.username, &label)
            .map_err(|error| error.at_hop(label.clone()))?;
    }
    if params.host.starts_with("offline.") {
        return Err(Error::new(ErrorKind::ConnectionFailed, params.host.clone()));
    }
    connections()
        .lock()
        .expect("the connection log is poisoned")
        .push(Logged {
            remote: remote.to_path_buf(),
            host: params.host.clone(),
            options: options.clone(),
            route: route.clone(),
        });
    let known = match params.protocol {
        Protocol::Sftp => Some((
            &trust_store.known_hosts,
            ErrorKind::UnknownHostKey,
            ErrorKind::HostKeyChanged,
        )),
        Protocol::FtpsExplicit | Protocol::FtpsImplicit => Some((
            &trust_store.certificates,
            ErrorKind::UntrustedCertificate,
            ErrorKind::CertificateChanged,
        )),
        Protocol::Ftp => None,
    };
    if let Some((file, unknown, changed)) = known {
        check_host_key(file, &params.host, params.port, trusted, unknown, changed)?;
    }
    sign_in(&params.authentication, &params.username, &params.label())?;
    Ok(Folder::remote(remote, params.label()))
}

fn check_host_key(
    file: &Path,
    host: &str,
    port: u16,
    trusted: &[HostKeyTrust],
    unknown: ErrorKind,
    changed: ErrorKind,
) -> Result<()> {
    let address = format!("{host}:{port}");
    let offered = fingerprint(host);
    let saved = fs::read_to_string(file).unwrap_or_default();
    let entry = saved.lines().find_map(|line| {
        line.split_once(' ')
            .filter(|(name, _)| *name == address)
            .map(|(_, value)| value.to_string())
    });
    match entry {
        Some(value) if value == offered => Ok(()),
        Some(_) => Err(Error::new(changed, address)),
        None => match trusted
            .iter()
            .find(|trust| trust.applies_to(host, port, &offered))
        {
            Some(trust) => {
                if trust.remember {
                    fs::write(file, format!("{saved}{address} {offered}\n"))
                        .map_err(|error| Error::from_io(&error, &address))?;
                }
                Ok(())
            }
            None => Err(Error::new(unknown, offered).at_address(host, port)),
        },
    }
}

fn sign_in(authentication: &Authentication, username: &str, label: &str) -> Result<()> {
    let accepted = match authentication {
        Authentication::Password(offered) => offered == password(),
        Authentication::KeyFile { path, .. } => path.is_file(),
        Authentication::Agent if username == AGENT_USER => true,
        Authentication::Agent => {
            return Err(Error::new(
                ErrorKind::AuthenticationFailed,
                format!("{label}: {AGENT_MISSING}"),
            ));
        }
    };
    if accepted {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::AuthenticationFailed,
            label.to_string(),
        ))
    }
}
