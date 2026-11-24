mod common;

use std::path::PathBuf;

use hoplix_core::{
    Authentication, ConnectionParams, ErrorKind, FileSystem, HostKeyTrust, Protocol, TrustStore,
    connect, remove_recursive,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use common::test_password;

// Setup

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn params() -> ConnectionParams {
    let host = variable("HOPLIX_TEST_SFTP_HOST").expect("HOPLIX_TEST_SFTP_HOST is not set");
    let username = variable("HOPLIX_TEST_SFTP_USER").expect("HOPLIX_TEST_SFTP_USER is not set");
    let port =
        variable("HOPLIX_TEST_SFTP_PORT").map_or(22, |port| port.parse().expect("invalid port"));
    let authentication = match variable("HOPLIX_TEST_SFTP_KEY") {
        Some(path) => Authentication::KeyFile {
            path: PathBuf::from(path),
            passphrase: variable("HOPLIX_TEST_SFTP_PASSPHRASE"),
        },
        None => Authentication::Password(
            variable("HOPLIX_TEST_SFTP_PASSWORD").expect("neither a key nor a password is set"),
        ),
    };
    ConnectionParams {
        protocol: Protocol::Sftp,
        host,
        port,
        username,
        authentication,
    }
}

fn known_hosts_file() -> TrustStore {
    let directory = std::env::temp_dir().join(format!("hoplix-sftp-trust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    TrustStore::in_directory(&directory)
}

// Host keys

#[tokio::test]
#[ignore = "needs an SFTP server, see HOPLIX_TEST_SFTP_* variables"]
async fn host_keys_are_verified_and_remembered() {
    let params = params();
    let known_hosts = known_hosts_file();

    let error = connect(&params, &known_hosts, None)
        .await
        .expect_err("unknown key accepted");
    assert_eq!(error.kind(), ErrorKind::UnknownHostKey);
    let fingerprint = error.detail().to_string();
    assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");

    let wrong = HostKeyTrust {
        fingerprint: "SHA256:wrong".to_string(),
        remember: true,
    };
    let error = connect(&params, &known_hosts, Some(wrong))
        .await
        .expect_err("wrong key accepted");
    assert_eq!(error.kind(), ErrorKind::UnknownHostKey);

    let once = HostKeyTrust {
        fingerprint: fingerprint.clone(),
        remember: false,
    };
    connect(&params, &known_hosts, Some(once))
        .await
        .expect("trusted once")
        .close()
        .await;
    assert!(!known_hosts.known_hosts.exists());

    let always = HostKeyTrust {
        fingerprint,
        remember: true,
    };
    connect(&params, &known_hosts, Some(always))
        .await
        .expect("trusted always")
        .close()
        .await;
    connect(&params, &known_hosts, None)
        .await
        .expect("remembered key")
        .close()
        .await;

    let original =
        std::fs::read_to_string(&known_hosts.known_hosts).expect("reading known hosts failed");
    if let Some(other) = variable("HOPLIX_TEST_SFTP_OTHER_KEY") {
        let mut parts = original.split_whitespace();
        let host = parts.next().expect("host missing");
        std::fs::write(&known_hosts.known_hosts, format!("{host} {other}\n"))
            .expect("writing failed");
        let error = connect(&params, &known_hosts, None)
            .await
            .expect_err("changed key accepted");
        assert_eq!(error.kind(), ErrorKind::HostKeyChanged);
    }
    std::fs::remove_file(known_hosts.known_hosts).expect("cleaning up failed");
}

#[tokio::test]
#[ignore = "needs an SFTP server, see HOPLIX_TEST_SFTP_* variables"]
async fn wrong_credentials_are_rejected() {
    let mut params = params();
    params.authentication = Authentication::Password(test_password());
    params.username = format!("{}-missing", params.username);
    let known_hosts = known_hosts_file();
    let fingerprint = connect(&params, &known_hosts, None)
        .await
        .expect_err("connected")
        .detail()
        .to_string();
    let trust = HostKeyTrust {
        fingerprint,
        remember: false,
    };
    let error = connect(&params, &known_hosts, Some(trust))
        .await
        .expect_err("logged in");
    assert_eq!(error.kind(), ErrorKind::AuthenticationFailed);
}

// Operations

#[tokio::test]
#[ignore = "needs an SFTP server, see HOPLIX_TEST_SFTP_* variables"]
async fn file_operations_work_remotely() {
    let params = params();
    let known_hosts = known_hosts_file();
    let fingerprint = connect(&params, &known_hosts, None)
        .await
        .expect_err("connected")
        .detail()
        .to_string();
    let trust = HostKeyTrust {
        fingerprint,
        remember: false,
    };
    let remote = connect(&params, &known_hosts, Some(trust))
        .await
        .expect("connecting failed");
    let file_system: &dyn FileSystem = remote.as_ref();

    let home = file_system.home().await.expect("home failed");
    assert!(home.starts_with('/'), "{home}");
    let root = format!("/tmp/hoplix-test-{}", std::process::id());
    file_system
        .create_directory(&root)
        .await
        .expect("creating failed");
    let error = file_system
        .create_directory(&root)
        .await
        .expect_err("created twice");
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);

    let file = file_system.join(&root, "data.txt");
    let mut writer = file_system
        .open_write(&file, 0)
        .await
        .expect("opening failed");
    writer
        .write_all(b"hello world")
        .await
        .expect("writing failed");
    writer.shutdown().await.expect("closing failed");
    drop(writer);

    let mut writer = file_system
        .open_write(&file, 6)
        .await
        .expect("resuming failed");
    writer.write_all(b"there").await.expect("writing failed");
    writer.shutdown().await.expect("closing failed");
    drop(writer);

    let mut reader = file_system
        .open_read(&file, 0)
        .await
        .expect("opening failed");
    let mut content = String::new();
    reader
        .read_to_string(&mut content)
        .await
        .expect("reading failed");
    assert_eq!(content, "hello there");
    let mut reader = file_system
        .open_read(&file, 6)
        .await
        .expect("opening failed");
    let mut rest = String::new();
    reader
        .read_to_string(&mut rest)
        .await
        .expect("reading failed");
    assert_eq!(rest, "there");

    let metadata = file_system.metadata(&file).await.expect("metadata failed");
    assert_eq!(metadata.name, "data.txt");
    assert_eq!(metadata.size, Some(11));

    let nested = file_system.join(&root, "nested");
    file_system
        .create_directory(&nested)
        .await
        .expect("creating failed");
    let renamed = file_system.join(&root, "renamed.txt");
    file_system
        .rename(&file, &renamed)
        .await
        .expect("renaming failed");
    let mut names: Vec<_> = file_system
        .list(&root)
        .await
        .expect("listing failed")
        .into_iter()
        .map(|entry| (entry.name.clone(), entry.is_directory()))
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            ("nested".to_string(), true),
            ("renamed.txt".to_string(), false)
        ]
    );

    let missing = file_system
        .list(&file_system.join(&root, "missing"))
        .await
        .expect_err("listed");
    assert_eq!(missing.kind(), ErrorKind::NotFound);

    let error = file_system
        .remove_directory(&root)
        .await
        .expect_err("removed a full folder");
    assert_eq!(error.kind(), ErrorKind::DirectoryNotEmpty);

    remove_recursive(file_system, &root)
        .await
        .expect("removing failed");
    let gone = file_system.metadata(&root).await.expect_err("still there");
    assert_eq!(gone.kind(), ErrorKind::NotFound);
    file_system.close().await;
}
