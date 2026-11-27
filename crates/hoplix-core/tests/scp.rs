use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use hoplix_core::{
    Authentication, ConnectionOptions, ConnectionParams, ErrorKind, FileSystem, HashAlgorithm,
    HostKeyTrust, LocalFileSystem, Protocol, Route, ScpFileSystem, TrustStore, connect, copy_file,
    remove_recursive,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Setup

const NAMES: [&str; 4] = ["a b", "it's \"quoted\"", "Grüße $HOME `id`", "-dash\\back"];

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
        protocol: Protocol::Scp,
        host,
        port,
        username,
        authentication,
    }
}

fn local_directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("hoplix-scp-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("creating failed");
    directory
}

fn trust_store() -> TrustStore {
    let directory = local_directory("trust");
    TrustStore::in_directory(&directory)
}

async fn connected(params: &ConnectionParams, trust_store: &TrustStore) -> Arc<dyn FileSystem> {
    let error = connect(params, trust_store, None)
        .await
        .expect_err("an unknown key was trusted");
    assert_eq!(error.kind(), ErrorKind::UnknownHostKey);
    let trust = HostKeyTrust::for_error(&error, false).expect("no trust");
    connect(params, trust_store, Some(trust))
        .await
        .expect("connecting failed")
}

fn pattern(size: usize, seed: u8) -> Vec<u8> {
    (0..size)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn local(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

async fn remote_hash(file_system: &dyn FileSystem, path: &str) -> Vec<u8> {
    file_system
        .checksum(path, HashAlgorithm::Sha256)
        .await
        .expect("hashing failed")
}

// Operations

#[tokio::test]
#[ignore = "needs an SSH server with scp, see HOPLIX_TEST_SFTP_* variables"]
async fn scp_file_operations_work_remotely() {
    let params = params();
    let trust_store = trust_store();
    let remote = connected(&params, &trust_store).await;
    let file_system: &dyn FileSystem = remote.as_ref();
    assert!(!file_system.supports_resume());
    assert!(
        file_system
            .checksum_algorithms()
            .await
            .contains(&HashAlgorithm::Sha256)
    );

    let home = file_system.home().await.expect("home failed");
    assert!(home.starts_with('/'), "{home}");
    let root = file_system.join(&home, &format!("hoplix-scp-test-{}", std::process::id()));
    let _ = remove_recursive(file_system, &root).await;
    file_system
        .create_directory(&root)
        .await
        .expect("creating failed");
    assert_eq!(
        file_system
            .create_directory(&root)
            .await
            .map_err(|e| e.kind()),
        Err(ErrorKind::AlreadyExists)
    );

    let work = local_directory("files");
    for (index, name) in NAMES.iter().enumerate() {
        let folder = file_system.join(&root, name);
        file_system
            .create_directory(&folder)
            .await
            .expect("creating failed");
        let source = work.join(format!("source-{index}.bin"));
        let size = if index == 0 { 1_048_577 } else { 1000 * index };
        let data = pattern(size, index as u8);
        std::fs::write(&source, &data).expect("writing failed");
        let target = file_system.join(&folder, &format!("file '{index}' ä\nzwei.bin"));
        let copied = copy_file(
            &LocalFileSystem,
            &local(&source),
            file_system,
            &target,
            false,
        )
        .await
        .expect("uploading failed");
        assert_eq!(copied, size as u64);
        let entry = file_system
            .metadata(&target)
            .await
            .expect("metadata failed");
        assert_eq!(entry.size, Some(size as u64));
        assert!(!entry.is_directory());
        assert_eq!(
            remote_hash(file_system, &target).await,
            HashAlgorithm::Sha256.digest(&data)
        );
        let back = work.join(format!("back-{index}.bin"));
        copy_file(file_system, &target, &LocalFileSystem, &local(&back), true)
            .await
            .expect("downloading failed");
        assert_eq!(std::fs::read(&back).expect("reading failed"), data);
    }

    let mut names: Vec<String> = file_system
        .list(&root)
        .await
        .expect("listing failed")
        .into_iter()
        .filter(|entry| entry.is_directory())
        .map(|entry| entry.name)
        .collect();
    names.sort();
    let mut expected: Vec<String> = NAMES.iter().map(|name| (*name).to_string()).collect();
    expected.sort();
    assert_eq!(names, expected);

    let folder = file_system.join(&root, NAMES[1]);
    let file = file_system.join(&folder, "file '1' ä\nzwei.bin");
    let renamed = file_system.join(&folder, "renamed 'x' $y.bin");
    file_system
        .rename(&file, &renamed)
        .await
        .expect("renaming failed");
    assert_eq!(
        file_system.metadata(&file).await.map_err(|e| e.kind()),
        Err(ErrorKind::NotFound)
    );
    let other = file_system.join(&root, NAMES[2]);
    let occupied = file_system.join(&other, "file '2' ä\nzwei.bin");
    assert_eq!(
        file_system
            .rename(&renamed, &occupied)
            .await
            .map_err(|e| e.kind()),
        Err(ErrorKind::AlreadyExists)
    );

    let time = UNIX_EPOCH + Duration::from_secs(1_773_579_600);
    file_system
        .set_modified(&renamed, time)
        .await
        .expect("setting the time failed");
    file_system
        .set_permissions(&renamed, 0o640)
        .await
        .expect("chmod failed");
    let properties = file_system
        .properties(&renamed)
        .await
        .expect("properties failed");
    assert_eq!(properties.entry.modified, Some(time));
    assert_eq!(properties.permissions, Some(0o640));

    assert_eq!(
        file_system
            .open_read(&renamed, 5)
            .await
            .map(drop)
            .map_err(|e| e.kind()),
        Err(ErrorKind::Unsupported)
    );
    let spooled = file_system.join(&root, "spooled.txt");
    let mut writer = file_system
        .open_write(&spooled, 0)
        .await
        .expect("opening failed");
    writer
        .write_all(b"spooled data")
        .await
        .expect("writing failed");
    writer.shutdown().await.expect("closing failed");
    drop(writer);
    let mut reader = file_system
        .open_read(&spooled, 0)
        .await
        .expect("opening failed");
    let mut content = String::new();
    reader
        .read_to_string(&mut content)
        .await
        .expect("reading failed");
    assert_eq!(content, "spooled data");

    assert_eq!(
        file_system
            .remove_directory(&folder)
            .await
            .map_err(|e| e.kind()),
        Err(ErrorKind::DirectoryNotEmpty)
    );
    file_system
        .remove_file(&renamed)
        .await
        .expect("removing failed");
    file_system
        .remove_directory(&folder)
        .await
        .expect("removing failed");

    remove_recursive(file_system, &root)
        .await
        .expect("cleaning up failed");
    assert_eq!(
        file_system.metadata(&root).await.map_err(|e| e.kind()),
        Err(ErrorKind::NotFound)
    );
    file_system.close().await;
    std::fs::remove_dir_all(work).expect("cleaning up failed");
}

#[tokio::test]
#[ignore = "needs an SSH server with scp, see HOPLIX_TEST_SFTP_* variables"]
async fn scp_trees_copy_recursively() {
    let params = params();
    let trust_store = trust_store();
    let probe = connected(&params, &trust_store).await;
    let home = probe.home().await.expect("home failed");
    probe.close().await;
    let error = connect(&params, &trust_store, None)
        .await
        .expect_err("trusted");
    let trust = HostKeyTrust::for_error(&error, false).expect("no trust");
    let file_system = ScpFileSystem::connect_prompting(
        &params,
        &Route::default(),
        &ConnectionOptions::default(),
        &trust_store.known_hosts,
        &[trust],
        None,
    )
    .await
    .expect("connecting failed");

    let work = local_directory("tree");
    let tree = work.join("tree 'top' ä");
    std::fs::create_dir_all(tree.join("sub $dir").join("deeper")).expect("creating failed");
    std::fs::write(tree.join("one.txt"), b"one").expect("writing failed");
    std::fs::write(
        tree.join("sub $dir").join("two `x`.bin"),
        pattern(70_000, 3),
    )
    .expect("writing failed");
    std::fs::write(tree.join("sub $dir").join("deeper").join("empty"), b"")
        .expect("writing failed");

    let root = file_system.join(&home, &format!("hoplix-scp-tree-{}", std::process::id()));
    let _ = remove_recursive(&file_system, &root).await;
    file_system
        .create_directory(&root)
        .await
        .expect("creating failed");
    let sent = file_system
        .upload_tree(&tree, &root)
        .await
        .expect("uploading failed");
    assert_eq!((sent.files, sent.directories, sent.bytes), (3, 3, 70_003));
    let uploaded = file_system.join(&file_system.join(&root, "tree 'top' ä"), "sub $dir");
    let names: Vec<String> = file_system
        .list(&uploaded)
        .await
        .expect("listing failed")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert_eq!(
        remote_hash(&file_system, &file_system.join(&uploaded, "two `x`.bin")).await,
        HashAlgorithm::Sha256.digest(&pattern(70_000, 3))
    );

    let back = work.join("back");
    std::fs::create_dir_all(&back).expect("creating failed");
    let received = file_system
        .download_tree(&file_system.join(&root, "tree 'top' ä"), &back)
        .await
        .expect("downloading failed");
    assert_eq!(received, sent);
    let copy = back.join("tree 'top' ä");
    assert_eq!(
        std::fs::read(copy.join("one.txt")).expect("missing"),
        b"one"
    );
    assert_eq!(
        std::fs::read(copy.join("sub $dir").join("two `x`.bin")).expect("missing"),
        pattern(70_000, 3)
    );
    assert!(copy.join("sub $dir").join("deeper").join("empty").is_file());
    assert_eq!(
        std::fs::metadata(copy.join("one.txt"))
            .and_then(|metadata| metadata.modified())
            .expect("no time"),
        std::fs::metadata(tree.join("one.txt"))
            .and_then(|metadata| metadata.modified())
            .map(|time| UNIX_EPOCH
                + Duration::from_secs(time.duration_since(UNIX_EPOCH).expect("old time").as_secs()))
            .expect("no time")
    );

    remove_recursive(&file_system, &root)
        .await
        .expect("cleaning up failed");
    file_system.close().await;
    std::fs::remove_dir_all(work).expect("cleaning up failed");
}
