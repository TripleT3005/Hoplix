use std::cmp::Ordering;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::{
    ConflictPolicy, Entry, ErrorKind, FileSystem, JobId, JobOptions, Result, TransferQueue,
    TransferRequest, compare_names,
};

// Constants

pub const DEFAULT_TIME_TOLERANCE: Duration = Duration::from_secs(2);
const MAX_DEPTH: usize = 64;

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareStatus {
    OnlyLeft,
    OnlyRight,
    NewerLeft,
    NewerRight,
    SizeDiffers,
    KindDiffers,
    Equal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    text: String,
    folded: Vec<char>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareOptions {
    pub recursive: bool,
    pub size_only: bool,
    pub time_tolerance: Duration,
    pub skip_hidden: bool,
    pub exclude: Vec<Pattern>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparedEntry {
    pub path: String,
    pub name: String,
    pub left: Option<Entry>,
    pub right: Option<Entry>,
    pub status: CompareStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncDirection {
    #[default]
    LeftToRight,
    RightToLeft,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
    #[default]
    Update,
    Mirror,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncOptions {
    pub compare: CompareOptions,
    pub direction: SyncDirection,
    pub mode: SyncMode,
    pub delete_extra: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    CreateDirectory {
        side: Side,
        path: String,
    },
    Copy {
        from: Side,
        path: String,
        size: u64,
        modified: Option<SystemTime>,
    },
    Delete {
        side: Side,
        path: String,
        is_directory: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncPlan {
    pub actions: Vec<SyncAction>,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncSummary {
    pub to_right: usize,
    pub to_left: usize,
    pub bytes_to_right: u64,
    pub bytes_to_left: u64,
    pub directories: usize,
    pub deletions: usize,
    pub conflicts: usize,
}

#[derive(Debug, Clone)]
pub struct SyncEndpoint {
    pub file_system: Arc<dyn FileSystem>,
    pub path: String,
}

// Statuses

impl CompareStatus {
    pub fn is_difference(self) -> bool {
        self != Self::Equal
    }

    #[must_use]
    pub fn mirrored(self) -> Self {
        match self {
            Self::OnlyLeft => Self::OnlyRight,
            Self::OnlyRight => Self::OnlyLeft,
            Self::NewerLeft => Self::NewerRight,
            Self::NewerRight => Self::NewerLeft,
            other => other,
        }
    }
}

impl Side {
    #[must_use]
    pub fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

impl ComparedEntry {
    pub fn is_directory(&self) -> bool {
        self.left
            .as_ref()
            .or(self.right.as_ref())
            .is_some_and(Entry::is_directory)
    }

    pub fn depth(&self) -> usize {
        self.path.matches('/').count()
    }

    fn source(&self, side: Side) -> Option<&Entry> {
        match side {
            Side::Left => self.left.as_ref(),
            Side::Right => self.right.as_ref(),
        }
    }
}

// Patterns

impl Pattern {
    pub fn new(text: &str) -> Self {
        let text = text.trim().to_string();
        let folded = text.chars().flat_map(char::to_lowercase).collect();
        Self { text, folded }
    }

    pub fn parse_list(text: &str) -> Vec<Self> {
        text.split([';', ',', '\n'])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(Self::new)
            .collect()
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn matches(&self, candidate: &str) -> bool {
        let candidate: Vec<char> = candidate.chars().flat_map(char::to_lowercase).collect();
        glob(&self.folded, &candidate)
    }
}

fn glob(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some(&c) if c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

// Options

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            recursive: false,
            size_only: false,
            time_tolerance: DEFAULT_TIME_TOLERANCE,
            skip_hidden: false,
            exclude: Vec::new(),
        }
    }
}

impl CompareOptions {
    pub fn includes(&self, name: &str, path: &str) -> bool {
        if self.skip_hidden && name.starts_with('.') {
            return false;
        }
        !self.exclude.iter().any(|pattern| {
            pattern.matches(name) || (pattern.as_str().contains('/') && pattern.matches(path))
        })
    }

    fn status(&self, left: Option<&Entry>, right: Option<&Entry>) -> CompareStatus {
        let (left, right) = match (left, right) {
            (Some(left), Some(right)) => (left, right),
            (Some(_), None) => return CompareStatus::OnlyLeft,
            (None, _) => return CompareStatus::OnlyRight,
        };
        if left.is_directory() != right.is_directory() {
            return CompareStatus::KindDiffers;
        }
        if left.is_directory() {
            return CompareStatus::Equal;
        }
        if !self.size_only
            && let (Some(a), Some(b)) = (left.modified, right.modified)
        {
            let (a, b) = coarsen(a, b);
            if newer(a, b, self.time_tolerance) {
                return CompareStatus::NewerLeft;
            }
            if newer(b, a, self.time_tolerance) {
                return CompareStatus::NewerRight;
            }
        }
        match (left.size, right.size) {
            (Some(a), Some(b)) if a != b => CompareStatus::SizeDiffers,
            _ => CompareStatus::Equal,
        }
    }
}

fn coarsen(a: SystemTime, b: SystemTime) -> (SystemTime, SystemTime) {
    let unit = precision(a).max(precision(b));
    (truncate(a, unit), truncate(b, unit))
}

fn precision(time: SystemTime) -> u64 {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(elapsed) if elapsed.subsec_nanos() == 0 && elapsed.as_secs() % 86_400 == 0 => 86_400,
        Ok(elapsed) if elapsed.subsec_nanos() == 0 && elapsed.as_secs() % 60 == 0 => 60,
        _ => 1,
    }
}

fn truncate(time: SystemTime, unit: u64) -> SystemTime {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(time, |elapsed| {
            SystemTime::UNIX_EPOCH + Duration::from_secs(elapsed.as_secs() / unit * unit)
        })
}

fn newer(a: SystemTime, b: SystemTime, tolerance: Duration) -> bool {
    a.duration_since(b)
        .is_ok_and(|difference| difference > tolerance)
}

// Comparing

pub fn compare_entries(
    prefix: &str,
    left: &[Entry],
    right: &[Entry],
    options: &CompareOptions,
) -> Vec<ComparedEntry> {
    let path_of = |name: &str| {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        }
    };
    let mut names: Vec<&str> = left
        .iter()
        .chain(right)
        .map(|entry| entry.name.as_str())
        .filter(|name| options.includes(name, &path_of(name)))
        .collect();
    names.sort_unstable();
    names.dedup();
    let mut result: Vec<ComparedEntry> = names
        .into_iter()
        .map(|name| {
            let left = left.iter().find(|entry| entry.name == name).cloned();
            let right = right.iter().find(|entry| entry.name == name).cloned();
            let status = options.status(left.as_ref(), right.as_ref());
            ComparedEntry {
                path: path_of(name),
                name: name.to_string(),
                left,
                right,
                status,
            }
        })
        .collect();
    result.sort_by(|a, b| {
        b.is_directory()
            .cmp(&a.is_directory())
            .then_with(|| compare_names(&a.name, &b.name))
    });
    result
}

pub async fn compare(
    left: &SyncEndpoint,
    right: &SyncEndpoint,
    options: &CompareOptions,
) -> Result<Vec<ComparedEntry>> {
    let mut result = Vec::new();
    let mut pending = VecDeque::from([(
        String::new(),
        Some(left.path.clone()),
        Some(right.path.clone()),
        0,
    )]);
    while let Some((prefix, left_path, right_path, depth)) = pending.pop_front() {
        let left_entries = match &left_path {
            Some(path) => left.file_system.list(path).await?,
            None => Vec::new(),
        };
        let right_entries = match &right_path {
            Some(path) => right.file_system.list(path).await?,
            None => Vec::new(),
        };
        let entries = compare_entries(&prefix, &left_entries, &right_entries, options);
        if options.recursive && depth < MAX_DEPTH {
            for entry in &entries {
                if entry.status == CompareStatus::KindDiffers || !entry.is_directory() {
                    continue;
                }
                let walkable = [&entry.left, &entry.right]
                    .into_iter()
                    .flatten()
                    .all(|side| side.is_directory() && !side.is_link);
                if !walkable {
                    continue;
                }
                let child =
                    |side: &Option<Entry>, base: &Option<String>, endpoint: &SyncEndpoint| {
                        side.as_ref()
                            .and(base.as_ref())
                            .map(|base| endpoint.file_system.join(base, &entry.name))
                    };
                pending.push_back((
                    entry.path.clone(),
                    child(&entry.left, &left_path, left),
                    child(&entry.right, &right_path, right),
                    depth + 1,
                ));
            }
        }
        result.extend(entries);
    }
    result.sort_by(|a, b| compare_paths(&a.path, &b.path));
    Ok(result)
}

pub fn refine(entries: &[ComparedEntry], options: &CompareOptions) -> Vec<ComparedEntry> {
    entries
        .iter()
        .filter(|entry| options.recursive || !entry.path.contains('/'))
        .filter(|entry| {
            let mut prefix = String::new();
            entry.path.split('/').all(|part| {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                options.includes(part, &prefix)
            })
        })
        .map(|entry| ComparedEntry {
            status: options.status(entry.left.as_ref(), entry.right.as_ref()),
            ..entry.clone()
        })
        .collect()
}

fn compare_paths(a: &str, b: &str) -> Ordering {
    let mut left = a.split('/');
    let mut right = b.split('/');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ordering = compare_names(x, y);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

// Planning

pub fn build_plan(entries: &[ComparedEntry], options: &SyncOptions) -> SyncPlan {
    let mut deletions = Vec::new();
    let mut directories = Vec::new();
    let mut copies = Vec::new();
    let mut conflicts = Vec::new();
    for entry in entries {
        if entry.is_directory() && !options.compare.recursive {
            continue;
        }
        let decision = match options.direction {
            SyncDirection::LeftToRight => one_way(entry, Side::Left, options),
            SyncDirection::RightToLeft => one_way(entry, Side::Right, options),
            SyncDirection::Both => both_ways(entry),
        };
        match decision {
            Decision::Nothing => {}
            Decision::Conflict => conflicts.push(entry.path.clone()),
            Decision::Copy(from) if entry.is_directory() => {
                directories.push(SyncAction::CreateDirectory {
                    side: from.other(),
                    path: entry.path.clone(),
                });
            }
            Decision::Copy(from) => copies.push(SyncAction::Copy {
                from,
                path: entry.path.clone(),
                size: entry
                    .source(from)
                    .and_then(|source| source.size)
                    .unwrap_or(0),
                modified: entry.source(from).and_then(|source| source.modified),
            }),
            Decision::Delete(side) => deletions.push((
                entry.depth(),
                SyncAction::Delete {
                    side,
                    path: entry.path.clone(),
                    is_directory: entry.is_directory(),
                },
            )),
        }
    }
    deletions.sort_by_key(|(depth, action)| {
        let is_directory = matches!(
            action,
            SyncAction::Delete {
                is_directory: true,
                ..
            }
        );
        (is_directory, is_directory.then(|| usize::MAX - depth))
    });
    let mut actions: Vec<SyncAction> = deletions.into_iter().map(|(_, action)| action).collect();
    actions.extend(directories);
    actions.extend(copies);
    SyncPlan { actions, conflicts }
}

enum Decision {
    Nothing,
    Copy(Side),
    Delete(Side),
    Conflict,
}

fn one_way(entry: &ComparedEntry, source: Side, options: &SyncOptions) -> Decision {
    let status = match source {
        Side::Left => entry.status,
        Side::Right => entry.status.mirrored(),
    };
    let mirror = options.mode == SyncMode::Mirror;
    match status {
        CompareStatus::OnlyLeft | CompareStatus::NewerLeft | CompareStatus::SizeDiffers => {
            Decision::Copy(source)
        }
        CompareStatus::NewerRight if mirror => Decision::Copy(source),
        CompareStatus::OnlyRight if mirror && options.delete_extra => {
            Decision::Delete(source.other())
        }
        CompareStatus::KindDiffers => Decision::Conflict,
        CompareStatus::NewerRight | CompareStatus::OnlyRight | CompareStatus::Equal => {
            Decision::Nothing
        }
    }
}

fn both_ways(entry: &ComparedEntry) -> Decision {
    match entry.status {
        CompareStatus::OnlyLeft | CompareStatus::NewerLeft => Decision::Copy(Side::Left),
        CompareStatus::OnlyRight | CompareStatus::NewerRight => Decision::Copy(Side::Right),
        CompareStatus::SizeDiffers | CompareStatus::KindDiffers => Decision::Conflict,
        CompareStatus::Equal => Decision::Nothing,
    }
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn has_deletions(&self) -> bool {
        self.actions
            .iter()
            .any(|action| matches!(action, SyncAction::Delete { .. }))
    }

    pub fn summary(&self) -> SyncSummary {
        let mut summary = SyncSummary {
            conflicts: self.conflicts.len(),
            ..SyncSummary::default()
        };
        for action in &self.actions {
            match action {
                SyncAction::CreateDirectory { .. } => summary.directories += 1,
                SyncAction::Delete { .. } => summary.deletions += 1,
                SyncAction::Copy {
                    from: Side::Left,
                    size,
                    ..
                } => {
                    summary.to_right += 1;
                    summary.bytes_to_right += size;
                }
                SyncAction::Copy {
                    from: Side::Right,
                    size,
                    ..
                } => {
                    summary.to_left += 1;
                    summary.bytes_to_left += size;
                }
            }
        }
        summary
    }
}

impl SyncSummary {
    pub fn total_bytes(&self) -> u64 {
        self.bytes_to_right + self.bytes_to_left
    }
}

// Running

impl SyncEndpoint {
    pub fn new(file_system: Arc<dyn FileSystem>, path: impl Into<String>) -> Self {
        Self {
            file_system,
            path: path.into(),
        }
    }

    pub fn resolve(&self, relative: &str) -> String {
        relative
            .split('/')
            .filter(|part| !part.is_empty())
            .fold(self.path.clone(), |base, part| {
                self.file_system.join(&base, part)
            })
    }
}

pub async fn plan(
    left: &SyncEndpoint,
    right: &SyncEndpoint,
    options: &SyncOptions,
) -> Result<SyncPlan> {
    let entries = compare(left, right, &options.compare).await?;
    Ok(build_plan(&entries, options))
}

pub async fn execute_plan(
    plan: &SyncPlan,
    left: &SyncEndpoint,
    right: &SyncEndpoint,
    queue: &TransferQueue,
) -> Result<Vec<JobId>> {
    let endpoint = |side: Side| match side {
        Side::Left => left,
        Side::Right => right,
    };
    let mut jobs = Vec::new();
    for action in &plan.actions {
        match action {
            SyncAction::Delete {
                side,
                path,
                is_directory,
            } => {
                let target = endpoint(*side);
                let full = target.resolve(path);
                let result = if *is_directory {
                    target.file_system.remove_directory(&full).await
                } else {
                    target.file_system.remove_file(&full).await
                };
                match result {
                    Err(error)
                        if !matches!(
                            error.kind(),
                            ErrorKind::DirectoryNotEmpty | ErrorKind::NotFound
                        ) =>
                    {
                        return Err(error);
                    }
                    _ => {}
                }
            }
            SyncAction::CreateDirectory { side, path } => {
                let target = endpoint(*side);
                let full = target.resolve(path);
                if let Err(error) = target.file_system.create_directory(&full).await {
                    match target.file_system.metadata(&full).await {
                        Ok(existing) if existing.is_directory() => {}
                        _ => return Err(error),
                    }
                }
            }
            SyncAction::Copy {
                from,
                path,
                modified,
                ..
            } => {
                let source = endpoint(*from);
                let target = endpoint(from.other());
                let (directory, name) = match path.rsplit_once('/') {
                    Some((parent, name)) => (target.resolve(parent), name),
                    None => (target.path.clone(), path.as_str()),
                };
                let request = TransferRequest {
                    source: Arc::clone(&source.file_system),
                    source_path: source.resolve(path),
                    target: Arc::clone(&target.file_system),
                    target_directory: directory,
                    target_name: Some(name.to_string()),
                };
                jobs.push(queue.add_with(
                    request,
                    JobOptions {
                        policy: Some(ConflictPolicy::Overwrite),
                        keep_modified: true,
                        modified: *modified,
                    },
                ));
            }
        }
    }
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use tokio::sync::mpsc;
    use tokio::time::timeout;

    use super::*;
    use crate::{
        BoxFuture, EntryKind, Error, JobStatus, LocalFileSystem, Reader, TransferEvent, Writer,
    };

    // Fakes

    #[derive(Debug, Default)]
    struct MemoryFileSystem {
        folders: HashMap<String, Vec<Entry>>,
    }

    impl MemoryFileSystem {
        fn with(mut self, folder: &str, entries: Vec<Entry>) -> Self {
            self.folders.insert(folder.to_string(), entries);
            self
        }
    }

    fn unsupported<T: Send + 'static>(path: &str) -> BoxFuture<'static, Result<T>> {
        let path = path.to_string();
        Box::pin(async move { Err(Error::new(ErrorKind::Other, path)) })
    }

    impl FileSystem for MemoryFileSystem {
        fn home(&self) -> BoxFuture<'_, Result<String>> {
            Box::pin(async { Ok("/".to_string()) })
        }

        fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
            let found = self
                .folders
                .get(path)
                .cloned()
                .ok_or_else(|| Error::new(ErrorKind::NotFound, path));
            Box::pin(async move { found })
        }

        fn metadata(&self, path: &str) -> BoxFuture<'_, Result<Entry>> {
            unsupported(path)
        }

        fn create_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
            unsupported(path)
        }

        fn remove_file(&self, path: &str) -> BoxFuture<'_, Result<()>> {
            unsupported(path)
        }

        fn remove_directory(&self, path: &str) -> BoxFuture<'_, Result<()>> {
            unsupported(path)
        }

        fn rename(&self, from: &str, _to: &str) -> BoxFuture<'_, Result<()>> {
            unsupported(from)
        }

        fn open_read(&self, path: &str, _offset: u64) -> BoxFuture<'_, Result<Reader>> {
            unsupported(path)
        }

        fn open_write(&self, path: &str, _offset: u64) -> BoxFuture<'_, Result<Writer>> {
            unsupported(path)
        }

        fn join(&self, base: &str, name: &str) -> String {
            format!("{}/{name}", base.trim_end_matches('/'))
        }

        fn parent(&self, path: &str) -> Option<String> {
            path.rsplit_once('/').map(|(parent, _)| parent.to_string())
        }
    }

    // Helpers

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_001 + seconds)
    }

    fn file(name: &str, size: u64, seconds: u64) -> Entry {
        Entry {
            name: name.to_string(),
            kind: EntryKind::File,
            size: Some(size),
            modified: Some(at(seconds)),
            is_link: false,
        }
    }

    fn folder(name: &str) -> Entry {
        Entry {
            name: name.to_string(),
            kind: EntryKind::Directory,
            size: None,
            modified: None,
            is_link: false,
        }
    }

    fn statuses(entries: &[ComparedEntry]) -> Vec<(&str, CompareStatus)> {
        entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.status))
            .collect()
    }

    fn compared(path: &str, status: CompareStatus, is_directory: bool) -> ComparedEntry {
        let entry = if is_directory {
            folder(path.rsplit('/').next().unwrap_or(path))
        } else {
            file(path.rsplit('/').next().unwrap_or(path), 10, 0)
        };
        let (left, right) = match status {
            CompareStatus::OnlyLeft => (Some(entry), None),
            CompareStatus::OnlyRight => (None, Some(entry)),
            _ => (Some(entry.clone()), Some(entry)),
        };
        ComparedEntry {
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            left,
            right,
            status,
        }
    }

    fn options(direction: SyncDirection, mode: SyncMode, delete_extra: bool) -> SyncOptions {
        SyncOptions {
            compare: CompareOptions {
                recursive: true,
                ..CompareOptions::default()
            },
            direction,
            mode,
            delete_extra,
        }
    }

    fn temporary_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-sync-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("creating the test directory failed");
        directory
    }

    fn write(path: &Path, content: &[u8], seconds: u64) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("creating failed");
        }
        std::fs::write(path, content).expect("writing failed");
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|file| file.set_modified(at(seconds)))
            .expect("setting the time failed");
    }

    fn local(path: &Path) -> SyncEndpoint {
        SyncEndpoint::new(Arc::new(LocalFileSystem), path.to_string_lossy())
    }

    async fn wait_for_jobs(events: &mut mpsc::UnboundedReceiver<TransferEvent>, jobs: &[JobId]) {
        let mut finished = Vec::new();
        timeout(Duration::from_secs(20), async {
            while finished.len() < jobs.len() {
                if let Some(TransferEvent::Updated(snapshot)) = events.recv().await
                    && jobs.contains(&snapshot.id)
                    && snapshot.status.is_finished()
                    && !finished.contains(&snapshot.id)
                {
                    assert_eq!(snapshot.status, JobStatus::Completed, "{snapshot:?}");
                    finished.push(snapshot.id);
                }
            }
        })
        .await
        .expect("timed out");
    }

    // Patterns

    #[test]
    fn patterns_match_like_simple_globs() {
        let pattern = Pattern::new("*.TMP");
        assert!(pattern.matches("cache.tmp"));
        assert!(pattern.matches(".tmp"));
        assert!(!pattern.matches("cache.tmp.txt"));
        assert!(Pattern::new("report-??.pdf").matches("report-07.pdf"));
        assert!(!Pattern::new("report-??.pdf").matches("report-7.pdf"));
        assert!(Pattern::new("a*b*c").matches("aXXbYYc"));
        assert!(!Pattern::new("a*b*c").matches("aXXbYY"));
        assert!(Pattern::new("*").matches(""));
        assert_eq!(
            Pattern::parse_list(" *.tmp; node_modules ,,.git\n")
                .iter()
                .map(Pattern::as_str)
                .collect::<Vec<_>>(),
            ["*.tmp", "node_modules", ".git"]
        );
    }

    // Comparing

    #[test]
    fn times_within_the_tolerance_count_as_equal() {
        let left = [file("same.txt", 5, 100), file("newer.txt", 5, 110)];
        let right = [file("same.txt", 5, 101), file("newer.txt", 5, 100)];
        let result = compare_entries("", &left, &right, &CompareOptions::default());
        assert_eq!(
            statuses(&result),
            [
                ("newer.txt", CompareStatus::NewerLeft),
                ("same.txt", CompareStatus::Equal)
            ]
        );
        let strict = CompareOptions {
            time_tolerance: Duration::ZERO,
            ..CompareOptions::default()
        };
        let result = compare_entries("", &right, &left, &strict);
        assert_eq!(
            statuses(&result),
            [
                ("newer.txt", CompareStatus::NewerRight),
                ("same.txt", CompareStatus::NewerLeft)
            ]
        );
    }

    #[test]
    fn coarse_listing_times_are_compared_at_their_precision() {
        let exact = |seconds: u64| Entry {
            modified: Some(
                SystemTime::UNIX_EPOCH
                    + Duration::from_millis(1_700_000_000_000 + seconds * 1000 + 250),
            ),
            ..file("a.txt", 5, 0)
        };
        let listed = |seconds: u64| Entry {
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
            ..file("a.txt", 5, 0)
        };
        let options = CompareOptions::default();
        let minute = 1_700_000_040;
        assert_eq!(
            options.status(Some(&exact(40 + 59)), Some(&listed(minute))),
            CompareStatus::Equal
        );
        assert_eq!(
            options.status(Some(&exact(40 + 61)), Some(&listed(minute))),
            CompareStatus::NewerLeft
        );
        let day = 1_699_920_000;
        assert_eq!(
            options.status(Some(&listed(day)), Some(&exact(3600))),
            CompareStatus::Equal
        );
        assert_eq!(
            options.status(Some(&exact(10)), Some(&exact(0))),
            CompareStatus::NewerLeft
        );
    }

    #[test]
    fn size_only_ignores_times() {
        let left = [file("a.txt", 5, 0), file("b.txt", 6, 0)];
        let right = [file("a.txt", 5, 500), file("b.txt", 7, 500)];
        let options = CompareOptions {
            size_only: true,
            ..CompareOptions::default()
        };
        assert_eq!(
            statuses(&compare_entries("", &left, &right, &options)),
            [
                ("a.txt", CompareStatus::Equal),
                ("b.txt", CompareStatus::SizeDiffers)
            ]
        );
        let unknown = Entry {
            modified: None,
            ..file("a.txt", 9, 0)
        };
        assert_eq!(
            compare_entries("", &[unknown], &right, &CompareOptions::default())[0].status,
            CompareStatus::SizeDiffers
        );
    }

    #[test]
    fn missing_entries_kinds_hidden_files_and_exclusions() {
        let left = [
            folder("docs"),
            file("only-left.txt", 1, 0),
            file("clash", 1, 0),
            file(".hidden", 1, 0),
            file("cache.tmp", 1, 0),
        ];
        let right = [
            folder("clash"),
            file("only-right.txt", 1, 0),
            folder("docs"),
        ];
        let options = CompareOptions {
            skip_hidden: true,
            exclude: Pattern::parse_list("*.tmp"),
            ..CompareOptions::default()
        };
        assert_eq!(
            statuses(&compare_entries("sub", &left, &right, &options)),
            [
                ("sub/docs", CompareStatus::Equal),
                ("sub/clash", CompareStatus::KindDiffers),
                ("sub/only-left.txt", CompareStatus::OnlyLeft),
                ("sub/only-right.txt", CompareStatus::OnlyRight),
            ]
        );
        let by_path = CompareOptions {
            exclude: Pattern::parse_list("sub/only-*"),
            ..CompareOptions::default()
        };
        assert_eq!(
            compare_entries("sub", &left, &right, &by_path).len(),
            4,
            "paths with a slash are matched against the whole path"
        );
    }

    #[tokio::test]
    async fn fake_trees_compare_recursively() {
        let left = MemoryFileSystem::default()
            .with(
                "/l",
                vec![folder("both"), folder("new"), file("top.txt", 3, 0)],
            )
            .with("/l/both", vec![file("inner.txt", 4, 50)])
            .with("/l/new", vec![file("deep.txt", 1, 0)]);
        let right = MemoryFileSystem::default()
            .with("/r", vec![folder("both"), file("top.txt", 3, 0)])
            .with("/r/both", vec![file("inner.txt", 4, 0)]);
        let left = SyncEndpoint::new(Arc::new(left), "/l");
        let right = SyncEndpoint::new(Arc::new(right), "/r");
        let flat = compare(&left, &right, &CompareOptions::default())
            .await
            .expect("comparing failed");
        assert_eq!(flat.len(), 3);
        let options = CompareOptions {
            recursive: true,
            ..CompareOptions::default()
        };
        let tree = compare(&left, &right, &options)
            .await
            .expect("comparing failed");
        assert_eq!(
            statuses(&tree),
            [
                ("both", CompareStatus::Equal),
                ("both/inner.txt", CompareStatus::NewerLeft),
                ("new", CompareStatus::OnlyLeft),
                ("new/deep.txt", CompareStatus::OnlyLeft),
                ("top.txt", CompareStatus::Equal),
            ]
        );
        assert_eq!(left.resolve("both/inner.txt"), "/l/both/inner.txt");

        let refined = refine(
            &tree,
            &CompareOptions {
                recursive: true,
                size_only: true,
                exclude: Pattern::parse_list("new"),
                ..CompareOptions::default()
            },
        );
        assert_eq!(
            statuses(&refined),
            [
                ("both", CompareStatus::Equal),
                ("both/inner.txt", CompareStatus::Equal),
                ("top.txt", CompareStatus::Equal),
            ]
        );
        assert_eq!(refine(&tree, &CompareOptions::default()).len(), 3);
    }

    // Planning

    fn sample() -> Vec<ComparedEntry> {
        vec![
            compared("new", CompareStatus::OnlyLeft, true),
            compared("new/a.txt", CompareStatus::OnlyLeft, false),
            compared("extra", CompareStatus::OnlyRight, true),
            compared("extra/b.txt", CompareStatus::OnlyRight, false),
            compared("left-newer.txt", CompareStatus::NewerLeft, false),
            compared("right-newer.txt", CompareStatus::NewerRight, false),
            compared("size.txt", CompareStatus::SizeDiffers, false),
            compared("clash", CompareStatus::KindDiffers, false),
            compared("same.txt", CompareStatus::Equal, false),
        ]
    }

    #[test]
    fn update_copies_new_and_newer_files_only() {
        let plan = build_plan(
            &sample(),
            &options(SyncDirection::LeftToRight, SyncMode::Update, true),
        );
        assert_eq!(
            plan.actions,
            [
                SyncAction::CreateDirectory {
                    side: Side::Right,
                    path: "new".to_string()
                },
                SyncAction::Copy {
                    from: Side::Left,
                    path: "new/a.txt".to_string(),
                    size: 10,
                    modified: Some(at(0))
                },
                SyncAction::Copy {
                    from: Side::Left,
                    path: "left-newer.txt".to_string(),
                    size: 10,
                    modified: Some(at(0))
                },
                SyncAction::Copy {
                    from: Side::Left,
                    path: "size.txt".to_string(),
                    size: 10,
                    modified: Some(at(0))
                },
            ]
        );
        assert_eq!(plan.conflicts, ["clash"]);
        assert!(!plan.has_deletions());
    }

    #[test]
    fn mirror_overwrites_newer_targets_and_deletes_only_when_asked() {
        let keep = build_plan(
            &sample(),
            &options(SyncDirection::LeftToRight, SyncMode::Mirror, false),
        );
        assert!(!keep.has_deletions());
        assert!(keep.actions.contains(&SyncAction::Copy {
            from: Side::Left,
            path: "right-newer.txt".to_string(),
            size: 10,
            modified: Some(at(0))
        }));

        let plan = build_plan(
            &sample(),
            &options(SyncDirection::LeftToRight, SyncMode::Mirror, true),
        );
        assert_eq!(
            plan.actions[..2],
            [
                SyncAction::Delete {
                    side: Side::Right,
                    path: "extra/b.txt".to_string(),
                    is_directory: false
                },
                SyncAction::Delete {
                    side: Side::Right,
                    path: "extra".to_string(),
                    is_directory: true
                },
            ]
        );
        let summary = plan.summary();
        assert_eq!(summary.to_right, 4);
        assert_eq!(summary.to_left, 0);
        assert_eq!(summary.bytes_to_right, 40);
        assert_eq!(summary.deletions, 2);
        assert_eq!(summary.directories, 1);
        assert_eq!(summary.conflicts, 1);
    }

    #[test]
    fn right_to_left_mirrors_the_rules() {
        let plan = build_plan(
            &sample(),
            &options(SyncDirection::RightToLeft, SyncMode::Mirror, true),
        );
        let summary = plan.summary();
        assert_eq!(summary.to_left, 4);
        assert_eq!(summary.to_right, 0);
        assert!(plan.actions.contains(&SyncAction::Delete {
            side: Side::Left,
            path: "new/a.txt".to_string(),
            is_directory: false
        }));
        assert!(plan.actions.contains(&SyncAction::CreateDirectory {
            side: Side::Left,
            path: "extra".to_string()
        }));
    }

    #[test]
    fn both_ways_lets_the_newer_file_win_and_never_deletes() {
        let plan = build_plan(
            &sample(),
            &options(SyncDirection::Both, SyncMode::Mirror, true),
        );
        assert!(!plan.has_deletions());
        assert!(plan.actions.contains(&SyncAction::Copy {
            from: Side::Right,
            path: "right-newer.txt".to_string(),
            size: 10,
            modified: Some(at(0))
        }));
        assert!(plan.actions.contains(&SyncAction::Copy {
            from: Side::Left,
            path: "left-newer.txt".to_string(),
            size: 10,
            modified: Some(at(0))
        }));
        assert_eq!(plan.conflicts, ["size.txt", "clash"]);
        let summary = plan.summary();
        assert_eq!((summary.to_right, summary.to_left), (2, 2));
        assert_eq!(summary.directories, 2);
    }

    #[test]
    fn flat_plans_leave_folders_alone() {
        let mut options = options(SyncDirection::LeftToRight, SyncMode::Mirror, true);
        options.compare.recursive = false;
        let entries = [
            compared("new", CompareStatus::OnlyLeft, true),
            compared("extra", CompareStatus::OnlyRight, true),
            compared("a.txt", CompareStatus::OnlyLeft, false),
        ];
        let plan = build_plan(&entries, &options);
        assert_eq!(plan.actions.len(), 1);
        assert!(build_plan(&[], &options).is_empty());
    }

    // Running

    #[tokio::test(flavor = "multi_thread")]
    async fn plans_run_through_the_queue_and_keep_times() {
        let directory = temporary_directory("run");
        let left = directory.join("left");
        let right = directory.join("right");
        write(&left.join("new/deep/a.txt"), b"alpha", 10);
        write(&left.join("changed.txt"), b"new content", 500);
        write(&right.join("changed.txt"), b"old", 100);
        write(&left.join("same.txt"), b"same", 100);
        write(&right.join("same.txt"), b"same", 100);
        write(&right.join("extra/old.txt"), b"x", 0);
        write(&right.join("extra.log"), b"x", 0);
        write(&left.join("skip.tmp"), b"x", 0);
        let (left, right) = (local(&left), local(&right));
        let mut options = options(SyncDirection::LeftToRight, SyncMode::Mirror, true);
        options.compare.exclude = Pattern::parse_list("*.tmp");

        let plan = plan(&left, &right, &options)
            .await
            .expect("planning failed");
        let summary = plan.summary();
        assert_eq!(summary.to_right, 2);
        assert_eq!(summary.deletions, 3);
        assert_eq!(summary.directories, 2);

        let queue = TransferQueue::default();
        let mut events = queue.subscribe();
        tokio::spawn(queue.clone().drive());
        let jobs = execute_plan(&plan, &left, &right, &queue)
            .await
            .expect("running failed");
        assert_eq!(jobs.len(), 2);
        wait_for_jobs(&mut events, &jobs).await;

        let root = directory.join("right");
        assert_eq!(
            std::fs::read(root.join("new/deep/a.txt")).expect("reading failed"),
            b"alpha"
        );
        assert_eq!(
            std::fs::read(root.join("changed.txt")).expect("reading failed"),
            b"new content"
        );
        assert!(!root.join("extra").exists());
        assert!(!root.join("extra.log").exists());
        assert!(!root.join("skip.tmp").exists());
        assert_eq!(
            std::fs::metadata(root.join("changed.txt"))
                .and_then(|metadata| metadata.modified())
                .expect("reading the time failed"),
            at(500)
        );
        let again = compare(&left, &right, &options.compare)
            .await
            .expect("comparing failed");
        assert!(
            again.iter().all(|entry| !entry.status.is_difference()),
            "{again:?}"
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn both_ways_exchanges_files() {
        let directory = temporary_directory("both");
        let left = directory.join("left");
        let right = directory.join("right");
        write(&left.join("from-left.txt"), b"L", 0);
        write(&right.join("from-right.txt"), b"R", 0);
        write(&left.join("shared.txt"), b"old", 100);
        write(&right.join("shared.txt"), b"newer", 900);
        let (left, right) = (local(&left), local(&right));
        let options = options(SyncDirection::Both, SyncMode::Update, false);
        let plan = plan(&left, &right, &options)
            .await
            .expect("planning failed");
        let queue = TransferQueue::default();
        let mut events = queue.subscribe();
        tokio::spawn(queue.clone().drive());
        let jobs = execute_plan(&plan, &left, &right, &queue)
            .await
            .expect("running failed");
        wait_for_jobs(&mut events, &jobs).await;
        assert_eq!(
            std::fs::read(directory.join("left/shared.txt")).expect("reading failed"),
            b"newer"
        );
        assert!(directory.join("left/from-right.txt").is_file());
        assert!(directory.join("right/from-left.txt").is_file());
        let again = compare(&left, &right, &options.compare)
            .await
            .expect("comparing failed");
        assert!(again.iter().all(|entry| !entry.status.is_difference()));
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
