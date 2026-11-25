use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use hoplix_core::{
    Authentication, BoxFuture, ConnectionOptions, ConnectionParams, Entry, Error, ErrorKind,
    FileSystem, HostKeyTrust, LocalFileSystem, Protocol, Reader, Result, TrustStore, Writer,
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

pub fn connections() -> &'static Mutex<Vec<(String, ConnectionOptions)>> {
    static CONNECTIONS: OnceLock<Mutex<Vec<(String, ConnectionOptions)>>> = OnceLock::new();
    CONNECTIONS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn options_used_for(host: &str) -> Option<ConnectionOptions> {
    connections()
        .lock()
        .expect("the connection log is poisoned")
        .iter()
        .rev()
        .find(|(name, _)| name == host)
        .map(|(_, options)| options.clone())
}

pub fn fingerprint(host: &str) -> String {
    format!("SHA256:fake-{host}")
}

pub fn connector(remote: PathBuf) -> Connector {
    Arc::new(move |params, options, trust_store, trust| {
        let remote = remote.clone();
        Box::pin(async move { connect(&remote, &params, &options, &trust_store, trust) })
    })
}

fn connect(
    remote: &Path,
    params: &ConnectionParams,
    options: &ConnectionOptions,
    trust_store: &TrustStore,
    trust: Option<HostKeyTrust>,
) -> Result<Arc<dyn FileSystem>> {
    if params.host.starts_with("offline.") {
        return Err(Error::new(ErrorKind::ConnectionFailed, params.host.clone()));
    }
    connections()
        .lock()
        .expect("the connection log is poisoned")
        .push((params.host.clone(), options.clone()));
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
        let host = format!("{}:{}", params.host, params.port);
        let offered = fingerprint(&params.host);
        let saved = fs::read_to_string(file).unwrap_or_default();
        let entry = saved.lines().find_map(|line| {
            line.split_once(' ')
                .filter(|(name, _)| *name == host)
                .map(|(_, value)| value.to_string())
        });
        match entry {
            Some(value) if value == offered => {}
            Some(_) => return Err(Error::new(changed, host)),
            None => match trust {
                Some(trust) if trust.fingerprint == offered => {
                    if trust.remember {
                        fs::write(file, format!("{saved}{host} {offered}\n"))
                            .map_err(|error| Error::from_io(&error, &host))?;
                    }
                }
                _ => return Err(Error::new(unknown, offered)),
            },
        }
    }
    let accepted = match &params.authentication {
        Authentication::Password(offered) => offered == password(),
        Authentication::KeyFile { path, .. } => path.is_file(),
    };
    if !accepted {
        return Err(Error::new(ErrorKind::AuthenticationFailed, params.label()));
    }
    Ok(Folder::remote(remote, params.label()))
}
