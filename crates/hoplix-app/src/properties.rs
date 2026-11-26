use std::sync::Arc;

use hoplix_core::{
    Error, FolderSize, PermissionChange, Properties, folder_size, mode_text, parse_mode,
    set_permissions_with,
};
use iced::Task;

use crate::app::{Dialog, Hoplix, LoadState, Message, TabId};

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    Loading,
    Ready(Box<Properties>),
    Failed(Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeState {
    Idle,
    Counting,
    Done(FolderSize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertiesDialog {
    pub tab: TabId,
    pub path: String,
    pub name: String,
    pub remote: bool,
    pub can_change: bool,
    pub loaded: Loaded,
    pub size: SizeState,
    pub mode: String,
    pub recursive: bool,
    pub files: bool,
    pub folders: bool,
    pub applying: bool,
    pub error: Option<Error>,
}

#[derive(Debug, Clone)]
pub enum PropertiesMessage {
    Open,
    Loaded(String, Result<Properties, Error>),
    CountSize,
    CancelSize,
    Counted(String, FolderSize),
    Mode(String),
    Bit(u32, bool),
    Recursive(bool),
    Files(bool),
    Folders(bool),
    Applied(Result<usize, Error>),
}

// Dialog

impl PropertiesDialog {
    pub fn properties(&self) -> Option<&Properties> {
        match &self.loaded {
            Loaded::Ready(properties) => Some(properties),
            _ => None,
        }
    }

    pub fn is_directory(&self) -> bool {
        self.properties()
            .is_some_and(|properties| properties.entry.is_directory())
    }

    pub fn shows_permissions(&self) -> bool {
        self.properties()
            .is_some_and(|properties| properties.permissions.is_some() || self.can_change)
            && self
                .properties()
                .is_some_and(|properties| properties.attributes.is_none())
    }

    pub fn edited_mode(&self) -> Option<u32> {
        parse_mode(&self.mode)
    }

    pub fn has_changes(&self) -> bool {
        let original = self
            .properties()
            .and_then(|properties| properties.permissions);
        match self.edited_mode() {
            Some(mode) => Some(mode) != original || (self.recursive && self.is_directory()),
            None => false,
        }
    }

    pub fn can_confirm(&self) -> bool {
        if self.applying {
            return false;
        }
        if !self.can_change || !self.shows_permissions() {
            return true;
        }
        self.edited_mode().is_some()
    }

    pub fn change(&self) -> Option<PermissionChange> {
        let recursive = self.recursive && self.is_directory();
        Some(PermissionChange {
            mode: self.edited_mode()?,
            recursive,
            files: !recursive || self.files,
            folders: !recursive || self.folders,
        })
    }

    fn set_bit(&mut self, bit: u32, on: bool) {
        let current = self
            .edited_mode()
            .or_else(|| {
                self.properties()
                    .and_then(|properties| properties.permissions)
            })
            .unwrap_or(0);
        let mode = if on { current | bit } else { current & !bit };
        self.mode = mode_text(mode);
    }
}

// Messages

impl Hoplix {
    pub(crate) fn update_properties(&mut self, message: PropertiesMessage) -> Task<Message> {
        if let PropertiesMessage::Open = message {
            return self.open_properties();
        }
        let Some(Dialog::Properties(dialog)) = &mut self.dialog else {
            return Task::none();
        };
        let tab = dialog.tab;
        match message {
            PropertiesMessage::Open => {}
            PropertiesMessage::Loaded(path, result) => {
                if path != dialog.path {
                    return Task::none();
                }
                match result {
                    Ok(properties) => {
                        dialog.mode = properties.permissions.map(mode_text).unwrap_or_default();
                        dialog.loaded = Loaded::Ready(Box::new(properties));
                    }
                    Err(error) => dialog.loaded = Loaded::Failed(error),
                }
            }
            PropertiesMessage::CountSize => {
                dialog.size = SizeState::Counting;
                let path = dialog.path.clone();
                let Some(file_system) = self.tab_by_id(tab).map(|tab| Arc::clone(&tab.file_system))
                else {
                    return Task::none();
                };
                let counted = path.clone();
                let (task, handle) = Task::perform(
                    async move { folder_size(file_system, &path).await },
                    move |size| {
                        Message::Properties(PropertiesMessage::Counted(counted.clone(), size))
                    },
                )
                .abortable();
                self.stop_properties_task();
                self.properties_task = Some(handle);
                return task;
            }
            PropertiesMessage::CancelSize => {
                dialog.size = SizeState::Idle;
                self.stop_properties_task();
            }
            PropertiesMessage::Counted(path, size) => {
                if path == dialog.path {
                    dialog.size = SizeState::Done(size);
                }
                self.properties_task = None;
            }
            PropertiesMessage::Mode(mode) => {
                dialog.mode = mode;
                dialog.error = None;
            }
            PropertiesMessage::Bit(bit, on) => {
                dialog.set_bit(bit, on);
                dialog.error = None;
            }
            PropertiesMessage::Recursive(value) => dialog.recursive = value,
            PropertiesMessage::Files(value) => dialog.files = value,
            PropertiesMessage::Folders(value) => dialog.folders = value,
            PropertiesMessage::Applied(result) => {
                dialog.applying = false;
                match result {
                    Ok(_) => {
                        self.dialog = None;
                        return self.refresh(tab);
                    }
                    Err(error) => dialog.error = Some(error),
                }
            }
        }
        Task::none()
    }

    fn open_properties(&mut self) -> Task<Message> {
        let Some(tab) = self
            .focused_tab()
            .filter(|tab| !tab.path.is_empty() && tab.state != LoadState::Disconnected)
        else {
            return Task::none();
        };
        let path = match tab.selected_entry() {
            Some(entry) => tab.file_system.join(&tab.path, &entry.name),
            None if tab.selection.is_empty() => tab.path.clone(),
            None => return Task::none(),
        };
        let file_system = Arc::clone(&tab.file_system);
        self.dialog = Some(Dialog::Properties(Box::new(PropertiesDialog {
            tab: tab.id,
            name: file_system.file_name(&path).to_string(),
            path: path.clone(),
            remote: file_system.is_remote(),
            can_change: file_system.can_set_permissions(),
            loaded: Loaded::Loading,
            size: SizeState::Idle,
            mode: String::new(),
            recursive: false,
            files: true,
            folders: true,
            applying: false,
            error: None,
        })));
        Task::perform(
            async move {
                let result = file_system.properties(&path).await;
                (path, result)
            },
            |(path, result)| Message::Properties(PropertiesMessage::Loaded(path, result)),
        )
    }

    pub(crate) fn confirm_properties(&mut self) -> Task<Message> {
        let Some(Dialog::Properties(dialog)) = &mut self.dialog else {
            return Task::none();
        };
        let change = dialog
            .change()
            .filter(|_| dialog.can_change && dialog.has_changes());
        let Some(change) = change else {
            self.dialog = None;
            self.stop_properties_task();
            return Task::none();
        };
        dialog.applying = true;
        dialog.error = None;
        let path = dialog.path.clone();
        let tab = dialog.tab;
        let Some(file_system) = self.tab_by_id(tab).map(|tab| Arc::clone(&tab.file_system)) else {
            return Task::none();
        };
        Task::perform(
            async move { set_permissions_with(file_system, &path, change).await },
            |result| Message::Properties(PropertiesMessage::Applied(result)),
        )
    }

    pub(crate) fn stop_properties_task(&mut self) {
        if let Some(handle) = self.properties_task.take() {
            handle.abort();
        }
    }
}
