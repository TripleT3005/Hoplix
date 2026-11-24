mod arguments;
mod target;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use hoplix_core::{
    Authentication, CompareOptions, CompareStatus, ComparedEntry, ConflictPolicy, ConnectionParams,
    DEFAULT_PARALLELISM, Error, ErrorKind, FileSystem, HostKeyTrust, JobId, JobSnapshot, JobStatus,
    Keyring, LocalFileSystem, Pattern, RetryPolicy, SecretBackend, Site, SiteList, SortKey,
    SortOrder, SpeedLimits, SyncAction, SyncDirection, SyncEndpoint, SyncMode, SyncOptions,
    SyncPlan, TransferEvent, TransferQueue, TransferRequest, TrustStore, config_dir,
    remove_recursive, secret_name, sort_entries, unlock_vault,
};

use arguments::{Command, Invocation, Options, USAGE};
use target::{ANONYMOUS, RemotePath, RemoteTarget, SiteTarget, Target};

// Constants

const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

// Types

struct Opened {
    file_system: Arc<dyn FileSystem>,
    path: String,
}

// Entry point

fn main() -> ExitCode {
    let invocation = match arguments::parse(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("hoplix-cli: {message}\n\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hoplix-cli: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    match runtime.block_on(run(invocation)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("hoplix-cli: {message}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

// Commands

async fn run(invocation: Invocation) -> Result<(), String> {
    let options = &invocation.options;
    match invocation.command {
        Command::Help => print!("{USAGE}"),
        Command::Version => println!("hoplix-cli {}", env!("CARGO_PKG_VERSION")),
        Command::Sites => {
            let sites = load_sites()?;
            let sorted = sites.sorted();
            let width = sorted
                .iter()
                .map(|site| site.display_path().chars().count())
                .max()
                .unwrap_or(0);
            for site in sorted {
                println!("{:<width$}  {}", site.display_path(), site.address());
            }
        }
        Command::List(target) => {
            let opened = open(&target, options).await?;
            let mut entries = opened
                .file_system
                .list(&opened.path)
                .await
                .map_err(describe)?;
            sort_entries(&mut entries, SortKey::Name, SortOrder::Ascending);
            for entry in entries {
                let kind = match (entry.is_directory(), entry.is_link) {
                    (true, false) => "d",
                    (true, true) => "D",
                    (false, false) => "-",
                    (false, true) => "l",
                };
                let size = entry.size.map(|size| size.to_string()).unwrap_or_default();
                println!("{kind} {size:>14}  {}", entry.name);
            }
            opened.file_system.close().await;
        }
        Command::Stat(target) => {
            let opened = open(&target, options).await?;
            let entry = opened
                .file_system
                .metadata(&opened.path)
                .await
                .map_err(describe)?;
            println!("path: {}", opened.path);
            println!(
                "type: {}",
                if entry.is_directory() {
                    "directory"
                } else {
                    "file"
                }
            );
            println!("link: {}", if entry.is_link { "yes" } else { "no" });
            if let Some(size) = entry.size {
                println!("size: {size}");
            }
            if let Some(seconds) = entry
                .modified
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            {
                println!("modified: {} (Unix time)", seconds.as_secs());
            }
            opened.file_system.close().await;
        }
        Command::MakeDirectory(target) => {
            let opened = open(&target, options).await?;
            opened
                .file_system
                .create_directory(&opened.path)
                .await
                .map_err(describe)?;
            opened.file_system.close().await;
        }
        Command::Remove { target, recursive } => {
            let opened = open(&target, options).await?;
            let file_system = opened.file_system.as_ref();
            let entry = file_system.metadata(&opened.path).await.map_err(describe)?;
            let result = if entry.is_directory() && !entry.is_link {
                if recursive {
                    remove_recursive(file_system, &opened.path).await
                } else {
                    file_system.remove_directory(&opened.path).await
                }
            } else {
                file_system.remove_file(&opened.path).await
            };
            result.map_err(describe)?;
            file_system.close().await;
        }
        Command::Copy {
            from,
            to,
            recursive,
        } => {
            let source = open(&from, options).await?;
            let target = open(&to, options).await?;
            let result = transfer(&source, &target, recursive, options).await;
            source.file_system.close().await;
            target.file_system.close().await;
            result?;
        }
        Command::Compare {
            left,
            right,
            recursive,
        } => {
            let left = open(&left, options).await?;
            let right = open(&right, options).await?;
            let result = compare_folders(&left, &right, recursive, options).await;
            left.file_system.close().await;
            right.file_system.close().await;
            result?;
        }
        Command::Sync {
            from,
            to,
            recursive,
        } => {
            if from == to {
                return Err("source and target are the same folder".to_string());
            }
            let source = open(&from, options).await?;
            let target = open(&to, options).await?;
            let result = synchronise(&source, &target, recursive, options).await;
            source.file_system.close().await;
            target.file_system.close().await;
            result?;
        }
        Command::Move { from, to } => {
            let same = match (&from, &to) {
                (Target::Local(_), Target::Local(_)) => true,
                (Target::Remote(a), Target::Remote(b)) => a.same_server(b),
                (Target::Site(a), Target::Site(b)) => a.reference == b.reference,
                _ => false,
            };
            if !same {
                return Err("mv only works within the same server".to_string());
            }
            let opened = open(&from, options).await?;
            let destination = match &to {
                Target::Local(path) => path.clone(),
                Target::Remote(remote) => resolve(&opened, &remote.path).await?,
                Target::Site(site) => {
                    let path = site
                        .path
                        .as_ref()
                        .ok_or("mv needs a path in the destination")?;
                    resolve(&opened, path).await?
                }
            };
            opened
                .file_system
                .rename(&opened.path, &destination)
                .await
                .map_err(describe)?;
            opened.file_system.close().await;
        }
    }
    Ok(())
}

// Transfers

async fn transfer(
    source: &Opened,
    target: &Opened,
    recursive: bool,
    options: &Options,
) -> Result<(), String> {
    let entry = source
        .file_system
        .metadata(&source.path)
        .await
        .map_err(describe)?;
    if entry.is_directory() && !recursive {
        return Err(format!("{} is a folder: add -r to copy it", source.path));
    }
    let (directory, name) = match target.file_system.metadata(&target.path).await {
        Ok(existing) if existing.is_directory() => (target.path.clone(), None),
        Ok(_) if entry.is_directory() => {
            return Err(format!("{} is a file, not a folder", target.path));
        }
        Ok(_) => split(target),
        Err(error) if error.kind() == ErrorKind::NotFound => split(target),
        Err(error) => return Err(describe(error)),
    };
    let queue = queue_for(1, options);
    queue.set_policy(if options.resume {
        ConflictPolicy::Resume
    } else if options.skip_existing {
        ConflictPolicy::Skip
    } else if options.keep_both {
        ConflictPolicy::Rename
    } else {
        ConflictPolicy::Overwrite
    });
    let mut events = queue.subscribe();
    let driver = tokio::spawn(queue.clone().drive());
    queue.add(TransferRequest {
        source: Arc::clone(&source.file_system),
        source_path: source.path.clone(),
        target: Arc::clone(&target.file_system),
        target_directory: directory,
        target_name: name,
    });
    let interactive = std::io::stderr().is_terminal();
    let mut last_line = Instant::now() - PROGRESS_INTERVAL;
    let result = loop {
        let Some(event) = events.recv().await else {
            break Err("the transfer stopped unexpectedly".to_string());
        };
        let TransferEvent::Updated(job) = event else {
            continue;
        };
        match &job.status {
            JobStatus::Completed => {
                if interactive {
                    eprint!("\r\x1b[2K");
                }
                let copied = job.files_done - job.skipped;
                if job.skipped > 0 {
                    println!(
                        "{copied} file(s) copied to {}, {} skipped because they already exist",
                        job.target_path, job.skipped
                    );
                } else {
                    println!(
                        "{copied} file(s), {} bytes copied to {}",
                        job.transferred, job.target_path
                    );
                }
                break Ok(());
            }
            JobStatus::Failed(error) => {
                if interactive {
                    eprintln!();
                }
                break Err(describe(error.clone()));
            }
            JobStatus::Cancelled => break Err("the transfer was cancelled".to_string()),
            JobStatus::Retrying(attempt) => {
                eprintln!(
                    "connection lost, retry {attempt} of {}",
                    queue.retry_policy().attempts
                );
            }
            JobStatus::Running if interactive && last_line.elapsed() >= PROGRESS_INTERVAL => {
                last_line = Instant::now();
                eprint!("\r\x1b[2K{}", progress_line(&job));
            }
            _ => {}
        }
    };
    driver.abort();
    result
}

// Comparing

fn compare_options(recursive: bool, options: &Options) -> CompareOptions {
    CompareOptions {
        recursive,
        size_only: options.size_only,
        skip_hidden: options.skip_hidden,
        exclude: options
            .exclude
            .iter()
            .flat_map(|text| Pattern::parse_list(text))
            .collect(),
        ..CompareOptions::default()
    }
}

async fn folder(opened: &Opened) -> Result<SyncEndpoint, String> {
    let entry = opened
        .file_system
        .metadata(&opened.path)
        .await
        .map_err(describe)?;
    if !entry.is_directory() {
        return Err(format!("{} is not a folder", opened.path));
    }
    Ok(SyncEndpoint::new(
        Arc::clone(&opened.file_system),
        opened.path.clone(),
    ))
}

async fn compare_folders(
    left: &Opened,
    right: &Opened,
    recursive: bool,
    options: &Options,
) -> Result<(), String> {
    let left = folder(left).await?;
    let right = folder(right).await?;
    let entries = hoplix_core::compare(&left, &right, &compare_options(recursive, options))
        .await
        .map_err(describe)?;
    let differences: Vec<&ComparedEntry> = entries
        .iter()
        .filter(|entry| entry.status.is_difference())
        .collect();
    for entry in &differences {
        println!("{:<12} {}", status_word(entry.status), display_path(entry));
    }
    let equal = entries.len() - differences.len();
    println!("{} difference(s), {equal} equal", differences.len());
    Ok(())
}

fn status_word(status: CompareStatus) -> &'static str {
    match status {
        CompareStatus::OnlyLeft => "only-first",
        CompareStatus::OnlyRight => "only-second",
        CompareStatus::NewerLeft => "newer-first",
        CompareStatus::NewerRight => "newer-second",
        CompareStatus::SizeDiffers => "size",
        CompareStatus::KindDiffers => "type",
        CompareStatus::Equal => "equal",
    }
}

fn display_path(entry: &ComparedEntry) -> String {
    if entry.is_directory() && entry.status != CompareStatus::KindDiffers {
        format!("{}/", entry.path)
    } else {
        entry.path.clone()
    }
}

// Synchronising

async fn synchronise(
    source: &Opened,
    target: &Opened,
    recursive: bool,
    options: &Options,
) -> Result<(), String> {
    let left = folder(source).await?;
    let right = match target.file_system.metadata(&target.path).await {
        Err(error) if error.kind() == ErrorKind::NotFound && !options.dry_run => {
            target
                .file_system
                .create_directory(&target.path)
                .await
                .map_err(describe)?;
            folder(target).await?
        }
        _ => folder(target).await?,
    };
    let sync_options = SyncOptions {
        compare: compare_options(recursive, options),
        direction: SyncDirection::LeftToRight,
        mode: if options.mirror || options.delete {
            SyncMode::Mirror
        } else {
            SyncMode::Update
        },
        delete_extra: options.delete,
    };
    let plan = hoplix_core::plan(&left, &right, &sync_options)
        .await
        .map_err(describe)?;
    if options.dry_run {
        for line in plan_lines(&plan) {
            println!("{line}");
        }
    }
    println!("{}", plan_summary(&plan));
    if options.dry_run || plan.is_empty() {
        return Ok(());
    }
    let queue = queue_for(DEFAULT_PARALLELISM, options);
    let mut events = queue.subscribe();
    let driver = tokio::spawn(queue.clone().drive());
    let result = match hoplix_core::execute_plan(&plan, &left, &right, &queue).await {
        Ok(jobs) => wait_for(&mut events, &jobs, queue.retry_policy().attempts).await,
        Err(error) => Err(describe(error)),
    };
    driver.abort();
    result
}

fn plan_lines(plan: &SyncPlan) -> Vec<String> {
    let mut lines: Vec<String> = plan
        .actions
        .iter()
        .map(|action| match action {
            SyncAction::CreateDirectory { path, .. } => format!("create    {path}/"),
            SyncAction::Copy { path, size, .. } => format!("copy      {path} ({size} bytes)"),
            SyncAction::Delete {
                path, is_directory, ..
            } => format!("delete    {path}{}", if *is_directory { "/" } else { "" }),
        })
        .collect();
    lines.extend(
        plan.conflicts
            .iter()
            .map(|path| format!("skip      {path} (file and folder with the same name)")),
    );
    lines
}

fn plan_summary(plan: &SyncPlan) -> String {
    let summary = plan.summary();
    let mut text = format!(
        "{} file(s) to copy ({} bytes), {} folder(s) to create, {} to delete",
        summary.to_right, summary.bytes_to_right, summary.directories, summary.deletions
    );
    if summary.conflicts > 0 {
        text.push_str(&format!(", {} skipped", summary.conflicts));
    }
    text
}

async fn wait_for(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<TransferEvent>,
    jobs: &[JobId],
    retries: u32,
) -> Result<(), String> {
    let interactive = std::io::stderr().is_terminal();
    let mut finished: Vec<JobId> = Vec::new();
    let mut failures = Vec::new();
    while finished.len() < jobs.len() {
        let Some(event) = events.recv().await else {
            return Err("the transfer stopped unexpectedly".to_string());
        };
        let TransferEvent::Updated(job) = event else {
            continue;
        };
        if !jobs.contains(&job.id) || finished.contains(&job.id) {
            continue;
        }
        match &job.status {
            JobStatus::Completed => finished.push(job.id),
            JobStatus::Failed(error) => {
                finished.push(job.id);
                failures.push(format!("{}: {}", job.source_path, describe(error.clone())));
            }
            JobStatus::Cancelled => {
                finished.push(job.id);
                failures.push(format!("{}: cancelled", job.source_path));
            }
            JobStatus::Retrying(attempt) => {
                eprintln!("connection lost, retry {attempt} of {retries}");
            }
            _ => {}
        }
        if interactive {
            eprint!("\r\x1b[2K{}/{} file(s)", finished.len(), jobs.len());
        }
    }
    if interactive {
        eprint!("\r\x1b[2K");
    }
    println!("{} file(s) copied", jobs.len() - failures.len());
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} file(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        ))
    }
}

fn queue_for(parallelism: usize, options: &Options) -> TransferQueue {
    let queue = TransferQueue::new(parallelism);
    let defaults = RetryPolicy::default();
    queue.set_retry_policy(RetryPolicy {
        attempts: options.retries.unwrap_or(defaults.attempts),
        delay: options.retry_delay.map_or(defaults.delay, |seconds| {
            Duration::from_secs(u64::from(seconds))
        }),
    });
    queue.set_speed_limits(SpeedLimits {
        download: options.limit_download,
        upload: options.limit_upload,
    });
    queue
}

fn split(target: &Opened) -> (String, Option<String>) {
    let file_system = target.file_system.as_ref();
    let name = file_system.file_name(&target.path).to_string();
    let directory = file_system
        .parent(&target.path)
        .unwrap_or_else(|| ".".to_string());
    (directory, Some(name))
}

fn progress_line(job: &JobSnapshot) -> String {
    let percent = job.fraction() * 100.0;
    let files = match job.files_total {
        Some(total) if total > 1 => format!(" ({}/{total} files)", job.files_done),
        _ => String::new(),
    };
    let speed = job.bytes_per_second / 1024.0 / 1024.0;
    let remaining = job
        .remaining_seconds()
        .map(|seconds| format!(", {}:{:02} left", seconds / 60, seconds % 60))
        .unwrap_or_default();
    let name = job.current_file.as_deref().unwrap_or(&job.name);
    format!("{name}: {percent:5.1} %{files}, {speed:.1} MiB/s{remaining}")
}

// Connecting

async fn open(target: &Target, options: &Options) -> Result<Opened, String> {
    match target {
        Target::Local(path) => Ok(Opened {
            file_system: Arc::new(LocalFileSystem),
            path: path.clone(),
        }),
        Target::Remote(remote) => {
            let params = ConnectionParams {
                protocol: remote.protocol,
                host: remote.host.clone(),
                port: remote.port,
                username: remote.username.clone(),
                authentication: authentication(remote, options)?,
            };
            let file_system = connect(&params, options).await?;
            let path = locate(file_system.as_ref(), &remote.path).await?;
            Ok(Opened { file_system, path })
        }
        Target::Site(target) => {
            let sites = load_sites()?;
            let site = find_site(&sites, target)?;
            let params = site_params(site, options).await?;
            let file_system = connect(&params, options).await?;
            let path = target
                .path
                .clone()
                .unwrap_or_else(|| RemotePath::from_site(&site.remote_path));
            let path = locate(file_system.as_ref(), &path).await?;
            Ok(Opened { file_system, path })
        }
    }
}

async fn locate(file_system: &dyn FileSystem, path: &RemotePath) -> Result<String, String> {
    match path {
        RemotePath::Absolute(path) => Ok(path.clone()),
        RemotePath::Home(relative) => {
            let home = file_system.home().await.map_err(describe)?;
            Ok(if relative.is_empty() {
                home
            } else {
                file_system.join(&home, relative)
            })
        }
    }
}

async fn resolve(opened: &Opened, path: &RemotePath) -> Result<String, String> {
    locate(opened.file_system.as_ref(), path).await
}

async fn connect(
    params: &ConnectionParams,
    options: &Options,
) -> Result<Arc<dyn FileSystem>, String> {
    let mut trust_store = TrustStore::default_location().ok_or("no folder for the trust files")?;
    if let Some(path) = &options.known_hosts {
        trust_store.known_hosts.clone_from(path);
    }
    if let Some(path) = &options.trusted_certificates {
        trust_store.certificates.clone_from(path);
    }
    match hoplix_core::connect(params, &trust_store, None).await {
        Ok(file_system) => Ok(file_system),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::UnknownHostKey | ErrorKind::UntrustedCertificate
            ) =>
        {
            let fingerprint = error.detail().to_string();
            let what = if error.kind() == ErrorKind::UnknownHostKey {
                "server key"
            } else {
                "certificate"
            };
            if !options.accept_host_key && !options.remember_host_key {
                return Err(format!(
                    "the {what} of {} is not trusted ({fingerprint}). Check it and run again with --accept-host-key or --remember-host-key",
                    params.host
                ));
            }
            eprintln!("Trusting {what} {fingerprint}");
            let trust = HostKeyTrust {
                fingerprint,
                remember: options.remember_host_key,
            };
            hoplix_core::connect(params, &trust_store, Some(trust))
                .await
                .map_err(describe)
        }
        Err(error) => Err(describe(error)),
    }
}

fn authentication(remote: &RemoteTarget, options: &Options) -> Result<Authentication, String> {
    if let Some(key) = &options.key {
        let passphrase = options.passphrase_env.as_deref().map(secret).transpose()?;
        return Ok(Authentication::KeyFile {
            path: PathBuf::from(key),
            passphrase,
        });
    }
    match options.password_env.as_deref() {
        Some(name) => Ok(Authentication::Password(secret(name)?)),
        None if remote.username == ANONYMOUS && !remote.protocol.supports_key_files() => {
            Ok(Authentication::Password(format!("{ANONYMOUS}@")))
        }
        None => Err("sign in needs --key <file> or --password-env <name>".to_string()),
    }
}

// Sites

fn load_sites() -> Result<SiteList, String> {
    let directory = config_dir().ok_or("no Hoplix configuration folder")?;
    SiteList::load(&directory).map_err(|error| format!("the sites could not be read: {error}"))
}

fn find_site<'a>(sites: &'a SiteList, target: &SiteTarget) -> Result<&'a Site, String> {
    sites.find(&target.reference).map_err(|matches| {
        if matches.is_empty() {
            format!(
                "no saved site is called {} (see hoplix-cli sites)",
                target.reference
            )
        } else {
            let names: Vec<String> = matches.iter().map(|site| site.display_path()).collect();
            format!(
                "several sites are called {}: {}. Use folder/name (with %2F) instead",
                target.reference,
                names.join(", ")
            )
        }
    })
}

async fn site_params(site: &Site, options: &Options) -> Result<ConnectionParams, String> {
    let mut params = site.to_params(None);
    if let Some(key) = &options.key {
        let passphrase = options.passphrase_env.as_deref().map(secret).transpose()?;
        params.authentication = Authentication::KeyFile {
            path: PathBuf::from(key),
            passphrase,
        };
        return Ok(params);
    }
    if let Some(name) = &options.password_env {
        params.authentication = Authentication::Password(secret(name)?);
        return Ok(params);
    }
    let saved = if site.save_secrets {
        saved_secret(site, options).await?
    } else {
        None
    };
    if saved.is_none() && !site.uses_key_file() {
        return Err(format!(
            "no password is saved for the site {}: pass --password-env <name>",
            site.name
        ));
    }
    Ok(site.to_params(saved))
}

async fn saved_secret(site: &Site, options: &Options) -> Result<Option<String>, String> {
    let directory = config_dir().ok_or("no Hoplix configuration folder")?;
    let name = secret_name(&site.id);
    match SecretBackend::detect(&directory) {
        SecretBackend::Keyring => Keyring::default().get(&name).await.map_err(describe),
        SecretBackend::Vault => {
            let variable = options
                .master_password_env
                .as_deref()
                .ok_or_else(|| describe(Error::new(ErrorKind::VaultLocked, "")))?;
            let vault = unlock_vault(directory, secret(variable)?)
                .await
                .map_err(describe)?;
            vault.get(&name).map_err(describe)
        }
    }
}

fn secret(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("the environment variable {name} is not set"))
}

// Errors

fn describe(error: hoplix_core::Error) -> String {
    let detail = error.detail();
    match error.kind() {
        ErrorKind::NotFound => format!("not found: {detail}"),
        ErrorKind::PermissionDenied => format!("permission denied: {detail}"),
        ErrorKind::NotADirectory => format!("not a folder: {detail}"),
        ErrorKind::AlreadyExists => format!("already exists: {detail}"),
        ErrorKind::DirectoryNotEmpty => format!("folder is not empty (use rm -r): {detail}"),
        ErrorKind::ConnectionFailed => format!("connection failed: {detail}"),
        ErrorKind::AuthenticationFailed => format!("sign-in failed for {detail}"),
        ErrorKind::InvalidKey => format!("the key file could not be read: {detail}"),
        ErrorKind::UnknownHostKey => format!("unknown server key: {detail}"),
        ErrorKind::UntrustedCertificate => format!("untrusted certificate: {detail}"),
        ErrorKind::CertificateChanged => format!(
            "WARNING: the server certificate has changed ({detail}) and is not signed by a trusted authority. Someone could be intercepting the connection. Remove the old entry from the trusted certificates file if the change is expected"
        ),
        ErrorKind::HostKeyChanged => format!(
            "WARNING: the server key has changed ({detail}). Someone could be intercepting the connection. Remove the old entry from the known hosts file if the change is expected"
        ),
        ErrorKind::WrongMasterPassword => "the master password is wrong".to_string(),
        ErrorKind::VaultLocked => {
            "the saved passwords are locked: pass --master-password-env <name>".to_string()
        }
        ErrorKind::SecretStorage => format!("the system keyring failed: {detail}"),
        ErrorKind::Other => detail.to_string(),
    }
}
