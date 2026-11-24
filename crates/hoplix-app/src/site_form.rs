use hoplix_core::{Site, normalize_folder};
use iced::widget::text_editor;

use crate::connect_form::{ConnectForm, Field, FormError};

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteFormError {
    MissingName,
    Connection(FormError),
}

#[derive(Debug, Clone)]
pub enum SiteField {
    Name(String),
    Folder(String),
    RemotePath(String),
    LocalPath(String),
    Notes(text_editor::Action),
    Connection(Field),
}

#[derive(Debug, Default)]
pub struct SiteForm {
    pub id: String,
    pub is_new: bool,
    pub name: String,
    pub folder: String,
    pub connection: ConnectForm,
    pub remote_path: String,
    pub local_path: String,
    pub notes: text_editor::Content,
    pub had_secret: bool,
    pub invalid: Option<SiteFormError>,
}

impl SiteFormError {
    pub fn key(self) -> &'static str {
        match self {
            Self::MissingName => "sites.invalid.name",
            Self::Connection(error) => error.key(),
        }
    }
}

// Construction

impl SiteForm {
    pub fn edit(site: &Site) -> Self {
        let mut connection = ConnectForm::default();
        connection.fill_from_site(site);
        Self {
            id: site.id.clone(),
            is_new: false,
            name: site.name.clone(),
            folder: site.folder.clone(),
            connection,
            remote_path: site.remote_path.clone(),
            local_path: site.local_path.clone(),
            notes: text_editor::Content::with_text(&site.notes),
            had_secret: site.save_secrets,
            invalid: None,
        }
    }

    pub fn create(site: &Site) -> Self {
        Self {
            is_new: true,
            ..Self::edit(site)
        }
    }

    pub fn from_connection(site: &Site, connection: &ConnectForm) -> Self {
        let mut form = Self::create(site);
        form.connection = ConnectForm {
            connecting: false,
            error: None,
            invalid: None,
            site: Some(site.id.clone()),
            ..connection.clone()
        };
        form
    }

    // Editing

    pub fn apply(&mut self, field: SiteField) {
        match field {
            SiteField::Name(name) => self.name = name,
            SiteField::Folder(folder) => self.folder = folder,
            SiteField::RemotePath(path) => self.remote_path = path,
            SiteField::LocalPath(path) => self.local_path = path,
            SiteField::Notes(action) => self.notes.perform(action),
            SiteField::Connection(field) => self.connection.apply(field),
        }
        self.invalid = None;
    }

    // Validation

    pub fn to_site(&self) -> Result<(Site, Option<String>), SiteFormError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(SiteFormError::MissingName);
        }
        let params = self
            .connection
            .to_params()
            .map_err(SiteFormError::Connection)?;
        let notes = self.notes.text();
        let site = Site {
            id: self.id.clone(),
            name: name.to_string(),
            folder: normalize_folder(&self.folder),
            protocol: params.protocol,
            host: params.host,
            port: params.port,
            username: params.username,
            auth: self.connection.site_auth(),
            key_path: self
                .connection
                .key_path
                .trim()
                .trim_matches('"')
                .to_string(),
            remote_path: self.remote_path.trim().to_string(),
            local_path: self.local_path.trim().trim_matches('"').to_string(),
            notes: notes.trim_end().to_string(),
            save_secrets: self.connection.save_password,
        };
        let secret = Some(self.connection.secret().to_string()).filter(|secret| !secret.is_empty());
        Ok((site, secret))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect_form::AuthMethod;
    use hoplix_core::{Protocol, SiteAuth};

    fn site() -> Site {
        Site {
            host: "files.example.test".to_string(),
            username: "demo".to_string(),
            folder: "Work".to_string(),
            remote_path: "/srv".to_string(),
            notes: "first\nsecond".to_string(),
            save_secrets: true,
            ..Site::new("Web")
        }
    }

    #[test]
    fn editing_keeps_the_site_and_reports_the_secret() {
        let original = site();
        let mut form = SiteForm::edit(&original);
        assert!(!form.is_new && form.had_secret);
        let (saved, secret) = form.to_site().expect("valid");
        assert_eq!(saved, original);
        assert_eq!(secret, None);

        form.apply(SiteField::Name("  Web 2 ".to_string()));
        form.apply(SiteField::Folder(" Work / Clients ".to_string()));
        form.apply(SiteField::Connection(Field::Password("pw".to_string())));
        let (saved, secret) = form.to_site().expect("valid");
        assert_eq!(saved.name, "Web 2");
        assert_eq!(saved.folder, "Work/Clients");
        assert_eq!(secret.as_deref(), Some("pw"));
    }

    #[test]
    fn key_files_store_the_passphrase_as_secret() {
        let mut form = SiteForm::create(&site());
        assert!(form.is_new);
        form.apply(SiteField::Connection(Field::Method(AuthMethod::KeyFile)));
        assert_eq!(
            form.to_site().map(|_| ()),
            Err(SiteFormError::Connection(FormError::MissingKey))
        );
        form.apply(SiteField::Connection(Field::KeyPath(
            "\"C:\\id\"".to_string(),
        )));
        form.apply(SiteField::Connection(Field::Passphrase(
            "phrase".to_string(),
        )));
        let (saved, secret) = form.to_site().expect("valid");
        assert_eq!(saved.auth, SiteAuth::KeyFile);
        assert_eq!(saved.key_path, "C:\\id");
        assert_eq!(secret.as_deref(), Some("phrase"));
    }

    #[test]
    fn invalid_forms_are_reported() {
        let mut form = SiteForm::create(&site());
        form.apply(SiteField::Name(" ".to_string()));
        assert_eq!(form.to_site().map(|_| ()), Err(SiteFormError::MissingName));
        form.apply(SiteField::Name("x".to_string()));
        form.apply(SiteField::Connection(Field::Host(String::new())));
        assert_eq!(
            form.to_site().map(|_| ()),
            Err(SiteFormError::Connection(FormError::MissingHost))
        );
    }

    #[test]
    fn connect_forms_become_new_sites() {
        let mut connection = ConnectForm::default();
        connection.apply(Field::Protocol(Protocol::FtpsExplicit));
        connection.apply(Field::Host("ftp.example.test".to_string()));
        connection.apply(Field::Username("demo".to_string()));
        connection.apply(Field::Password("pw".to_string()));
        connection.apply(Field::SavePassword(true));
        let target = Site::new("ftp.example.test");
        let form = SiteForm::from_connection(&target, &connection);
        let (saved, secret) = form.to_site().expect("valid");
        assert_eq!(saved.id, target.id);
        assert_eq!(saved.protocol, Protocol::FtpsExplicit);
        assert_eq!(saved.port, 21);
        assert!(saved.save_secrets);
        assert_eq!(secret.as_deref(), Some("pw"));
    }
}
