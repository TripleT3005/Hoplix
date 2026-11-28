use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::MethodKind;
use russh::client::{self, AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::Instant;

use crate::connection::{Authentication, ConnectionParams, HostAddress, HostKeyTrust};
use crate::keyboard_interactive::{
    self, AuthChannel, Methods, Outcome, PromptField, PromptRequest, Prompter, SharedPrompter,
    SignIn, Step,
};
use crate::route::Route;
use crate::{BoxFuture, ConnectionOptions, Error, ErrorKind, Result, proxy, ssh_agent};

// Constants

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const PROMPT_PAUSE: Duration = Duration::from_secs(24 * 60 * 60);
const MIN_REMAINING: Duration = Duration::from_secs(5);

// Reconnecting

static AUTO_RECONNECT: AtomicBool = AtomicBool::new(true);

pub fn set_auto_reconnect(enabled: bool) {
    AUTO_RECONNECT.store(enabled, Ordering::Relaxed);
}

pub fn auto_reconnect() -> bool {
    AUTO_RECONNECT.load(Ordering::Relaxed)
}

// Types

pub(crate) struct SshTarget {
    pub params: ConnectionParams,
    route: Route,
    options: ConnectionOptions,
    known_hosts: PathBuf,
    accepted_keys: Vec<HostKeyTrust>,
    prompter: Option<SharedPrompter>,
}

pub(crate) struct SshLink {
    pub handle: Handle<HostKeyCheck>,
    jumps: Vec<Handle<HostKeyCheck>>,
}

pub(crate) struct HostKeyCheck {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    trusted: Vec<HostKeyTrust>,
    rejection: Arc<Mutex<Option<Error>>>,
    accepted: Arc<Mutex<Option<String>>>,
}

pub(crate) struct Hop<'a> {
    host: &'a str,
    port: u16,
    username: &'a str,
    authentication: &'a Authentication,
    label: String,
    is_target: bool,
}

struct PausingPrompter {
    inner: SharedPrompter,
    deadline: Arc<Mutex<Instant>>,
}

struct HandleAuth<'a> {
    handle: &'a mut Handle<HostKeyCheck>,
    hop: &'a Hop<'a>,
}

type Slot<T> = Arc<Mutex<Option<T>>>;

// Host keys

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

impl HostKeyCheck {
    fn verify(&self, key: &PublicKey) -> std::result::Result<(), Error> {
        let fingerprint = fingerprint(key);
        match russh::keys::check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(()),
            Err(russh::keys::Error::KeyChanged { .. }) => {
                Err(Error::new(ErrorKind::HostKeyChanged, fingerprint)
                    .at_address(&self.host, self.port))
            }
            Err(error) => Err(Error::new(ErrorKind::Other, error.to_string())),
            Ok(false) => match self
                .trusted
                .iter()
                .find(|trust| trust.applies_to(&self.host, self.port, &fingerprint))
            {
                Some(trust) => {
                    if trust.remember {
                        russh::keys::known_hosts::learn_known_hosts_path(
                            &self.host,
                            self.port,
                            key,
                            &self.known_hosts,
                        )
                        .map_err(|error| Error::new(ErrorKind::Other, error.to_string()))?;
                    }
                    Ok(())
                }
                None => Err(Error::new(ErrorKind::UnknownHostKey, fingerprint)
                    .at_address(&self.host, self.port)),
            },
        }
    }
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let key = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(certificate) => {
                PublicKey::from(certificate.public_key().clone())
            }
        };
        match self.verify(&key) {
            Ok(()) => {
                if let Ok(mut accepted) = self.accepted.lock() {
                    *accepted = Some(fingerprint(&key));
                }
                Ok(true)
            }
            Err(error) => {
                if let Ok(mut rejection) = self.rejection.lock() {
                    *rejection = Some(error);
                }
                Ok(false)
            }
        }
    }
}

// Targets

impl SshTarget {
    pub async fn connect(
        params: &ConnectionParams,
        route: &Route,
        options: &ConnectionOptions,
        known_hosts: &Path,
        trusted: &[HostKeyTrust],
        prompter: Option<SharedPrompter>,
    ) -> Result<(Self, SshLink)> {
        let (link, accepted_keys) = establish(
            params,
            route,
            options,
            known_hosts,
            trusted.to_vec(),
            prompter.clone(),
        )
        .await?;
        let target = Self {
            params: params.clone(),
            route: route.clone(),
            options: options.clone(),
            known_hosts: known_hosts.to_path_buf(),
            accepted_keys,
            prompter,
        };
        Ok((target, link))
    }

    pub async fn reconnect(&self) -> Result<SshLink> {
        establish(
            &self.params,
            &self.route,
            &self.options,
            &self.known_hosts,
            self.accepted_keys.clone(),
            self.prompter.clone(),
        )
        .await
        .map(|(link, _)| link)
    }

    pub fn closed_error(&self) -> Error {
        Error::new(ErrorKind::ConnectionFailed, self.params.host.clone())
    }
}

impl SshLink {
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    pub async fn disconnect(&self) {
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await;
        for jump in self.jumps.iter().rev() {
            let _ = jump
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
        }
    }
}

// Connecting

impl Prompter for PausingPrompter {
    fn answer(&self, request: PromptRequest) -> BoxFuture<'_, Option<Vec<String>>> {
        Box::pin(async move {
            let now = Instant::now();
            let remaining = self
                .deadline
                .lock()
                .map(|mut deadline| {
                    let remaining = deadline.saturating_duration_since(now);
                    *deadline = now + PROMPT_PAUSE;
                    remaining
                })
                .unwrap_or(MIN_REMAINING);
            let answer = self.inner.answer(request).await;
            if let Ok(mut deadline) = self.deadline.lock() {
                *deadline = Instant::now() + remaining.max(MIN_REMAINING);
            }
            answer
        })
    }
}

async fn establish(
    params: &ConnectionParams,
    route: &Route,
    options: &ConnectionOptions,
    known_hosts: &Path,
    trusted: Vec<HostKeyTrust>,
    prompter: Option<SharedPrompter>,
) -> Result<(SshLink, Vec<HostKeyTrust>)> {
    let deadline = Arc::new(Mutex::new(
        Instant::now() + options.timeout_or(CONNECT_TIMEOUT),
    ));
    let prompter = prompter.map(|inner| {
        Arc::new(PausingPrompter {
            inner,
            deadline: Arc::clone(&deadline),
        }) as SharedPrompter
    });
    let mut work = std::pin::pin!(open_connection(
        params,
        route,
        options,
        known_hosts,
        trusted,
        prompter.as_ref()
    ));
    loop {
        let until = deadline
            .lock()
            .map(|deadline| *deadline)
            .unwrap_or_else(|_| Instant::now());
        tokio::select! {
            result = &mut work => return result,
            () = tokio::time::sleep_until(until) => {
                let expired = deadline.lock().map_or(true, |deadline| *deadline <= Instant::now());
                if expired {
                    return Err(Error::new(ErrorKind::ConnectionFailed, params.host.clone()));
                }
            }
        }
    }
}

pub(crate) fn hops<'a>(params: &'a ConnectionParams, route: &'a Route) -> Vec<Hop<'a>> {
    let mut hops: Vec<Hop<'a>> = route
        .jump_hosts
        .iter()
        .map(|jump| Hop {
            host: &jump.host,
            port: jump.port,
            username: &jump.username,
            authentication: &jump.authentication,
            label: jump.label(),
            is_target: false,
        })
        .collect();
    hops.push(Hop {
        host: &params.host,
        port: params.port,
        username: &params.username,
        authentication: &params.authentication,
        label: params.label(),
        is_target: true,
    });
    hops
}

async fn open_connection(
    params: &ConnectionParams,
    route: &Route,
    options: &ConnectionOptions,
    known_hosts: &Path,
    trusted: Vec<HostKeyTrust>,
    prompter: Option<&SharedPrompter>,
) -> Result<(SshLink, Vec<HostKeyTrust>)> {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(INACTIVITY_TIMEOUT),
        keepalive_interval: Some(options.keepalive().unwrap_or(KEEPALIVE_INTERVAL)),
        ..client::Config::default()
    });
    let mut accepted_keys = Vec::new();
    let mut jumps: Vec<Handle<HostKeyCheck>> = Vec::new();
    let mut target = None;
    for hop in hops(params, route) {
        let (check, rejection, accepted) = host_key_check(&hop, known_hosts, &trusted);
        let connected = match jumps.last() {
            None => {
                let stream = proxy::open_tcp(route.proxy.as_ref(), hop.host, hop.port)
                    .await
                    .map_err(|error| at_hop(error, &hop))?;
                start_session(&config, stream, check, &hop, &rejection).await
            }
            Some(previous) => {
                let channel = previous
                    .channel_open_direct_tcpip(hop.host, u32::from(hop.port), "127.0.0.1", 0)
                    .await
                    .map_err(|error| at_hop(connection_error(hop.host, &error), &hop))?;
                start_session(&config, channel.into_stream(), check, &hop, &rejection).await
            }
        };
        let mut handle = connected?;
        if let Some(fingerprint) = accepted.lock().ok().and_then(|mut slot| slot.take()) {
            accepted_keys.push(accepted_key(&hop, fingerprint));
        }
        authenticate(&mut handle, &hop, prompter)
            .await
            .map_err(|error| at_hop(error, &hop))?;
        if hop.is_target {
            target = Some(handle);
        } else {
            jumps.push(handle);
        }
    }
    let Some(handle) = target else {
        return Err(Error::new(ErrorKind::ConnectionFailed, params.host.clone()));
    };
    Ok((SshLink { handle, jumps }, accepted_keys))
}

pub(crate) fn accepted_key(hop: &Hop<'_>, fingerprint: String) -> HostKeyTrust {
    HostKeyTrust {
        address: HostAddress::new(hop.host, hop.port),
        fingerprint,
        remember: false,
    }
}

pub(crate) fn host_key_check(
    hop: &Hop<'_>,
    known_hosts: &Path,
    trusted: &[HostKeyTrust],
) -> (HostKeyCheck, Slot<Error>, Slot<String>) {
    let rejection = Arc::new(Mutex::new(None));
    let accepted = Arc::new(Mutex::new(None));
    let check = HostKeyCheck {
        host: hop.host.to_string(),
        port: hop.port,
        known_hosts: known_hosts.to_path_buf(),
        trusted: trusted.to_vec(),
        rejection: Arc::clone(&rejection),
        accepted: Arc::clone(&accepted),
    };
    (check, rejection, accepted)
}

async fn start_session<S>(
    config: &Arc<client::Config>,
    stream: S,
    check: HostKeyCheck,
    hop: &Hop<'_>,
    rejection: &Slot<Error>,
) -> Result<Handle<HostKeyCheck>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    client::connect_stream(Arc::clone(config), stream, check)
        .await
        .map_err(|error| {
            let rejected = rejection.lock().ok().and_then(|mut slot| slot.take());
            at_hop(
                rejected.unwrap_or_else(|| connection_error(hop.host, &error)),
                hop,
            )
        })
}

fn at_hop(error: Error, hop: &Hop<'_>) -> Error {
    if hop.is_target || error.hop().is_some() {
        error
    } else {
        error.at_hop(hop.label.clone())
    }
}

// Authentication

async fn authenticate(
    handle: &mut Handle<HostKeyCheck>,
    hop: &Hop<'_>,
    prompter: Option<&SharedPrompter>,
) -> Result<()> {
    let password = match hop.authentication {
        Authentication::Password(password) => Some(password.as_str()),
        Authentication::KeyFile { .. } | Authentication::Agent => None,
    };
    let context = SignIn {
        label: &hop.label,
        jump: !hop.is_target,
        password,
        prompter,
    };
    let mut channel = HandleAuth { handle, hop };
    keyboard_interactive::sign_in(&mut channel, &context).await
}

fn methods(set: &russh::MethodSet) -> Methods {
    Methods {
        known: !set.is_empty(),
        password: set.contains(&MethodKind::Password),
        keyboard: set.contains(&MethodKind::KeyboardInteractive),
    }
}

fn outcome(result: &AuthResult) -> Outcome {
    match result {
        AuthResult::Success => Outcome::Success,
        AuthResult::Failure {
            remaining_methods,
            partial_success,
        } => Outcome::Failure {
            methods: methods(remaining_methods),
            partial: *partial_success,
        },
    }
}

fn step(response: KeyboardInteractiveAuthResponse) -> Step {
    match response {
        KeyboardInteractiveAuthResponse::Success => Step::Done(Outcome::Success),
        KeyboardInteractiveAuthResponse::Failure {
            remaining_methods,
            partial_success,
        } => Step::Done(Outcome::Failure {
            methods: methods(&remaining_methods),
            partial: partial_success,
        }),
        KeyboardInteractiveAuthResponse::InfoRequest {
            name,
            instructions,
            prompts,
        } => Step::Ask {
            name,
            instruction: instructions,
            fields: prompts
                .into_iter()
                .map(|prompt| PromptField {
                    text: prompt.prompt,
                    echo: prompt.echo,
                })
                .collect(),
        },
    }
}

impl HandleAuth<'_> {
    fn user(&self) -> String {
        self.hop.username.to_string()
    }

    fn failed(&self, error: &russh::Error) -> Error {
        connection_error(self.hop.host, error)
    }

    async fn primary_outcome(&mut self) -> Result<Outcome> {
        let user = self.user();
        match self.hop.authentication {
            Authentication::KeyFile { path, passphrase } => {
                let key =
                    russh::keys::load_secret_key(path, passphrase.as_deref()).map_err(|error| {
                        Error::new(
                            ErrorKind::InvalidKey,
                            format!("{}: {error}", path.display()),
                        )
                    })?;
                let hash = self
                    .handle
                    .best_supported_rsa_hash()
                    .await
                    .ok()
                    .flatten()
                    .flatten();
                let result = self
                    .handle
                    .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                    .await
                    .map_err(|error| self.failed(&error))?;
                Ok(outcome(&result))
            }
            Authentication::Agent => {
                let result = ssh_agent::authenticate(self.handle, &user, &self.hop.label).await?;
                Ok(outcome(&result))
            }
            Authentication::Password(_) => Ok(Outcome::Failure {
                methods: Methods::default(),
                partial: false,
            }),
        }
    }
}

impl AuthChannel for HandleAuth<'_> {
    fn probe(&mut self) -> BoxFuture<'_, Result<Outcome>> {
        Box::pin(async move {
            let user = self.user();
            let result = self
                .handle
                .authenticate_none(user)
                .await
                .map_err(|error| self.failed(&error))?;
            Ok(outcome(&result))
        })
    }

    fn primary(&mut self) -> BoxFuture<'_, Result<Outcome>> {
        Box::pin(self.primary_outcome())
    }

    fn password(&mut self, password: String) -> BoxFuture<'_, Result<Outcome>> {
        Box::pin(async move {
            let user = self.user();
            let result = self
                .handle
                .authenticate_password(user, password)
                .await
                .map_err(|error| self.failed(&error))?;
            Ok(outcome(&result))
        })
    }

    fn keyboard_start(&mut self) -> BoxFuture<'_, Result<Step>> {
        Box::pin(async move {
            let user = self.user();
            let response = self
                .handle
                .authenticate_keyboard_interactive_start(user, None)
                .await
                .map_err(|error| self.failed(&error))?;
            Ok(step(response))
        })
    }

    fn keyboard_respond(&mut self, answers: Vec<String>) -> BoxFuture<'_, Result<Step>> {
        Box::pin(async move {
            let response = self
                .handle
                .authenticate_keyboard_interactive_respond(answers)
                .await
                .map_err(|error| self.failed(&error))?;
            Ok(step(response))
        })
    }
}

pub(crate) fn connection_error(host: &str, error: &russh::Error) -> Error {
    Error::new(ErrorKind::ConnectionFailed, format!("{host}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JumpHost, Protocol};

    const SHARED_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
    const JUMP: &str = "bastion.example.test";
    const TARGET: &str = "127.0.0.1";

    fn shared_key() -> PublicKey {
        PublicKey::from_openssh(SHARED_KEY).expect("invalid test key")
    }

    fn known_hosts(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-ssh-hops-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        directory.join("known_hosts")
    }

    fn routed() -> (ConnectionParams, Route) {
        let params = ConnectionParams {
            protocol: Protocol::Sftp,
            host: TARGET.to_string(),
            port: 22,
            username: "demo".to_string(),
            authentication: Authentication::Agent,
        };
        let route = Route {
            proxy: None,
            jump_hosts: vec![JumpHost {
                host: JUMP.to_string(),
                port: 22,
                username: "hop".to_string(),
                authentication: Authentication::Agent,
            }],
        };
        (params, route)
    }

    fn verify_hops(known_hosts: &Path, trusted: &[HostKeyTrust]) -> Vec<Result<()>> {
        let (params, route) = routed();
        hops(&params, &route)
            .iter()
            .map(|hop| {
                host_key_check(hop, known_hosts, trusted)
                    .0
                    .verify(&shared_key())
            })
            .collect()
    }

    fn jump_trust(remember: bool) -> HostKeyTrust {
        HostKeyTrust {
            address: HostAddress::new(JUMP, 22),
            fingerprint: fingerprint(&shared_key()),
            remember,
        }
    }

    fn is_known(known_hosts: &Path, host: &str) -> bool {
        russh::keys::check_known_hosts_path(host, 22, &shared_key(), known_hosts)
            .is_ok_and(|known| known)
    }

    // Host keys

    #[test]
    fn a_trusted_jump_host_key_does_not_trust_the_target() {
        let known_hosts = known_hosts("once");
        let results = verify_hops(&known_hosts, &[jump_trust(false)]);
        assert_eq!(results[0], Ok(()));
        let error = results[1].clone().expect_err("the target was trusted");
        assert_eq!(error.kind(), ErrorKind::UnknownHostKey);
        assert_eq!(error.detail(), fingerprint(&shared_key()));
        assert_eq!(error.address(), Some(&HostAddress::new(TARGET, 22)));
        assert!(!known_hosts.exists());

        let target = HostKeyTrust::for_error(&error, false).expect("no trust");
        let results = verify_hops(&known_hosts, &[jump_trust(false), target]);
        assert!(results.iter().all(Result::is_ok), "{results:?}");
    }

    #[test]
    fn remembering_only_learns_the_confirmed_host() {
        let known_hosts = known_hosts("remember");
        let results = verify_hops(&known_hosts, &[jump_trust(true)]);
        assert_eq!(results[0], Ok(()));
        assert_eq!(
            results[1].as_ref().map_err(Error::kind),
            Err(ErrorKind::UnknownHostKey)
        );
        assert!(is_known(&known_hosts, JUMP));
        assert!(!is_known(&known_hosts, TARGET));
        let results = verify_hops(&known_hosts, &[]);
        assert_eq!(results[0], Ok(()));
        assert!(results[1].is_err());
        if let Some(directory) = known_hosts.parent() {
            std::fs::remove_dir_all(directory).expect("cleaning up failed");
        }
    }

    #[test]
    fn reconnecting_keeps_the_trust_of_each_hop() {
        let (params, route) = routed();
        let accepted: Vec<HostKeyTrust> = hops(&params, &route)
            .iter()
            .map(|hop| accepted_key(hop, fingerprint(&shared_key())))
            .collect();
        assert_eq!(
            accepted
                .iter()
                .map(|trust| (trust.address.host.as_str(), trust.remember))
                .collect::<Vec<_>>(),
            [(JUMP, false), (TARGET, false)]
        );
        let known_hosts = known_hosts("reconnect");
        let results = verify_hops(&known_hosts, &accepted);
        assert!(results.iter().all(Result::is_ok), "{results:?}");
        let results = verify_hops(&known_hosts, &accepted[..1]);
        assert!(results[0].is_ok() && results[1].is_err());
        assert!(!known_hosts.exists());
    }

    // Methods

    #[test]
    fn method_sets_and_prompts_are_translated() {
        let set = russh::MethodSet::from(
            [MethodKind::PublicKey, MethodKind::KeyboardInteractive].as_slice(),
        );
        assert_eq!(
            methods(&set),
            Methods {
                known: true,
                password: false,
                keyboard: true,
            }
        );
        assert_eq!(methods(&russh::MethodSet::empty()), Methods::default());
        let asked = step(KeyboardInteractiveAuthResponse::InfoRequest {
            name: "otp".to_string(),
            instructions: "Enter the code".to_string(),
            prompts: vec![russh::client::Prompt {
                prompt: "Code: ".to_string(),
                echo: false,
            }],
        });
        assert_eq!(
            asked,
            Step::Ask {
                name: "otp".to_string(),
                instruction: "Enter the code".to_string(),
                fields: vec![PromptField {
                    text: "Code: ".to_string(),
                    echo: false,
                }],
            }
        );
        assert_eq!(
            step(KeyboardInteractiveAuthResponse::Success),
            Step::Done(Outcome::Success)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn prompts_pause_the_connect_timeout() {
        struct Slow;
        impl Prompter for Slow {
            fn answer(&self, _request: PromptRequest) -> BoxFuture<'_, Option<Vec<String>>> {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    Some(Vec::new())
                })
            }
        }
        let deadline = Arc::new(Mutex::new(Instant::now() + Duration::from_secs(20)));
        let prompter = PausingPrompter {
            inner: Arc::new(Slow),
            deadline: Arc::clone(&deadline),
        };
        let request = PromptRequest {
            host: "demo@example.test".to_string(),
            jump: false,
            name: String::new(),
            instruction: String::new(),
            fields: Vec::new(),
        };
        let started = Instant::now();
        assert_eq!(prompter.answer(request).await, Some(Vec::new()));
        let left = deadline.lock().expect("poisoned").duration_since(started);
        assert_eq!(left, Duration::from_secs(140));
    }
}
