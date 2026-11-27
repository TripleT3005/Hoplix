use hoplix_core::{
    Bookmarks, ConnectionOptions, FileNameEncoding, FtpMode, MAX_SITE_TRANSFERS, Protocol, Site,
    normalize_folder, seconds_from_text, seconds_text,
};
use iced::widget::text_editor;

use crate::connect_form::{ConnectForm, Field, FormError};

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteFormError {
    MissingName,
    Connection(FormError),
    InvalidTimeout,
    InvalidKeepalive,
    InvalidMaxTransfers,
    ActiveModeWithProxy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SiteSecrets {
    pub main: Option<String>,
    pub jump: Option<String>,
    pub proxy: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SiteField {
    Name(String),
    Folder(String),
    RemotePath(String),
    LocalPath(String),
    Notes(text_editor::Action),
    Connection(Field),
    Option(OptionField),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionField {
    Toggle,
    ConnectTimeout(String),
    Keepalive(String),
    FtpMode(FtpMode),
    ExternalIp(bool),
    Encoding(FileNameEncoding),
    MaxTransfers(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsForm {
    pub open: bool,
    pub connect_timeout: String,
    pub keepalive: String,
    pub ftp_mode: FtpMode,
    pub ftp_external_ip: bool,
    pub ftp_encoding: FileNameEncoding,
    pub max_transfers: String,
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
    pub had_jump_secret: bool,
    pub had_proxy_secret: bool,
    pub options: OptionsForm,
    pub invalid: Option<SiteFormError>,
}

impl SiteFormError {
    pub fn key(self) -> &'static str {
        match self {
            Self::MissingName => "sites.invalid.name",
            Self::Connection(error) => error.key(),
            Self::InvalidTimeout => "sites.invalid.timeout",
            Self::InvalidKeepalive => "sites.invalid.keepalive",
            Self::InvalidMaxTransfers => "sites.invalid.max_transfers",
            Self::ActiveModeWithProxy => "sites.invalid.active_proxy",
        }
    }
}

// Options

impl Default for OptionsForm {
    fn default() -> Self {
        Self::from_options(&ConnectionOptions::default(), None)
    }
}

impl OptionsForm {
    pub fn from_options(options: &ConnectionOptions, max_transfers: Option<usize>) -> Self {
        Self {
            open: false,
            connect_timeout: seconds_text(options.connect_timeout),
            keepalive: seconds_text(options.keepalive_interval),
            ftp_mode: options.ftp_mode,
            ftp_external_ip: options.ftp_external_ip,
            ftp_encoding: options.ftp_encoding,
            max_transfers: max_transfers
                .map(|count| count.to_string())
                .unwrap_or_default(),
        }
    }

    pub fn apply(&mut self, field: OptionField) {
        let digits = |text: &str| text.chars().all(|c| c.is_ascii_digit());
        match field {
            OptionField::Toggle => self.open = !self.open,
            OptionField::ConnectTimeout(text) if digits(&text) => self.connect_timeout = text,
            OptionField::Keepalive(text) if digits(&text) => self.keepalive = text,
            OptionField::MaxTransfers(text) if digits(&text) => self.max_transfers = text,
            OptionField::ConnectTimeout(_)
            | OptionField::Keepalive(_)
            | OptionField::MaxTransfers(_) => {}
            OptionField::FtpMode(mode) => self.ftp_mode = mode,
            OptionField::ExternalIp(enabled) => self.ftp_external_ip = enabled,
            OptionField::Encoding(encoding) => self.ftp_encoding = encoding,
        }
    }

    pub fn to_options(&self) -> Result<(ConnectionOptions, Option<usize>), SiteFormError> {
        let connect_timeout =
            seconds_from_text(&self.connect_timeout).ok_or(SiteFormError::InvalidTimeout)?;
        let keepalive_interval =
            seconds_from_text(&self.keepalive).ok_or(SiteFormError::InvalidKeepalive)?;
        let max_transfers = match self.max_transfers.trim() {
            "" => None,
            text => Some(
                text.parse::<usize>()
                    .ok()
                    .filter(|count| (1..=MAX_SITE_TRANSFERS).contains(count))
                    .ok_or(SiteFormError::InvalidMaxTransfers)?,
            ),
        };
        let options = ConnectionOptions {
            connect_timeout,
            keepalive_interval,
            ftp_mode: self.ftp_mode,
            ftp_external_ip: self.ftp_external_ip,
            ftp_encoding: self.ftp_encoding,
            s3: hoplix_core::S3Options::default(),
        };
        Ok((options, max_transfers))
    }

    pub fn is_default(&self) -> bool {
        self.to_options()
            .is_ok_and(|(options, max)| options == ConnectionOptions::default() && max.is_none())
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
            had_jump_secret: site.jump.as_ref().is_some_and(|jump| jump.save_secret),
            had_proxy_secret: site.proxy.as_ref().is_some_and(|proxy| proxy.save_secret),
            options: OptionsForm::from_options(&site.options, site.max_transfers),
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
            SiteField::Option(field) => self.options.apply(field),
        }
        self.invalid = None;
    }

    pub fn reveal_invalid(&mut self, invalid: SiteFormError) {
        let advanced = match invalid {
            SiteFormError::Connection(error) => error.is_route(),
            SiteFormError::MissingName => false,
            SiteFormError::InvalidTimeout
            | SiteFormError::InvalidKeepalive
            | SiteFormError::InvalidMaxTransfers
            | SiteFormError::ActiveModeWithProxy => true,
        };
        if advanced {
            self.options.open = true;
        }
        self.invalid = Some(invalid);
    }

    // Validation

    pub fn to_site(&self) -> Result<(Site, SiteSecrets), SiteFormError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(SiteFormError::MissingName);
        }
        let params = self
            .connection
            .to_params()
            .map_err(SiteFormError::Connection)?;
        let jump = self
            .connection
            .jump_settings()
            .map_err(SiteFormError::Connection)?;
        let proxy = self
            .connection
            .proxy_settings()
            .map_err(SiteFormError::Connection)?;
        let (options, max_transfers) = self.options.to_options()?;
        let options = self.connection.connection_options(options);
        if proxy.is_some()
            && params.protocol != Protocol::Sftp
            && options.ftp_mode == FtpMode::Active
        {
            return Err(SiteFormError::ActiveModeWithProxy);
        }
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
            save_secrets: self.connection.save_password && self.connection.uses_secret(),
            options,
            max_transfers,
            jump,
            proxy,
            bookmarks: Bookmarks::default(),
        };
        let filled = |secret: &str| Some(secret.to_string()).filter(|secret| !secret.is_empty());
        let secrets = SiteSecrets {
            main: filled(self.connection.secret()),
            jump: site
                .jump
                .as_ref()
                .and_then(|_| filled(self.connection.jump.secret())),
            proxy: site
                .proxy
                .as_ref()
                .filter(|proxy| proxy.uses_secret())
                .and_then(|_| filled(&self.connection.proxy.password)),
        };
        Ok((site, secrets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect_form::AuthMethod;
    use crate::ui_tests::test_password;
    use hoplix_core::SiteAuth;
    use hoplix_core::{FileNameEncoding, FtpMode};

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
        assert_eq!(secret, SiteSecrets::default());

        form.apply(SiteField::Name("  Web 2 ".to_string()));
        form.apply(SiteField::Folder(" Work / Clients ".to_string()));
        let password = test_password();
        form.apply(SiteField::Connection(Field::Password(password.clone())));
        let (saved, secret) = form.to_site().expect("valid");
        assert_eq!(saved.name, "Web 2");
        assert_eq!(saved.folder, "Work/Clients");
        assert_eq!(secret.main, Some(password));
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
        let passphrase = test_password();
        form.apply(SiteField::Connection(Field::Passphrase(passphrase.clone())));
        let (saved, secret) = form.to_site().expect("valid");
        assert_eq!(saved.auth, SiteAuth::KeyFile);
        assert_eq!(saved.key_path, "C:\\id");
        assert_eq!(secret.main, Some(passphrase));
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
    fn advanced_options_are_validated_and_saved() {
        let original = site();
        let mut form = SiteForm::edit(&original);
        assert!(form.options.is_default());
        assert!(form.options.ftp_external_ip);
        form.apply(SiteField::Option(OptionField::ConnectTimeout(
            "45".to_string(),
        )));
        form.apply(SiteField::Option(OptionField::Keepalive("x".to_string())));
        assert_eq!(form.options.keepalive, "");
        form.apply(SiteField::Option(OptionField::Keepalive("15".to_string())));
        form.apply(SiteField::Option(OptionField::FtpMode(FtpMode::Active)));
        form.apply(SiteField::Option(OptionField::ExternalIp(false)));
        form.apply(SiteField::Option(OptionField::Encoding(
            FileNameEncoding::Windows1252,
        )));
        form.apply(SiteField::Option(OptionField::MaxTransfers(
            "3".to_string(),
        )));
        let (saved, _) = form.to_site().expect("valid");
        assert_eq!(
            saved.options.connect_timeout,
            Some(std::time::Duration::from_secs(45))
        );
        assert_eq!(
            saved.options.keepalive_interval,
            Some(std::time::Duration::from_secs(15))
        );
        assert_eq!(saved.options.ftp_mode, FtpMode::Active);
        assert!(!saved.options.ftp_external_ip);
        assert_eq!(saved.options.ftp_encoding, FileNameEncoding::Windows1252);
        assert_eq!(saved.max_transfers, Some(3));
        assert_eq!(SiteForm::edit(&saved).options.max_transfers, "3");

        form.apply(SiteField::Option(OptionField::MaxTransfers(
            "9".to_string(),
        )));
        assert_eq!(
            form.to_site().map(|_| ()),
            Err(SiteFormError::InvalidMaxTransfers)
        );
        form.apply(SiteField::Option(OptionField::MaxTransfers(String::new())));
        form.apply(SiteField::Option(OptionField::ConnectTimeout(
            "4000".to_string(),
        )));
        assert_eq!(
            form.to_site().map(|_| ()),
            Err(SiteFormError::InvalidTimeout)
        );
        form.options.open = false;
        form.reveal_invalid(SiteFormError::InvalidTimeout);
        assert!(form.options.open);
        form.apply(SiteField::Option(OptionField::ConnectTimeout(
            "0".to_string(),
        )));
        let (saved, _) = form.to_site().expect("valid");
        assert_eq!(saved.options.connect_timeout, None);
        assert_eq!(saved.max_transfers, None);
    }

    #[test]
    fn jump_hosts_and_proxies_become_part_of_the_site() {
        use crate::route_form::{JumpField, ProxyField};
        use hoplix_core::ProxyKind;

        let mut form = SiteForm::edit(&site());
        form.apply(SiteField::Connection(Field::Method(AuthMethod::Agent)));
        form.apply(SiteField::Connection(Field::Jump(JumpField::Enabled(true))));
        form.options.open = false;
        let invalid = form.to_site().map(|_| ()).expect_err("jump host is empty");
        assert_eq!(
            invalid,
            SiteFormError::Connection(FormError::MissingJumpHost)
        );
        form.reveal_invalid(invalid);
        assert!(form.options.open);
        form.apply(SiteField::Connection(Field::Jump(JumpField::Host(
            "bastion.test".to_string(),
        ))));
        form.apply(SiteField::Connection(Field::Jump(JumpField::Username(
            "hop".to_string(),
        ))));
        let jump_password = test_password();
        form.apply(SiteField::Connection(Field::Jump(JumpField::Password(
            jump_password.clone(),
        ))));
        form.apply(SiteField::Connection(Field::Jump(JumpField::SavePassword(
            true,
        ))));
        form.apply(SiteField::Connection(Field::Proxy(ProxyField::Kind(Some(
            ProxyKind::Socks5,
        )))));
        form.apply(SiteField::Connection(Field::Proxy(ProxyField::Host(
            "proxy.test".to_string(),
        ))));
        let proxy_password = test_password();
        form.apply(SiteField::Connection(Field::Proxy(ProxyField::Password(
            proxy_password.clone(),
        ))));
        let (saved, secrets) = form.to_site().expect("valid");
        assert_eq!(saved.auth, SiteAuth::Agent);
        assert!(!saved.save_secrets);
        assert!(saved.jump.as_ref().is_some_and(|jump| jump.save_secret));
        assert_eq!(saved.proxy.as_ref().map(|proxy| proxy.port), Some(1080));
        assert_eq!(secrets.main, None);
        assert_eq!(secrets.jump, Some(jump_password));
        assert_eq!(secrets.proxy, None);
        form.apply(SiteField::Connection(Field::Proxy(ProxyField::Username(
            "me".to_string(),
        ))));
        assert_eq!(form.to_site().expect("valid").1.proxy, Some(proxy_password));
        let edited = SiteForm::edit(&saved);
        assert!(edited.had_jump_secret && !edited.had_proxy_secret);
        assert!(edited.connection.advanced_open);

        form.apply(SiteField::Connection(Field::Protocol(
            hoplix_core::Protocol::Ftp,
        )));
        form.apply(SiteField::Option(OptionField::FtpMode(FtpMode::Active)));
        assert_eq!(
            form.to_site().map(|_| ()),
            Err(SiteFormError::ActiveModeWithProxy)
        );
        form.apply(SiteField::Option(OptionField::FtpMode(FtpMode::Passive)));
        let (saved, _) = form.to_site().expect("valid");
        assert_eq!(saved.jump, None);
        assert!(saved.proxy.is_some());
    }

    #[test]
    fn connect_forms_become_new_sites() {
        let mut connection = ConnectForm::default();
        connection.apply(Field::Protocol(Protocol::FtpsExplicit));
        connection.apply(Field::Host("ftp.example.test".to_string()));
        connection.apply(Field::Username("demo".to_string()));
        let password = test_password();
        connection.apply(Field::Password(password.clone()));
        connection.apply(Field::SavePassword(true));
        let target = Site::new("ftp.example.test");
        let form = SiteForm::from_connection(&target, &connection);
        let (saved, secret) = form.to_site().expect("valid");
        assert_eq!(saved.id, target.id);
        assert_eq!(saved.protocol, Protocol::FtpsExplicit);
        assert_eq!(saved.port, 21);
        assert!(saved.save_secrets);
        assert_eq!(secret.main, Some(password));
    }
}
