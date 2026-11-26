use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use std::collections::{HashMap, VecDeque};

use hoplix_core::{
    Bookmarks, BoxFuture, Conflict, ConflictDecision, ConnectionOptions, ConnectionParams, Entry,
    Error, ErrorKind, FileSystem, HostAddress, HostKeyTrust, JobId, JobSnapshot, Keyring,
    LocalFileSystem, Moved, RecentList, Route, SecretBackend, SiteList, SortKey, SortOrder,
    TransferEvent, TransferQueue, TrustStore, Vault, config_dir, is_valid_name, remove_recursive,
    sort_entries,
};
use iced::keyboard::{Key, Modifiers};
use iced::theme::Mode;
use iced::widget::scrollable::Viewport;
use iced::{Element, Point, Subscription, Task, Theme, window};

use crate::commands::{Action, Palette};
use crate::compare::{CompareMessage, CompareState, SyncDialog};
use crate::connect_form::{ConnectForm, Field};
use crate::edits::{EditMessage, Edits, Opener};
use crate::file_drop::FileDrop;
use crate::file_list::{BookmarksDialog, ListMessage, ListState};
use crate::i18n::{self, LanguageInfo, Strings};
use crate::preview::{PreviewMessage, PreviewState};
use crate::properties::{PropertiesDialog, PropertiesMessage};
use crate::queue_store::{HistoryMessage, HistoryView, QueueStore};
use crate::search::{SearchDialog, SearchMessage, SearchResults};
use crate::session::{Offline, PreviewLayout, SavedTab, Session, TabOrigin, WindowState};
use crate::settings::{Layout, Settings, ThemePreference};
use crate::site_actions::PendingAction;
use crate::site_exchange::{ExchangeMessage, ExportDialog, ImportDialog};
use crate::site_form::{SiteField, SiteForm};
use crate::transfer_settings::TransferSetting;
use crate::transfers::{Drag, DropTarget, TransferAction, TransferTarget};
use crate::updates::{CheckState, UpdateMessage, UpdateSource};
use crate::{
    edits, file_list, opener, preview, queue_store, theme, title_bar, transfers, ui, updates,
};

// Types

pub type TabId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    Loading,
    Ready,
    Failed(Error),
    Disconnected,
}

#[derive(Debug)]
pub struct Tab {
    pub id: TabId,
    pub file_system: Arc<dyn FileSystem>,
    pub path: String,
    pub path_input: String,
    pub entries: Vec<Entry>,
    pub selection: Vec<usize>,
    pub(crate) anchor: Option<usize>,
    pub(crate) cursor: Option<usize>,
    pub state: LoadState,
    pub sort_key: SortKey,
    pub sort_order: SortOrder,
    pub title: Option<String>,
    pub origin: TabOrigin,
    pub(crate) viewport: Option<Viewport>,
    pub list: ListState,
    pub search: Option<SearchResults>,
    start_path: Option<String>,
    request: u64,
}

#[derive(Debug, Default)]
pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Browser,
    Connect,
    Sites,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteSelection {
    Site(String),
    Folder(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MasterMode {
    Unlock,
    Create,
    Change,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    NewFolder {
        tab: TabId,
        name: String,
    },
    Rename {
        tab: TabId,
        original: String,
        name: String,
    },
    Delete {
        tab: TabId,
        names: Vec<String>,
        is_directory: bool,
    },
    Conflict {
        conflict: Conflict,
        apply_to_all: bool,
    },
    HostKey {
        fingerprint: String,
        certificate: bool,
        host: String,
        jump: bool,
        address: HostAddress,
    },
    NewSiteFolder {
        parent: String,
        name: String,
    },
    DeleteSite {
        id: String,
        name: String,
    },
    DeleteSiteFolder {
        folder: String,
        sites: usize,
    },
    MasterPassword {
        mode: MasterMode,
        password: String,
        confirm: String,
        error: Option<Error>,
        busy: bool,
    },
    Sync(Box<SyncDialog>),
    Update {
        version: String,
        url: String,
    },
    ExportSites(Box<ExportDialog>),
    ImportSites(Box<ImportDialog>),
    Search(Box<SearchDialog>),
    Properties(Box<PropertiesDialog>),
    Bookmarks(Box<BookmarksDialog>),
}

#[derive(Debug, Clone)]
pub enum Message {
    SystemThemeChanged(Mode),
    ShowPage(Page),
    ThemeSelected(ThemePreference),
    LanguageSelected(Option<String>),
    LayoutSelected(Layout),
    NewTab(usize),
    SelectTab(TabId),
    CloseTab(TabId),
    Tab(TabId, TabMessage),
    HomeResolved(TabId, Result<String, Error>),
    Listed(TabId, u64, String, Result<Vec<Entry>, Error>),
    ConnectField(Field),
    ConnectSubmitted,
    Connected(Result<Arc<dyn FileSystem>, Error>),
    HostKeyAnswered(Option<bool>),
    DialogInput(String),
    DialogConfirmed,
    DialogCancelled,
    OperationFinished(TabId, Result<(), Error>),
    SiteSelected(SiteSelection),
    SiteNew,
    SiteFolderNew,
    SiteDuplicate,
    SiteDelete,
    SiteField(SiteField),
    SiteSave {
        connect: bool,
    },
    SiteEditCancelled,
    SiteConnect(String),
    SecretsLoaded(String, Result<Vec<(String, String)>, Error>),
    SecretSaved(Result<(), Error>),
    ConnectSaveAsSite,
    SecretBackendSelected(SecretBackend),
    ChangeMasterPassword,
    DialogConfirmInput(String),
    VaultUnlocked(Result<Vault, Error>),
    VaultCreated(Result<Moved<Vault>, Error>),
    VaultChanged(Result<Vault, Error>),
    KeyringRestored(Result<Moved<()>, Error>),
    TransferEvent(TransferEvent),
    Transfer(TransferAction),
    TransferTo {
        from: TabId,
        to: TabId,
    },
    ConflictAnswered(ConflictDecision),
    ConflictApplyToAll(bool),
    TransferSetting(TransferSetting),
    ModifiersChanged(Modifiers),
    DragEntered(DropTarget),
    DragLeft(DropTarget, Option<DropTarget>),
    CursorMoved(Point),
    MouseReleased,
    KeyPressed {
        key: Key,
        modifiers: Modifiers,
        captured: bool,
    },
    RunAction(Action),
    Compare(CompareMessage),
    PaletteQuery(String),
    PaletteMove(isize),
    PaletteSubmitted,
    PaletteClosed,
    Reconnect(TabId),
    Window(window::Id, window::Event),
    WindowClosing(window::Id, bool),
    Edit(EditMessage),
    Update(UpdateMessage),
    RecentSelected(usize),
    RecentRemoved(usize),
    RecentCleared,
    RememberRecent(bool),
    SiteExchange(ExchangeMessage),
    History(HistoryMessage),
    List(ListMessage),
    Search(SearchMessage),
    Properties(PropertiesMessage),
    Preview(PreviewMessage),
}

#[derive(Debug, Clone)]
pub enum TabMessage {
    Select(usize),
    Open(usize),
    Up,
    Refresh,
    PathEdited(String),
    PathSubmitted,
    SortBy(SortKey),
    NewFolder,
    Rename,
    Delete,
    CopyToOtherPane,
    Scrolled(Viewport),
    Edit,
}

pub struct Hoplix {
    pub settings: Settings,
    pub system_mode: Mode,
    pub languages: Vec<LanguageInfo>,
    pub strings: Strings,
    pub panes: Vec<Pane>,
    pub focused_pane: usize,
    pub page: Page,
    pub notice: Option<String>,
    pub connect_form: ConnectForm,
    pub recent: RecentList,
    pub dialog: Option<Dialog>,
    pub sites: SiteList,
    pub site_selection: Option<SiteSelection>,
    pub site_form: Option<SiteForm>,
    pub secret_backend: SecretBackend,
    pub vault: Option<Vault>,
    pub(crate) pending: Option<PendingAction>,
    pub(crate) config_dir: Option<PathBuf>,
    pub transfers: TransferQueue,
    pub transfer_jobs: Vec<JobSnapshot>,
    pub transfer_panel_open: bool,
    pub transfer_targets: HashMap<JobId, TransferTarget>,
    pub(crate) conflicts: VecDeque<Conflict>,
    pub(crate) store: QueueStore,
    pub history: HistoryView,
    pub(crate) modifiers: Modifiers,
    pub drag: Option<Drag>,
    pub drop_target: Option<DropTarget>,
    pub palette: Option<Palette>,
    pub compare: CompareState,
    pub(crate) window: WindowState,
    window_id: Option<window::Id>,
    pub(crate) reconnect: Option<(TabId, String)>,
    pub(crate) trusted: Vec<HostKeyTrust>,
    pub(crate) services: Services,
    pub edits: Edits,
    pub update_state: CheckState,
    pub bookmarks: Bookmarks,
    pub(crate) search_handles: HashMap<TabId, iced::task::Handle>,
    pub(crate) search_generation: u64,
    pub(crate) properties_task: Option<iced::task::Handle>,
    pub preview: PreviewState,
    pub pointer: Option<DropTarget>,
    pub file_drop: Option<FileDrop>,
    next_tab_id: TabId,
}

pub(crate) type Connector = Arc<
    dyn Fn(
            ConnectionParams,
            Route,
            ConnectionOptions,
            TrustStore,
            Vec<HostKeyTrust>,
        ) -> BoxFuture<'static, Result<Arc<dyn FileSystem>, Error>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub(crate) struct Services {
    pub keyring: Keyring,
    pub connector: Connector,
    pub local: Arc<dyn FileSystem>,
    pub opener: Opener,
    pub temp_dir: PathBuf,
    pub update_source: UpdateSource,
    pub update_delay: Duration,
    pub preview_delay: Duration,
}

impl Default for Services {
    fn default() -> Self {
        Self {
            keyring: Keyring::default(),
            connector: Arc::new(|params, route, options, trust_store, trusted| {
                Box::pin(async move {
                    hoplix_core::connect_via_with(&params, &route, &options, &trust_store, &trusted)
                        .await
                })
            }),
            local: Arc::new(LocalFileSystem),
            opener: Arc::new(opener::open),
            temp_dir: std::env::temp_dir(),
            update_source: updates::github_source(),
            update_delay: updates::STARTUP_DELAY,
            preview_delay: preview::DEBOUNCE,
        }
    }
}

// Panes

impl Pane {
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }
}

impl Tab {
    pub fn selected_entry(&self) -> Option<&Entry> {
        match self.selection.as_slice() {
            [index] => self.entries.get(*index),
            _ => None,
        }
    }

    pub fn selected_entries(&self) -> Vec<&Entry> {
        self.selection
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .collect()
    }

    pub fn is_selected(&self, index: usize) -> bool {
        self.selection.contains(&index)
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selection.clear();
        self.anchor = None;
        self.cursor = None;
    }
}

impl Dialog {
    pub fn can_confirm(&self) -> bool {
        match self {
            Self::NewFolder { name, .. } => is_valid_name(name),
            Self::Rename { original, name, .. } => is_valid_name(name) && name.trim() != original,
            Self::NewSiteFolder { name, .. } => is_valid_name(name),
            Self::MasterPassword {
                mode,
                password,
                confirm,
                busy,
                ..
            } => {
                !busy
                    && !password.is_empty()
                    && (*mode == MasterMode::Unlock || password == confirm)
            }
            Self::Sync(dialog) => dialog.can_start(),
            Self::ExportSites(dialog) => dialog.can_confirm(),
            Self::ImportSites(dialog) => dialog.can_confirm(),
            Self::Search(dialog) => dialog.can_start(),
            Self::Properties(dialog) => dialog.can_confirm(),
            Self::Bookmarks(_) => true,
            Self::Delete { .. }
            | Self::Conflict { .. }
            | Self::HostKey { .. }
            | Self::DeleteSite { .. }
            | Self::DeleteSiteFolder { .. }
            | Self::Update { .. } => true,
        }
    }
}

// Application

impl Hoplix {
    pub fn new() -> (Self, Task<Message>) {
        let services = Services::default();
        edits::remove_stale_folders_in_background(services.temp_dir.clone());
        Self::load(config_dir(), services)
    }

    pub(crate) fn load(config_dir: Option<PathBuf>, services: Services) -> (Self, Task<Message>) {
        let settings = Settings::load(config_dir.as_deref());
        let languages = i18n::available_languages();
        let strings = i18n::load(&i18n::resolve(settings.language.as_deref(), &languages));
        let mut app = Self::with_settings(settings, config_dir);
        app.services = services;
        app.apply_transfer_settings();
        app.languages = languages;
        app.strings = strings;
        if let Some(directory) = &app.config_dir {
            match SiteList::load(directory) {
                Ok(sites) => app.sites = sites,
                Err(error) => {
                    app.notice = Some(
                        app.strings
                            .format("sites.load_failed", &[("detail", &error.to_string())]),
                    );
                }
            }
        }
        app.load_recent();
        app.load_history();
        app.restore_queue();
        if let Some(directory) = &app.config_dir {
            app.bookmarks = Bookmarks::load(directory).unwrap_or_default();
        }
        let session = app
            .config_dir
            .as_deref()
            .map(Session::load)
            .unwrap_or_default();
        let mut tasks = vec![read_system_theme()];
        tasks.push(app.restore_session(&session));
        tasks.push(app.start_update_check());
        app.offer_update();
        (app, Task::batch(tasks))
    }

    pub(crate) fn with_settings(settings: Settings, config_dir: Option<PathBuf>) -> Self {
        let secret_backend = config_dir
            .as_deref()
            .map(SecretBackend::detect)
            .unwrap_or_default();
        Self {
            system_mode: theme::mode_override().unwrap_or_default(),
            languages: Vec::new(),
            strings: Strings::default(),
            panes: Vec::new(),
            focused_pane: 0,
            page: Page::Browser,
            notice: None,
            connect_form: ConnectForm::default(),
            recent: RecentList::default(),
            dialog: None,
            sites: SiteList::default(),
            site_selection: None,
            site_form: None,
            secret_backend,
            vault: None,
            pending: None,
            config_dir,
            transfers: TransferQueue::default(),
            transfer_jobs: Vec::new(),
            transfer_panel_open: false,
            transfer_targets: HashMap::new(),
            conflicts: VecDeque::new(),
            store: QueueStore::default(),
            history: HistoryView::default(),
            modifiers: Modifiers::default(),
            drag: None,
            drop_target: None,
            palette: None,
            compare: CompareState::default(),
            window: WindowState::default(),
            window_id: None,
            reconnect: None,
            trusted: Vec::new(),
            next_tab_id: 1,
            services: Services::default(),
            edits: Edits::default(),
            update_state: CheckState::default(),
            bookmarks: Bookmarks::default(),
            search_handles: HashMap::new(),
            search_generation: 0,
            properties_task: None,
            preview: PreviewState::default(),
            pointer: None,
            file_drop: None,
            settings,
        }
    }

    pub fn title(&self) -> String {
        let name = self.strings.get("app.name");
        match self.focused_tab() {
            Some(tab) if !tab.path.is_empty() => {
                format!("{} – {name}", ui::tab_title(tab))
            }
            _ => name.to_string(),
        }
    }

    pub fn theme(&self) -> Theme {
        let mode = match self.settings.theme {
            ThemePreference::System => theme::mode_override().unwrap_or(self.system_mode),
            ThemePreference::Light => Mode::Light,
            ThemePreference::Dark => Mode::Dark,
        };
        theme::app_theme(mode)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let theme_changes = if theme::mode_override().is_some() {
            Subscription::none()
        } else {
            iced::system::theme_changes().map(Message::SystemThemeChanged)
        };
        Subscription::batch([
            theme_changes,
            transfers::subscription(self),
            edits::subscription(self),
            queue_store::subscription(self),
            file_list::subscription(),
            preview::subscription(self),
        ])
    }

    pub fn view(&self) -> Element<'_, Message> {
        ui::view(self)
    }

    // Update

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let active = self.active_tabs();
        let task = self.handle(message);
        let task = match self.follow_active_tabs(&active) {
            Some(follow) => Task::batch([task, follow]),
            None => task,
        };
        match self.sync_preview() {
            Some(preview) => Task::batch([task, preview]),
            None => task,
        }
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SystemThemeChanged(mode) => {
                self.system_mode = mode;
                return self.apply_title_bar();
            }
            Message::ShowPage(page) => self.page = page,
            Message::ThemeSelected(theme) => {
                self.settings.theme = theme;
                self.save_settings();
                return self.apply_title_bar();
            }
            Message::LanguageSelected(language) => {
                self.settings.language = language;
                self.strings = i18n::load(&i18n::resolve(
                    self.settings.language.as_deref(),
                    &self.languages,
                ));
                self.save_settings();
            }
            Message::LayoutSelected(layout) => {
                self.settings.layout = layout;
                self.save_settings();
                return self.apply_layout();
            }
            Message::NewTab(pane) => return self.open_local_tab(pane, None),
            Message::SelectTab(id) => {
                if let Some((pane, index)) = self.locate(id) {
                    self.panes[pane].active = index;
                    self.focused_pane = pane;
                }
            }
            Message::CloseTab(id) => return self.close_tab(id),
            Message::Tab(id, message) => return self.update_tab(id, message),
            Message::HomeResolved(id, result) => return self.resolve_home(id, result),
            Message::Listed(id, request, path, result) => {
                let revealed = self.finish_listing(id, request, path, result);
                if self.tab_by_id(id).is_some_and(|tab| tab.search.is_none()) {
                    self.stop_search(id);
                }
                let listed = self.tab_listed(id);
                return match revealed {
                    Some(position) => Task::batch([listed, Self::scroll_to_revealed(id, position)]),
                    None => listed,
                };
            }
            Message::Compare(message) => return self.update_compare(message),
            Message::ConnectField(field) => self.connect_form.edit(field),
            Message::ConnectSubmitted => return self.connect(None),
            Message::Connected(result) => return self.finish_connect(result),
            Message::HostKeyAnswered(answer) => return self.answer_host_key(answer),
            Message::DialogInput(value) => match &mut self.dialog {
                Some(
                    Dialog::NewFolder { name, .. }
                    | Dialog::Rename { name, .. }
                    | Dialog::NewSiteFolder { name, .. },
                ) => *name = value,
                Some(Dialog::MasterPassword {
                    password, error, ..
                }) => {
                    *password = value;
                    *error = None;
                }
                _ => {}
            },
            Message::DialogConfirmInput(value) => {
                if let Some(Dialog::MasterPassword { confirm, error, .. }) = &mut self.dialog {
                    *confirm = value;
                    *error = None;
                }
            }
            Message::DialogConfirmed => return self.confirm_dialog(),
            Message::DialogCancelled => {
                if matches!(self.dialog, Some(Dialog::HostKey { .. })) {
                    return self.answer_host_key(None);
                }
                if let Some(Dialog::Conflict { conflict, .. }) = &self.dialog {
                    self.transfers.cancel(conflict.job);
                    self.dialog = None;
                    self.show_next_conflict();
                    return Task::none();
                }
                if matches!(self.dialog, Some(Dialog::MasterPassword { .. })) {
                    self.pending = None;
                }
                if matches!(self.dialog, Some(Dialog::Properties(_))) {
                    self.stop_properties_task();
                }
                self.dialog = None;
            }
            Message::OperationFinished(id, result) => match result {
                Ok(()) => return self.refresh(id),
                Err(error) => {
                    if let Some(tab) = self.tab_mut(id) {
                        tab.state = LoadState::Failed(error);
                    }
                }
            },
            Message::KeyPressed {
                key,
                modifiers,
                captured,
            } => return self.handle_key(&key, modifiers, captured),
            Message::RunAction(action) => {
                self.palette = None;
                return self.run_action(action);
            }
            message @ (Message::PaletteQuery(_)
            | Message::PaletteMove(_)
            | Message::PaletteSubmitted
            | Message::PaletteClosed) => return self.update_palette(message),
            Message::Reconnect(id) => return self.reconnect_tab(id),
            Message::Window(id, event) => return self.track_window(id, event),
            Message::WindowClosing(id, maximized) => {
                self.window.maximized = maximized;
                self.save_session();
                self.persist(true);
                self.remove_edit_folder();
                return window::close(id);
            }
            message @ (Message::TransferEvent(_)
            | Message::Transfer(_)
            | Message::TransferTo { .. }
            | Message::ConflictAnswered(_)
            | Message::ConflictApplyToAll(_)
            | Message::ModifiersChanged(_)
            | Message::DragEntered(_)
            | Message::DragLeft(..)
            | Message::CursorMoved(_)
            | Message::MouseReleased) => {
                self.track_pointer(&message);
                return self.update_transfers(message);
            }
            Message::Edit(message) => return self.update_edits(message),
            Message::SiteExchange(message) => return self.update_exchange(message),
            Message::History(message) => return self.update_history(message),
            message @ (Message::RecentSelected(_)
            | Message::RecentRemoved(_)
            | Message::RecentCleared
            | Message::RememberRecent(_)) => return self.update_recent(message),
            Message::Update(message) => return self.update_updates(message),
            Message::List(message) => return self.update_list(message),
            Message::Search(message) => return self.update_search(message),
            Message::Properties(message) => return self.update_properties(message),
            Message::Preview(message) => return self.update_preview(message),
            Message::TransferSetting(setting) => self.update_transfer_setting(setting),
            message => return self.update_sites(message),
        }
        Task::none()
    }

    fn update_tab(&mut self, id: TabId, message: TabMessage) -> Task<Message> {
        if let TabMessage::Scrolled(viewport) = message {
            if let Some(tab) = self.tab_mut(id) {
                tab.viewport = Some(viewport);
            }
            return Task::none();
        }
        if let Some((pane, _)) = self.locate(id) {
            self.focused_pane = pane;
        }
        let modifiers = self.modifiers;
        let visible = self.tab_by_id(id).and_then(|tab| self.visible_indices(tab));
        let Some(tab) = self.tab_mut(id) else {
            return Task::none();
        };
        match message {
            TabMessage::Select(index) => {
                tab.cursor = Some(index);
                let collapse = transfers::select(tab, index, modifiers);
                if let Some(visible) = &visible {
                    tab.selection.retain(|item| visible.contains(item));
                }
                self.drag = Some(Drag::new(id, collapse));
            }
            TabMessage::Scrolled(_) => {}
            TabMessage::CopyToOtherPane => {
                if let Some(target) = self.other_pane_tab(id) {
                    return self.start_transfers(id, target);
                }
            }
            TabMessage::Open(index) => {
                let Some(entry) = tab.entries.get(index) else {
                    return Task::none();
                };
                if !entry.is_directory() {
                    return self.open_file(id, index, false);
                }
                let path = tab.file_system.join(&tab.path, &entry.name);
                return self.navigate(id, path);
            }
            TabMessage::Up => {
                if let Some(parent) = tab.file_system.parent(&tab.path) {
                    return self.navigate(id, parent);
                }
            }
            TabMessage::Refresh => return self.refresh(id),
            TabMessage::PathEdited(path) => tab.path_input = path,
            TabMessage::PathSubmitted => {
                let path = tab.path_input.trim().to_string();
                if !path.is_empty() {
                    return self.navigate(id, path);
                }
            }
            TabMessage::SortBy(key) => {
                tab.sort_order = if tab.sort_key == key {
                    tab.sort_order.reversed()
                } else {
                    SortOrder::Ascending
                };
                tab.sort_key = key;
                tab.clear_selection();
                sort_entries(&mut tab.entries, tab.sort_key, tab.sort_order);
            }
            TabMessage::NewFolder => {
                self.dialog = Some(Dialog::NewFolder {
                    tab: id,
                    name: String::new(),
                });
                return ui::focus_input();
            }
            TabMessage::Rename => {
                if let Some(entry) = tab.selected_entry() {
                    self.dialog = Some(Dialog::Rename {
                        tab: id,
                        original: entry.name.clone(),
                        name: entry.name.clone(),
                    });
                    return ui::focus_input();
                }
            }
            TabMessage::Edit => {
                if let [index] = tab.selection[..] {
                    return self.open_file(id, index, true);
                }
            }
            TabMessage::Delete => {
                let entries = tab.selected_entries();
                if !entries.is_empty() {
                    self.dialog = Some(Dialog::Delete {
                        tab: id,
                        names: entries.iter().map(|entry| entry.name.clone()).collect(),
                        is_directory: entries.iter().any(|entry| entry.is_directory()),
                    });
                }
            }
        }
        Task::none()
    }

    // Tabs

    pub(crate) fn open_local_tab(&mut self, pane: usize, start: Option<String>) -> Task<Message> {
        self.open_tab(pane, Arc::clone(&self.services.local), None, start)
    }

    pub(crate) fn open_tab(
        &mut self,
        pane: usize,
        file_system: Arc<dyn FileSystem>,
        title: Option<String>,
        start: Option<String>,
    ) -> Task<Message> {
        let Some(target) = self.panes.get_mut(pane) else {
            return Task::none();
        };
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let origin = if file_system.label().is_some() {
            TabOrigin::Direct
        } else {
            TabOrigin::Local
        };
        target.tabs.push(Tab {
            id,
            file_system: Arc::clone(&file_system),
            path: String::new(),
            path_input: String::new(),
            entries: Vec::new(),
            selection: Vec::new(),
            anchor: None,
            cursor: None,
            state: LoadState::Loading,
            sort_key: SortKey::default(),
            sort_order: SortOrder::default(),
            title,
            origin,
            viewport: None,
            list: ListState::default(),
            search: None,
            start_path: start,
            request: 0,
        });
        target.active = target.tabs.len() - 1;
        self.focused_pane = pane;
        Task::perform(async move { file_system.home().await }, move |result| {
            Message::HomeResolved(id, result)
        })
    }

    fn close_tab(&mut self, id: TabId) -> Task<Message> {
        let Some((pane, index)) = self.locate(id) else {
            return Task::none();
        };
        let target = &mut self.panes[pane];
        let tab = target.tabs.remove(index);
        if target.active > index || target.active >= target.tabs.len() {
            target.active = target.active.saturating_sub(1);
        }
        self.stop_edits_for_tab(id);
        self.store.connections.remove(&id);
        self.stop_search(id);
        let file_system = tab.file_system;
        Task::future(async move { file_system.close().await }).discard()
    }

    fn resolve_home(&mut self, id: TabId, result: Result<String, Error>) -> Task<Message> {
        let Some(tab) = self.tab_mut(id) else {
            return Task::none();
        };
        let fallback = if tab.file_system.label().is_some() || !cfg!(windows) {
            "/"
        } else {
            "C:\\"
        };
        let home = result.unwrap_or_else(|_| fallback.to_string());
        let path = match tab.start_path.take() {
            Some(start) if is_absolute(&start) => start,
            Some(start) => tab.file_system.join(&home, &start),
            None => home,
        };
        self.navigate(id, path)
    }

    pub(crate) fn refresh(&mut self, id: TabId) -> Task<Message> {
        let Some(path) = self.tab_mut(id).map(|tab| tab.path.clone()) else {
            return Task::none();
        };
        self.navigate(id, path)
    }

    pub(crate) fn navigate(&mut self, id: TabId, path: String) -> Task<Message> {
        let Some(tab) = self.tab_mut(id) else {
            return Task::none();
        };
        tab.request += 1;
        tab.state = LoadState::Loading;
        tab.list.step = None;
        let request = tab.request;
        let file_system = Arc::clone(&tab.file_system);
        Task::perform(
            async move {
                let result = file_system.list(&path).await;
                (path, result)
            },
            move |(path, result)| Message::Listed(id, request, path, result),
        )
    }

    fn finish_listing(
        &mut self,
        id: TabId,
        request: u64,
        path: String,
        result: Result<Vec<Entry>, Error>,
    ) -> Option<(usize, usize)> {
        let tab = self.tab_mut(id).filter(|tab| tab.request == request)?;
        match result {
            Ok(mut entries) => {
                sort_entries(&mut entries, tab.sort_key, tab.sort_order);
                tab.path_input.clone_from(&path);
                if tab.path != path {
                    tab.clear_selection();
                    tab.arrive();
                    tab.set_filter(String::new());
                    tab.search = None;
                } else {
                    let names: Vec<String> = tab
                        .selected_entries()
                        .iter()
                        .map(|entry| entry.name.clone())
                        .collect();
                    tab.selection = entries
                        .iter()
                        .enumerate()
                        .filter(|(_, entry)| names.contains(&entry.name))
                        .map(|(index, _)| index)
                        .collect();
                    tab.anchor = tab.selection.first().copied();
                }
                tab.entries = entries;
                tab.path = path;
                tab.state = LoadState::Ready;
                tab.reveal_pending()
            }
            Err(error) => {
                tab.forget_pending();
                tab.path_input.clone_from(&tab.path);
                tab.state = LoadState::Failed(error);
                None
            }
        }
    }

    // Connections

    pub(crate) fn connect(&mut self, trust: Option<HostKeyTrust>) -> Task<Message> {
        let route = self.connect_form.to_route();
        let params = match self
            .connect_form
            .to_params()
            .and_then(|params| route.clone().map(|route| (params, route)))
        {
            Ok(params) => params,
            Err(invalid) => {
                if invalid.is_route() {
                    self.connect_form.advanced_open = true;
                }
                self.connect_form.invalid = Some(invalid);
                return Task::none();
            }
        };
        let (params, route) = params;
        let Some(trust_store) = self.config_dir.as_deref().map(TrustStore::in_directory) else {
            self.connect_form.error = Some(Error::new(ErrorKind::Other, "known_hosts"));
            return Task::none();
        };
        let options = self
            .connect_form
            .site
            .as_ref()
            .and_then(|id| self.sites.get(id))
            .map(|site| site.options.clone())
            .unwrap_or_default();
        match trust {
            Some(trust) => self.trusted.push(trust),
            None => self.trusted.clear(),
        }
        self.connect_form.connecting = true;
        self.connect_form.error = None;
        Task::perform(
            (self.services.connector)(params, route, options, trust_store, self.trusted.clone()),
            Message::Connected,
        )
    }

    fn finish_connect(&mut self, result: Result<Arc<dyn FileSystem>, Error>) -> Task<Message> {
        self.connect_form.connecting = false;
        let error = match result {
            Ok(file_system) => {
                self.trusted.clear();
                return self.open_connection(file_system);
            }
            Err(error) => error,
        };
        let jumps = usize::from(self.connect_form.uses_jump());
        let repeated = self.trusted.iter().any(|trust| trust.answers(&error));
        if let Some(pending) = HostKeyTrust::for_error(&error, false)
            .filter(|_| !repeated && self.trusted.len() <= jumps)
        {
            self.dialog = Some(Dialog::HostKey {
                fingerprint: pending.fingerprint,
                certificate: error.kind() == ErrorKind::UntrustedCertificate,
                host: error
                    .hop()
                    .map_or_else(|| self.connect_form.host.trim().to_string(), str::to_string),
                jump: error.hop().is_some(),
                address: pending.address,
            });
            return Task::none();
        }
        self.trusted.clear();
        self.connect_form.error = Some(error);
        self.page = Page::Connect;
        Task::none()
    }

    fn answer_host_key(&mut self, answer: Option<bool>) -> Task<Message> {
        let Some(Dialog::HostKey {
            fingerprint,
            address,
            ..
        }) = self.dialog.take()
        else {
            return Task::none();
        };
        match answer {
            Some(remember) => self.connect(Some(HostKeyTrust {
                address,
                fingerprint,
                remember,
            })),
            None => {
                self.trusted.clear();
                Task::none()
            }
        }
    }

    // Dialogs

    fn confirm_dialog(&mut self) -> Task<Message> {
        let Some(dialog) = self.dialog.clone().filter(Dialog::can_confirm) else {
            return Task::none();
        };
        match dialog {
            Dialog::HostKey { .. } => return self.answer_host_key(Some(false)),
            Dialog::Conflict { .. } => return self.answer_conflict(ConflictDecision::Overwrite),
            Dialog::Sync(_) => return self.start_sync(),
            Dialog::Update { url, .. } => {
                self.dialog = None;
                self.open_release_page(&url);
                return Task::none();
            }
            Dialog::ExportSites(_) | Dialog::ImportSites(_) => {
                return self.confirm_exchange_dialog();
            }
            Dialog::Search(search) => {
                self.dialog = None;
                return self.start_search(&search);
            }
            Dialog::Properties(_) => return self.confirm_properties(),
            Dialog::Bookmarks(bookmarks) => {
                self.dialog = None;
                self.confirm_bookmarks(&bookmarks);
                return Task::none();
            }
            Dialog::NewSiteFolder { .. }
            | Dialog::DeleteSite { .. }
            | Dialog::DeleteSiteFolder { .. }
            | Dialog::MasterPassword { .. } => return self.confirm_site_dialog(dialog),
            Dialog::NewFolder { .. } | Dialog::Rename { .. } | Dialog::Delete { .. } => {}
        }
        self.dialog = None;
        let (id, operation) = match dialog {
            Dialog::NewFolder { tab, name } => (tab, Operation::CreateDirectory(name)),
            Dialog::Rename {
                tab,
                original,
                name,
            } => (tab, Operation::Rename(original, name)),
            Dialog::Delete { tab, names, .. } => (tab, Operation::Delete(names)),
            _ => return Task::none(),
        };
        let Some(tab) = self.tab_mut(id) else {
            return Task::none();
        };
        let file_system = Arc::clone(&tab.file_system);
        let directory = tab.path.clone();
        tab.state = LoadState::Loading;
        Task::perform(
            async move { operation.run(file_system.as_ref(), &directory).await },
            move |result| Message::OperationFinished(id, result),
        )
    }

    // Layout

    fn apply_layout(&mut self) -> Task<Message> {
        let wanted = self.settings.layout.pane_count();
        let mut tasks = Vec::new();
        while self.panes.len() > wanted {
            if let Some(removed) = self.panes.pop() {
                let first = &mut self.panes[0];
                first.tabs.extend(removed.tabs);
            }
        }
        while self.panes.len() < wanted {
            self.panes.push(Pane::default());
            tasks.push(self.open_local_tab(self.panes.len() - 1, None));
        }
        self.focused_pane = self.focused_pane.min(self.panes.len() - 1);
        Task::batch(tasks)
    }

    // Lookup

    pub fn focused_tab(&self) -> Option<&Tab> {
        self.panes.get(self.focused_pane).and_then(Pane::active_tab)
    }

    pub(crate) fn locate(&self, id: TabId) -> Option<(usize, usize)> {
        self.panes.iter().enumerate().find_map(|(pane, target)| {
            target
                .tabs
                .iter()
                .position(|tab| tab.id == id)
                .map(|index| (pane, index))
        })
    }

    pub(crate) fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.panes
            .iter_mut()
            .flat_map(|pane| pane.tabs.iter_mut())
            .find(|tab| tab.id == id)
    }

    // Settings

    pub(crate) fn save_settings(&mut self) {
        let Some(directory) = &self.config_dir else {
            return;
        };
        self.notice = self.settings.save(directory).err().map(|error| {
            self.strings
                .format("settings.save_failed", &[("detail", &error.to_string())])
        });
    }

    // Session

    fn restore_session(&mut self, session: &Session) -> Task<Message> {
        if let Some(window) = session.window {
            self.window = window;
        }
        if let Some(layout) = session.preview {
            self.preview.open = layout.open;
            self.preview.width = layout.width;
        }
        let pane_count = self.settings.layout.pane_count();
        self.panes = (0..pane_count).map(|_| Pane::default()).collect();
        let mut tasks = Vec::new();
        let mut active: Vec<Option<usize>> = vec![None; pane_count];
        for saved in &session.tabs {
            let pane = saved.pane.min(pane_count - 1);
            let opened = match &saved.origin {
                TabOrigin::Local => {
                    let start = Some(saved.path.clone()).filter(|path| !path.is_empty());
                    tasks.push(self.open_local_tab(pane, start));
                    true
                }
                TabOrigin::Site(id) => self.open_offline_tab(pane, id, &saved.path),
                TabOrigin::Direct => false,
            };
            if opened && saved.active {
                active[pane] = Some(self.panes[pane].tabs.len() - 1);
            }
        }
        for (index, pane) in self.panes.iter_mut().enumerate() {
            if let Some(position) = active[index] {
                pane.active = position;
            }
        }
        for pane in 0..pane_count {
            if self.panes[pane].tabs.is_empty() {
                tasks.push(self.open_local_tab(pane, None));
            }
        }
        self.focused_pane = session.focused_pane.min(pane_count - 1);
        Task::batch(tasks)
    }

    fn open_offline_tab(&mut self, pane: usize, site_id: &str, path: &str) -> bool {
        let Some(site) = self.sites.get(site_id).cloned() else {
            return false;
        };
        let Some(target) = self.panes.get_mut(pane) else {
            return false;
        };
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        target.tabs.push(Tab {
            id,
            file_system: Offline::shared(&site.name),
            path: path.to_string(),
            path_input: path.to_string(),
            entries: Vec::new(),
            selection: Vec::new(),
            anchor: None,
            cursor: None,
            state: LoadState::Disconnected,
            sort_key: SortKey::default(),
            sort_order: SortOrder::default(),
            title: Some(site.name.clone()),
            origin: TabOrigin::Site(site.id.clone()),
            viewport: None,
            list: ListState::default(),
            search: None,
            start_path: None,
            request: 0,
        });
        target.active = target.tabs.len() - 1;
        true
    }

    fn reconnect_tab(&mut self, id: TabId) -> Task<Message> {
        let Some(TabOrigin::Site(site)) = self.find_origin(id) else {
            return Task::none();
        };
        if let Some((pane, index)) = self.locate(id) {
            self.panes[pane].active = index;
            self.focused_pane = pane;
        }
        self.reconnect = Some((id, site.clone()));
        self.connect_site(&site)
    }

    pub(crate) fn take_reconnect(
        &mut self,
        file_system: &Arc<dyn FileSystem>,
    ) -> Option<Task<Message>> {
        let (id, site) = self.reconnect.take()?;
        if self.connect_form.site.as_deref() != Some(site.as_str()) {
            return None;
        }
        let tab = self.tab_mut(id)?;
        tab.file_system = Arc::clone(file_system);
        tab.state = LoadState::Loading;
        tab.clear_selection();
        let path = tab.path.clone();
        if path.is_empty() {
            let file_system = Arc::clone(file_system);
            return Some(Task::perform(
                async move { file_system.home().await },
                move |result| Message::HomeResolved(id, result),
            ));
        }
        Some(self.navigate(id, path))
    }

    pub(crate) fn set_origin(&mut self, id: TabId, origin: TabOrigin) {
        if let Some(tab) = self.tab_mut(id) {
            tab.origin = origin;
        }
    }

    pub(crate) fn last_tab_id(&self) -> TabId {
        self.next_tab_id - 1
    }

    fn find_origin(&self, id: TabId) -> Option<TabOrigin> {
        self.panes
            .iter()
            .flat_map(|pane| pane.tabs.iter())
            .find(|tab| tab.id == id)
            .map(|tab| tab.origin.clone())
    }

    pub(crate) fn session(&self) -> Session {
        let tabs = self
            .panes
            .iter()
            .enumerate()
            .flat_map(|(pane, target)| {
                target
                    .tabs
                    .iter()
                    .enumerate()
                    .map(move |(index, tab)| SavedTab {
                        pane,
                        origin: tab.origin.clone(),
                        path: tab.path.clone(),
                        active: index == target.active,
                    })
            })
            .collect();
        Session {
            window: Some(self.window),
            focused_pane: self.focused_pane,
            preview: Some(PreviewLayout {
                open: self.preview.open,
                width: self.preview.width,
            }),
            tabs,
        }
    }

    fn save_session(&mut self) {
        let Some(directory) = &self.config_dir else {
            return;
        };
        let _ = self.session().save(directory);
    }

    fn apply_title_bar(&self) -> Task<Message> {
        self.window_id
            .map_or_else(Task::none, |id| title_bar::apply(id, &self.theme()))
    }

    fn track_window(&mut self, id: window::Id, event: window::Event) -> Task<Message> {
        match event {
            window::Event::Opened { .. } => {
                self.window_id = Some(id);
                return self.apply_title_bar();
            }
            window::Event::Moved(position) => {
                self.window.x = Some(position.x);
                self.window.y = Some(position.y);
            }
            window::Event::Resized(size) => {
                self.window.width = size.width;
                self.window.height = size.height;
            }
            window::Event::CloseRequested => {
                return window::is_maximized(id)
                    .map(move |maximized| Message::WindowClosing(id, maximized));
            }
            window::Event::FileHovered(_)
            | window::Event::FileDropped(_)
            | window::Event::FilesHoveredLeft => {
                if self.page == Page::Browser && self.dialog.is_none() {
                    self.file_event(&event);
                } else {
                    self.file_drop = None;
                }
            }
            _ => {}
        }
        Task::none()
    }
}

// Operations

#[derive(Debug, Clone, PartialEq, Eq)]
enum Operation {
    CreateDirectory(String),
    Rename(String, String),
    Delete(Vec<String>),
}

impl Operation {
    async fn run(self, file_system: &dyn FileSystem, directory: &str) -> Result<(), Error> {
        match self {
            Self::CreateDirectory(name) => {
                let path = file_system.join(directory, name.trim());
                file_system.create_directory(&path).await
            }
            Self::Rename(from, to) => {
                let source = file_system.join(directory, &from);
                let target = file_system.join(directory, to.trim());
                file_system.rename(&source, &target).await
            }
            Self::Delete(names) => {
                for name in names {
                    let path = file_system.join(directory, &name);
                    remove_recursive(file_system, &path).await?;
                }
                Ok(())
            }
        }
    }
}

// Helpers

fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || std::path::Path::new(path).is_absolute()
}

// Tasks

fn read_system_theme() -> Task<Message> {
    if theme::mode_override().is_some() {
        Task::none()
    } else {
        iced::system::theme().map(Message::SystemThemeChanged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hoplix_core::EntryKind;

    fn app_with_layout(layout: Layout) -> Hoplix {
        let settings = Settings {
            layout,
            ..Settings::default()
        };
        let mut app = Hoplix::with_settings(settings, None);
        app.system_mode = Mode::Light;
        for _ in 0..layout.pane_count() {
            app.panes.push(Pane::default());
            let _ = app.open_local_tab(app.panes.len() - 1, None);
        }
        app
    }

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.to_string(),
            kind,
            size: Some(1),
            modified: None,
            is_link: false,
        }
    }

    fn file(name: &str) -> Entry {
        entry(name, EntryKind::File)
    }

    fn temporary_directory(name: &str) -> std::path::PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("creating the test directory failed");
        directory
    }

    // Tabs

    #[test]
    fn tabs_open_and_close() {
        let mut app = app_with_layout(Layout::Modern);
        let _ = app.update(Message::NewTab(0));
        assert_eq!(app.panes[0].tabs.len(), 2);
        assert_eq!(app.panes[0].active, 1);
        let _ = app.update(Message::CloseTab(2));
        assert_eq!(app.panes[0].tabs.len(), 1);
        assert_eq!(app.panes[0].active, 0);
        let _ = app.update(Message::CloseTab(1));
        assert!(app.panes[0].tabs.is_empty());
        assert!(app.focused_tab().is_none());
    }

    #[test]
    fn layout_switch_merges_and_splits_panes() {
        let mut app = app_with_layout(Layout::Classic);
        app.focused_pane = 1;
        app.settings.layout = Layout::Modern;
        let _ = app.apply_layout();
        assert_eq!(app.panes.len(), 1);
        assert_eq!(app.panes[0].tabs.len(), 2);
        assert_eq!(app.focused_pane, 0);
        app.settings.layout = Layout::Classic;
        let _ = app.apply_layout();
        assert_eq!(app.panes.len(), 2);
        assert_eq!(app.panes[1].tabs.len(), 1);
    }

    // Listing

    #[test]
    fn stale_listings_are_ignored_and_failures_keep_entries() {
        let mut app = app_with_layout(Layout::Modern);
        let _ = app.navigate(1, "/first".to_string());
        let _ = app.navigate(1, "/second".to_string());
        app.finish_listing(1, 1, "/first".to_string(), Ok(vec![file("old")]));
        assert_eq!(app.panes[0].tabs[0].state, LoadState::Loading);

        app.finish_listing(1, 2, "/second".to_string(), Ok(vec![file("b"), file("a")]));
        let tab = &app.panes[0].tabs[0];
        assert_eq!(tab.path, "/second");
        assert_eq!(tab.entries[0].name, "a");

        let _ = app.navigate(1, "/missing".to_string());
        let error = Error::new(ErrorKind::NotFound, "/missing");
        app.finish_listing(1, 3, "/missing".to_string(), Err(error.clone()));
        let tab = &app.panes[0].tabs[0];
        assert_eq!(tab.state, LoadState::Failed(error));
        assert_eq!(tab.path, "/second");
        assert_eq!(tab.path_input, "/second");
        assert_eq!(tab.entries.len(), 2);
    }

    #[test]
    fn sorting_toggles_the_order() {
        let mut app = app_with_layout(Layout::Modern);
        app.panes[0].tabs[0].entries = vec![file("a"), file("b")];
        let _ = app.update(Message::Tab(1, TabMessage::SortBy(SortKey::Name)));
        assert_eq!(app.panes[0].tabs[0].sort_order, SortOrder::Descending);
        assert_eq!(app.panes[0].tabs[0].entries[0].name, "b");
        let _ = app.update(Message::Tab(1, TabMessage::SortBy(SortKey::Size)));
        assert_eq!(app.panes[0].tabs[0].sort_order, SortOrder::Ascending);
    }

    // Dialogs

    #[test]
    fn dialogs_open_for_the_selection_and_validate_names() {
        let mut app = app_with_layout(Layout::Modern);
        app.panes[0].tabs[0].entries = vec![entry("docs", EntryKind::Directory), file("a.txt")];

        let _ = app.update(Message::Tab(1, TabMessage::Rename));
        assert_eq!(app.dialog, None);

        let _ = app.update(Message::Tab(1, TabMessage::Select(0)));
        let _ = app.update(Message::Tab(1, TabMessage::Delete));
        assert_eq!(
            app.dialog,
            Some(Dialog::Delete {
                tab: 1,
                names: vec!["docs".to_string()],
                is_directory: true
            })
        );
        let _ = app.update(Message::DialogCancelled);
        assert_eq!(app.dialog, None);

        let _ = app.update(Message::Tab(1, TabMessage::Rename));
        assert!(!app.dialog.as_ref().is_some_and(Dialog::can_confirm));
        let _ = app.update(Message::DialogInput("a/b".to_string()));
        assert!(!app.dialog.as_ref().is_some_and(Dialog::can_confirm));
        let _ = app.update(Message::DialogInput("documents".to_string()));
        assert!(app.dialog.as_ref().is_some_and(Dialog::can_confirm));
    }

    #[test]
    fn host_key_answers_close_the_prompt() {
        let mut app = app_with_layout(Layout::Modern);
        let error = Error::new(ErrorKind::UnknownHostKey, "SHA256:abc").at_address("host.test", 22);
        let _ = app.update(Message::Connected(Err(error)));
        assert_eq!(
            app.dialog,
            Some(Dialog::HostKey {
                fingerprint: "SHA256:abc".to_string(),
                certificate: false,
                host: String::new(),
                jump: false,
                address: HostAddress::new("host.test", 22),
            })
        );
        let _ = app.update(Message::DialogCancelled);
        assert_eq!(app.dialog, None);

        let error = Error::new(ErrorKind::UntrustedCertificate, "SHA256:AB:CD")
            .at_address("host.test", 990);
        let _ = app.update(Message::Connected(Err(error)));
        assert_eq!(
            app.dialog,
            Some(Dialog::HostKey {
                fingerprint: "SHA256:AB:CD".to_string(),
                certificate: true,
                host: String::new(),
                jump: false,
                address: HostAddress::new("host.test", 990),
            })
        );
        let _ = app.update(Message::DialogCancelled);
        assert!(!app.connect_form.connecting);

        let failure = Error::new(ErrorKind::AuthenticationFailed, "demo@host");
        let _ = app.update(Message::Connected(Err(failure.clone())));
        assert_eq!(app.connect_form.error, Some(failure));
    }

    #[test]
    fn invalid_connect_forms_do_not_connect() {
        let mut app = app_with_layout(Layout::Modern);
        let _ = app.update(Message::ConnectSubmitted);
        assert!(!app.connect_form.connecting);
        assert!(app.connect_form.invalid.is_some());
    }

    // Session

    #[test]
    fn sessions_restore_local_and_offline_tabs_and_reconnect_in_place() {
        let settings = Settings {
            layout: Layout::Classic,
            ..Settings::default()
        };
        let mut app = Hoplix::with_settings(settings, None);
        let site = hoplix_core::Site::new("Web");
        let site_id = site.id.clone();
        app.sites.upsert(site);
        let session = Session {
            window: None,
            focused_pane: 1,
            preview: None,
            tabs: vec![
                SavedTab {
                    pane: 0,
                    origin: TabOrigin::Local,
                    path: "/data".to_string(),
                    active: false,
                },
                SavedTab {
                    pane: 0,
                    origin: TabOrigin::Site(site_id.clone()),
                    path: "/var/www".to_string(),
                    active: true,
                },
                SavedTab {
                    pane: 7,
                    origin: TabOrigin::Site("gone".to_string()),
                    path: "/x".to_string(),
                    active: true,
                },
            ],
        };
        let _ = app.restore_session(&session);
        assert_eq!(app.panes.len(), 2);
        assert_eq!(app.panes[0].tabs.len(), 2);
        assert_eq!(app.panes[0].active, 1);
        assert_eq!(app.panes[0].tabs[1].state, LoadState::Disconnected);
        assert_eq!(app.panes[1].tabs.len(), 1);
        assert_eq!(app.focused_pane, 1);

        let saved = app.session();
        assert_eq!(saved.tabs.len(), 3);
        assert_eq!(saved.tabs[1].origin, TabOrigin::Site(site_id.clone()));
        assert_eq!(saved.tabs[1].path, "/var/www");
        assert!(saved.tabs[1].active);

        let offline = app.panes[0].tabs[1].id;
        let _ = app.update(Message::Reconnect(offline));
        assert_eq!(app.page, Page::Connect);
        assert_eq!(app.focused_pane, 0);
        let _ = app.open_connection(Arc::new(LocalFileSystem));
        assert_eq!(app.page, Page::Browser);
        assert_eq!(app.panes[0].tabs.len(), 2);
        let tab = &app.panes[0].tabs[1];
        assert_eq!(tab.state, LoadState::Loading);
        assert_eq!(tab.origin, TabOrigin::Site(site_id));
        assert!(tab.file_system.label().is_none());
    }

    #[test]
    fn direct_connections_are_not_saved() {
        let mut app = app_with_layout(Layout::Modern);
        let _ = app.open_connection(crate::session::Offline::shared("Direct"));
        assert_eq!(app.panes[0].tabs[1].origin, TabOrigin::Direct);
        let text = app.session().to_text();
        assert_eq!(Session::parse(&text).tabs.len(), 1);
    }

    // Operations

    #[tokio::test]
    async fn operations_change_the_local_file_system() {
        let directory = temporary_directory("operations");
        let path = directory.to_string_lossy().into_owned();
        let file_system = LocalFileSystem;

        Operation::CreateDirectory(" new ".to_string())
            .run(&file_system, &path)
            .await
            .expect("creating failed");
        assert!(directory.join("new").is_dir());
        Operation::Rename("new".to_string(), "renamed".to_string())
            .run(&file_system, &path)
            .await
            .expect("renaming failed");
        std::fs::write(directory.join("renamed/inner.txt"), b"x").expect("writing failed");
        Operation::Delete(vec!["renamed".to_string()])
            .run(&file_system, &path)
            .await
            .expect("deleting failed");
        assert!(!directory.join("renamed").exists());
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
