use std::cmp::Ordering;
use std::time::SystemTime;

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Directory,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    pub is_link: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Kind,
    Size,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Ascending,
    Descending,
}

impl Entry {
    pub fn is_directory(&self) -> bool {
        self.kind == EntryKind::Directory
    }
}

impl SortOrder {
    #[must_use]
    pub fn reversed(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}

// Sorting

pub fn sort_entries(entries: &mut [Entry], key: SortKey, order: SortOrder) {
    entries.sort_by(|a, b| {
        let directories_first = b.is_directory().cmp(&a.is_directory());
        let by_name = || compare_names(&a.name, &b.name);
        let by_key = match key {
            SortKey::Name => by_name(),
            SortKey::Kind => extension(&a.name)
                .cmp(&extension(&b.name))
                .then_with(by_name),
            SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
            SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
        };
        let by_key = match order {
            SortOrder::Ascending => by_key,
            SortOrder::Descending => by_key.reverse(),
        };
        directories_first.then(by_key)
    });
}

pub fn compare_names(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) if l.is_ascii_digit() && r.is_ascii_digit() => {
                let left_number = take_digits(&mut left);
                let right_number = take_digits(&mut right);
                let ordering = compare_numbers(&left_number, &right_number);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(l), Some(r)) => {
                let ordering = l.to_lowercase().cmp(r.to_lowercase());
                if ordering != Ordering::Equal {
                    return ordering;
                }
                left.next();
                right.next();
            }
        }
    }
}

// Helpers

fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut digits = String::new();
    while let Some(c) = chars.next_if(char::is_ascii_digit) {
        digits.push(c);
    }
    digits
}

fn compare_numbers(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, extension)| extension.to_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.to_string(),
            kind,
            size: Some(size),
            modified: None,
            is_link: false,
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    // Names

    #[test]
    fn names_compare_naturally_and_ignore_case() {
        let mut list = vec!["file10", "File2", "file1", "file02b", "alpha"];
        list.sort_by(|a, b| compare_names(a, b));
        assert_eq!(list, ["alpha", "file1", "File2", "file02b", "file10"]);
    }

    #[test]
    fn equal_names_fall_back_to_exact_order() {
        assert_eq!(compare_names("a", "A"), "a".cmp("A"));
        assert_eq!(compare_names("abc", "abc"), Ordering::Equal);
    }

    // Entries

    #[test]
    fn directories_come_first_in_both_orders() {
        let mut entries = vec![
            entry("b.txt", EntryKind::File, 5),
            entry("z", EntryKind::Directory, 0),
            entry("a.txt", EntryKind::File, 9),
            entry("c", EntryKind::Directory, 0),
        ];
        sort_entries(&mut entries, SortKey::Name, SortOrder::Ascending);
        assert_eq!(names(&entries), ["c", "z", "a.txt", "b.txt"]);
        sort_entries(&mut entries, SortKey::Name, SortOrder::Descending);
        assert_eq!(names(&entries), ["z", "c", "b.txt", "a.txt"]);
    }

    #[test]
    fn entries_sort_by_size_and_kind() {
        let mut entries = vec![
            entry("big.zip", EntryKind::File, 900),
            entry("small.txt", EntryKind::File, 1),
            entry("mid.rs", EntryKind::File, 50),
            entry(".hidden", EntryKind::File, 7),
        ];
        sort_entries(&mut entries, SortKey::Size, SortOrder::Ascending);
        assert_eq!(
            names(&entries),
            ["small.txt", ".hidden", "mid.rs", "big.zip"]
        );
        sort_entries(&mut entries, SortKey::Kind, SortOrder::Ascending);
        assert_eq!(
            names(&entries),
            [".hidden", "mid.rs", "small.txt", "big.zip"]
        );
    }

    #[test]
    fn entries_sort_by_modification_time() {
        let at = |name: &str, seconds: u64| Entry {
            modified: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds)),
            ..entry(name, EntryKind::File, 1)
        };
        let mut entries = vec![
            at("new", 300),
            at("old", 100),
            entry("unknown", EntryKind::File, 1),
        ];
        sort_entries(&mut entries, SortKey::Modified, SortOrder::Ascending);
        assert_eq!(names(&entries), ["unknown", "old", "new"]);
    }

    #[test]
    fn sort_order_reverses() {
        assert_eq!(SortOrder::Ascending.reversed(), SortOrder::Descending);
        assert_eq!(SortOrder::Descending.reversed(), SortOrder::Ascending);
    }
}
