use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use hoplix_core::{
    Authentication, BoxFuture, ConnectionOptions, ConnectionParams, Error, ErrorKind, FileSystem,
    HostKeyTrust, JumpHost, PROMPT_UNAVAILABLE, PromptRequest, Prompter, Protocol, Route,
    SharedPrompter, TrustStore, connect_via_prompting, is_cancelled, is_password_prompt,
};
use ring::hmac;

// Setup

fn variable(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("{name} is not set"))
}

fn port(name: &str) -> u16 {
    variable(name).parse().expect("invalid port")
}

fn key() -> Authentication {
    Authentication::KeyFile {
        path: PathBuf::from(variable("HOPLIX_TEST_KI_KEY")),
        passphrase: None,
    }
}

fn params(protocol: Protocol, port: u16, authentication: Authentication) -> ConnectionParams {
    ConnectionParams {
        protocol,
        host: variable("HOPLIX_TEST_KI_HOST"),
        port,
        username: variable("HOPLIX_TEST_KI_USER"),
        authentication,
    }
}

fn trust_store(name: &str) -> TrustStore {
    let directory =
        std::env::temp_dir().join(format!("hoplix-ki-trust-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    TrustStore::in_directory(&directory)
}

// One-time codes

fn base32(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bits = 0u64;
    let mut count = 0;
    let mut bytes = Vec::new();
    for character in text.bytes().filter(|byte| *byte != b'=') {
        let value = ALPHABET
            .iter()
            .position(|letter| *letter == character.to_ascii_uppercase())
            .expect("not base32") as u64;
        bits = bits << 5 | value;
        count += 5;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
        }
    }
    bytes
}

fn current_code() -> String {
    let secret = base32(&variable("HOPLIX_TEST_KI_TOTP"));
    let step = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs()
        / 30;
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &secret);
    let tag = hmac::sign(&key, &step.to_be_bytes());
    let digest = tag.as_ref();
    let offset = usize::from(digest[19] & 0x0f);
    let value = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    format!("{:06}", value % 1_000_000)
}

// Prompters

#[derive(Default)]
struct Authenticator {
    cancel: bool,
    requests: Mutex<Vec<PromptRequest>>,
}

impl Authenticator {
    fn shared(cancel: bool) -> Arc<Self> {
        Arc::new(Self {
            cancel,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<PromptRequest> {
        self.requests.lock().expect("poisoned").clone()
    }
}

impl Prompter for Authenticator {
    fn answer(&self, request: PromptRequest) -> BoxFuture<'_, Option<Vec<String>>> {
        let answers = request
            .fields
            .iter()
            .map(|field| {
                if is_password_prompt(&field.text) {
                    variable("HOPLIX_TEST_KI_PASSWORD")
                } else {
                    current_code()
                }
            })
            .collect();
        self.requests.lock().expect("poisoned").push(request);
        let cancel = self.cancel;
        Box::pin(async move { (!cancel).then_some(answers) })
    }
}

async fn open(
    params: &ConnectionParams,
    route: &Route,
    trust_store: &TrustStore,
    prompter: Option<SharedPrompter>,
) -> Result<Arc<dyn FileSystem>, Error> {
    let options = ConnectionOptions::default();
    let mut trusted: Vec<HostKeyTrust> = Vec::new();
    loop {
        match connect_via_prompting(
            params,
            route,
            &options,
            trust_store,
            &trusted,
            prompter.clone(),
        )
        .await
        {
            Err(error) if error.kind() == ErrorKind::UnknownHostKey && trusted.len() < 3 => {
                trusted.push(HostKeyTrust::for_error(&error, false).expect("no trust"));
            }
            result => return result,
        }
    }
}

async fn browse(file_system: &dyn FileSystem) {
    let home = file_system.home().await.expect("home failed");
    file_system.list(&home).await.expect("listing failed");
    file_system.close().await;
}

// Flows

#[tokio::test]
#[ignore = "needs an sshd with keyboard-interactive, see HOPLIX_TEST_KI_* variables"]
async fn a_key_and_a_one_time_code_sign_in_for_sftp_and_scp() {
    for protocol in [Protocol::Sftp, Protocol::Scp] {
        let params = params(protocol, port("HOPLIX_TEST_KI_KEY_PORT"), key());
        let authenticator = Authenticator::shared(false);
        let prompter = Arc::clone(&authenticator) as SharedPrompter;
        let remote = open(
            &params,
            &Route::default(),
            &trust_store("key"),
            Some(prompter),
        )
        .await
        .expect("signing in failed");
        browse(remote.as_ref()).await;
        let requests = authenticator.requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert!(!requests[0].jump);
        assert_eq!(requests[0].host, params.label());
        assert_eq!(requests[0].fields.len(), 1);
        assert!(!requests[0].fields[0].echo);
        assert!(!is_password_prompt(&requests[0].fields[0].text));
    }
}

#[tokio::test]
#[ignore = "needs an sshd with keyboard-interactive, see HOPLIX_TEST_KI_* variables"]
async fn the_saved_password_answers_the_password_prompt() {
    let password = Authentication::Password(variable("HOPLIX_TEST_KI_PASSWORD"));
    let params = params(
        Protocol::Sftp,
        port("HOPLIX_TEST_KI_PASSWORD_PORT"),
        password,
    );
    let authenticator = Authenticator::shared(false);
    let prompter = Arc::clone(&authenticator) as SharedPrompter;
    let remote = open(
        &params,
        &Route::default(),
        &trust_store("saved"),
        Some(prompter),
    )
    .await
    .expect("signing in failed");
    browse(remote.as_ref()).await;
    let requests = authenticator.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(!is_password_prompt(&requests[0].fields[0].text));

    let empty = Authentication::Password(String::new());
    let params = self::params(Protocol::Scp, port("HOPLIX_TEST_KI_PASSWORD_PORT"), empty);
    let authenticator = Authenticator::shared(false);
    let prompter = Arc::clone(&authenticator) as SharedPrompter;
    let remote = open(
        &params,
        &Route::default(),
        &trust_store("typed"),
        Some(prompter),
    )
    .await
    .expect("signing in failed");
    browse(remote.as_ref()).await;
    let requests = authenticator.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(is_password_prompt(&requests[0].fields[0].text));
}

#[tokio::test]
#[ignore = "needs an sshd with keyboard-interactive, see HOPLIX_TEST_KI_* variables"]
async fn cancelling_and_missing_prompters_fail_clearly() {
    let params = params(Protocol::Sftp, port("HOPLIX_TEST_KI_KEY_PORT"), key());
    let authenticator = Authenticator::shared(true);
    let prompter = Arc::clone(&authenticator) as SharedPrompter;
    let error = open(
        &params,
        &Route::default(),
        &trust_store("cancel"),
        Some(prompter),
    )
    .await
    .expect_err("signed in");
    assert!(is_cancelled(&error), "{error:?}");
    assert_eq!(authenticator.requests().len(), 1);

    let error = open(&params, &Route::default(), &trust_store("none"), None)
        .await
        .expect_err("signed in");
    assert_eq!(error.kind(), ErrorKind::AuthenticationFailed);
    assert!(error.detail().ends_with(PROMPT_UNAVAILABLE), "{error:?}");
}

#[tokio::test]
#[ignore = "needs an sshd with keyboard-interactive, see HOPLIX_TEST_KI_* variables"]
async fn jump_hosts_ask_with_their_own_name() {
    let route = Route {
        proxy: None,
        jump_hosts: vec![JumpHost {
            host: variable("HOPLIX_TEST_KI_HOST"),
            port: port("HOPLIX_TEST_KI_KEY_PORT"),
            username: variable("HOPLIX_TEST_KI_USER"),
            authentication: key(),
        }],
    };
    let target = ConnectionParams {
        protocol: Protocol::Scp,
        host: "127.0.0.1".to_string(),
        port: port("HOPLIX_TEST_KI_PASSWORD_PORT"),
        username: variable("HOPLIX_TEST_KI_USER"),
        authentication: Authentication::Password(variable("HOPLIX_TEST_KI_PASSWORD")),
    };
    let authenticator = Authenticator::shared(false);
    let prompter = Arc::clone(&authenticator) as SharedPrompter;
    let remote = open(&target, &route, &trust_store("jump"), Some(prompter))
        .await
        .expect("signing in failed");
    browse(remote.as_ref()).await;
    let mut requests = authenticator.requests();
    let last = requests.pop().expect("no prompt");
    assert!(!last.jump);
    assert_eq!(last.host, target.label());
    assert!(!requests.is_empty());
    for request in requests {
        assert!(request.jump);
        assert_eq!(request.host, route.jump_hosts[0].label());
    }
}
