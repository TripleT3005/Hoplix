use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use hoplix_core::{
    EXPORT_FILE_NAME, Error, ExportError, ExportFile, ExportScope, SealedExport, SecretBackend,
    Site, SiteExport, decrypt_export, encrypt_export,
};
use iced::Task;

use crate::app::{Dialog, Hoplix, MasterMode, Message, SiteSelection};
use crate::i18n::Strings;
use crate::site_actions::PendingAction;
use crate::{local_time, ui};

// Constants

pub const PATH_INPUT: &str = "exchange-path";
pub const PASSWORD_INPUT: &str = "exchange-password";
pub const CONFIRM_INPUT: &str = "exchange-confirm";

// Types

#[derive(Clone, PartialEq, Eq)]
pub struct ExportDialog {
    pub scope: ExportScope,
    pub path: String,
    pub include_passwords: bool,
    pub password: String,
    pub confirm: String,
    pub error: Option<ExchangeError>,
    pub busy: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ImportDialog {
    pub path: String,
    pub folder: String,
    pub new_folder: String,
    pub password: String,
    pub sealed: Option<SealedExport>,
    pub error: Option<ExchangeError>,
    pub busy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeError {
    MissingFolder(String),
    NotFound(String),
    Io(String),
    Format(ExportError),
    Secrets(Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub sites: usize,
    pub secrets: usize,
    pub path: String,
}

#[derive(Debug, Clone)]
pub enum ExchangeMessage {
    OpenExport,
    OpenImport,
    Scope(ExportScope),
    Path(String),
    IncludePasswords(bool),
    Password(String),
    Confirm(String),
    Folder(String),
    Exported(Result<Exported, ExchangeError>),
    Decrypted(Result<SiteExport, ExchangeError>),
}

// Dialogs

impl ExportDialog {
    pub fn can_confirm(&self) -> bool {
        !self.busy
            && !clean_path(&self.path).is_empty()
            && (!self.include_passwords
                || (!self.password.is_empty() && self.password == self.confirm))
    }
}

impl ImportDialog {
    pub fn can_confirm(&self) -> bool {
        !self.busy
            && !clean_path(&self.path).is_empty()
            && (self.sealed.is_none() || !self.password.is_empty())
    }
}

impl fmt::Debug for ExportDialog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExportDialog")
            .field("scope", &self.scope)
            .field("path", &self.path)
            .field("include_passwords", &self.include_passwords)
            .field("error", &self.error)
            .field("busy", &self.busy)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ImportDialog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportDialog")
            .field("path", &self.path)
            .field("folder", &self.folder)
            .field("encrypted", &self.sealed.is_some())
            .field("error", &self.error)
            .field("busy", &self.busy)
            .finish_non_exhaustive()
    }
}

// Messages

impl Hoplix {
    pub(crate) fn update_exchange(&mut self, message: ExchangeMessage) -> Task<Message> {
        match message {
            ExchangeMessage::OpenExport => {
                let scope = match &self.site_selection {
                    Some(SiteSelection::Site(id)) => ExportScope::Site(id.clone()),
                    Some(SiteSelection::Folder(folder)) => ExportScope::Folder(folder.clone()),
                    None => ExportScope::All,
                };
                self.dialog = Some(Dialog::ExportSites(Box::new(ExportDialog {
                    scope,
                    path: default_path(),
                    include_passwords: false,
                    password: String::new(),
                    confirm: String::new(),
                    error: None,
                    busy: false,
                })));
                return ui::focus_id(PATH_INPUT);
            }
            ExchangeMessage::OpenImport => {
                let new_folder = self
                    .strings
                    .format("sites.import.folder_name", &[("date", &today())]);
                self.dialog = Some(Dialog::ImportSites(Box::new(ImportDialog {
                    path: default_path(),
                    folder: new_folder.clone(),
                    new_folder,
                    password: String::new(),
                    sealed: None,
                    error: None,
                    busy: false,
                })));
                return ui::focus_id(PATH_INPUT);
            }
            ExchangeMessage::Exported(result) => self.finish_export(result),
            ExchangeMessage::Decrypted(result) => return self.finish_decrypt(result),
            field => self.edit_exchange(field),
        }
        Task::none()
    }

    fn edit_exchange(&mut self, field: ExchangeMessage) {
        match &mut self.dialog {
            Some(Dialog::ExportSites(dialog)) => {
                match field {
                    ExchangeMessage::Scope(scope) => dialog.scope = scope,
                    ExchangeMessage::Path(path) => dialog.path = path,
                    ExchangeMessage::IncludePasswords(include) => {
                        dialog.include_passwords = include;
                        if !include {
                            dialog.password.clear();
                            dialog.confirm.clear();
                        }
                    }
                    ExchangeMessage::Password(password) => dialog.password = password,
                    ExchangeMessage::Confirm(confirm) => dialog.confirm = confirm,
                    _ => return,
                }
                dialog.error = None;
            }
            Some(Dialog::ImportSites(dialog)) => {
                match field {
                    ExchangeMessage::Path(path) => {
                        dialog.path = path;
                        dialog.sealed = None;
                        dialog.password.clear();
                    }
                    ExchangeMessage::Password(password) => dialog.password = password,
                    ExchangeMessage::Folder(folder) => dialog.folder = folder,
                    _ => return,
                }
                dialog.error = None;
            }
            _ => {}
        }
    }

    pub(crate) fn confirm_exchange_dialog(&mut self) -> Task<Message> {
        match &self.dialog {
            Some(Dialog::ExportSites(_)) => self.start_export(),
            Some(Dialog::ImportSites(_)) => self.start_import(),
            _ => Task::none(),
        }
    }

    // Export

    pub(crate) fn start_export(&mut self) -> Task<Message> {
        let vault_missing = !self.vault_file_exists();
        let Some(Dialog::ExportSites(dialog)) = &mut self.dialog else {
            return Task::none();
        };
        let path = PathBuf::from(clean_path(&dialog.path));
        if let Some(error) = missing_folder(&path) {
            dialog.error = Some(error);
            return Task::none();
        }
        let list = SiteExport::select(&self.sites, &dialog.scope);
        if !dialog.include_passwords {
            let export = SiteExport {
                list,
                secrets: BTreeMap::new(),
            };
            let result = write_file(&path, &export.to_plain_text()).map(|()| Exported {
                sites: export.site_count(),
                secrets: 0,
                path: path.to_string_lossy().into_owned(),
            });
            self.finish_export(result);
            return Task::none();
        }
        let password = dialog.password.clone();
        let saved: Vec<String> = list
            .sites
            .iter()
            .flat_map(Site::saved_secret_names)
            .collect();
        let done = |result| Message::SiteExchange(ExchangeMessage::Exported(result));
        match (self.secret_backend, &self.vault) {
            (SecretBackend::Keyring, _) => {
                dialog.busy = true;
                dialog.error = None;
                let keyring = self.services.keyring.clone();
                Task::perform(
                    async move {
                        let mut secrets = BTreeMap::new();
                        for name in saved {
                            let secret =
                                keyring.get(&name).await.map_err(ExchangeError::Secrets)?;
                            if let Some(secret) = secret.filter(|secret| !secret.is_empty()) {
                                secrets.insert(name, secret);
                            }
                        }
                        write_encrypted(SiteExport { list, secrets }, password, path).await
                    },
                    done,
                )
            }
            (SecretBackend::Vault, Some(vault)) => {
                let mut secrets = BTreeMap::new();
                for name in saved {
                    match vault.get(&name) {
                        Ok(Some(secret)) if !secret.is_empty() => {
                            secrets.insert(name, secret);
                        }
                        Ok(_) => {}
                        Err(error) => {
                            dialog.error = Some(ExchangeError::Secrets(error));
                            return Task::none();
                        }
                    }
                }
                dialog.busy = true;
                dialog.error = None;
                Task::perform(
                    write_encrypted(SiteExport { list, secrets }, password, path),
                    done,
                )
            }
            (SecretBackend::Vault, None) if vault_missing => {
                dialog.busy = true;
                dialog.error = None;
                Task::perform(
                    write_encrypted(
                        SiteExport {
                            list,
                            secrets: BTreeMap::new(),
                        },
                        password,
                        path,
                    ),
                    done,
                )
            }
            (SecretBackend::Vault, None) => {
                let Some(Dialog::ExportSites(dialog)) = self.dialog.take() else {
                    return Task::none();
                };
                self.pending = Some(PendingAction::Export(dialog));
                self.open_master_dialog(MasterMode::Unlock);
                ui::focus_input()
            }
        }
    }

    fn finish_export(&mut self, result: Result<Exported, ExchangeError>) {
        match result {
            Ok(exported) => {
                if matches!(self.dialog, Some(Dialog::ExportSites(_))) {
                    self.dialog = None;
                }
                self.notice = Some(self.strings.format(
                    "sites.export.done",
                    &[
                        ("count", &exported.sites.to_string()),
                        ("passwords", &exported.secrets.to_string()),
                        ("path", &exported.path),
                    ],
                ));
            }
            Err(error) => {
                if let Some(Dialog::ExportSites(dialog)) = &mut self.dialog {
                    dialog.error = Some(error);
                    dialog.busy = false;
                }
            }
        }
    }

    // Import

    fn start_import(&mut self) -> Task<Message> {
        let Some(Dialog::ImportSites(dialog)) = &mut self.dialog else {
            return Task::none();
        };
        if let Some(sealed) = dialog.sealed.clone() {
            dialog.busy = true;
            dialog.error = None;
            let password = dialog.password.clone();
            return Task::perform(
                async move {
                    decrypt_export(sealed, password)
                        .await
                        .map_err(ExchangeError::Format)
                },
                |result| Message::SiteExchange(ExchangeMessage::Decrypted(result)),
            );
        }
        match read_file(Path::new(&clean_path(&dialog.path))) {
            Ok(ExportFile::Plain(export)) => self.finish_import(export),
            Ok(ExportFile::Encrypted(sealed)) => {
                dialog.sealed = Some(sealed);
                dialog.error = None;
                ui::focus_id(PASSWORD_INPUT)
            }
            Err(error) => {
                dialog.error = Some(error);
                Task::none()
            }
        }
    }

    fn finish_decrypt(&mut self, result: Result<SiteExport, ExchangeError>) -> Task<Message> {
        match result {
            Ok(export) => self.finish_import(export),
            Err(error) => {
                if let Some(Dialog::ImportSites(dialog)) = &mut self.dialog {
                    dialog.error = Some(error);
                    dialog.busy = false;
                    dialog.password.clear();
                }
                ui::focus_id(PASSWORD_INPUT)
            }
        }
    }

    fn finish_import(&mut self, export: SiteExport) -> Task<Message> {
        let Some(Dialog::ImportSites(dialog)) = self.dialog.take() else {
            return Task::none();
        };
        let summary = export.import_into(&mut self.sites, &dialog.folder);
        self.save_sites();
        self.site_form = None;
        self.site_selection =
            (!summary.folder.is_empty()).then(|| SiteSelection::Folder(summary.folder.clone()));
        let place = if summary.folder.is_empty() {
            self.strings.get("sites.folder.root").to_string()
        } else {
            summary.folder.clone()
        };
        self.notice = Some(self.strings.format(
            "sites.import.done",
            &[
                ("count", &summary.sites.to_string()),
                ("passwords", &summary.secrets.to_string()),
                ("folder", &place),
            ],
        ));
        self.store_secrets(summary.stored)
    }

    pub(crate) fn store_secrets(&mut self, secrets: Vec<(String, String)>) -> Task<Message> {
        if secrets.is_empty() {
            return Task::none();
        }
        if self.secret_backend == SecretBackend::Vault && self.vault.is_none() {
            return self.request_vault(PendingAction::StoreSecrets(secrets));
        }
        let mut tasks = Vec::new();
        for (name, secret) in secrets {
            tasks.push(self.store_secret(name, Some(secret)));
        }
        Task::batch(tasks)
    }
}

// Texts

pub fn error_text(strings: &Strings, error: &ExchangeError) -> String {
    match error {
        ExchangeError::MissingFolder(folder) => {
            strings.format("sites.exchange.missing_folder", &[("path", folder)])
        }
        ExchangeError::NotFound(path) => {
            strings.format("sites.exchange.not_found", &[("path", path)])
        }
        ExchangeError::Io(detail) => strings.format("sites.exchange.io", &[("detail", detail)]),
        ExchangeError::Format(error) => strings
            .get(match error {
                ExportError::NotASiteList => "sites.exchange.not_a_list",
                ExportError::NewerVersion => "sites.exchange.newer",
                ExportError::WrongPassword => "sites.exchange.wrong_password",
                ExportError::Damaged => "sites.exchange.damaged",
                ExportError::EmptyPassword | ExportError::Encryption => "sites.exchange.encryption",
            })
            .to_string(),
        ExchangeError::Secrets(error) => strings.format(
            "secrets.failed",
            &[("detail", &ui::error_text(strings, error))],
        ),
    }
}

// Files

pub fn clean_path(path: &str) -> String {
    path.trim().trim_matches('"').to_string()
}

pub fn default_path() -> String {
    documents_folder()
        .join(EXPORT_FILE_NAME)
        .to_string_lossy()
        .into_owned()
}

fn missing_folder(path: &Path) -> Option<ExchangeError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())?;
    (!parent.is_dir()).then(|| ExchangeError::MissingFolder(parent.to_string_lossy().into_owned()))
}

fn write_file(path: &Path, text: &str) -> Result<(), ExchangeError> {
    fs::write(path, text).map_err(|error| ExchangeError::Io(error.to_string()))
}

fn read_file(path: &Path) -> Result<ExportFile, ExchangeError> {
    let text = fs::read_to_string(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => ExchangeError::NotFound(path.to_string_lossy().into_owned()),
        io::ErrorKind::InvalidData => ExchangeError::Format(ExportError::NotASiteList),
        _ => ExchangeError::Io(error.to_string()),
    })?;
    ExportFile::parse(&text).map_err(ExchangeError::Format)
}

async fn write_encrypted(
    export: SiteExport,
    password: String,
    path: PathBuf,
) -> Result<Exported, ExchangeError> {
    let sites = export.site_count();
    let secrets = export.secrets.len();
    let text = encrypt_export(export, password)
        .await
        .map_err(ExchangeError::Format)?;
    write_file(&path, &text)?;
    Ok(Exported {
        sites,
        secrets,
        path: path.to_string_lossy().into_owned(),
    })
}

fn today() -> String {
    local_time::local(SystemTime::now())
        .map(|time| local_time::format(&time, "{year}-{month}-{day}"))
        .unwrap_or_else(|| hoplix_core::utc_date(SystemTime::now()))
}

pub(crate) fn documents_folder() -> PathBuf {
    known_documents_folder()
        .or_else(|| {
            std::env::home_dir().map(|home| {
                let documents = home.join("Documents");
                if documents.is_dir() { documents } else { home }
            })
        })
        .unwrap_or_default()
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn known_documents_folder() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Documents, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
    };

    let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
    let result = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_Documents,
            KF_FLAG_DEFAULT as u32,
            std::ptr::null_mut(),
            &mut raw,
        )
    };
    if raw.is_null() {
        return None;
    }
    let path = (result >= 0).then(|| {
        let length = (0..)
            .take_while(|&index| unsafe { *raw.add(index) } != 0)
            .count();
        let wide = unsafe { std::slice::from_raw_parts(raw, length) };
        PathBuf::from(OsString::from_wide(wide))
    });
    unsafe { CoTaskMemFree(raw.cast()) };
    path.filter(|path| path.is_dir())
}

#[cfg(not(windows))]
fn known_documents_folder() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_end_with_the_export_file_name() {
        assert!(default_path().ends_with(EXPORT_FILE_NAME));
        assert_eq!(
            clean_path("  \"C:\\Data\\list.hoplix-sites\" "),
            "C:\\Data\\list.hoplix-sites"
        );
        assert_eq!(today().len(), 10);
    }

    #[test]
    fn dialogs_validate_their_input() {
        let mut export = ExportDialog {
            scope: ExportScope::All,
            path: " ".to_string(),
            include_passwords: false,
            password: String::new(),
            confirm: String::new(),
            error: None,
            busy: false,
        };
        assert!(!export.can_confirm());
        export.path = "list.hoplix-sites".to_string();
        assert!(export.can_confirm());
        export.include_passwords = true;
        assert!(!export.can_confirm());
        let password = crate::ui_tests::test_password();
        export.password.clone_from(&password);
        assert!(!export.can_confirm());
        export.confirm = password;
        assert!(export.can_confirm());
        assert!(!format!("{export:?}").contains(&export.password));
        export.busy = true;
        assert!(!export.can_confirm());
    }

    #[test]
    fn missing_folders_are_reported() {
        let missing = std::env::temp_dir()
            .join("hoplix-no-such-folder")
            .join("list");
        assert!(matches!(
            missing_folder(&missing),
            Some(ExchangeError::MissingFolder(_))
        ));
        assert_eq!(missing_folder(Path::new("list.hoplix-sites")), None);
        assert!(matches!(
            read_file(&missing),
            Err(ExchangeError::NotFound(_))
        ));
    }
}
