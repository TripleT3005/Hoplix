use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hoplix_core::{
    Authentication, ChecksumMode, ConnectionParams, ErrorKind, FileSystem, HostKeyTrust,
    JobOptions, JobStatus, LocalFileSystem, Location, Protocol, SavedJob, SavedQueue, SpeedLimits,
    TransferEvent, TransferQueue, TransferRequest, TrustStore, connect, hash_file,
    remove_recursive,
};
use tokio::sync::mpsc::UnboundedReceiver;

// Setup

const SIZE: usize = 6 * 1024 * 1024;
const RATE: u64 = 1024 * 1024;

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn required(name: &str) -> String {
    variable(name).unwrap_or_else(|| panic!("{name} is not set"))
}

fn sftp_params() -> ConnectionParams {
    ConnectionParams {
        protocol: Protocol::Sftp,
        host: required("HOPLIX_TEST_SFTP_HOST"),
        port: variable("HOPLIX_TEST_SFTP_PORT").map_or(22, |port| port.parse().expect("port")),
        username: required("HOPLIX_TEST_SFTP_USER"),
        authentication: Authentication::KeyFile {
            path: PathBuf::from(required("HOPLIX_TEST_SFTP_KEY")),
            passphrase: variable("HOPLIX_TEST_SFTP_PASSPHRASE"),
        },
    }
}

fn ftp_params() -> ConnectionParams {
    let protocol = variable("HOPLIX_TEST_FTP_PROTOCOL").map_or(Protocol::Ftp, |scheme| {
        Protocol::ALL
            .into_iter()
            .find(|protocol| protocol.scheme() == scheme)
            .expect("unknown protocol")
    });
    ConnectionParams {
        protocol,
        host: required("HOPLIX_TEST_FTP_HOST"),
        port: variable("HOPLIX_TEST_FTP_PORT")
            .map_or(protocol.default_port(), |port| port.parse().expect("port")),
        username: required("HOPLIX_TEST_FTP_USER"),
        authentication: Authentication::Password(required("HOPLIX_TEST_FTP_PASSWORD")),
    }
}

fn scratch(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("hoplix-restore-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("creating failed");
    directory
}

async fn open(params: &ConnectionParams, store: &TrustStore) -> Arc<dyn FileSystem> {
    let trust = match connect(params, store, None).await {
        Ok(file_system) => return file_system,
        Err(error) => HostKeyTrust::for_error(&error, false)
            .unwrap_or_else(|| panic!("connecting failed: {error}")),
    };
    connect(params, store, Some(trust))
        .await
        .expect("connecting with trust failed")
}

async fn wait_for(
    events: &mut UnboundedReceiver<TransferEvent>,
    wanted: impl Fn(&hoplix_core::JobSnapshot) -> bool,
) -> hoplix_core::JobSnapshot {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Some(TransferEvent::Updated(snapshot)) = events.recv().await
                && wanted(&snapshot)
            {
                return snapshot;
            }
        }
    })
    .await
    .expect("timed out")
}

fn content() -> Vec<u8> {
    let mut state: u32 = 0x1234_5678;
    (0..SIZE)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

// Round trip

async fn restore_round_trip(params: ConnectionParams, root: String, name: &str) {
    let directory = scratch(name);
    let store = TrustStore::in_directory(&directory.join("trust"));
    let source_path = directory.join("upload.bin");
    std::fs::write(&source_path, content()).expect("writing failed");
    let source = source_path.to_string_lossy().into_owned();
    let local: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);

    let remote = open(&params, &store).await;
    let folder = remote.join(&root, &format!("restore-{name}-{}", std::process::id()));
    remote
        .create_directory(&folder)
        .await
        .expect("creating the test folder failed");

    let first = TransferQueue::new(1);
    first.set_speed_limits(SpeedLimits {
        download: None,
        upload: Some(RATE),
    });
    let mut events = first.subscribe();
    let driver = tokio::spawn(first.clone().drive());
    let id = first.add(TransferRequest {
        source: Arc::clone(&local),
        source_path: source.clone(),
        target: Arc::clone(&remote),
        target_directory: folder.clone(),
        target_name: None,
    });
    wait_for(&mut events, |job| {
        job.id == id && job.transferred > 2 * RATE
    })
    .await;
    first.pause(id);
    wait_for(&mut events, |job| {
        job.id == id && job.status == JobStatus::Paused
    })
    .await;
    let state = first.job_states().remove(0);
    driver.abort();
    remote.close().await;
    let text = SavedQueue {
        jobs: vec![SavedJob {
            source: Location::Local,
            source_path: state.source_path,
            target: Location::Site("test".to_string()),
            target_directory: state.target_directory,
            target_name: state.target_name,
            policy: state.policy,
            keep_modified: state.keep_modified,
            resume: state.resume,
        }],
    }
    .to_text();
    if let Authentication::Password(secret) = &params.authentication {
        assert!(!text.contains(secret.as_str()));
    }
    let saved = SavedQueue::parse(&text).jobs.remove(0);
    assert_eq!(saved.resume.partial.len(), 1);

    let remote = open(&params, &store).await;
    let target = remote.join(&folder, "upload.bin");
    let partial = remote
        .metadata(&target)
        .await
        .expect("no partial file")
        .size
        .expect("no size");
    assert!(partial > 0 && partial < SIZE as u64, "{partial}");

    let second = TransferQueue::new(1);
    second.set_checksum_mode(ChecksumMode::WhenSupported);
    let mut events = second.subscribe();
    let driver = tokio::spawn(second.clone().drive());
    let id = second.add_paused(
        TransferRequest {
            source: Arc::clone(&local),
            source_path: saved.source_path,
            target: Arc::clone(&remote),
            target_directory: saved.target_directory,
            target_name: saved.target_name,
        },
        JobOptions {
            policy: saved.policy,
            ..JobOptions::default()
        },
        saved.resume,
    );
    assert_eq!(second.snapshots()[0].status, JobStatus::Paused);
    second.resume(id);
    let done = wait_for(&mut events, |job| job.id == id && job.status.is_finished()).await;
    driver.abort();
    assert_eq!(done.status, JobStatus::Completed, "{done:?}");
    eprintln!(
        "{}: resumed at {partial} of {SIZE} bytes, checksum {:?}",
        params.protocol.scheme(),
        done.checksum
    );

    let back = directory.join("back.bin");
    hoplix_core::copy_file(
        remote.as_ref(),
        &target,
        local.as_ref(),
        &back.to_string_lossy(),
        false,
    )
    .await
    .expect("downloading failed");
    let algorithm = hoplix_core::HashAlgorithm::Sha256;
    assert_eq!(
        hash_file(local.as_ref(), &back.to_string_lossy(), algorithm).await,
        hash_file(local.as_ref(), &source, algorithm).await
    );

    remove_recursive(remote.as_ref(), &folder)
        .await
        .expect("cleaning up the server failed");
    let gone = remote.metadata(&folder).await.expect_err("still there");
    assert_eq!(gone.kind(), ErrorKind::NotFound);
    remote.close().await;
    std::fs::remove_dir_all(directory).expect("cleaning up failed");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an SFTP server, see HOPLIX_TEST_SFTP_* and HOPLIX_TEST_SFTP_ROOT"]
async fn paused_sftp_uploads_resume_after_a_restore() {
    restore_round_trip(sftp_params(), required("HOPLIX_TEST_SFTP_ROOT"), "sftp").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an FTP server, see HOPLIX_TEST_FTP_* and HOPLIX_TEST_FTP_ROOT"]
async fn paused_ftp_uploads_resume_after_a_restore() {
    restore_round_trip(ftp_params(), required("HOPLIX_TEST_FTP_ROOT"), "ftp").await;
}
