use std::sync::Arc;

use hoplix_core::{
    Authentication, ConnectionParams, ErrorKind, FileSystem, HostKeyTrust, Protocol, TrustStore,
    connect, copy_file, remove_recursive,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Setup

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn params() -> ConnectionParams {
    let protocol = variable("HOPLIX_TEST_FTP_PROTOCOL").map_or(Protocol::Ftp, |scheme| {
        Protocol::ALL
            .into_iter()
            .find(|protocol| protocol.scheme() == scheme)
            .expect("unknown protocol")
    });
    let host = variable("HOPLIX_TEST_FTP_HOST").expect("HOPLIX_TEST_FTP_HOST is not set");
    let username = variable("HOPLIX_TEST_FTP_USER").expect("HOPLIX_TEST_FTP_USER is not set");
    let password =
        variable("HOPLIX_TEST_FTP_PASSWORD").expect("HOPLIX_TEST_FTP_PASSWORD is not set");
    let port = variable("HOPLIX_TEST_FTP_PORT").map_or(protocol.default_port(), |port| {
        port.parse().expect("invalid port")
    });
    ConnectionParams {
        protocol,
        host,
        port,
        username,
        authentication: Authentication::Password(password),
    }
}

fn trust_store(name: &str) -> TrustStore {
    let directory = std::env::temp_dir().join(format!("hoplix-ftp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    TrustStore::in_directory(&directory)
}

async fn open(params: &ConnectionParams, store: &TrustStore) -> Arc<dyn FileSystem> {
    let trust = match connect(params, store, None).await {
        Ok(file_system) => return file_system,
        Err(error) if error.kind() == ErrorKind::UntrustedCertificate => HostKeyTrust {
            fingerprint: error.detail().to_string(),
            remember: false,
        },
        Err(error) => panic!("connecting failed: {error}"),
    };
    connect(params, store, Some(trust))
        .await
        .expect("connecting with trust failed")
}

// Certificates

#[tokio::test]
#[ignore = "needs an FTPS server with a self-signed certificate, see HOPLIX_TEST_FTP_*"]
async fn self_signed_certificates_are_confirmed_and_remembered() {
    let params = params();
    if !params.protocol.uses_tls() {
        eprintln!("skipped: HOPLIX_TEST_FTP_PROTOCOL is not ftpes or ftps");
        return;
    }
    let store = trust_store("certificates");

    let error = connect(&params, &store, None)
        .await
        .expect_err("untrusted certificate accepted");
    assert_eq!(error.kind(), ErrorKind::UntrustedCertificate);
    let fingerprint = error.detail().to_string();
    assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");

    let wrong = HostKeyTrust {
        fingerprint: "SHA256:00".to_string(),
        remember: true,
    };
    let error = connect(&params, &store, Some(wrong))
        .await
        .expect_err("wrong fingerprint accepted");
    assert_eq!(error.kind(), ErrorKind::UntrustedCertificate);

    let always = HostKeyTrust {
        fingerprint: fingerprint.clone(),
        remember: true,
    };
    connect(&params, &store, Some(always))
        .await
        .expect("trusted always")
        .close()
        .await;
    connect(&params, &store, None)
        .await
        .expect("remembered certificate")
        .close()
        .await;

    std::fs::write(
        &store.certificates,
        format!("{}:{} SHA256:00\n", params.host, params.port),
    )
    .expect("writing failed");
    let error = connect(&params, &store, None)
        .await
        .expect_err("changed certificate accepted");
    assert_eq!(error.kind(), ErrorKind::CertificateChanged);
    if let Some(directory) = store.certificates.parent() {
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}

#[tokio::test]
#[ignore = "needs an FTP server, see HOPLIX_TEST_FTP_* variables"]
async fn wrong_passwords_are_rejected() {
    let mut params = params();
    params.authentication = Authentication::Password("definitely-wrong".to_string());
    let store = trust_store("password");
    let trust = match connect(&params, &store, None).await {
        Err(error) if error.kind() == ErrorKind::UntrustedCertificate => Some(HostKeyTrust {
            fingerprint: error.detail().to_string(),
            remember: false,
        }),
        Err(error) => {
            assert_eq!(error.kind(), ErrorKind::AuthenticationFailed);
            return;
        }
        Ok(_) => panic!("logged in with a wrong password"),
    };
    let error = connect(&params, &store, trust)
        .await
        .expect_err("logged in with a wrong password");
    assert_eq!(error.kind(), ErrorKind::AuthenticationFailed);
}

// Operations

#[tokio::test]
#[ignore = "needs an FTP server, see HOPLIX_TEST_FTP_* variables"]
async fn file_operations_work_remotely() {
    let params = params();
    let remote = open(&params, &trust_store("operations")).await;
    let file_system: &dyn FileSystem = remote.as_ref();

    let home = file_system.home().await.expect("home failed");
    assert!(home.starts_with('/'), "{home}");
    let root = file_system.join(&home, &format!("hoplix-test-{}", std::process::id()));
    file_system
        .create_directory(&root)
        .await
        .expect("creating failed");
    let error = file_system
        .create_directory(&root)
        .await
        .expect_err("created twice");
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);

    let file = file_system.join(&root, "data ü.txt");
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
    drop(reader);
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
    drop(reader);

    let metadata = file_system.metadata(&file).await.expect("metadata failed");
    assert_eq!(metadata.name, "data ü.txt");
    assert_eq!(metadata.size, Some(11));
    assert!(!metadata.is_directory());

    let local = hoplix_core::LocalFileSystem;
    let download = std::env::temp_dir()
        .join(format!("hoplix-ftp-download-{}", std::process::id()))
        .to_string_lossy()
        .into_owned();
    std::fs::write(&download, b"hello").expect("writing failed");
    let copied = copy_file(file_system, &file, &local, &download, true)
        .await
        .expect("resuming the download failed");
    assert_eq!(copied, 6);
    assert_eq!(
        std::fs::read(&download).expect("reading failed"),
        b"hello there"
    );
    std::fs::remove_file(&download).expect("cleaning up failed");

    let nested = file_system.join(&root, "nested folder");
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
        .map(|entry| (entry.name.clone(), entry.is_directory(), entry.size))
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            ("nested folder".to_string(), true, None),
            ("renamed.txt".to_string(), false, Some(11))
        ]
    );

    let missing = file_system
        .list(&file_system.join(&root, "missing"))
        .await
        .expect_err("listed");
    assert_eq!(missing.kind(), ErrorKind::NotFound);
    let not_a_folder = file_system.list(&renamed).await.expect_err("listed a file");
    assert_eq!(not_a_folder.kind(), ErrorKind::NotADirectory);

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
