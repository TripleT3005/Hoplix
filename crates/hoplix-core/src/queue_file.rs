use std::fs;
use std::io;
use std::path::Path;

use crate::key_value::{self, Section};
use crate::sites::write_atomically;
use crate::{ConflictPolicy, RecentConnection, ResumePoint};

// Constants

pub const QUEUE_FILE: &str = "queue.conf";
const JOB_SECTION: &str = "job";
const SOURCE_SECTION: &str = "source";
const TARGET_SECTION: &str = "target";

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Local,
    Site(String),
    Direct(Box<RecentConnection>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedJob {
    pub source: Location,
    pub source_path: String,
    pub target: Location,
    pub target_directory: String,
    pub target_name: Option<String>,
    pub policy: Option<ConflictPolicy>,
    pub keep_modified: bool,
    pub resume: ResumePoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavedQueue {
    pub jobs: Vec<SavedJob>,
}

// Locations

impl Location {
    pub fn is_local(&self) -> bool {
        *self == Self::Local
    }

    fn kind(&self) -> (&'static str, String) {
        match self {
            Self::Local => ("local", String::new()),
            Self::Site(id) => ("site", id.clone()),
            Self::Direct(_) => ("direct", String::new()),
        }
    }
}

fn policy_key(policy: ConflictPolicy) -> &'static str {
    match policy {
        ConflictPolicy::Ask => "ask",
        ConflictPolicy::Overwrite => "overwrite",
        ConflictPolicy::Skip => "skip",
        ConflictPolicy::Resume => "resume",
        ConflictPolicy::Rename => "rename",
    }
}

fn policy_from(key: &str) -> Option<ConflictPolicy> {
    [
        ConflictPolicy::Ask,
        ConflictPolicy::Overwrite,
        ConflictPolicy::Skip,
        ConflictPolicy::Resume,
        ConflictPolicy::Rename,
    ]
    .into_iter()
    .find(|policy| policy_key(*policy) == key)
}

// Serialization

impl SavedJob {
    fn to_text(&self) -> String {
        let (source, source_site) = self.source.kind();
        let (target, target_site) = self.target.kind();
        let mut pairs = vec![
            ("source", source.to_string()),
            ("source_site", source_site),
            ("source_path", self.source_path.clone()),
            ("target", target.to_string()),
            ("target_site", target_site),
            ("target_directory", self.target_directory.clone()),
            ("target_name", self.target_name.clone().unwrap_or_default()),
            (
                "policy",
                self.policy.map(policy_key).unwrap_or_default().to_string(),
            ),
            ("keep_modified", self.keep_modified.to_string()),
            ("next_item", self.resume.next_item.to_string()),
            (
                "checkpoint",
                self.resume.checkpoint.clone().unwrap_or_default(),
            ),
        ];
        for (target, path) in &self.resume.partial {
            pairs.push(("partial_target", target.clone()));
            pairs.push(("partial_path", path.clone()));
        }
        for target in &self.resume.fresh {
            pairs.push(("fresh", target.clone()));
        }
        let mut text = format!("\n[{JOB_SECTION}]\n{}", key_value::write(&pairs));
        for (name, location) in [
            (SOURCE_SECTION, &self.source),
            (TARGET_SECTION, &self.target),
        ] {
            if let Location::Direct(connection) = location {
                text.push_str(&format!(
                    "[{name}]\n{}",
                    key_value::write(&connection.pairs())
                ));
            }
        }
        text
    }

    fn from_sections(
        job: &Section,
        source: Option<&Section>,
        target: Option<&Section>,
    ) -> Option<Self> {
        let location = |kind: &str, site: &str, details: Option<&Section>| -> Option<Location> {
            match job.get(kind)? {
                "local" => Some(Location::Local),
                "site" => job
                    .get(site)
                    .filter(|id| !id.is_empty())
                    .map(|id| Location::Site(id.to_string())),
                "direct" => details
                    .and_then(RecentConnection::from_section)
                    .map(|connection| Location::Direct(Box::new(connection))),
                _ => None,
            }
        };
        let source_path = job.get("source_path").filter(|path| !path.is_empty())?;
        let target_directory = job
            .get("target_directory")
            .filter(|path| !path.is_empty())?;
        let mut partial: Vec<(String, String)> = Vec::new();
        let mut fresh = Vec::new();
        let mut pending_target: Option<String> = None;
        for (key, value) in &job.pairs {
            match key.as_str() {
                "partial_target" => pending_target = Some(value.clone()),
                "partial_path" => {
                    if let Some(target) = pending_target.take()
                        && !value.is_empty()
                    {
                        partial.push((target, value.clone()));
                    }
                }
                "fresh" if !value.is_empty() => fresh.push(value.clone()),
                _ => {}
            }
        }
        let checkpoint = job
            .get("checkpoint")
            .filter(|path| !path.is_empty())
            .map(str::to_string);
        let next_item = job
            .get("next_item")
            .and_then(|value| value.parse().ok())
            .filter(|_| checkpoint.is_some())
            .unwrap_or(0);
        Some(Self {
            source: location("source", "source_site", source)?,
            source_path: source_path.to_string(),
            target: location("target", "target_site", target)?,
            target_directory: target_directory.to_string(),
            target_name: job
                .get("target_name")
                .filter(|name| !name.is_empty())
                .map(str::to_string),
            policy: job.get("policy").and_then(policy_from),
            keep_modified: job.get("keep_modified") == Some("true"),
            resume: ResumePoint {
                next_item,
                checkpoint: checkpoint.filter(|_| next_item > 0),
                partial,
                fresh,
            },
        })
    }
}

impl SavedQueue {
    pub fn parse(text: &str) -> Self {
        let sections = key_value::parse_sections(text);
        let mut jobs = Vec::new();
        let mut index = 0;
        while index < sections.len() {
            if sections[index].name != JOB_SECTION {
                index += 1;
                continue;
            }
            let job = &sections[index];
            let mut source = None;
            let mut target = None;
            index += 1;
            while let Some(section) = sections.get(index) {
                match section.name.as_str() {
                    SOURCE_SECTION if source.is_none() => source = Some(section),
                    TARGET_SECTION if target.is_none() => target = Some(section),
                    _ => break,
                }
                index += 1;
            }
            if let Some(saved) = SavedJob::from_sections(job, source, target) {
                jobs.push(saved);
            }
        }
        Self { jobs }
    }

    pub fn to_text(&self) -> String {
        let mut text = String::from("# Hoplix unfinished transfers (no passwords)\n");
        for job in &self.jobs {
            text.push_str(&job.to_text());
        }
        text
    }

    // Storage

    pub fn load(directory: &Path) -> Self {
        match fs::read(directory.join(QUEUE_FILE)) {
            Ok(bytes) => Self::parse(&String::from_utf8_lossy(&bytes)),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, directory: &Path) -> io::Result<()> {
        let path = directory.join(QUEUE_FILE);
        if self.jobs.is_empty() {
            return match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        write_atomically(&path, self.to_text().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_password;
    use crate::{Authentication, ConnectionParams, Protocol, SiteJump, SiteProxy};
    use std::path::PathBuf;

    fn temporary_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-queue-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("creating failed");
        directory
    }

    fn direct(password: &str) -> RecentConnection {
        let params = ConnectionParams {
            protocol: Protocol::Sftp,
            host: "files.example.test".to_string(),
            port: 2222,
            username: "demo".to_string(),
            authentication: Authentication::KeyFile {
                path: PathBuf::from("C:\\keys\\id_ed25519"),
                passphrase: Some(password.to_string()),
            },
        };
        RecentConnection::from_params(&params).with_route(
            Some(SiteJump {
                host: "bastion.test".to_string(),
                port: 22,
                username: "hop".to_string(),
                auth: crate::SiteAuth::Password,
                key_path: String::new(),
                save_secret: true,
            }),
            Some(SiteProxy {
                kind: crate::ProxyKind::Socks5,
                host: "proxy.test".to_string(),
                port: 1080,
                username: "me".to_string(),
                save_secret: true,
            }),
        )
    }

    fn queue(password: &str) -> SavedQueue {
        SavedQueue {
            jobs: vec![
                SavedJob {
                    source: Location::Local,
                    source_path: "C:\\Data\\new\nline.bin".to_string(),
                    target: Location::Site("abc123".to_string()),
                    target_directory: "/var/www".to_string(),
                    target_name: None,
                    policy: Some(ConflictPolicy::Overwrite),
                    keep_modified: true,
                    resume: ResumePoint {
                        next_item: 0,
                        checkpoint: None,
                        partial: vec![(
                            "/var/www/new\nline.bin".to_string(),
                            "/var/www/new\nline (1).bin".to_string(),
                        )],
                        fresh: vec!["/var/www/new\nline.bin".to_string()],
                    },
                },
                SavedJob {
                    source: Location::Direct(Box::new(direct(password))),
                    source_path: "/home/demo/photos".to_string(),
                    target: Location::Local,
                    target_directory: "D:\\Backup".to_string(),
                    target_name: Some("photos 2026".to_string()),
                    policy: None,
                    keep_modified: false,
                    resume: ResumePoint {
                        next_item: 4,
                        checkpoint: Some("D:\\Backup\\photos 2026\\b.jpg".to_string()),
                        partial: Vec::new(),
                        fresh: Vec::new(),
                    },
                },
            ],
        }
    }

    #[test]
    fn queues_round_trip_without_secrets() {
        let directory = temporary_directory("round-trip");
        let password = test_password();
        let saved = queue(&password);
        saved.save(&directory).expect("saving failed");
        let text = fs::read_to_string(directory.join(QUEUE_FILE)).expect("reading failed");
        assert!(!text.contains(&password));
        assert!(!text.contains("save_secret"));
        assert!(!text.contains("password ="));
        let loaded = SavedQueue::load(&directory);
        assert_eq!(loaded.jobs.len(), 2);
        assert_eq!(loaded.jobs[0], saved.jobs[0]);
        let Location::Direct(connection) = &loaded.jobs[1].source else {
            panic!("the direct connection was lost");
        };
        assert_eq!(connection.host, "files.example.test");
        assert_eq!(connection.port, 2222);
        assert_eq!(connection.key_path, "C:\\keys\\id_ed25519");
        assert!(connection.uses_key_file());
        assert_eq!(
            connection.jump.as_ref().map(|jump| jump.host.as_str()),
            Some("bastion.test")
        );
        assert_eq!(loaded.jobs[1].resume, saved.jobs[1].resume);
        assert_eq!(loaded, SavedQueue::parse(&saved.to_text()));

        SavedQueue::default()
            .save(&directory)
            .expect("saving failed");
        assert!(!directory.join(QUEUE_FILE).exists());
        assert_eq!(SavedQueue::load(&directory), SavedQueue::default());
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn corrupt_files_keep_the_valid_jobs() {
        let directory = temporary_directory("corrupt");
        let password = test_password();
        let mut text = queue(&password).to_text();
        text.push_str(
            "\n[job]\nsource = gopher\nsource_path = /x\ntarget = local\ntarget_directory = /y\n",
        );
        text.push_str(
            "[job]\nsource = direct\nsource_path = /x\ntarget = local\ntarget_directory = /y\n",
        );
        text.push_str(
            "[job]\nsource = site\nsource_path = /x\ntarget = local\ntarget_directory = /y\n",
        );
        text.push_str("[job]\nsource = local\ntarget = local\n");
        text.push_str("garbage without equals\n[target]\nhost = lost\n");
        text.push_str("[job]\nsource = local\nsource_path = /a\ntarget = local\ntarget_directory = /b\nnext_item = many\npartial_path = orphan\n");
        let parsed = SavedQueue::parse(&text);
        assert_eq!(parsed.jobs.len(), 3);
        assert_eq!(parsed.jobs[2].resume, ResumePoint::default());
        assert_eq!(parsed.jobs[2].policy, None);

        fs::write(directory.join(QUEUE_FILE), [0xff, 0xfe, 0x00, b'[', 0x80])
            .expect("writing failed");
        assert_eq!(SavedQueue::load(&directory), SavedQueue::default());
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
