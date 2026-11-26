use std::sync::Arc;
use std::time::Duration;

use hoplix_core::{Entry, Error, FileSystem, decode_windows_1252, read_head};
use iced::{Subscription, Task, event, mouse};

use crate::app::{Hoplix, Message, TabId};

// Constants

pub const PREVIEW_BYTES: usize = 256 * 1024;
pub const DEBOUNCE: Duration = Duration::from_millis(300);
pub const DEFAULT_WIDTH: f32 = 380.0;
pub const MIN_WIDTH: f32 = 220.0;
pub const MAX_WIDTH: f32 = 900.0;
pub const DEFAULT_LIMIT_MIB: u32 = 2;
pub const LIMIT_CHOICES: [u32; 8] = [0, 1, 2, 5, 10, 20, 50, 100];
const SNIFF_BYTES: usize = 8 * 1024;
const MAX_CONTROL_PERMILLE: usize = 10;
const BINARY_EXTENSIONS: [&str; 52] = [
    "7z", "apk", "avi", "avif", "bin", "bmp", "bz2", "class", "dll", "dmg", "doc", "docx", "dylib",
    "exe", "flac", "gif", "gz", "heic", "ico", "iso", "jar", "jpeg", "jpg", "lz", "m4a", "mkv",
    "mov", "mp3", "mp4", "msi", "o", "odp", "ods", "odt", "ogg", "otf", "pdf", "png", "ppt",
    "pptx", "rar", "so", "tar", "tif", "tiff", "ttf", "wav", "webm", "webp", "woff", "xls", "zip",
];

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1252,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPreview {
    pub text: String,
    pub lines: usize,
    pub truncated: bool,
    pub encoding: Encoding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    Text(TextPreview),
    Binary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub tab: TabId,
    pub path: String,
    pub entry: Entry,
    pub remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Content {
    #[default]
    Nothing,
    Several(usize),
    Folder,
    Waiting,
    Loading,
    Confirm,
    Text(TextPreview),
    NoPreview,
    Failed(Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Show(Content),
    Load { delay: bool },
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Resize {
    origin: Option<f32>,
    width: f32,
}

#[derive(Debug)]
pub struct PreviewState {
    pub open: bool,
    pub width: f32,
    pub subject: Option<Subject>,
    pub content: Content,
    generation: u64,
    handle: Option<iced::task::Handle>,
    resize: Option<Resize>,
}

#[derive(Debug, Clone)]
pub enum PreviewMessage {
    Toggle,
    Settled(u64),
    Loaded(u64, Result<Vec<u8>, Error>),
    LoadRequested,
    ResizeStarted,
    Resized(f32),
    ResizeEnded,
    LimitSelected(u32),
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            open: false,
            width: DEFAULT_WIDTH,
            subject: None,
            content: Content::Nothing,
            generation: 0,
            handle: None,
            resize: None,
        }
    }
}

// State

impl PreviewState {
    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_resizing(&self) -> bool {
        self.resize.is_some()
    }

    pub(crate) fn cancel(&mut self) -> u64 {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
        self.generation += 1;
        self.generation
    }

    pub(crate) fn follow(&mut self, wanted: Option<Subject>, empty: Content) -> bool {
        if wanted == self.subject && (wanted.is_some() || self.content == empty) {
            return false;
        }
        self.cancel();
        if wanted.is_none() {
            self.content = empty;
        }
        self.subject = wanted;
        true
    }

    pub(crate) fn accepts(&self, generation: u64) -> bool {
        generation == self.generation
    }

    pub(crate) fn start_resize(&mut self) {
        self.resize = Some(Resize {
            origin: None,
            width: self.width,
        });
    }

    pub(crate) fn resize_to(&mut self, x: f32) {
        let Some(resize) = &mut self.resize else {
            return;
        };
        match resize.origin {
            None => resize.origin = Some(x),
            Some(origin) => {
                self.width = (resize.width + origin - x).clamp(MIN_WIDTH, MAX_WIDTH);
            }
        }
    }

    pub(crate) fn end_resize(&mut self) {
        self.resize = None;
    }
}

// Planning

pub fn plan(subject: &Subject, limit: u64) -> Plan {
    if subject.entry.is_directory() {
        return Plan::Show(Content::Folder);
    }
    if is_binary_name(&subject.entry.name) {
        return Plan::Show(Content::NoPreview);
    }
    if subject.remote && subject.entry.size.is_some_and(|size| size > limit) {
        return Plan::Show(Content::Confirm);
    }
    Plan::Load {
        delay: subject.remote,
    }
}

pub fn limit_bytes(mebibytes: u32) -> u64 {
    u64::from(mebibytes) * 1024 * 1024
}

pub fn is_binary_name(name: &str) -> bool {
    name.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .is_some_and(|(_, extension)| {
            let extension = extension.to_ascii_lowercase();
            BINARY_EXTENSIONS.contains(&extension.as_str())
        })
}

// Decoding

pub fn decode(bytes: &[u8], total: Option<u64>) -> Decoded {
    let truncated = bytes.len() > PREVIEW_BYTES
        || total.is_some_and(|total| total > u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    let bytes = &bytes[..bytes.len().min(PREVIEW_BYTES)];
    let (text, encoding) = if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        (decode_utf8(rest), Encoding::Utf8)
    } else if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        (decode_utf16(rest, u16::from_le_bytes), Encoding::Utf16Le)
    } else if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        (decode_utf16(rest, u16::from_be_bytes), Encoding::Utf16Be)
    } else {
        let sample = &bytes[..bytes.len().min(SNIFF_BYTES)];
        if sample.contains(&0) || has_many_controls(sample) {
            return Decoded::Binary;
        }
        match std::str::from_utf8(bytes) {
            Ok(text) => (text.to_string(), Encoding::Utf8),
            Err(error) if error.error_len().is_none() && error.valid_up_to() > 0 => (
                String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned(),
                Encoding::Utf8,
            ),
            Err(_) => (decode_windows_1252(bytes), Encoding::Windows1252),
        }
    };
    let text = normalize(&text);
    Decoded::Text(TextPreview {
        lines: line_count(&text),
        text,
        truncated,
        encoding,
    })
}

fn decode_utf8(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(error) if error.error_len().is_none() => {
            String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned()
        }
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units = bytes.as_chunks::<2>().0.iter().map(|&pair| unit(pair));
    char::decode_utf16(units)
        .map(|character| character.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

fn has_many_controls(sample: &[u8]) -> bool {
    let controls = sample
        .iter()
        .filter(|&&byte| byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r' | 0x0c | 0x1b))
        .count();
    controls * 1000 > sample.len() * MAX_CONTROL_PERMILLE
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\0', "\u{FFFD}")
}

pub fn line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.lines().count()
}

// Messages

impl Hoplix {
    pub(crate) fn update_preview(&mut self, message: PreviewMessage) -> Task<Message> {
        match message {
            PreviewMessage::Toggle => self.preview.open = !self.preview.open,
            PreviewMessage::Settled(generation) => {
                if self.preview.accepts(generation) && self.preview.content == Content::Waiting {
                    return self.load_preview();
                }
            }
            PreviewMessage::Loaded(generation, result) => {
                if !self.preview.accepts(generation) {
                    return Task::none();
                }
                self.preview.handle = None;
                let total = self
                    .preview
                    .subject
                    .as_ref()
                    .and_then(|subject| subject.entry.size);
                self.preview.content = match result {
                    Ok(bytes) => match decode(&bytes, total) {
                        Decoded::Text(text) => Content::Text(text),
                        Decoded::Binary => Content::NoPreview,
                    },
                    Err(error) => Content::Failed(error),
                };
            }
            PreviewMessage::LoadRequested => {
                if self.preview.content == Content::Confirm {
                    return self.load_preview();
                }
            }
            PreviewMessage::ResizeStarted => self.preview.start_resize(),
            PreviewMessage::Resized(x) => self.preview.resize_to(x),
            PreviewMessage::ResizeEnded => self.preview.end_resize(),
            PreviewMessage::LimitSelected(mebibytes) => {
                self.settings.preview_limit = mebibytes;
                self.save_settings();
            }
        }
        Task::none()
    }

    pub(crate) fn sync_preview(&mut self) -> Task<Message> {
        let (wanted, empty) = if self.preview.open {
            self.preview_subject()
        } else {
            (None, Content::Nothing)
        };
        if !self.preview.follow(wanted, empty) {
            return Task::none();
        }
        let Some(subject) = &self.preview.subject else {
            return Task::none();
        };
        match plan(subject, limit_bytes(self.settings.preview_limit)) {
            Plan::Show(content) => {
                self.preview.content = content;
                Task::none()
            }
            Plan::Load { delay } => {
                self.preview.content = Content::Waiting;
                let generation = self.preview.generation;
                let wait = if delay {
                    self.services.preview_delay
                } else {
                    Duration::ZERO
                };
                let settle = async move {
                    if !wait.is_zero() {
                        tokio::time::sleep(wait).await;
                    }
                };
                let (task, handle) = Task::perform(settle, move |()| {
                    Message::Preview(PreviewMessage::Settled(generation))
                })
                .abortable();
                self.preview.handle = Some(handle);
                task
            }
        }
    }

    fn preview_subject(&self) -> (Option<Subject>, Content) {
        let Some(tab) = self.focused_tab() else {
            return (None, Content::Nothing);
        };
        if tab.path.is_empty() || tab.search.is_some() {
            return (None, Content::Nothing);
        }
        match tab.selection.len() {
            0 => (None, Content::Nothing),
            1 => {
                let subject = tab.selected_entry().map(|entry| Subject {
                    tab: tab.id,
                    path: tab.file_system.join(&tab.path, &entry.name),
                    entry: entry.clone(),
                    remote: tab.file_system.is_remote(),
                });
                (subject, Content::Nothing)
            }
            count => (None, Content::Several(count)),
        }
    }

    fn load_preview(&mut self) -> Task<Message> {
        let Some(subject) = self.preview.subject.clone() else {
            return Task::none();
        };
        let Some(file_system) = self
            .tab_by_id(subject.tab)
            .map(|tab| Arc::clone(&tab.file_system))
        else {
            return Task::none();
        };
        if let Some(handle) = self.preview.handle.take() {
            handle.abort();
        }
        self.preview.content = Content::Loading;
        let generation = self.preview.generation;
        let (task, handle) =
            Task::perform(read_preview(file_system, subject.path), move |result| {
                Message::Preview(PreviewMessage::Loaded(generation, result))
            })
            .abortable();
        self.preview.handle = Some(handle);
        task
    }
}

async fn read_preview(file_system: Arc<dyn FileSystem>, path: String) -> Result<Vec<u8>, Error> {
    read_head(file_system.as_ref(), &path, PREVIEW_BYTES + 1).await
}

// Resizing

pub fn subscription(app: &Hoplix) -> Subscription<Message> {
    if app.preview.is_resizing() {
        event::listen_with(resize_event)
    } else {
        Subscription::none()
    }
}

pub(crate) fn resize_event(
    event: iced::Event,
    _status: event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
            Some(Message::Preview(PreviewMessage::Resized(position.x)))
        }
        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
            Some(Message::Preview(PreviewMessage::ResizeEnded))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hoplix_core::EntryKind;

    fn subject(name: &str, size: u64, remote: bool) -> Subject {
        Subject {
            tab: 1,
            path: format!("/srv/{name}"),
            entry: Entry {
                name: name.to_string(),
                kind: EntryKind::File,
                size: Some(size),
                modified: None,
                is_link: false,
            },
            remote,
        }
    }

    fn text_of(decoded: Decoded) -> TextPreview {
        match decoded {
            Decoded::Text(text) => text,
            Decoded::Binary => panic!("decoded as binary"),
        }
    }

    // Detection

    #[test]
    fn text_is_detected_by_content() {
        let text = text_of(decode(b"one\r\ntwo\nthree", Some(14)));
        assert_eq!(text.text, "one\ntwo\nthree");
        assert_eq!(text.lines, 3);
        assert!(!text.truncated);
        assert_eq!(text.encoding, Encoding::Utf8);
        assert_eq!(decode(b"PK\x03\x04\0\0\x14\0", None), Decoded::Binary);
        assert_eq!(
            decode(&[0x01, 0x02, 0x03, 0x04, b'a', b'b'], None),
            Decoded::Binary
        );
        assert_eq!(text_of(decode(b"", Some(0))).lines, 0);
        assert_eq!(text_of(decode(b"tab\tand\x1b[0m colour\n", None)).lines, 1);
    }

    #[test]
    fn utf8_falls_back_to_windows_1252() {
        let utf8 = text_of(decode("Grüße".as_bytes(), None));
        assert_eq!(
            (utf8.text.as_str(), utf8.encoding),
            ("Grüße", Encoding::Utf8)
        );
        let legacy = text_of(decode(b"Gr\xFC\xDFe \x80", None));
        assert_eq!(legacy.text, "Grüße €");
        assert_eq!(legacy.encoding, Encoding::Windows1252);
        let bom = text_of(decode(b"\xEF\xBB\xBFhello", None));
        assert_eq!((bom.text.as_str(), bom.encoding), ("hello", Encoding::Utf8));
        let wide = text_of(decode(b"\xFF\xFEh\0i\0", None));
        assert_eq!(
            (wide.text.as_str(), wide.encoding),
            ("hi", Encoding::Utf16Le)
        );
        let big = text_of(decode(b"\xFE\xFF\0h\0i", None));
        assert_eq!((big.text.as_str(), big.encoding), ("hi", Encoding::Utf16Be));
    }

    #[test]
    fn long_files_are_truncated_at_the_preview_size() {
        let mut bytes = "ä".repeat(PREVIEW_BYTES / 2).into_bytes();
        bytes.insert(0, b'x');
        let text = text_of(decode(&bytes, Some(bytes.len() as u64)));
        assert!(text.truncated);
        assert_eq!(text.encoding, Encoding::Utf8);
        assert_eq!(text.text.len(), PREVIEW_BYTES - 1);
        assert!(text.text.ends_with('ä'));

        let head = vec![b'a'; 100];
        assert!(text_of(decode(&head, Some(5000))).truncated);
        assert!(!text_of(decode(&head, Some(100))).truncated);
        assert!(!text_of(decode(&head, None)).truncated);
        let over = vec![b'a'; PREVIEW_BYTES + 1];
        let text = text_of(decode(&over, None));
        assert!(text.truncated);
        assert_eq!(text.text.len(), PREVIEW_BYTES);
    }

    #[test]
    fn names_and_sizes_decide_whether_to_load() {
        let limit = limit_bytes(DEFAULT_LIMIT_MIB);
        assert_eq!(limit, 2 * 1024 * 1024);
        assert_eq!(
            plan(&subject("notes.txt", 10, false), limit),
            Plan::Load { delay: false }
        );
        assert_eq!(
            plan(&subject("notes.txt", 10, true), limit),
            Plan::Load { delay: true }
        );
        assert_eq!(
            plan(&subject("big.log", limit + 1, true), limit),
            Plan::Show(Content::Confirm)
        );
        assert_eq!(
            plan(&subject("big.log", limit + 1, false), limit),
            Plan::Load { delay: false }
        );
        assert_eq!(
            plan(&subject("photo.JPG", 10, true), limit),
            Plan::Show(Content::NoPreview)
        );
        assert_eq!(
            plan(&subject("empty.txt", 0, true), 0),
            Plan::Load { delay: true }
        );
        let mut folder = subject("docs", 0, true);
        folder.entry.kind = EntryKind::Directory;
        assert_eq!(plan(&folder, limit), Plan::Show(Content::Folder));
        assert!(!is_binary_name(".zip"));
        assert!(!is_binary_name("README"));
    }

    // Debounce

    #[test]
    fn new_subjects_cancel_older_loads() {
        let mut state = PreviewState::default();
        assert!(state.follow(Some(subject("a.txt", 1, true)), Content::Nothing));
        let first = state.generation();
        assert!(!state.follow(Some(subject("a.txt", 1, true)), Content::Nothing));
        assert_eq!(state.generation, first);
        assert!(state.follow(Some(subject("b.txt", 1, true)), Content::Nothing));
        assert!(!state.accepts(first));
        assert!(state.accepts(state.generation()));
        let mut changed = subject("b.txt", 2, true);
        changed.entry.size = Some(3);
        assert!(state.follow(Some(changed), Content::Nothing));
        assert!(state.follow(None, Content::Several(2)));
        assert_eq!(state.content, Content::Several(2));
        assert!(!state.follow(None, Content::Several(2)));
        assert!(state.follow(None, Content::Nothing));
    }

    #[test]
    fn dragging_the_divider_changes_the_width_within_limits() {
        let mut state = PreviewState::default();
        state.start_resize();
        state.resize_to(500.0);
        assert_eq!(state.width, DEFAULT_WIDTH);
        state.resize_to(450.0);
        assert_eq!(state.width, DEFAULT_WIDTH + 50.0);
        state.resize_to(5000.0);
        assert_eq!(state.width, MIN_WIDTH);
        state.resize_to(-5000.0);
        assert_eq!(state.width, MAX_WIDTH);
        state.end_resize();
        state.resize_to(0.0);
        assert_eq!(state.width, MAX_WIDTH);
        assert!(!state.is_resizing());
    }
}
