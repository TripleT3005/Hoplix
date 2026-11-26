use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::Path;

use crate::key_value::{self, Section};
use crate::sites::write_atomically;
use crate::{TransferDirection, Verification};

// Constants

pub const HISTORY_FILE: &str = "history.conf";
pub const DEFAULT_HISTORY_LIMIT: usize = 500;
pub const MAX_HISTORY_LIMIT: usize = 10_000;
const ENTRY_SECTION: &str = "transfer";

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryResult {
    Done,
    Skipped,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub finished: u64,
    pub direction: TransferDirection,
    pub source: String,
    pub target: String,
    pub size: u64,
    pub duration_ms: u64,
    pub result: HistoryResult,
    pub error: String,
    pub checksum: Option<Verification>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    entries: VecDeque<HistoryEntry>,
    limit: usize,
}

pub struct ExportLabels<'a> {
    pub header: [&'a str; 9],
    pub result: &'a dyn Fn(HistoryResult) -> String,
    pub direction: &'a dyn Fn(TransferDirection) -> String,
    pub checksum: &'a dyn Fn(Option<Verification>) -> String,
    pub time: &'a dyn Fn(u64) -> String,
}

// Results

impl HistoryResult {
    pub const ALL: [Self; 4] = [Self::Done, Self::Skipped, Self::Failed, Self::Cancelled];

    pub fn key(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|result| result.key() == key)
    }
}

fn direction_key(direction: TransferDirection) -> &'static str {
    match direction {
        TransferDirection::Local => "local",
        TransferDirection::Download => "download",
        TransferDirection::Upload => "upload",
        TransferDirection::ServerToServer => "server",
    }
}

fn direction_from(key: &str) -> Option<TransferDirection> {
    [
        TransferDirection::Local,
        TransferDirection::Download,
        TransferDirection::Upload,
        TransferDirection::ServerToServer,
    ]
    .into_iter()
    .find(|direction| direction_key(*direction) == key)
}

// Entries

impl HistoryEntry {
    pub fn bytes_per_second(&self) -> Option<f64> {
        (self.duration_ms > 0 && self.size > 0)
            .then(|| self.size as f64 * 1000.0 / self.duration_ms as f64)
    }

    fn pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("finished", self.finished.to_string()),
            ("direction", direction_key(self.direction).to_string()),
            ("source", self.source.clone()),
            ("target", self.target.clone()),
            ("size", self.size.to_string()),
            ("duration_ms", self.duration_ms.to_string()),
            ("result", self.result.key().to_string()),
            ("error", self.error.clone()),
            (
                "checksum",
                self.checksum.map(Verification::key).unwrap_or_default(),
            ),
        ]
    }

    fn from_section(section: &Section) -> Option<Self> {
        let number = |key: &str| section.get(key).and_then(|value| value.parse::<u64>().ok());
        Some(Self {
            finished: number("finished")?,
            direction: section.get("direction").and_then(direction_from)?,
            source: section.get("source")?.to_string(),
            target: section.get("target")?.to_string(),
            size: number("size").unwrap_or(0),
            duration_ms: number("duration_ms").unwrap_or(0),
            result: section.get("result").and_then(HistoryResult::from_key)?,
            error: section.get("error").unwrap_or_default().to_string(),
            checksum: section.get("checksum").and_then(Verification::from_key),
        })
    }
}

// History

impl Default for History {
    fn default() -> Self {
        Self::new(DEFAULT_HISTORY_LIMIT)
    }
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            limit: limit.clamp(1, MAX_HISTORY_LIMIT),
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.clamp(1, MAX_HISTORY_LIMIT);
        self.trim();
    }

    pub fn push(&mut self, entry: HistoryEntry) {
        self.entries.push_front(entry);
        self.trim();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = &HistoryEntry> {
        self.entries.iter()
    }

    pub fn filtered(&self, result: Option<HistoryResult>) -> Vec<&HistoryEntry> {
        self.entries
            .iter()
            .filter(|entry| result.is_none_or(|wanted| entry.result == wanted))
            .collect()
    }

    fn trim(&mut self) {
        self.entries.truncate(self.limit);
    }

    // Serialization

    pub fn parse(text: &str, limit: usize) -> Self {
        let mut history = Self::new(limit);
        history.entries = key_value::parse_sections(text)
            .iter()
            .filter(|section| section.name == ENTRY_SECTION)
            .filter_map(HistoryEntry::from_section)
            .collect();
        history.trim();
        history
    }

    pub fn to_text(&self) -> String {
        let mut text = String::from("# Hoplix transfer history (no passwords)\n");
        for entry in &self.entries {
            text.push_str(&format!(
                "\n[{ENTRY_SECTION}]\n{}",
                key_value::write(&entry.pairs())
            ));
        }
        text
    }

    pub fn load(directory: &Path, limit: usize) -> Self {
        match fs::read(directory.join(HISTORY_FILE)) {
            Ok(bytes) => Self::parse(&String::from_utf8_lossy(&bytes), limit),
            Err(_) => Self::new(limit),
        }
    }

    pub fn save(&self, directory: &Path) -> io::Result<()> {
        let path = directory.join(HISTORY_FILE);
        if self.entries.is_empty() {
            return match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        write_atomically(&path, self.to_text().as_bytes())
    }
}

// Export

fn export_fields(entry: &HistoryEntry, labels: &ExportLabels<'_>) -> [String; 9] {
    let speed = entry
        .bytes_per_second()
        .map(|speed| format!("{speed:.0}"))
        .unwrap_or_default();
    [
        (labels.time)(entry.finished),
        (labels.direction)(entry.direction),
        entry.source.clone(),
        entry.target.clone(),
        entry.size.to_string(),
        format!("{:.1}", entry.duration_ms as f64 / 1000.0),
        speed,
        if entry.error.is_empty() {
            (labels.result)(entry.result)
        } else {
            format!("{}: {}", (labels.result)(entry.result), entry.error)
        },
        (labels.checksum)(entry.checksum),
    ]
}

pub fn csv_field(value: &str) -> String {
    let guarded = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if guarded.contains([',', '"', '\n', '\r', ';']) {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

pub fn to_csv(entries: &[&HistoryEntry], labels: &ExportLabels<'_>) -> String {
    let mut text = csv_row(labels.header.iter().map(|label| (*label).to_string()));
    for entry in entries {
        text.push_str(&csv_row(export_fields(entry, labels).into_iter()));
    }
    text
}

fn csv_row(fields: impl Iterator<Item = String>) -> String {
    let fields: Vec<String> = fields.map(|field| csv_field(&field)).collect();
    format!("{}\r\n", fields.join(","))
}

pub fn to_plain_text(entries: &[&HistoryEntry], labels: &ExportLabels<'_>) -> String {
    let mut text = String::new();
    for entry in entries {
        let fields = export_fields(entry, labels);
        let lines: Vec<String> = labels
            .header
            .iter()
            .zip(fields.iter())
            .filter(|(_, value)| !value.is_empty())
            .map(|(label, value)| format!("{label}: {}", value.replace(['\r', '\n'], " ")))
            .collect();
        text.push_str(&lines.join("\n"));
        text.push_str("\n\n");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashAlgorithm;
    use std::path::PathBuf;

    fn temporary_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-history-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("creating failed");
        directory
    }

    fn entry(index: u64, result: HistoryResult) -> HistoryEntry {
        HistoryEntry {
            finished: 1_790_000_000 + index,
            direction: TransferDirection::Download,
            source: format!("sftp://demo@files.example.test/file{index}.bin"),
            target: format!("C:\\Data\\file{index}.bin"),
            size: 2048 * index,
            duration_ms: 500,
            result,
            error: if result == HistoryResult::Failed {
                "Connection lost, \"retry\"\nlater".to_string()
            } else {
                String::new()
            },
            checksum: Some(Verification::Matched(HashAlgorithm::Sha256)),
        }
    }

    fn labels<'a>(
        result: &'a dyn Fn(HistoryResult) -> String,
        direction: &'a dyn Fn(TransferDirection) -> String,
        checksum: &'a dyn Fn(Option<Verification>) -> String,
        time: &'a dyn Fn(u64) -> String,
    ) -> ExportLabels<'a> {
        ExportLabels {
            header: [
                "Time",
                "Direction",
                "Source",
                "Target",
                "Size",
                "Seconds",
                "Speed",
                "Result",
                "Checksum",
            ],
            result,
            direction,
            checksum,
            time,
        }
    }

    #[test]
    fn the_ring_buffer_keeps_the_newest_entries() {
        let mut history = History::new(3);
        for index in 0..5 {
            history.push(entry(index, HistoryResult::Done));
        }
        assert_eq!(history.len(), 3);
        let finished: Vec<u64> = history.entries().map(|entry| entry.finished).collect();
        assert_eq!(finished, [1_790_000_004, 1_790_000_003, 1_790_000_002]);
        history.set_limit(1);
        assert_eq!(history.len(), 1);
        assert_eq!(History::new(0).limit(), 1);
        assert_eq!(History::new(usize::MAX).limit(), MAX_HISTORY_LIMIT);
        history.clear();
        assert!(history.is_empty());
    }

    #[test]
    fn history_round_trips_and_filters() {
        let directory = temporary_directory("round-trip");
        let mut history = History::new(10);
        for (index, result) in HistoryResult::ALL.into_iter().enumerate() {
            history.push(entry(index as u64, result));
        }
        history.save(&directory).expect("saving failed");
        let loaded = History::load(&directory, 10);
        assert_eq!(loaded, history);
        assert_eq!(History::load(&directory, 2).len(), 2);
        assert_eq!(loaded.filtered(Some(HistoryResult::Failed)).len(), 1);
        assert_eq!(loaded.filtered(None).len(), 4);
        let broken = History::parse(
            "[transfer]\nfinished = soon\n[transfer]\nfinished = 1\ndirection = sideways\nsource = a\ntarget = b\nresult = done\n[transfer]\nfinished = 1\ndirection = upload\nsource = a\ntarget = b\nresult = done\nchecksum = matched:whirlpool\n",
            10,
        );
        assert_eq!(broken.len(), 1);
        assert_eq!(
            broken.entries().next().map(|entry| entry.checksum),
            Some(None)
        );
        History::new(5).save(&directory).expect("saving failed");
        assert!(!directory.join(HISTORY_FILE).exists());
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn csv_fields_are_quoted_and_guarded() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
        assert_eq!(csv_field("=SUM(A1)"), "'=SUM(A1)");
        assert_eq!(csv_field("-1;x"), "\"'-1;x\"");
    }

    #[test]
    fn exports_list_every_column() {
        let result = |result: HistoryResult| result.key().to_string();
        let direction = |direction: TransferDirection| direction_key(direction).to_string();
        let checksum =
            |checksum: Option<Verification>| checksum.map(Verification::key).unwrap_or_default();
        let time = |seconds: u64| seconds.to_string();
        let labels = labels(&result, &direction, &checksum, &time);
        let done = entry(2, HistoryResult::Done);
        let failed = entry(3, HistoryResult::Failed);
        let csv = to_csv(&[&done, &failed], &labels);
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(
            lines[0],
            "Time,Direction,Source,Target,Size,Seconds,Speed,Result,Checksum"
        );
        assert_eq!(
            lines[1],
            "1790000002,download,sftp://demo@files.example.test/file2.bin,C:\\Data\\file2.bin,4096,0.5,8192,done,matched:sha256"
        );
        assert!(lines[2].contains("\"failed: Connection lost, \"\"retry\"\"\nlater\""));
        let text = to_plain_text(&[&failed], &labels);
        assert!(text.contains("Result: failed: Connection lost, \"retry\" later\n"));
        assert!(text.contains("Source: sftp://demo@files.example.test/file3.bin\n"));
    }
}
