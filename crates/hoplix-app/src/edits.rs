use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hoplix_core::{
    ConflictPolicy, Error, ErrorKind, FileSystem, JobId, JobOptions, JobStatus, TransferRequest,
};
use iced::futures::channel::oneshot;
use iced::{Subscription, Task};

use crate::app::{Hoplix, Message, TabId};
use crate::transfers::{Direction, TransferTarget};

// Constants

pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
const FOLDER_PREFIX: &str = "hoplix-edit";
const FOLDER_ATTEMPTS: u128 = 16;
const FALLBACK_NAME: &str = "file";
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

// Types

pub type EditId = u64;

pub(crate) type Opener = Arc<dyn Fn(&Path, &str) -> Result<(), Error> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditState {
    Downloading,
    Watching,
    Changed,
    Uploading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    modified: Option<SystemTime>,
    size: u64,
}

#[derive(Debug)]
pub struct EditedFile {
    pub id: EditId,
    pub tab: TabId,
    pub name: String,
    pub remote_path: String,
    pub local_path: PathBuf,
    pub state: EditState,
    file_system: Arc<dyn FileSystem>,
    remote_directory: String,
    editor: String,
    job: Option<JobId>,
    stamp: Option<FileStamp>,
}

#[derive(Debug, Default)]
pub struct Edits {
    pub files: Vec<EditedFile>,
    root: Option<PathBuf>,
    next_id: EditId,
}

#[derive(Debug, Clone)]
pub enum EditMessage {
    Poll,
    Upload(EditId),
    Ignore(EditId),
    Stop(EditId),
    Opened(Result<(), Error>),
    EditorEdited(String),
    UploadAutomaticallyToggled(bool),
}

// Change detection

impl FileStamp {
    pub fn read(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok().filter(fs::Metadata::is_file)?;
        Some(Self {
            modified: metadata.modified().ok(),
            size: metadata.len(),
        })
    }
}

pub fn has_changed(baseline: Option<FileStamp>, current: Option<FileStamp>) -> bool {
    current.is_some() && current != baseline
}

// Temporary files

pub fn session_folder_name(process: u32, seed: u128) -> String {
    format!("{FOLDER_PREFIX}-{process}-{seed:x}")
}

pub fn local_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect();
    let mut cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        cleaned = FALLBACK_NAME.to_string();
    }
    let stem = cleaned.split('.').next().unwrap_or_default().trim_end();
    if RESERVED_NAMES
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        cleaned.insert(0, '_');
    }
    cleaned
}

fn create_private_folder(path: &Path, recursive: bool) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn create_session_folder(base: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(base)?;
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for attempt in 0..FOLDER_ATTEMPTS {
        let path = base.join(session_folder_name(std::process::id(), seed + attempt));
        match create_private_folder(&path, false) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

// Stale folders

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Exited,
    Other,
    Hoplix,
    Unknown,
}

pub fn session_folder_process(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(FOLDER_PREFIX)?.strip_prefix('-')?;
    let (process, seed) = rest.split_once('-')?;
    let is_number = |text: &str, radix: u32, length: usize| {
        (1..=length).contains(&text.len())
            && text
                .chars()
                .all(|character| character.is_digit(radix) && !character.is_ascii_uppercase())
    };
    if !is_number(process, 10, 10) || !is_number(seed, 16, 32) {
        return None;
    }
    process.parse().ok()
}

pub fn remove_stale_folders(base: &Path, state: impl Fn(u32) -> ProcessState) -> usize {
    let Ok(items) = fs::read_dir(base) else {
        return 0;
    };
    let own = std::process::id();
    let mut removed = 0;
    for item in items.flatten() {
        let Some(process) = item
            .file_name()
            .to_str()
            .and_then(session_folder_process)
            .filter(|&process| process != own)
        else {
            continue;
        };
        if !item.file_type().is_ok_and(|kind| kind.is_dir()) || !is_owned(&item.path()) {
            continue;
        }
        if matches!(state(process), ProcessState::Exited | ProcessState::Other)
            && fs::remove_dir_all(item.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

pub fn remove_stale_folders_in_background(base: PathBuf) {
    std::thread::spawn(move || remove_stale_folders(&base, process_state));
}

fn classify_process(name: &str) -> ProcessState {
    let name = name.trim().to_lowercase();
    if name.is_empty() {
        return ProcessState::Unknown;
    }
    let own = std::env::current_exe().ok().and_then(|path| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().to_lowercase())
    });
    let is_own = own.is_some_and(|own| own == name || (name.len() >= 15 && own.starts_with(&name)));
    if is_own || name.contains("hoplix") {
        ProcessState::Hoplix
    } else {
        ProcessState::Other
    }
}

#[cfg(unix)]
fn is_owned(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    #[allow(unsafe_code)]
    let user = unsafe { libc::geteuid() };
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.uid() == user)
}

#[cfg(not(unix))]
fn is_owned(_path: &Path) -> bool {
    true
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub fn process_state(process: u32) -> ProcessState {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process) };
    if handle.is_null() {
        return if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
            ProcessState::Exited
        } else {
            ProcessState::Unknown
        };
    }
    let mut code = 0u32;
    let state = if unsafe { GetExitCodeProcess(handle, &mut code) } == 0 {
        ProcessState::Unknown
    } else if code != STILL_ACTIVE as u32 {
        ProcessState::Exited
    } else {
        let mut buffer = vec![0u16; 32_768];
        let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let found = unsafe {
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut length)
        };
        if found == 0 {
            ProcessState::Unknown
        } else {
            let image = String::from_utf16_lossy(&buffer[..length as usize]);
            Path::new(&image)
                .file_stem()
                .map_or(ProcessState::Unknown, |stem| {
                    classify_process(&stem.to_string_lossy())
                })
        }
    };
    unsafe { CloseHandle(handle) };
    state
}

#[cfg(target_os = "linux")]
pub fn process_state(process: u32) -> ProcessState {
    let folder = PathBuf::from(format!("/proc/{process}"));
    match fs::symlink_metadata(&folder) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if Path::new("/proc/self").exists() {
                ProcessState::Exited
            } else {
                ProcessState::Unknown
            }
        }
        Err(_) => ProcessState::Unknown,
        Ok(_) => fs::read_to_string(folder.join("comm"))
            .map_or(ProcessState::Unknown, |name| classify_process(&name)),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn process_state(_process: u32) -> ProcessState {
    ProcessState::Unknown
}

impl Edits {
    fn get_mut(&mut self, id: EditId) -> Option<&mut EditedFile> {
        self.files.iter_mut().find(|file| file.id == id)
    }

    #[cfg(test)]
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    fn next_folder(&mut self, base: &Path) -> io::Result<(EditId, PathBuf)> {
        let root = match &self.root {
            Some(root) => root.clone(),
            None => {
                let root = create_session_folder(base)?;
                self.root = Some(root.clone());
                root
            }
        };
        self.next_id += 1;
        let folder = root.join(self.next_id.to_string());
        create_private_folder(&folder, true)?;
        Ok((self.next_id, folder))
    }
}

// Messages

impl Hoplix {
    pub(crate) fn update_edits(&mut self, message: EditMessage) -> Task<Message> {
        match message {
            EditMessage::Poll => self.poll_edits(),
            EditMessage::Upload(id) => self.upload_edit(id),
            EditMessage::Ignore(id) => {
                if let Some(file) = self
                    .edits
                    .get_mut(id)
                    .filter(|file| file.state == EditState::Changed)
                {
                    file.stamp = FileStamp::read(&file.local_path);
                    file.state = EditState::Watching;
                }
            }
            EditMessage::Stop(id) => self.stop_edits(|file| file.id == id),
            EditMessage::Opened(result) => {
                if let Err(error) = result {
                    self.notice = Some(
                        self.strings
                            .format("edit.open_failed", &[("detail", error.detail())]),
                    );
                }
            }
            EditMessage::EditorEdited(editor) => {
                self.settings.editor = editor;
                self.save_settings();
            }
            EditMessage::UploadAutomaticallyToggled(value) => {
                self.settings.upload_changes_automatically = value;
                self.save_settings();
            }
        }
        Task::none()
    }

    // Opening

    pub(crate) fn open_file(&mut self, id: TabId, index: usize, use_editor: bool) -> Task<Message> {
        let Some(tab) = self
            .panes
            .iter()
            .flat_map(|pane| pane.tabs.iter())
            .find(|tab| tab.id == id)
            .filter(|tab| !tab.path.is_empty())
        else {
            return Task::none();
        };
        let Some(entry) = tab.entries.get(index).filter(|entry| !entry.is_directory()) else {
            return Task::none();
        };
        let editor = if use_editor {
            self.settings.editor.trim().to_string()
        } else {
            String::new()
        };
        let file_system = Arc::clone(&tab.file_system);
        let remote_path = file_system.join(&tab.path, &entry.name);
        if file_system.label().is_none() {
            return self.launch(PathBuf::from(remote_path), editor);
        }
        if let Some(file) = self.edits.files.iter().find(|file| {
            file.remote_path == remote_path
                && std::ptr::addr_eq(Arc::as_ptr(&file.file_system), Arc::as_ptr(&file_system))
        }) {
            if file.state == EditState::Downloading {
                return Task::none();
            }
            let path = file.local_path.clone();
            return self.launch(path, editor);
        }
        let name = entry.name.clone();
        let remote_directory = tab.path.clone();
        let site_limits = self.site_limits(&[tab]);
        let (edit, folder) = match self.edits.next_folder(&self.services.temp_dir) {
            Ok(created) => created,
            Err(error) => {
                self.notice = Some(
                    self.strings
                        .format("edit.folder_failed", &[("detail", &error.to_string())]),
                );
                return Task::none();
            }
        };
        let local_name = local_file_name(&name);
        let local_path = folder.join(&local_name);
        let job = self.transfers.add_with(
            TransferRequest {
                source: Arc::clone(&file_system),
                source_path: remote_path.clone(),
                target: Arc::clone(&self.services.local),
                target_directory: folder.to_string_lossy().into_owned(),
                target_name: Some(local_name),
            },
            JobOptions {
                policy: Some(ConflictPolicy::Overwrite),
                site_limits,
                ..JobOptions::default()
            },
        );
        self.edits.files.push(EditedFile {
            id: edit,
            tab: id,
            name,
            remote_path,
            local_path,
            state: EditState::Downloading,
            file_system,
            remote_directory,
            editor,
            job: Some(job),
            stamp: None,
        });
        Task::none()
    }

    fn launch(&self, path: PathBuf, editor: String) -> Task<Message> {
        let opener = Arc::clone(&self.services.opener);
        Task::perform(
            async move {
                let (sender, receiver) = oneshot::channel();
                std::thread::spawn(move || {
                    let _ = sender.send(opener(&path, &editor));
                });
                receiver
                    .await
                    .unwrap_or_else(|_| Err(Error::new(ErrorKind::Other, "")))
            },
            |result| Message::Edit(EditMessage::Opened(result)),
        )
    }

    // Watching

    fn poll_edits(&mut self) {
        let mut changed = Vec::new();
        for file in &mut self.edits.files {
            if file.state != EditState::Watching {
                continue;
            }
            let current = FileStamp::read(&file.local_path);
            if has_changed(file.stamp, current) {
                file.stamp = current;
                file.state = EditState::Changed;
                changed.push(file.id);
            }
        }
        if self.settings.upload_changes_automatically {
            for id in changed {
                self.upload_edit(id);
            }
        }
    }

    fn upload_edit(&mut self, id: EditId) {
        let local = Arc::clone(&self.services.local);
        let site_limits = self
            .edits
            .files
            .iter()
            .find(|file| file.id == id)
            .and_then(|file| self.tab_by_id(file.tab))
            .map(|tab| self.site_limits(&[tab]))
            .unwrap_or_default();
        let Some(file) = self
            .edits
            .get_mut(id)
            .filter(|file| matches!(file.state, EditState::Watching | EditState::Changed))
        else {
            return;
        };
        file.stamp = FileStamp::read(&file.local_path);
        let job = self.transfers.add_with(
            TransferRequest {
                source: local,
                source_path: file.local_path.to_string_lossy().into_owned(),
                target: Arc::clone(&file.file_system),
                target_directory: file.remote_directory.clone(),
                target_name: Some(file.name.clone()),
            },
            JobOptions {
                policy: Some(ConflictPolicy::Overwrite),
                site_limits: site_limits.clone(),
                ..JobOptions::default()
            },
        );
        file.job = Some(job);
        file.state = EditState::Uploading;
        self.transfer_targets.insert(
            job,
            TransferTarget {
                file_system: Arc::clone(&file.file_system),
                directory: file.remote_directory.clone(),
                direction: Direction::Upload,
                site_limits,
            },
        );
    }

    pub(crate) fn finish_edit_transfer(&mut self, job: JobId, status: &JobStatus) -> Task<Message> {
        let Some(index) = self
            .edits
            .files
            .iter()
            .position(|file| file.job == Some(job))
        else {
            return Task::none();
        };
        let file = &mut self.edits.files[index];
        file.job = None;
        let completed = *status == JobStatus::Completed;
        match file.state {
            EditState::Downloading if completed => {
                file.stamp = FileStamp::read(&file.local_path);
                file.state = EditState::Watching;
                let path = file.local_path.clone();
                let editor = file.editor.clone();
                return self.launch(path, editor);
            }
            EditState::Downloading => {
                let file = self.edits.files.remove(index);
                self.notice = Some(
                    self.strings
                        .format("edit.download_failed", &[("name", &file.name)]),
                );
            }
            EditState::Uploading if completed => file.state = EditState::Watching,
            EditState::Uploading => {
                file.state = EditState::Changed;
                let name = file.name.clone();
                self.notice = Some(
                    self.strings
                        .format("edit.upload_failed", &[("name", &name)]),
                );
            }
            EditState::Watching | EditState::Changed => {}
        }
        Task::none()
    }

    // Stopping

    pub(crate) fn stop_edits_for_tab(&mut self, tab: TabId) {
        self.stop_edits(|file| file.tab == tab);
    }

    fn stop_edits(&mut self, matches: impl Fn(&EditedFile) -> bool) {
        let transfers = &self.transfers;
        self.edits.files.retain(|file| {
            if !matches(file) {
                return true;
            }
            if let Some(job) = file.job.filter(|_| file.state == EditState::Downloading) {
                transfers.cancel(job);
            }
            false
        });
    }

    pub(crate) fn remove_edit_folder(&mut self) {
        self.stop_edits(|_| true);
        if let Some(root) = self.edits.root.take() {
            let _ = fs::remove_dir_all(root);
        }
    }
}

// Subscriptions

pub fn subscription(app: &Hoplix) -> Subscription<Message> {
    if app
        .edits
        .files
        .iter()
        .any(|file| file.state == EditState::Watching)
    {
        iced::time::every(POLL_INTERVAL).map(|_| Message::Edit(EditMessage::Poll))
    } else {
        Subscription::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-edits-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("creating the test directory failed");
        directory
    }

    // Change detection

    #[test]
    fn stamps_notice_size_and_time_changes() {
        let directory = temporary_directory("stamps");
        let path = directory.join("a.txt");
        assert_eq!(FileStamp::read(&path), None);
        fs::write(&path, b"one").expect("writing failed");
        let first = FileStamp::read(&path);
        assert!(first.is_some());
        assert!(!has_changed(first, FileStamp::read(&path)));

        fs::write(&path, b"three").expect("writing failed");
        let second = FileStamp::read(&path);
        assert!(has_changed(first, second));

        let earlier = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_modified(earlier))
            .expect("setting the time failed");
        assert!(has_changed(second, FileStamp::read(&path)));

        fs::remove_file(&path).expect("removing failed");
        assert!(!has_changed(second, FileStamp::read(&path)));
        assert_eq!(FileStamp::read(&directory), None);
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Temporary files

    #[test]
    fn local_names_are_safe_on_every_system() {
        assert_eq!(local_file_name("index.html"), "index.html");
        assert_eq!(local_file_name("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(local_file_name("notes. "), "notes");
        assert_eq!(local_file_name("..."), "file");
        assert_eq!(local_file_name("con.txt"), "_con.txt");
        assert_eq!(local_file_name("Lpt1"), "_Lpt1");
        assert_eq!(local_file_name("console.txt"), "console.txt");
        assert_eq!(local_file_name("tab\there"), "tab_here");
        assert_eq!(local_file_name("Übersicht.md"), "Übersicht.md");
    }

    #[test]
    fn every_session_and_file_gets_its_own_folder() {
        assert_eq!(session_folder_name(42, 255), "hoplix-edit-42-ff");
        let base = temporary_directory("folders");
        let mut first = Edits::default();
        let mut second = Edits::default();
        let (one, folder_one) = first.next_folder(&base).expect("creating failed");
        let (two, folder_two) = first.next_folder(&base).expect("creating failed");
        let (_, other) = second.next_folder(&base).expect("creating failed");
        assert_eq!((one, two), (1, 2));
        assert_ne!(folder_one, folder_two);
        assert_eq!(folder_one.parent(), first.root());
        assert_eq!(folder_two.parent(), first.root());
        assert_ne!(first.root(), second.root());
        assert!(folder_one.is_dir() && folder_two.is_dir() && other.is_dir());
        assert!(
            first
                .root()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("hoplix-edit-"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&folder_one)
                .expect("reading failed")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        fs::remove_dir_all(base).expect("cleaning up failed");
    }

    // Stale folders

    #[test]
    fn only_exact_folder_names_belong_to_a_session() {
        assert_eq!(session_folder_process("hoplix-edit-42-ff"), Some(42));
        assert_eq!(
            session_folder_process(&session_folder_name(7, u128::MAX)),
            Some(7)
        );
        for name in [
            "hoplix-edit-42",
            "hoplix-edit-42-",
            "hoplix-edit--ff",
            "hoplix-edit-42-FF",
            "hoplix-edit-42-fg",
            "hoplix-edit-+42-ff",
            "hoplix-edit-42-ff-1",
            "hoplix-edit-42-ff.txt",
            "hoplix-edit-99999999999-ff",
            "hoplix-edits-42-ff",
            "Hoplix-edit-42-ff",
            "my-hoplix-edit-42-ff",
            "hoplix-edit-42-11111111111111111111111111111111f",
        ] {
            assert_eq!(session_folder_process(name), None, "{name}");
        }
    }

    #[test]
    fn folders_of_ended_processes_are_removed() {
        let base = temporary_directory("stale");
        let own = std::process::id();
        let names = [
            ("hoplix-edit-101-1a", true),
            ("hoplix-edit-102-2b", true),
            ("hoplix-edit-103-3c", false),
            ("hoplix-edit-104-4d", false),
            ("hoplix-edit-101-x", false),
            ("hoplix-edit-101", false),
            ("other-folder", false),
        ];
        for (name, _) in names {
            fs::create_dir_all(base.join(name).join("1")).expect("creating failed");
            fs::write(base.join(name).join("1").join("a.txt"), b"a").expect("writing failed");
        }
        let own_folder = base.join(session_folder_name(own, 1));
        fs::create_dir_all(&own_folder).expect("creating failed");
        fs::write(base.join("hoplix-edit-105-5e"), b"not a folder").expect("writing failed");

        let asked = std::sync::Mutex::new(Vec::new());
        let removed = remove_stale_folders(&base, |process| {
            asked.lock().expect("lock poisoned").push(process);
            match process {
                101 => ProcessState::Exited,
                102 => ProcessState::Other,
                103 => ProcessState::Hoplix,
                _ => ProcessState::Unknown,
            }
        });
        assert_eq!(removed, 2);
        for (name, gone) in names {
            assert_eq!(!base.join(name).exists(), gone, "{name}");
        }
        assert!(own_folder.is_dir());
        assert!(base.join("hoplix-edit-105-5e").is_file());
        let mut asked = asked.into_inner().expect("lock poisoned");
        asked.sort_unstable();
        assert!(!asked.contains(&own));
        assert!(!asked.contains(&105));
        assert_eq!(
            remove_stale_folders(&base.join("missing"), |_| ProcessState::Exited),
            0
        );
        fs::remove_dir_all(base).expect("cleaning up failed");
    }

    #[test]
    fn process_names_tell_hoplix_apart() {
        assert_eq!(classify_process("hoplix"), ProcessState::Hoplix);
        assert_eq!(classify_process("Hoplix\n"), ProcessState::Hoplix);
        assert_eq!(classify_process("notepad"), ProcessState::Other);
        assert_eq!(classify_process(" "), ProcessState::Unknown);
        let own = std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .expect("no executable name");
        assert_eq!(classify_process(&own), ProcessState::Hoplix);
    }

    #[test]
    fn the_running_process_is_seen_as_alive() {
        #[cfg(any(windows, target_os = "linux"))]
        assert_eq!(process_state(std::process::id()), ProcessState::Hoplix);
        #[cfg(any(windows, target_os = "linux"))]
        assert_eq!(process_state(u32::MAX - 2), ProcessState::Exited);
    }
}
