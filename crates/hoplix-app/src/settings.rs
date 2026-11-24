use hoplix_core::DEFAULT_PARALLELISM;
use std::fs;
use std::io;
use std::path::Path;

use hoplix_core::key_value;

// Constants

const FILE_NAME: &str = "settings.conf";
pub const MAX_PARALLEL_TRANSFERS: usize = 8;

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    Modern,
    Classic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub theme: ThemePreference,
    pub language: Option<String>,
    pub layout: Layout,
    pub parallel_transfers: usize,
    pub editor: String,
    pub upload_changes_automatically: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::default(),
            language: None,
            layout: Layout::default(),
            parallel_transfers: DEFAULT_PARALLELISM,
            editor: String::new(),
            upload_changes_automatically: false,
        }
    }
}

impl ThemePreference {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

impl Layout {
    pub const ALL: [Self; 2] = [Self::Modern, Self::Classic];

    pub fn pane_count(self) -> usize {
        match self {
            Self::Modern => 1,
            Self::Classic => 2,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Modern => "modern",
            Self::Classic => "classic",
        }
    }
}

// Serialization

impl Settings {
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for (key, value) in key_value::parse(text) {
            match key.as_str() {
                "theme" => {
                    if let Some(theme) = ThemePreference::ALL.into_iter().find(|t| t.key() == value)
                    {
                        settings.theme = theme;
                    }
                }
                "language" => settings.language = Some(value).filter(|code| !code.is_empty()),
                "layout" => {
                    if let Some(layout) = Layout::ALL.into_iter().find(|l| l.key() == value) {
                        settings.layout = layout;
                    }
                }
                "parallel_transfers" => {
                    if let Ok(count) = value.parse::<usize>()
                        && (1..=MAX_PARALLEL_TRANSFERS).contains(&count)
                    {
                        settings.parallel_transfers = count;
                    }
                }
                "editor" => settings.editor = value,
                "upload_changes_automatically" => {
                    settings.upload_changes_automatically = value == "true";
                }
                _ => {}
            }
        }
        settings
    }

    pub fn to_text(&self) -> String {
        key_value::write(&[
            ("theme", self.theme.key().to_string()),
            ("language", self.language.clone().unwrap_or_default()),
            ("layout", self.layout.key().to_string()),
            ("parallel_transfers", self.parallel_transfers.to_string()),
            ("editor", self.editor.clone()),
            (
                "upload_changes_automatically",
                self.upload_changes_automatically.to_string(),
            ),
        ])
    }

    // Storage

    pub fn load(directory: Option<&Path>) -> Self {
        directory
            .and_then(|directory| fs::read_to_string(directory.join(FILE_NAME)).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn save(&self, directory: &Path) -> io::Result<()> {
        fs::create_dir_all(directory)?;
        let temporary = directory.join(format!("{FILE_NAME}.tmp"));
        fs::write(&temporary, self.to_text())?;
        fs::rename(temporary, directory.join(FILE_NAME))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let settings = Settings {
            theme: ThemePreference::Dark,
            language: Some("de".to_string()),
            layout: Layout::Classic,
            parallel_transfers: 4,
            editor: r#"C:\Tools\My Editor = "new".exe"#.to_string(),
            upload_changes_automatically: true,
        };
        assert_eq!(Settings::parse(&settings.to_text()), settings);
    }

    #[test]
    fn unknown_values_keep_the_defaults() {
        let settings = Settings::parse(
            "theme = purple\nlayout = spiral\nlanguage =\nother = 1\nparallel_transfers = 99\n",
        );
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn layouts_know_their_pane_count() {
        assert_eq!(Layout::Modern.pane_count(), 1);
        assert_eq!(Layout::Classic.pane_count(), 2);
    }
}
