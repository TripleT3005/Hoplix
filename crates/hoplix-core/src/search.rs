use std::collections::VecDeque;
use std::sync::Arc;

use crate::{Entry, Error, FileSystem, Pattern, Result, SortKey, SortOrder, sort_entries};

// Types

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NameFilter {
    #[default]
    All,
    Contains(String),
    Wildcard(Pattern),
}

#[derive(Debug)]
pub struct Visited {
    pub directory: String,
    pub depth: usize,
    pub result: Result<Vec<Entry>>,
}

pub struct Walk {
    file_system: Arc<dyn FileSystem>,
    pending: VecDeque<(String, usize)>,
    max_depth: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub directory: String,
    pub path: String,
    pub entry: Entry,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchBatch {
    pub directory: String,
    pub hits: Vec<SearchHit>,
    pub error: Option<Error>,
}

pub struct Search {
    walk: Walk,
    filter: NameFilter,
}

// Filters

impl NameFilter {
    pub fn new(text: &str) -> Self {
        let text = text.trim();
        if text.is_empty() {
            Self::All
        } else if text.contains(['*', '?']) {
            Self::Wildcard(Pattern::new(text))
        } else {
            Self::Contains(text.to_lowercase())
        }
    }

    pub fn is_all(&self) -> bool {
        *self == Self::All
    }

    pub fn matches(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Contains(part) => name.to_lowercase().contains(part.as_str()),
            Self::Wildcard(pattern) => pattern.matches(name),
        }
    }
}

// Walking

impl Walk {
    pub fn new(file_system: Arc<dyn FileSystem>, root: &str, max_depth: Option<usize>) -> Self {
        Self {
            file_system,
            pending: VecDeque::from([(root.to_string(), 1)]),
            max_depth: max_depth.map(|depth| depth.max(1)),
        }
    }

    pub async fn next(&mut self) -> Option<Visited> {
        let (directory, depth) = self.pending.pop_front()?;
        let result = self.file_system.list(&directory).await.map(|mut entries| {
            sort_entries(&mut entries, SortKey::Name, SortOrder::Ascending);
            entries
        });
        if let Ok(entries) = &result
            && self.max_depth.is_none_or(|limit| depth < limit)
        {
            self.pending.extend(
                entries
                    .iter()
                    .filter(|entry| entry.is_directory() && !entry.is_link)
                    .map(|entry| (self.file_system.join(&directory, &entry.name), depth + 1)),
            );
        }
        Some(Visited {
            directory,
            depth,
            result,
        })
    }
}

impl std::fmt::Debug for Walk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Walk")
            .field("pending", &self.pending)
            .field("max_depth", &self.max_depth)
            .finish_non_exhaustive()
    }
}

// Searching

impl Search {
    pub fn new(
        file_system: Arc<dyn FileSystem>,
        root: &str,
        pattern: &str,
        max_depth: Option<usize>,
    ) -> Self {
        Self {
            walk: Walk::new(file_system, root, max_depth),
            filter: NameFilter::new(pattern),
        }
    }

    pub async fn next_batch(&mut self) -> Option<SearchBatch> {
        let visited = self.walk.next().await?;
        let file_system = Arc::clone(&self.walk.file_system);
        let mut batch = SearchBatch {
            directory: visited.directory.clone(),
            ..SearchBatch::default()
        };
        match visited.result {
            Ok(entries) => {
                batch.hits = entries
                    .into_iter()
                    .filter(|entry| self.filter.matches(&entry.name))
                    .map(|entry| SearchHit {
                        path: file_system.join(&visited.directory, &entry.name),
                        directory: visited.directory.clone(),
                        entry,
                    })
                    .collect();
            }
            Err(error) => batch.error = Some(error),
        }
        Some(batch)
    }
}

impl std::fmt::Debug for Search {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Search")
            .field("walk", &self.walk)
            .field("filter", &self.filter)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BoxFuture, ErrorKind, LocalFileSystem, Reader, Writer};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Fake file system

    #[derive(Debug, Default)]
    struct Tree {
        folders: Vec<(&'static str, Vec<Entry>)>,
        listed: AtomicUsize,
        log: Mutex<Vec<String>>,
    }

    fn file(name: &str) -> Entry {
        Entry {
            name: name.to_string(),
            kind: crate::EntryKind::File,
            size: Some(1),
            modified: None,
            is_link: false,
            permissions: None,
            owner: None,
        }
    }

    fn folder(name: &str) -> Entry {
        Entry {
            kind: crate::EntryKind::Directory,
            size: None,
            ..file(name)
        }
    }

    fn unsupported<T>() -> BoxFuture<'static, Result<T>> {
        Box::pin(async { Err(Error::new(ErrorKind::Unsupported, "fake")) })
    }

    impl FileSystem for Tree {
        fn label(&self) -> Option<String> {
            Some("tree".to_string())
        }

        fn home(&self) -> BoxFuture<'_, Result<String>> {
            Box::pin(async { Ok("/".to_string()) })
        }

        fn list(&self, path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
            self.listed.fetch_add(1, Ordering::SeqCst);
            self.log.lock().expect("poisoned").push(path.to_string());
            let found = self
                .folders
                .iter()
                .find(|(name, _)| *name == path)
                .map(|(_, entries)| entries.clone());
            let path = path.to_string();
            Box::pin(
                async move { found.ok_or_else(|| Error::new(ErrorKind::PermissionDenied, path)) },
            )
        }

        fn metadata(&self, _path: &str) -> BoxFuture<'_, Result<Entry>> {
            unsupported()
        }

        fn create_directory(&self, _path: &str) -> BoxFuture<'_, Result<()>> {
            unsupported()
        }

        fn remove_file(&self, _path: &str) -> BoxFuture<'_, Result<()>> {
            unsupported()
        }

        fn remove_directory(&self, _path: &str) -> BoxFuture<'_, Result<()>> {
            unsupported()
        }

        fn rename(&self, _from: &str, _to: &str) -> BoxFuture<'_, Result<()>> {
            unsupported()
        }

        fn open_read(&self, _path: &str, _offset: u64) -> BoxFuture<'_, Result<Reader>> {
            unsupported()
        }

        fn open_write(&self, _path: &str, _offset: u64) -> BoxFuture<'_, Result<Writer>> {
            unsupported()
        }

        fn join(&self, base: &str, name: &str) -> String {
            crate::sftp::join_posix(base, name)
        }

        fn parent(&self, path: &str) -> Option<String> {
            crate::sftp::parent_posix(path)
        }
    }

    fn tree() -> Arc<Tree> {
        let link = Entry {
            is_link: true,
            ..folder("loop")
        };
        Arc::new(Tree {
            folders: vec![
                (
                    "/",
                    vec![folder("docs"), folder("locked"), file("Report.PDF"), link],
                ),
                (
                    "/docs",
                    vec![folder("old"), file("report-01.txt"), file("notes.md")],
                ),
                ("/docs/old", vec![file("report-00.txt")]),
            ],
            ..Tree::default()
        })
    }

    async fn collect(search: &mut Search) -> (Vec<String>, Vec<String>) {
        let mut paths = Vec::new();
        let mut failed = Vec::new();
        while let Some(batch) = search.next_batch().await {
            paths.extend(batch.hits.into_iter().map(|hit| hit.path));
            if batch.error.is_some() {
                failed.push(batch.directory);
            }
        }
        (paths, failed)
    }

    // Filters

    #[test]
    fn filters_match_parts_or_wildcards_ignoring_case() {
        let part = NameFilter::new(" REP ");
        assert!(part.matches("report.pdf"));
        assert!(part.matches("old-Report"));
        assert!(!part.matches("notes.md"));
        let wildcard = NameFilter::new("*.txt");
        assert!(wildcard.matches("A.TXT"));
        assert!(!wildcard.matches("a.txt.bak"));
        let single = NameFilter::new("report-0?.txt");
        assert!(single.matches("report-01.txt"));
        assert!(!single.matches("report-1.txt"));
        assert!(NameFilter::new("  ").is_all());
        assert!(NameFilter::new("").matches("anything"));
    }

    // Searching

    #[tokio::test]
    async fn searches_walk_every_level_and_report_unreadable_folders() {
        let tree = tree();
        let mut search = Search::new(tree.clone(), "/", "report", None);
        let (paths, failed) = collect(&mut search).await;
        assert_eq!(
            paths,
            [
                "/Report.PDF",
                "/docs/report-01.txt",
                "/docs/old/report-00.txt"
            ]
        );
        assert_eq!(failed, ["/locked"]);
        assert!(
            !tree
                .log
                .lock()
                .expect("poisoned")
                .iter()
                .any(|path| path.contains("loop"))
        );
    }

    #[tokio::test]
    async fn the_depth_limit_stops_descending() {
        let tree = tree();
        let mut search = Search::new(tree.clone(), "/", "*.txt", Some(2));
        let (paths, _) = collect(&mut search).await;
        assert_eq!(paths, ["/docs/report-01.txt"]);
        let mut search = Search::new(tree.clone(), "/", "*", Some(1));
        let (paths, failed) = collect(&mut search).await;
        assert_eq!(paths.len(), 4);
        assert!(failed.is_empty());
    }

    #[tokio::test]
    async fn stopping_a_search_lists_no_further_folders() {
        let tree = tree();
        let mut search = Search::new(tree.clone(), "/", "report", None);
        let first = search.next_batch().await.expect("no batch");
        assert_eq!(first.hits.len(), 1);
        drop(search);
        assert_eq!(tree.listed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn local_folders_are_searched() {
        let directory = std::env::temp_dir().join(format!("hoplix-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("a/b")).expect("creating failed");
        std::fs::write(directory.join("a/b/deep.log"), b"x").expect("writing failed");
        std::fs::write(directory.join("a/top.log"), b"x").expect("writing failed");
        std::fs::write(directory.join("other.txt"), b"x").expect("writing failed");
        let root = directory.to_string_lossy().into_owned();
        let mut search = Search::new(Arc::new(LocalFileSystem), &root, "*.LOG", None);
        let (paths, failed) = collect(&mut search).await;
        let expected: Vec<String> = [
            directory.join("a").join("top.log"),
            directory.join("a").join("b").join("deep.log"),
        ]
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
        assert_eq!(paths, expected);
        assert!(failed.is_empty());
        let mut shallow = Search::new(Arc::new(LocalFileSystem), &root, "*.log", Some(2));
        assert_eq!(collect(&mut shallow).await.0.len(), 1);
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
