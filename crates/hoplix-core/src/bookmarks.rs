use std::fs;
use std::io;
use std::path::Path;

use crate::key_value::{self, Section};
use crate::sites::write_atomically;

// Constants

pub const BOOKMARKS_FILE: &str = "bookmarks.conf";
const NAME_KEY: &str = "bookmark";
const PATH_KEY: &str = "bookmark.path";

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bookmarks {
    pub items: Vec<Bookmark>,
}

// Bookmarks

impl Bookmark {
    pub fn new(name: &str, path: &str) -> Self {
        let name = name.trim();
        Self {
            name: if name.is_empty() {
                path.to_string()
            } else {
                name.to_string()
            },
            path: path.to_string(),
        }
    }
}

impl Bookmarks {
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut items: Vec<Bookmark> = Vec::new();
        let mut name: Option<&str> = None;
        for (key, value) in pairs {
            match key {
                NAME_KEY => name = Some(value),
                PATH_KEY if !value.is_empty() => {
                    items.push(Bookmark::new(name.take().unwrap_or_default(), value));
                }
                _ => {}
            }
        }
        Self { items }
    }

    pub fn from_section(section: &Section) -> Self {
        Self::from_pairs(
            section
                .pairs
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
    }

    pub fn pairs(&self) -> Vec<(&'static str, String)> {
        self.items
            .iter()
            .flat_map(|bookmark| {
                [
                    (NAME_KEY, bookmark.name.clone()),
                    (PATH_KEY, bookmark.path.clone()),
                ]
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn contains(&self, path: &str) -> bool {
        self.items.iter().any(|bookmark| bookmark.path == path)
    }

    pub fn add(&mut self, name: &str, path: &str) -> bool {
        if self.contains(path) {
            return false;
        }
        self.items.push(Bookmark::new(name, path));
        true
    }

    pub fn rename(&mut self, index: usize, name: &str) {
        if let Some(bookmark) = self.items.get_mut(index) {
            let name = name.trim();
            if !name.is_empty() {
                bookmark.name = name.to_string();
            }
        }
    }

    pub fn remove(&mut self, index: usize) -> Option<Bookmark> {
        (index < self.items.len()).then(|| self.items.remove(index))
    }

    // Storage

    pub fn parse(text: &str) -> Self {
        let pairs = key_value::parse(text);
        Self::from_pairs(
            pairs
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
    }

    pub fn to_text(&self) -> String {
        format!("# Hoplix bookmarks\n{}", key_value::write(&self.pairs()))
    }

    pub fn load(directory: &Path) -> io::Result<Self> {
        match fs::read_to_string(directory.join(BOOKMARKS_FILE)) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, directory: &Path) -> io::Result<()> {
        write_atomically(&directory.join(BOOKMARKS_FILE), self.to_text().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarks_round_trip_with_odd_names_and_paths() {
        let mut bookmarks = Bookmarks::default();
        assert!(bookmarks.add("Projects = work", "C:\\Users\\demo\\Projects"));
        assert!(bookmarks.add("", "/var/www/new\nline"));
        assert!(!bookmarks.add("again", "C:\\Users\\demo\\Projects"));
        let parsed = Bookmarks::parse(&bookmarks.to_text());
        assert_eq!(parsed, bookmarks);
        assert_eq!(parsed.items[1].name, "/var/www/new\nline");
    }

    #[test]
    fn bookmarks_rename_and_remove() {
        let mut bookmarks = Bookmarks::default();
        bookmarks.add("a", "/a");
        bookmarks.add("b", "/b");
        bookmarks.rename(0, "  First ");
        bookmarks.rename(1, "   ");
        assert_eq!(bookmarks.items[0].name, "First");
        assert_eq!(bookmarks.items[1].name, "b");
        assert_eq!(
            bookmarks.remove(0).map(|bookmark| bookmark.path),
            Some("/a".to_string())
        );
        assert_eq!(bookmarks.remove(5), None);
        assert_eq!(bookmarks.items.len(), 1);
    }

    #[test]
    fn broken_lines_are_skipped() {
        let parsed = Bookmarks::parse("bookmark = lonely\nbookmark.path =\nbookmark.path = /x\n");
        assert_eq!(parsed.items, [Bookmark::new("lonely", "/x")]);
    }

    #[test]
    fn bookmarks_are_stored_in_the_folder() {
        let directory =
            std::env::temp_dir().join(format!("hoplix-bookmarks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        assert_eq!(
            Bookmarks::load(&directory).expect("loading failed"),
            Bookmarks::default()
        );
        let mut bookmarks = Bookmarks::default();
        bookmarks.add("Home", "/home/demo");
        bookmarks.save(&directory).expect("saving failed");
        assert_eq!(
            Bookmarks::load(&directory).expect("loading failed"),
            bookmarks
        );
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
