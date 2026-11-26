use std::sync::Arc;

use hoplix_core::{
    Error, ErrorKind, FileSystem, SecretBackend, SecretSlot, Site, change_master_password,
    create_vault, jump_secret_name, move_to_keyring, move_to_vault, proxy_secret_name, secret_name,
    site_id_of, unlock_vault,
};
use iced::Task;

use crate::app::{Dialog, Hoplix, MasterMode, Message, Page, SiteSelection};
use crate::session::TabOrigin;
use crate::site_exchange::ExportDialog;
use crate::site_form::SiteForm;
use crate::ui;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingAction {
    Connect { site: String, names: Vec<String> },
    StoreSecret { name: String, secret: String },
    StoreSecrets(Vec<(String, String)>),
    Export(Box<ExportDialog>),
    SwitchToKeyring,
    ChangePassword,
}

struct SecretUse {
    save: bool,
    secret: String,
    from_store: bool,
}

// Messages

impl Hoplix {
    pub(crate) fn update_sites(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SiteSelected(selection) => {
                self.site_form = match &selection {
                    SiteSelection::Site(id) => self.sites.get(id).map(SiteForm::edit),
                    SiteSelection::Folder(_) => None,
                };
                self.site_selection = Some(selection);
            }
            Message::SiteNew => {
                let mut site = Site::new(self.strings.get("sites.new_name"));
                site.folder = self.selected_folder();
                self.site_form = Some(SiteForm::create(&site));
                self.site_selection = None;
            }
            Message::SiteFolderNew => {
                self.dialog = Some(Dialog::NewSiteFolder {
                    parent: self.selected_folder(),
                    name: String::new(),
                });
                return ui::focus_input();
            }
            Message::SiteDuplicate => {
                if let Some(SiteSelection::Site(id)) = &self.site_selection
                    && let Some(original) = self.sites.get(id)
                {
                    let name = self
                        .strings
                        .format("sites.copy_name", &[("name", &original.name)]);
                    if let Some(copy) = self.sites.duplicate(&id.clone(), &name) {
                        self.save_sites();
                        self.site_form = Some(SiteForm::edit(&copy));
                        self.site_selection = Some(SiteSelection::Site(copy.id));
                    }
                }
            }
            Message::SiteDelete => {
                self.dialog = match &self.site_selection {
                    Some(SiteSelection::Site(id)) => {
                        self.sites.get(id).map(|site| Dialog::DeleteSite {
                            id: site.id.clone(),
                            name: site.name.clone(),
                        })
                    }
                    Some(SiteSelection::Folder(folder)) => Some(Dialog::DeleteSiteFolder {
                        folder: folder.clone(),
                        sites: self
                            .sites
                            .sites
                            .iter()
                            .filter(|site| {
                                site.folder == *folder
                                    || site.folder.starts_with(&format!("{folder}/"))
                            })
                            .count(),
                    }),
                    None => None,
                };
            }
            Message::SiteField(field) => {
                if let Some(form) = &mut self.site_form {
                    form.apply(field);
                }
            }
            Message::SiteSave { connect } => return self.save_site(connect),
            Message::SiteEditCancelled => {
                self.site_form = match &self.site_selection {
                    Some(SiteSelection::Site(id)) => self.sites.get(id).map(SiteForm::edit),
                    _ => None,
                };
            }
            Message::SiteConnect(id) => return self.connect_site(&id),
            Message::SecretsLoaded(id, result) => return self.finish_secret_load(&id, result),
            Message::SecretSaved(Err(error)) => self.report_secret_error(&error),
            Message::ConnectSaveAsSite => {
                let host = self.connect_form.host.trim();
                let name = if host.is_empty() {
                    self.strings.get("sites.new_name").to_string()
                } else {
                    host.to_string()
                };
                let site = Site::new(&name);
                self.site_form = Some(SiteForm::from_connection(&site, &self.connect_form));
                self.site_selection = None;
                self.page = Page::Sites;
            }
            Message::SecretBackendSelected(backend) => return self.select_backend(backend),
            Message::ChangeMasterPassword => {
                if self.vault.is_some() {
                    self.open_master_dialog(MasterMode::Change);
                } else {
                    self.pending = Some(PendingAction::ChangePassword);
                    self.open_master_dialog(MasterMode::Unlock);
                }
                return ui::focus_input();
            }
            Message::VaultUnlocked(result) => return self.finish_unlock(result),
            Message::VaultCreated(result) => match result {
                Ok(moved) => {
                    self.vault = Some(moved.target);
                    self.secret_backend = SecretBackend::Vault;
                    self.dialog = None;
                    self.notice = Some(if moved.leftovers.is_empty() {
                        self.strings.get("secrets.switched.vault").to_string()
                    } else {
                        self.strings.format(
                            "secrets.leftovers",
                            &[("count", &moved.leftovers.len().to_string())],
                        )
                    });
                }
                Err(error) => self.fail_master_dialog(error),
            },
            Message::VaultChanged(result) => match result {
                Ok(vault) => {
                    self.vault = Some(vault);
                    self.dialog = None;
                    self.notice = Some(self.strings.get("secrets.changed").to_string());
                }
                Err(error) => self.fail_master_dialog(error),
            },
            Message::KeyringRestored(result) => match result {
                Ok(_) => {
                    self.vault = None;
                    self.secret_backend = SecretBackend::Keyring;
                    self.notice = Some(self.strings.get("secrets.switched.keyring").to_string());
                }
                Err(error) => self.report_secret_error(&error),
            },
            _ => {}
        }
        Task::none()
    }

    // Dialogs

    pub(crate) fn confirm_site_dialog(&mut self, dialog: Dialog) -> Task<Message> {
        match dialog {
            Dialog::NewSiteFolder { parent, name } => {
                self.dialog = None;
                let path = if parent.is_empty() {
                    name
                } else {
                    format!("{parent}/{name}")
                };
                if let Some(folder) = self.sites.add_folder(&path) {
                    self.save_sites();
                    self.site_form = None;
                    self.site_selection = Some(SiteSelection::Folder(folder));
                }
                Task::none()
            }
            Dialog::DeleteSite { id, .. } => {
                self.dialog = None;
                let Some(removed) = self.sites.remove(&id) else {
                    return Task::none();
                };
                self.forget_selection(std::slice::from_ref(&removed.id));
                self.save_sites();
                self.forget_secrets(&[removed])
            }
            Dialog::DeleteSiteFolder { folder, .. } => {
                self.dialog = None;
                let removed = self.sites.remove_folder(&folder);
                let ids: Vec<String> = removed.iter().map(|site| site.id.clone()).collect();
                self.forget_selection(&ids);
                if self.site_selection == Some(SiteSelection::Folder(folder)) {
                    self.site_selection = None;
                }
                self.save_sites();
                self.forget_secrets(&removed)
            }
            Dialog::MasterPassword { mode, password, .. } => self.run_master_dialog(mode, password),
            _ => Task::none(),
        }
    }

    pub(crate) fn open_master_dialog(&mut self, mode: MasterMode) {
        self.dialog = Some(Dialog::MasterPassword {
            mode,
            password: String::new(),
            confirm: String::new(),
            error: None,
            busy: false,
        });
    }

    fn fail_master_dialog(&mut self, failure: Error) {
        if let Some(Dialog::MasterPassword {
            error,
            busy,
            password,
            confirm,
            ..
        }) = &mut self.dialog
        {
            *error = Some(failure);
            *busy = false;
            password.clear();
            confirm.clear();
        } else {
            self.report_secret_error(&failure);
        }
    }

    fn run_master_dialog(&mut self, mode: MasterMode, password: String) -> Task<Message> {
        let Some(directory) = self.config_dir.clone() else {
            self.fail_master_dialog(Error::new(ErrorKind::Other, "no configuration folder"));
            return Task::none();
        };
        if let Some(Dialog::MasterPassword { busy, .. }) = &mut self.dialog {
            *busy = true;
        }
        match mode {
            MasterMode::Unlock => {
                Task::perform(unlock_vault(directory, password), Message::VaultUnlocked)
            }
            MasterMode::Create => {
                let names = self.saved_secret_names();
                let keyring = self.services.keyring.clone();
                Task::perform(
                    async move {
                        let vault = create_vault(directory, password).await?;
                        move_to_vault(&keyring, vault, &names).await
                    },
                    Message::VaultCreated,
                )
            }
            MasterMode::Change => match self.vault.clone() {
                Some(vault) => Task::perform(
                    change_master_password(vault, password),
                    Message::VaultChanged,
                ),
                None => {
                    self.fail_master_dialog(Error::new(ErrorKind::VaultLocked, ""));
                    Task::none()
                }
            },
        }
    }

    fn finish_unlock(&mut self, result: Result<hoplix_core::Vault, Error>) -> Task<Message> {
        let mut vault = match result {
            Ok(vault) => vault,
            Err(error) => {
                self.fail_master_dialog(error);
                return Task::none();
            }
        };
        let sites = &self.sites;
        if let Err(error) = vault.retain(|name| {
            site_id_of(name)
                .and_then(|id| sites.get(id))
                .is_some_and(|site| site.saved_secret_names().iter().any(|saved| saved == name))
        }) {
            self.report_secret_error(&error);
        }
        self.vault = Some(vault);
        self.dialog = None;
        match self.pending.take() {
            Some(PendingAction::Connect { site, names }) => self.load_secrets(&site, names),
            Some(PendingAction::StoreSecret { name, secret }) => {
                self.store_secret(name, Some(secret))
            }
            Some(PendingAction::StoreSecrets(secrets)) => self.store_secrets(secrets),
            Some(PendingAction::Export(mut dialog)) => {
                dialog.busy = false;
                self.dialog = Some(Dialog::ExportSites(dialog));
                self.start_export()
            }
            Some(PendingAction::SwitchToKeyring) => self.restore_keyring(),
            Some(PendingAction::ChangePassword) => {
                self.open_master_dialog(MasterMode::Change);
                ui::focus_input()
            }
            None => Task::none(),
        }
    }

    // Backends

    fn select_backend(&mut self, backend: SecretBackend) -> Task<Message> {
        if backend == self.secret_backend {
            return Task::none();
        }
        match backend {
            SecretBackend::Vault => self.open_master_dialog(MasterMode::Create),
            SecretBackend::Keyring => {
                if self.vault.is_some() {
                    return self.restore_keyring();
                }
                self.pending = Some(PendingAction::SwitchToKeyring);
                self.open_master_dialog(MasterMode::Unlock);
            }
        }
        ui::focus_input()
    }

    fn restore_keyring(&mut self) -> Task<Message> {
        let Some(vault) = self.vault.clone() else {
            return Task::none();
        };
        let keyring = self.services.keyring.clone();
        Task::perform(
            async move { move_to_keyring(vault, &keyring).await },
            Message::KeyringRestored,
        )
    }

    fn saved_secret_names(&self) -> Vec<String> {
        self.sites
            .sites
            .iter()
            .flat_map(Site::saved_secret_names)
            .collect()
    }

    // Secrets

    pub(crate) fn store_secret(&mut self, name: String, secret: Option<String>) -> Task<Message> {
        let keyring = self.services.keyring.clone();
        match self.secret_backend {
            SecretBackend::Keyring => Task::perform(
                async move {
                    match secret {
                        Some(secret) => keyring.set(&name, &secret).await,
                        None => keyring.delete(&name).await,
                    }
                },
                Message::SecretSaved,
            ),
            SecretBackend::Vault => match (&mut self.vault, secret) {
                (Some(vault), secret) => {
                    let result = match secret {
                        Some(secret) => vault.set(&name, &secret),
                        None => vault.delete(&name).map(|_| ()),
                    };
                    if let Err(error) = result {
                        self.report_secret_error(&error);
                    }
                    Task::none()
                }
                (None, None) => Task::none(),
                (None, Some(secret)) => {
                    self.pending = Some(PendingAction::StoreSecret { name, secret });
                    self.open_master_dialog(MasterMode::Unlock);
                    ui::focus_input()
                }
            },
        }
    }

    fn load_secrets(&mut self, site: &str, names: Vec<String>) -> Task<Message> {
        let id = site.to_string();
        let keyring = self.services.keyring.clone();
        match self.secret_backend {
            SecretBackend::Keyring => Task::perform(
                async move {
                    let mut found = Vec::new();
                    for name in names {
                        if let Some(secret) = keyring.get(&name).await? {
                            found.push((name, secret));
                        }
                    }
                    Ok(found)
                },
                move |result| Message::SecretsLoaded(id.clone(), result),
            ),
            SecretBackend::Vault => match &self.vault {
                Some(vault) => {
                    let result = names
                        .into_iter()
                        .filter_map(|name| match vault.get(&name) {
                            Ok(Some(secret)) => Some(Ok((name, secret))),
                            Ok(None) => None,
                            Err(error) => Some(Err(error)),
                        })
                        .collect();
                    self.finish_secret_load(&id, result)
                }
                None => {
                    self.pending = Some(PendingAction::Connect { site: id, names });
                    self.open_master_dialog(MasterMode::Unlock);
                    ui::focus_input()
                }
            },
        }
    }

    fn finish_secret_load(
        &mut self,
        id: &str,
        result: Result<Vec<(String, String)>, Error>,
    ) -> Task<Message> {
        if self.connect_form.site.as_deref() != Some(id) {
            return Task::none();
        }
        match result {
            Ok(secrets) => {
                let form = &mut self.connect_form;
                for (name, secret) in secrets {
                    match SecretSlot::parse(&name) {
                        Some((_, SecretSlot::Site)) => {
                            form.set_secret(secret);
                            form.secret_from_store = true;
                        }
                        Some((_, SecretSlot::Jump(0))) => form.jump.set_secret(secret),
                        Some((_, SecretSlot::Proxy)) => form.proxy.set_secret(secret),
                        _ => {}
                    }
                }
                self.connect_when_ready()
            }
            Err(error) => {
                self.report_secret_error(&error);
                self.page = Page::Connect;
                Task::none()
            }
        }
    }

    fn connect_when_ready(&mut self) -> Task<Message> {
        if self.connect_form.ready_to_connect() {
            self.connect(None)
        } else {
            self.page = Page::Connect;
            Task::none()
        }
    }

    fn forget_secrets(&mut self, sites: &[Site]) -> Task<Message> {
        let names: Vec<String> = sites.iter().flat_map(Site::saved_secret_names).collect();
        let tasks: Vec<Task<Message>> = names
            .into_iter()
            .map(|name| self.store_secret(name, None))
            .collect();
        Task::batch(tasks)
    }

    fn report_secret_error(&mut self, error: &Error) {
        self.notice = Some(self.strings.format(
            "secrets.failed",
            &[("detail", &ui::error_text(&self.strings, error))],
        ));
    }

    // Sites

    fn selected_folder(&self) -> String {
        match &self.site_selection {
            Some(SiteSelection::Folder(folder)) => folder.clone(),
            Some(SiteSelection::Site(id)) => self
                .sites
                .get(id)
                .map(|site| site.folder.clone())
                .unwrap_or_default(),
            None => String::new(),
        }
    }

    fn forget_selection(&mut self, ids: &[String]) {
        if let Some(SiteSelection::Site(id)) = &self.site_selection
            && ids.contains(id)
        {
            self.site_selection = None;
        }
        if self
            .site_form
            .as_ref()
            .is_some_and(|form| ids.contains(&form.id))
        {
            self.site_form = None;
        }
    }

    fn save_site(&mut self, connect: bool) -> Task<Message> {
        let Some(form) = &mut self.site_form else {
            return Task::none();
        };
        let (mut site, secrets) = match form.to_site() {
            Ok(result) => result,
            Err(invalid) => {
                form.reveal_invalid(invalid);
                return Task::none();
            }
        };
        if let Some(existing) = self.sites.get(&site.id) {
            site.bookmarks = existing.bookmarks.clone();
        }
        let old_names = self
            .sites
            .get(&site.id)
            .map(Site::saved_secret_names)
            .unwrap_or_default();
        self.sites.upsert(site.clone());
        self.save_sites();
        self.site_form = Some(SiteForm::edit(&site));
        self.site_selection = Some(SiteSelection::Site(site.id.clone()));

        let new_names = site.saved_secret_names();
        let typed = [
            (secret_name(&site.id), secrets.main.clone()),
            (jump_secret_name(&site.id, 0), secrets.jump.clone()),
            (proxy_secret_name(&site.id), secrets.proxy.clone()),
        ];
        let mut tasks = Vec::new();
        for name in old_names.iter().filter(|name| !new_names.contains(name)) {
            tasks.push(self.store_secret(name.clone(), None));
        }
        for (name, value) in &typed {
            if let Some(value) = value
                && new_names.contains(name)
            {
                tasks.push(self.store_secret(name.clone(), Some(value.clone())));
            }
        }
        if connect {
            self.connect_form.fill_from_site(&site);
            if let Some(value) = secrets.main {
                self.connect_form.set_secret(value);
            }
            if let Some(value) = secrets.jump {
                self.connect_form.jump.set_secret(value);
            }
            if let Some(value) = secrets.proxy {
                self.connect_form.proxy.set_secret(value);
            }
            let missing: Vec<String> = new_names
                .into_iter()
                .filter(|name| {
                    typed
                        .iter()
                        .any(|(typed_name, value)| typed_name == name && value.is_none())
                })
                .collect();
            if missing.is_empty() {
                tasks.push(self.connect_when_ready());
            } else {
                tasks.push(self.load_secrets(&site.id, missing));
            }
        }
        Task::batch(tasks)
    }

    pub(crate) fn connect_site(&mut self, id: &str) -> Task<Message> {
        let Some(site) = self.sites.get(id).cloned() else {
            return Task::none();
        };
        self.connect_form.fill_from_site(&site);
        let names = site.saved_secret_names();
        if names.is_empty() {
            self.connect_when_ready()
        } else {
            self.load_secrets(&site.id, names)
        }
    }

    pub(crate) fn open_connection(&mut self, file_system: Arc<dyn FileSystem>) -> Task<Message> {
        self.remember_connection();
        let site = self
            .connect_form
            .site
            .as_ref()
            .and_then(|id| self.sites.get(id))
            .cloned();
        let form = &self.connect_form;
        let main = SecretUse {
            save: form.save_password && form.uses_secret(),
            secret: form.secret().to_string(),
            from_store: form.secret_from_store,
        };
        let jump = form.uses_jump().then(|| SecretUse {
            save: form.jump.save_password && form.jump.uses_secret(),
            secret: form.jump.secret().to_string(),
            from_store: form.jump.secret_from_store,
        });
        let proxy = (form.proxy.kind.is_some() && form.proxy.uses_secret()).then(|| SecretUse {
            save: form.proxy.save_password,
            secret: form.proxy.password.clone(),
            from_store: form.proxy.secret_from_store,
        });
        self.connect_form.clear_secrets();
        self.page = Page::Browser;
        let pane = self.focused_pane.min(self.panes.len().saturating_sub(1));
        let mut tasks = Vec::new();
        let (title, start) = match site {
            Some(mut site) => {
                let before = site.clone();
                let mut saved = site.save_secrets;
                tasks.extend(self.sync_secret(secret_name(&site.id), &mut saved, &main));
                site.save_secrets = saved;
                if let (Some(usage), Some(settings)) = (&jump, &mut site.jump) {
                    let mut saved = settings.save_secret;
                    tasks.extend(self.sync_secret(
                        jump_secret_name(&site.id, 0),
                        &mut saved,
                        usage,
                    ));
                    settings.save_secret = saved;
                }
                if let (Some(usage), Some(settings)) = (&proxy, &mut site.proxy) {
                    let mut saved = settings.save_secret;
                    tasks.extend(self.sync_secret(proxy_secret_name(&site.id), &mut saved, usage));
                    settings.save_secret = saved;
                }
                if site != before {
                    self.sites.upsert(site.clone());
                    self.save_sites();
                }
                if !site.local_path.is_empty() {
                    tasks.push(self.show_local_path(pane, site.local_path.clone()));
                }
                (
                    Some(site.name.clone()),
                    Some(site.remote_path.clone()).filter(|path| !path.is_empty()),
                )
            }
            None => (None, None),
        };
        if let Some(task) = self.take_reconnect(&file_system) {
            tasks.push(task);
            return Task::batch(tasks);
        }
        let origin = match &self.connect_form.site {
            Some(id) => TabOrigin::Site(id.clone()),
            None => TabOrigin::Direct,
        };
        tasks.push(self.open_tab(pane, file_system, title, start));
        self.set_origin(self.last_tab_id(), origin);
        Task::batch(tasks)
    }

    fn sync_secret(
        &mut self,
        name: String,
        saved: &mut bool,
        usage: &SecretUse,
    ) -> Option<Task<Message>> {
        if usage.save && !usage.secret.is_empty() && !usage.from_store {
            *saved = true;
            Some(self.store_secret(name, Some(usage.secret.clone())))
        } else if !usage.save && *saved {
            *saved = false;
            Some(self.store_secret(name, None))
        } else {
            None
        }
    }

    fn show_local_path(&mut self, remote_pane: usize, path: String) -> Task<Message> {
        if self.panes.len() < 2 {
            return self.open_local_tab(remote_pane, Some(path));
        }
        let other = usize::from(remote_pane == 0);
        let local = self.panes[other]
            .active_tab()
            .filter(|tab| tab.title.is_none() && tab.file_system.label().is_none())
            .map(|tab| tab.id);
        let task = match local {
            Some(id) => self.navigate(id, path),
            None => self.open_local_tab(other, Some(path)),
        };
        self.focused_pane = remote_pane;
        task
    }

    pub(crate) fn save_sites(&mut self) {
        let Some(directory) = &self.config_dir else {
            return;
        };
        if let Err(error) = self.sites.save(directory) {
            self.notice = Some(
                self.strings
                    .format("sites.save_failed", &[("detail", &error.to_string())]),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use crate::ui_tests::test_password;
    use hoplix_core::{SiteList, Vault};
    use std::path::PathBuf;

    fn temporary_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hoplix-sites-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("creating the test directory failed");
        directory
    }

    fn app(directory: &std::path::Path) -> Hoplix {
        let mut app = Hoplix::with_settings(Settings::default(), Some(directory.to_path_buf()));
        app.panes.push(crate::app::Pane::default());
        app
    }

    fn site(name: &str, folder: &str) -> Site {
        Site {
            host: "files.example.test".to_string(),
            username: "demo".to_string(),
            folder: folder.to_string(),
            ..Site::new(name)
        }
    }

    // Site list

    #[test]
    fn sites_are_created_saved_and_deleted() {
        let directory = temporary_directory("crud");
        let mut app = app(&directory);
        app.sites.add_folder("Work");
        app.site_selection = Some(SiteSelection::Folder("Work".to_string()));

        let _ = app.update(Message::SiteNew);
        let form = app.site_form.as_ref().expect("form opened");
        assert!(form.is_new);
        assert_eq!(form.folder, "Work");
        let _ = app.update(Message::SiteSave { connect: false });
        assert!(app.site_form.as_ref().expect("form kept").invalid.is_some());

        let _ = app.update(Message::SiteField(crate::site_form::SiteField::Connection(
            crate::connect_form::Field::Host("files.example.test".to_string()),
        )));
        let _ = app.update(Message::SiteField(crate::site_form::SiteField::Connection(
            crate::connect_form::Field::Username("demo".to_string()),
        )));
        let _ = app.update(Message::SiteSave { connect: false });
        assert_eq!(app.sites.sites.len(), 1);
        let id = app.sites.sites[0].id.clone();
        assert_eq!(app.site_selection, Some(SiteSelection::Site(id.clone())));
        let stored = SiteList::load(&directory).expect("sites saved");
        assert_eq!(stored.sites[0].folder, "Work");

        let _ = app.update(Message::SiteDuplicate);
        assert_eq!(app.sites.sites.len(), 2);

        let _ = app.update(Message::SiteSelected(SiteSelection::Folder(
            "Work".to_string(),
        )));
        let _ = app.update(Message::SiteDelete);
        assert_eq!(
            app.dialog,
            Some(Dialog::DeleteSiteFolder {
                folder: "Work".to_string(),
                sites: 2
            })
        );
        let _ = app.update(Message::DialogConfirmed);
        assert!(app.sites.sites.is_empty());
        assert!(app.site_form.is_none());
        assert!(SiteList::load(&directory).expect("saved").sites.is_empty());
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn folders_are_created_below_the_selection() {
        let directory = temporary_directory("folders");
        let mut app = app(&directory);
        app.sites.upsert(site("Web", "Work"));
        let id = app.sites.sites[0].id.clone();
        let _ = app.update(Message::SiteSelected(SiteSelection::Site(id)));
        let _ = app.update(Message::SiteFolderNew);
        assert!(!app.dialog.as_ref().is_some_and(Dialog::can_confirm));
        let _ = app.update(Message::DialogInput("Clients".to_string()));
        let _ = app.update(Message::DialogConfirmed);
        assert_eq!(
            app.site_selection,
            Some(SiteSelection::Folder("Work/Clients".to_string()))
        );
        assert!(
            app.sites
                .all_folders()
                .contains(&"Work/Clients".to_string())
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    // Connecting

    #[test]
    fn sites_without_saved_passwords_open_the_connect_page() {
        let directory = temporary_directory("connect");
        let mut app = app(&directory);
        app.sites.upsert(site("Web", ""));
        let id = app.sites.sites[0].id.clone();
        let _ = app.update(Message::SiteConnect(id.clone()));
        assert_eq!(app.page, Page::Connect);
        assert_eq!(app.connect_form.site.as_deref(), Some(id.as_str()));
        assert_eq!(app.connect_form.host, "files.example.test");
        assert!(!app.connect_form.connecting);
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn locked_vaults_ask_for_the_master_password_first() {
        let directory = temporary_directory("locked");
        let master = test_password();
        Vault::create_with_iterations(&directory, &master, 1_000).expect("created");
        let mut app = app(&directory);
        assert_eq!(app.secret_backend, SecretBackend::Vault);
        let mut saved = site("Web", "");
        saved.save_secrets = true;
        app.sites.upsert(saved.clone());

        let _ = app.update(Message::SiteConnect(saved.id.clone()));
        assert!(matches!(
            app.dialog,
            Some(Dialog::MasterPassword {
                mode: MasterMode::Unlock,
                ..
            })
        ));
        assert_eq!(
            app.pending,
            Some(PendingAction::Connect {
                site: saved.id.clone(),
                names: vec![secret_name(&saved.id)],
            })
        );

        let error = Error::new(ErrorKind::WrongMasterPassword, "");
        let _ = app.update(Message::VaultUnlocked(Err(error.clone())));
        assert!(matches!(
            &app.dialog,
            Some(Dialog::MasterPassword { error: Some(shown), busy: false, .. }) if *shown == error
        ));

        let secret = test_password();
        let mut vault = Vault::unlock(&directory, &master).expect("unlocked");
        vault.set(&secret_name(&saved.id), &secret).expect("stored");
        vault.set("site/orphan", &test_password()).expect("stored");
        let _ = app.update(Message::VaultUnlocked(Ok(vault)));
        assert_eq!(app.dialog, None);
        assert_eq!(app.pending, None);
        assert!(app.connect_form.connecting);
        assert_eq!(app.connect_form.password, secret);
        assert_eq!(
            app.vault.as_ref().expect("unlocked").names(),
            [secret_name(&saved.id)]
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn unlocked_vaults_store_and_forget_site_secrets() {
        let directory = temporary_directory("store");
        let master = test_password();
        let vault = Vault::create_with_iterations(&directory, &master, 1_000).expect("created");
        let mut app = app(&directory);
        app.vault = Some(vault);
        let mut saved = site("Web", "");
        saved.save_secrets = true;
        app.sites.upsert(saved.clone());

        let secret = test_password();
        let _ = app.store_secret(secret_name(&saved.id), Some(secret.clone()));
        assert_eq!(
            Vault::unlock(&directory, &master)
                .expect("unlocked")
                .get(&secret_name(&saved.id))
                .expect("readable")
                .as_deref(),
            Some(secret.as_str())
        );
        let _ = app.update(Message::SiteSelected(SiteSelection::Site(saved.id.clone())));
        let _ = app.update(Message::SiteDelete);
        let _ = app.update(Message::DialogConfirmed);
        assert!(
            Vault::unlock(&directory, &master)
                .expect("unlocked")
                .names()
                .is_empty()
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn master_dialogs_validate_their_input() {
        let directory = temporary_directory("master");
        let mut app = app(&directory);
        let _ = app.update(Message::SecretBackendSelected(SecretBackend::Vault));
        let can = |app: &Hoplix| app.dialog.as_ref().is_some_and(Dialog::can_confirm);
        assert!(!can(&app));
        let chosen = test_password();
        let _ = app.update(Message::DialogInput(chosen.clone()));
        assert!(!can(&app));
        let _ = app.update(Message::DialogConfirmInput(test_password()));
        assert!(!can(&app));
        let _ = app.update(Message::DialogConfirmInput(chosen));
        assert!(can(&app));
        let _ = app.update(Message::DialogCancelled);
        assert_eq!(app.dialog, None);
        assert_eq!(app.secret_backend, SecretBackend::Keyring);
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }

    #[test]
    fn connect_forms_can_become_sites() {
        let directory = temporary_directory("save-as");
        let mut app = app(&directory);
        app.connect_form.apply(crate::connect_form::Field::Host(
            "ftp.example.test".to_string(),
        ));
        let _ = app.update(Message::ConnectSaveAsSite);
        assert_eq!(app.page, Page::Sites);
        let form = app.site_form.as_ref().expect("form opened");
        assert!(form.is_new);
        assert_eq!(form.name, "ftp.example.test");
        assert!(app.sites.sites.is_empty());
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
