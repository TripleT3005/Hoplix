use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use hoplix_core::language::{ENGLISH_CODE, FILE_EXTENSION, LanguageFile};

// Constants

pub const FALLBACK_CODE: &str = ENGLISH_CODE;
const EMBEDDED_ENGLISH: &str = include_str!("../../../lang/en.lang");
const LANG_DIRECTORY: &str = "lang";
const MAX_FALLBACK_DEPTH: usize = 8;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageInfo {
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct Strings {
    tables: Vec<HashMap<String, String>>,
}

// Lookup

impl Strings {
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.tables
            .iter()
            .find_map(|table| table.get(key))
            .map_or(key, String::as_str)
    }

    pub fn format(&self, key: &str, arguments: &[(&str, &str)]) -> String {
        arguments
            .iter()
            .fold(self.get(key).to_string(), |text, (name, value)| {
                text.replace(&format!("{{{name}}}"), value)
            })
    }
}

// Loading

pub fn load(code: &str) -> Strings {
    let mut tables = Vec::new();
    let mut next = Some(code.to_string());
    while let Some(current) = next.take() {
        if current == FALLBACK_CODE || tables.len() >= MAX_FALLBACK_DEPTH {
            break;
        }
        let Some(file) = read_language(&current) else {
            break;
        };
        next = file.fallback().map(str::to_string);
        tables.push(file.entries);
    }
    if let Some(file) = read_language(FALLBACK_CODE) {
        tables.push(file.entries);
    }
    tables.push(embedded_english().entries);
    Strings { tables }
}

pub fn available_languages() -> Vec<LanguageInfo> {
    let mut languages = vec![info_from(&embedded_english())];
    for directory in lang_directories() {
        let Ok(reader) = fs::read_dir(directory) else {
            continue;
        };
        for item in reader.filter_map(Result::ok) {
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) != Some(FILE_EXTENSION) {
                continue;
            }
            let Some(code) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if languages.iter().any(|language| language.code == code) {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                languages.push(info_from(&LanguageFile::parse(code, &text)));
            }
        }
    }
    languages.sort_by_key(|language| language.name.to_lowercase());
    languages
}

pub fn resolve(preferred: Option<&str>, available: &[LanguageInfo]) -> String {
    let candidates = preferred
        .map(str::to_string)
        .into_iter()
        .chain(system_language());
    for candidate in candidates {
        if let Some(code) = best_match(&candidate, available) {
            return code;
        }
    }
    FALLBACK_CODE.to_string()
}

// Matching

fn best_match(candidate: &str, available: &[LanguageInfo]) -> Option<String> {
    let candidate = candidate.replace('_', "-");
    let mut tag = candidate
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .to_string();
    while !tag.is_empty() {
        if let Some(language) = available
            .iter()
            .find(|language| language.code.eq_ignore_ascii_case(&tag))
        {
            return Some(language.code.clone());
        }
        match tag.rfind('-') {
            Some(index) => tag.truncate(index),
            None => break,
        }
    }
    None
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn system_language() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    const LOCALE_NAME_MAX_LENGTH: usize = 85;
    let mut buffer = [0u16; LOCALE_NAME_MAX_LENGTH];
    let length = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    let length = usize::try_from(length).ok()?.checked_sub(1)?;
    String::from_utf16(buffer.get(..length)?).ok()
}

#[cfg(not(windows))]
fn system_language() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty() && value != "C" && value != "POSIX")
}

// Files

fn lang_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(directory) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join(LANG_DIRECTORY)))
    {
        directories.push(directory);
    }
    if cfg!(debug_assertions) {
        directories.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lang"));
    }
    directories
}

fn read_language(code: &str) -> Option<LanguageFile> {
    lang_directories()
        .into_iter()
        .find_map(|directory| {
            fs::read_to_string(directory.join(format!("{code}.{FILE_EXTENSION}"))).ok()
        })
        .map(|text| LanguageFile::parse(code, &text))
}

fn embedded_english() -> LanguageFile {
    LanguageFile::parse(FALLBACK_CODE, EMBEDDED_ENGLISH)
}

fn info_from(file: &LanguageFile) -> LanguageInfo {
    LanguageInfo {
        code: file.code.clone(),
        name: file.name().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn languages(codes: &[&str]) -> Vec<LanguageInfo> {
        codes
            .iter()
            .map(|code| LanguageInfo {
                code: (*code).to_string(),
                name: (*code).to_string(),
            })
            .collect()
    }

    fn table_from(text: &str) -> HashMap<String, String> {
        LanguageFile::parse("", text).entries
    }

    // Matching

    #[test]
    fn matches_regions_and_posix_locales() {
        let available = languages(&["en", "de", "pt-BR"]);
        assert_eq!(best_match("de-AT", &available).as_deref(), Some("de"));
        assert_eq!(best_match("de_DE.UTF-8", &available).as_deref(), Some("de"));
        assert_eq!(best_match("pt-br", &available).as_deref(), Some("pt-BR"));
        assert_eq!(best_match("fr-FR", &available), None);
    }

    #[test]
    fn preferred_language_wins() {
        assert_eq!(resolve(Some("de"), &languages(&["en", "de"])), "de");
        assert_eq!(resolve(Some("xx"), &languages(&["xx"])), "xx");
    }

    // Lookup

    #[test]
    fn missing_texts_fall_back_one_by_one() {
        let strings = Strings {
            tables: vec![
                table_from("greeting = Hallo {name}\n"),
                table_from("greeting = Hello {name}\nfarewell = Bye\n"),
            ],
        };
        assert_eq!(strings.format("greeting", &[("name", "Ada")]), "Hallo Ada");
        assert_eq!(strings.get("farewell"), "Bye");
        assert_eq!(strings.get("unknown.key"), "unknown.key");
    }

    // Files

    #[test]
    fn every_language_file_has_all_english_keys() {
        let english = table_from(EMBEDDED_ENGLISH);
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lang");
        for item in fs::read_dir(directory).expect("reading the lang folder failed") {
            let path = item.expect("reading an entry failed").path();
            let text = fs::read_to_string(&path).expect("reading a language file failed");
            let table = table_from(&text);
            let missing: Vec<_> = english
                .keys()
                .filter(|key| !table.contains_key(*key))
                .collect();
            assert!(missing.is_empty(), "{} lacks {missing:?}", path.display());
        }
    }

    #[test]
    fn german_loads_with_english_fallback() {
        let strings = load("de");
        assert_eq!(strings.get("language.name"), "Deutsch");
        assert_eq!(load("does-not-exist").get("language.name"), "English");
    }
}
