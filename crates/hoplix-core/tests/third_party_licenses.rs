use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// Constants

const DOCUMENT: &str = "THIRD-PARTY-LICENSES.txt";
const UPDATE_VARIABLE: &str = "UPDATE_THIRD_PARTY_LICENSES";
const TARGETS: [&str; 2] = ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"];
const INSTALLER: &str = "installer/hoplix.iss";
const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";
const RULE_WIDTH: usize = 72;

const PREFERRED_LICENSES: [&str; 15] = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-3-Clause",
    "BSD-2-Clause",
    "ISC",
    "Zlib",
    "0BSD",
    "Unlicense",
    "BSL-1.0",
    "CC0-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "CDLA-Permissive-2.0",
    "MPL-2.0",
];

const CONTENT_MARKERS: [(&str, &str); 14] = [
    ("Apache-2.0", "Apache License"),
    ("MIT", "Permission is hereby granted, free of charge"),
    ("MPL-2.0", "Mozilla Public License"),
    ("Unlicense", "This is free and unencumbered software"),
    ("BSL-1.0", "Boost Software License"),
    ("Zlib", "This software is provided 'as-is'"),
    ("Unicode-3.0", "UNICODE LICENSE V3"),
    ("Unicode-DFS-2016", "UNICODE, INC. LICENSE AGREEMENT"),
    ("CDLA-Permissive-2.0", "Community Data License Agreement"),
    ("CC0-1.0", "CC0 1.0 Universal"),
    ("BSD-3-Clause", "Neither the name of"),
    (
        "BSD-2-Clause",
        "Redistribution and use in source and binary forms",
    ),
    (
        "ISC",
        "Permission to use, copy, modify, and/or distribute this software for any",
    ),
    (
        "ISC",
        "Permission to use, copy, modify, and distribute this software for any",
    ),
];

const NAME_MARKERS: [(&str, &str); 13] = [
    ("Apache-2.0", "apache"),
    ("MIT", "mit"),
    ("MPL-2.0", "mpl"),
    ("Unlicense", "unlicense"),
    ("BSL-1.0", "bsl"),
    ("BSL-1.0", "boost"),
    ("Zlib", "zlib"),
    ("ISC", "isc"),
    ("BSD-3-Clause", "bsd"),
    ("BSD-2-Clause", "bsd"),
    ("Unicode-3.0", "unicode"),
    ("CDLA-Permissive-2.0", "cdla"),
    ("CC0-1.0", "cc0"),
];

const NO_NOTICE_LICENSES: [&str; 4] = ["0BSD", "BSL-1.0", "CC0-1.0", "Unlicense"];

const OTHER_LICENSE_NAMES: [&str; 2] = ["gpl", "0bsd"];

const EXTRA_NOTICE_FILES: [(&str, &str); 2] = [
    ("ring", "LICENSE"),
    ("ring", "src/polyfill/once_cell/LICENSE-MIT"),
];

const LICENSE_FILE_PREFIXES: [&str; 5] =
    ["license", "licence", "copying", "unlicense", "copyright"];

const MIT_TEMPLATE: &str = "Copyright (c) {copyright}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
";

const NO_NOTICE_TEMPLATE: &str =
    "The {license} license does not require its text to be included in binary
distributions, and the component ships no license file. Full text:
https://spdx.org/licenses/{license}.html
";

const HEADER: &str = "Hoplix - Third-party licenses
=============================

Hoplix contains the following third-party components. The list covers the
normal dependencies of hoplix, hoplix-cli and hoplix-core for Windows x64 and
Linux x64. Build scripts, procedural macros and test-only dependencies are not
part of the programs and are not listed.

For components offered under a choice of licenses (for example
\"MIT OR Apache-2.0\"), \"Used under\" names the licenses chosen.

This file is generated. To update it, run
UPDATE_THIRD_PARTY_LICENSES=1 cargo test -p hoplix-core --test third_party_licenses
";

// Types

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

#[derive(Debug, Clone, PartialEq)]
enum Expression {
    License(String),
    And(Vec<Expression>),
    Or(Vec<Expression>),
}

#[derive(Debug, Clone)]
struct Package {
    name: String,
    version: String,
    license: String,
    repository: String,
    authors: Vec<String>,
    directory: Option<PathBuf>,
    license_file: Option<PathBuf>,
}

#[derive(Debug)]
struct Section {
    license: String,
    text: String,
    packages: Vec<String>,
}

#[derive(Debug)]
struct Notice {
    text: String,
    sources: Vec<String>,
}

#[derive(Debug)]
struct Report {
    packages: Vec<(Package, Vec<String>)>,
    sections: Vec<Section>,
    notices: Vec<Notice>,
}

// JSON

impl Json {
    fn parse(text: &str) -> Result<Self, String> {
        let mut parser = JsonParser {
            bytes: text.as_bytes(),
            position: 0,
        };
        let value = parser.value()?;
        parser.skip_whitespace();
        if parser.position == parser.bytes.len() {
            Ok(value)
        } else {
            Err(format!("unexpected data at byte {}", parser.position))
        }
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.get(key),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(text) => Some(text),
            _ => None,
        }
    }

    fn as_array(&self) -> &[Json] {
        match self {
            Json::Array(items) => items,
            _ => &[],
        }
    }

    fn text(&self, key: &str) -> Option<String> {
        self.get(key)
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    }
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl JsonParser<'_> {
    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.position += 1;
        }
    }

    fn next(&mut self) -> Result<u8, String> {
        let byte = *self
            .bytes
            .get(self.position)
            .ok_or("unexpected end of JSON")?;
        self.position += 1;
        Ok(byte)
    }

    fn expect(&mut self, expected: u8) -> Result<(), String> {
        self.skip_whitespace();
        match self.next()? {
            byte if byte == expected => Ok(()),
            byte => Err(format!(
                "expected '{}' at byte {}, found '{}'",
                expected as char,
                self.position - 1,
                byte as char
            )),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.position..].starts_with(word.as_bytes()) {
            self.position += word.len();
            Ok(value)
        } else {
            Err(format!("invalid literal at byte {}", self.position))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.skip_whitespace();
        match self.bytes.get(self.position) {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(byte) if *byte == b'-' || byte.is_ascii_digit() => Ok(self.number()),
            _ => Err(format!("unexpected value at byte {}", self.position)),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut entries = BTreeMap::new();
        self.skip_whitespace();
        if self.bytes.get(self.position) == Some(&b'}') {
            self.position += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.expect(b':')?;
            entries.insert(key, self.value()?);
            self.skip_whitespace();
            match self.next()? {
                b',' => continue,
                b'}' => return Ok(Json::Object(entries)),
                _ => return Err(format!("invalid object at byte {}", self.position - 1)),
            }
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.bytes.get(self.position) == Some(&b']') {
            self.position += 1;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_whitespace();
            match self.next()? {
                b',' => continue,
                b']' => return Ok(Json::Array(items)),
                _ => return Err(format!("invalid array at byte {}", self.position - 1)),
            }
        }
    }

    fn number(&mut self) -> Json {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_digit() || b"+-.eE".contains(byte))
        {
            self.position += 1;
        }
        Json::Number(String::from_utf8_lossy(&self.bytes[start..self.position]).into_owned())
    }

    fn hex(&mut self) -> Result<u32, String> {
        let digits = self
            .bytes
            .get(self.position..self.position + 4)
            .ok_or("unexpected end of JSON")?;
        let value = std::str::from_utf8(digits)
            .ok()
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            .ok_or_else(|| format!("invalid escape at byte {}", self.position))?;
        self.position += 4;
        Ok(value)
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut bytes = Vec::new();
        loop {
            match self.next()? {
                b'"' => break,
                b'\\' => {
                    let escaped = match self.next()? {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut code = self.hex()?;
                            if (0xD800..0xDC00).contains(&code)
                                && self.bytes[self.position..].starts_with(b"\\u")
                            {
                                self.position += 2;
                                let low = self.hex()?;
                                code =
                                    0x10000 + ((code - 0xD800) << 10) + (low.wrapping_sub(0xDC00));
                            }
                            char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
                        }
                        byte => {
                            return Err(format!(
                                "invalid escape '\\{}' at byte {}",
                                byte as char,
                                self.position - 1
                            ));
                        }
                    };
                    let mut buffer = [0; 4];
                    bytes.extend_from_slice(escaped.encode_utf8(&mut buffer).as_bytes());
                }
                byte => bytes.push(byte),
            }
        }
        String::from_utf8(bytes).map_err(|error| error.to_string())
    }
}

// Expressions

fn tokenize(expression: &str) -> Vec<String> {
    expression
        .replace('/', " OR ")
        .replace('(', " ( ")
        .replace(')', " ) ")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn parse_expression(expression: &str) -> Result<Expression, String> {
    let tokens = tokenize(expression);
    let mut position = 0;
    let node = parse_or(&tokens, &mut position, expression)?;
    if position == tokens.len() {
        Ok(node)
    } else {
        Err(format!(
            "unexpected token in license expression: {expression}"
        ))
    }
}

fn parse_or(
    tokens: &[String],
    position: &mut usize,
    expression: &str,
) -> Result<Expression, String> {
    let mut options = vec![parse_and(tokens, position, expression)?];
    while tokens.get(*position).is_some_and(|token| token == "OR") {
        *position += 1;
        options.push(parse_and(tokens, position, expression)?);
    }
    Ok(if options.len() == 1 {
        options.remove(0)
    } else {
        Expression::Or(options)
    })
}

fn parse_and(
    tokens: &[String],
    position: &mut usize,
    expression: &str,
) -> Result<Expression, String> {
    let mut parts = vec![parse_atom(tokens, position, expression)?];
    while tokens.get(*position).is_some_and(|token| token == "AND") {
        *position += 1;
        parts.push(parse_atom(tokens, position, expression)?);
    }
    Ok(if parts.len() == 1 {
        parts.remove(0)
    } else {
        Expression::And(parts)
    })
}

fn parse_atom(
    tokens: &[String],
    position: &mut usize,
    expression: &str,
) -> Result<Expression, String> {
    let token = tokens
        .get(*position)
        .ok_or_else(|| format!("incomplete license expression: {expression}"))?;
    *position += 1;
    if token == "(" {
        let node = parse_or(tokens, position, expression)?;
        if tokens.get(*position).is_none_or(|token| token != ")") {
            return Err(format!("unbalanced license expression: {expression}"));
        }
        *position += 1;
        return Ok(node);
    }
    if tokens.get(*position).is_some_and(|next| next == "WITH") {
        let exception = tokens
            .get(*position + 1)
            .ok_or_else(|| format!("incomplete license expression: {expression}"))?;
        *position += 2;
        return Ok(Expression::License(format!("{token} WITH {exception}")));
    }
    Ok(Expression::License(token.clone()))
}

fn preference(license: &str) -> Option<usize> {
    PREFERRED_LICENSES.iter().position(|item| *item == license)
}

fn choose(node: &Expression) -> Option<Vec<String>> {
    match node {
        Expression::License(license) => preference(license).map(|_| vec![license.clone()]),
        Expression::And(parts) => parts.iter().try_fold(Vec::new(), |mut chosen, part| {
            chosen.extend(choose(part)?);
            Some(chosen)
        }),
        Expression::Or(options) => options.iter().filter_map(choose).min_by_key(|choice| {
            choice
                .iter()
                .filter_map(|license| preference(license))
                .max()
        }),
    }
}

fn choose_licenses(expression: &str) -> Result<Vec<String>, String> {
    let chosen = choose(&parse_expression(expression)?)
        .ok_or_else(|| format!("no accepted license in: {expression}"))?;
    let mut unique = Vec::new();
    for license in chosen {
        if !unique.contains(&license) {
            unique.push(license);
        }
    }
    Ok(unique)
}

// License texts

fn base_license(license: &str) -> &str {
    license.split(" WITH ").next().unwrap_or(license)
}

fn normalize(text: &str) -> String {
    let lines: Vec<_> = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .map(str::to_string)
        .collect();
    format!("{}\n", lines.join("\n").trim_matches('\n'))
}

fn read_text(path: &Path) -> String {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("reading {} failed: {error}", path.display()));
    normalize(&String::from_utf8_lossy(&bytes))
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn detect_licenses(file_name: &str, text: &str) -> BTreeSet<&'static str> {
    let lower = collapse_whitespace(&text.to_lowercase());
    let mut found: BTreeSet<_> = CONTENT_MARKERS
        .iter()
        .filter(|(_, marker)| lower.contains(&marker.to_lowercase()))
        .map(|(license, _)| *license)
        .collect();
    if found.contains("BSD-3-Clause") {
        found.remove("BSD-2-Clause");
    }
    if found.contains("Apache-2.0") && !text.contains("Permission is hereby granted") {
        found.remove("MIT");
    }
    let name = file_name.to_lowercase();
    let by_name: BTreeSet<_> = NAME_MARKERS
        .iter()
        .filter(|(_, marker)| name.contains(marker))
        .map(|(license, _)| *license)
        .collect();
    let both: BTreeSet<_> = found.intersection(&by_name).copied().collect();
    if both.is_empty() { found } else { both }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn top_level_files(package: &Package) -> Vec<PathBuf> {
    let Some(directory) = &package.directory else {
        return Vec::new();
    };
    let mut files: Vec<_> = fs::read_dir(directory)
        .unwrap_or_else(|error| {
            panic!(
                "reading the sources of {} {} failed: {error}. Run cargo fetch --locked.",
                package.name, package.version
            )
        })
        .map(|entry| entry.expect("reading a directory entry failed").path())
        .filter(|path| path.is_file())
        .collect();
    files.sort_by_key(|path| file_name(path));
    files
}

fn license_files(package: &Package) -> Vec<PathBuf> {
    let mut files: Vec<_> = package
        .license_file
        .iter()
        .filter(|path| path.is_file())
        .cloned()
        .collect();
    for path in top_level_files(package) {
        let name = file_name(&path).to_lowercase();
        if LICENSE_FILE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
            && !files.contains(&path)
        {
            files.push(path);
        }
    }
    files
}

fn notice_files(package: &Package) -> Vec<PathBuf> {
    let mut files: Vec<_> = top_level_files(package)
        .into_iter()
        .filter(|path| file_name(path).to_lowercase().contains("notice"))
        .collect();
    files.extend(license_files(package).into_iter().filter(|path| {
        let name = file_name(path).to_lowercase();
        let text = read_text(path);
        !OTHER_LICENSE_NAMES
            .iter()
            .any(|marker| name.contains(marker))
            && !NAME_MARKERS.iter().any(|(_, marker)| name.contains(marker))
            && text.to_lowercase().contains("copyright")
            && detect_licenses(&name, &text).is_empty()
    }));
    if let Some(directory) = &package.directory {
        for (name, file) in EXTRA_NOTICE_FILES {
            let path = directory.join(file);
            if package.name == name && !files.contains(&path) {
                assert!(
                    path.is_file(),
                    "{} has no {file}; check EXTRA_NOTICE_FILES",
                    label(package)
                );
                files.push(path);
            }
        }
    }
    files
}

fn relative_path(package: &Package, path: &Path) -> String {
    package
        .directory
        .as_ref()
        .and_then(|directory| path.strip_prefix(directory).ok())
        .map(|relative| {
            relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_else(|| file_name(path))
}

fn mit_text(package: &Package) -> String {
    let authors: Vec<_> = package
        .authors
        .iter()
        .map(|author| {
            author
                .split_once('<')
                .map_or(author.as_str(), |(name, _)| name)
                .trim()
                .to_string()
        })
        .filter(|author| !author.is_empty())
        .collect();
    let copyright = if authors.is_empty() {
        format!("The {} authors", package.name)
    } else {
        authors.join(", ")
    };
    MIT_TEMPLATE.replace("{copyright}", &copyright)
}

fn find_text(package: &Package, license: &str) -> Option<String> {
    let wanted = base_license(license);
    let found = license_files(package).into_iter().find_map(|path| {
        let text = read_text(&path);
        detect_licenses(&file_name(&path), &text)
            .contains(wanted)
            .then_some(text)
    });
    found.or_else(|| (wanted == "MIT").then(|| mit_text(package)))
}

// Cargo

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cargo(arguments: &[&str]) -> String {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(&cargo)
        .args(arguments)
        .current_dir(root())
        .output()
        .unwrap_or_else(|error| panic!("running {cargo} failed: {error}"));
    assert!(
        output.status.success(),
        "cargo {} failed:\n{}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("cargo writes UTF-8")
}

fn target_arguments(flag: &'static str) -> Vec<&'static str> {
    TARGETS.iter().flat_map(|target| [flag, *target]).collect()
}

fn shipped_crates() -> BTreeSet<(String, String)> {
    let mut arguments = vec![
        "tree",
        "--workspace",
        "--locked",
        "--edges",
        "normal,no-proc-macro",
        "--prefix",
        "none",
        "--format",
        "{p}",
    ];
    arguments.extend(target_arguments("--target"));
    cargo(&arguments)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let name = words.next()?;
            let version = words.next()?.strip_prefix('v')?;
            Some((name.to_string(), version.to_string()))
        })
        .collect()
}

fn registry_packages() -> BTreeMap<(String, String), Package> {
    let mut arguments = vec!["metadata", "--format-version", "1", "--locked"];
    arguments.extend(target_arguments("--filter-platform"));
    let metadata = Json::parse(&cargo(&arguments)).expect("cargo metadata returned invalid JSON");
    metadata
        .get("packages")
        .map(Json::as_array)
        .unwrap_or_default()
        .iter()
        .filter(|package| package.text("source").is_some())
        .map(|package| {
            let name = package.text("name").expect("a package has no name");
            let version = package.text("version").expect("a package has no version");
            let directory = package
                .text("manifest_path")
                .map(PathBuf::from)
                .and_then(|path| path.parent().map(Path::to_path_buf));
            let license_file = package
                .text("license_file")
                .zip(directory.as_ref())
                .map(|(file, directory)| directory.join(file));
            let repository = package
                .text("repository")
                .unwrap_or_else(|| format!("https://crates.io/crates/{name}"));
            let authors = package
                .get("authors")
                .map(Json::as_array)
                .unwrap_or_default()
                .iter()
                .filter_map(Json::as_str)
                .map(str::to_string)
                .collect();
            let package = Package {
                license: package.text("license").unwrap_or_default(),
                name: name.clone(),
                version: version.clone(),
                repository,
                authors,
                directory,
                license_file,
            };
            ((name, version), package)
        })
        .collect()
}

fn rust_standard_library() -> Package {
    Package {
        name: "Rust standard library".to_string(),
        version: String::new(),
        license: "MIT OR Apache-2.0".to_string(),
        repository: "https://github.com/rust-lang/rust".to_string(),
        authors: vec!["The Rust Project Contributors".to_string()],
        directory: None,
        license_file: None,
    }
}

fn shipped_packages() -> Vec<Package> {
    let registry = registry_packages();
    let local: BTreeSet<_> = ["hoplix-core", "hoplix-app", "hoplix-cli"]
        .into_iter()
        .collect();
    let mut packages: Vec<_> = shipped_crates()
        .into_iter()
        .filter(|(name, _)| !local.contains(name.as_str()))
        .map(|key| {
            registry
                .get(&key)
                .unwrap_or_else(|| panic!("cargo metadata does not list {} {}", key.0, key.1))
                .clone()
        })
        .collect();
    packages.push(rust_standard_library());
    packages.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    packages
}

// Report

fn label(package: &Package) -> String {
    format!("{} {}", package.name, package.version)
        .trim()
        .to_string()
}

fn build_report(packages: Vec<Package>) -> Result<Report, String> {
    let mut sections: Vec<Section> = Vec::new();
    let mut notices: Vec<Notice> = Vec::new();
    let mut chosen_licenses = Vec::new();
    let mut missing = Vec::new();
    let mut errors = Vec::new();
    for package in &packages {
        let chosen = if package.license.is_empty() {
            Err("no license declared".to_string())
        } else {
            choose_licenses(&package.license)
        };
        let chosen = match chosen {
            Ok(chosen) => chosen,
            Err(error) => {
                errors.push(format!("{}: {error}", label(package)));
                chosen_licenses.push(Vec::new());
                continue;
            }
        };
        for license in &chosen {
            match find_text(package, license) {
                Some(text) => add_to_section(&mut sections, license, text, label(package)),
                None => missing.push((package, license.clone())),
            }
        }
        for path in notice_files(package) {
            let source = format!("{} ({})", label(package), relative_path(package, &path));
            let text = read_text(&path);
            match notices
                .iter_mut()
                .find(|notice| same_text(&notice.text, &text))
            {
                Some(notice) => notice.sources.push(source),
                None => notices.push(Notice {
                    text,
                    sources: vec![source],
                }),
            }
        }
        chosen_licenses.push(chosen);
    }
    for (package, license) in missing {
        if NO_NOTICE_LICENSES.contains(&license.as_str()) {
            let text = NO_NOTICE_TEMPLATE.replace("{license}", &license);
            add_to_section(&mut sections, &license, text, label(package));
            continue;
        }
        let shared = sections
            .iter_mut()
            .filter(|section| section.license == license)
            .max_by_key(|section| section.packages.len());
        match shared {
            Some(section) if license == "Apache-2.0" => section.packages.push(label(package)),
            _ => errors.push(format!(
                "{}: no {license} license text found",
                label(package)
            )),
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    for section in &mut sections {
        section.packages.sort();
        section.packages.dedup();
    }
    sections.sort_by(|a, b| {
        (preference(&a.license), &a.packages[0]).cmp(&(preference(&b.license), &b.packages[0]))
    });
    Ok(Report {
        packages: packages.into_iter().zip(chosen_licenses).collect(),
        sections,
        notices,
    })
}

fn same_text(first: &str, second: &str) -> bool {
    collapse_whitespace(first) == collapse_whitespace(second)
}

fn add_to_section(sections: &mut Vec<Section>, license: &str, text: String, package: String) {
    match sections
        .iter_mut()
        .find(|section| section.license == license && same_text(&section.text, &text))
    {
        Some(section) => section.packages.push(package),
        None => sections.push(Section {
            license: license.to_string(),
            text,
            packages: vec![package],
        }),
    }
}

fn wrap(prefix: &str, items: &[String]) -> Vec<String> {
    let indent = " ".repeat(prefix.len());
    let mut lines = Vec::new();
    let mut line = String::new();
    for (index, item) in items.iter().enumerate() {
        let separator = if index + 1 < items.len() { "," } else { "" };
        let item = format!("{item}{separator}");
        if !line.is_empty() && prefix.len() + 1 + line.len() + 1 + item.len() > RULE_WIDTH {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&item);
    }
    lines.push(line);
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let lead = if index == 0 { prefix } else { indent.as_str() };
            format!("{lead} {line}")
        })
        .collect()
}

fn render(report: &Report) -> String {
    let rule = "-".repeat(RULE_WIDTH);
    let mut lines: Vec<String> = HEADER.lines().map(str::to_string).collect();
    lines.extend([
        String::new(),
        "Components".to_string(),
        "=".repeat(10),
        String::new(),
    ]);
    for (package, chosen) in &report.packages {
        lines.push(label(package));
        lines.push(format!("  License:    {}", package.license));
        lines.push(format!("  Used under: {}", chosen.join(" AND ")));
        lines.push(format!("  Source:     {}", package.repository));
    }
    lines.extend([String::new(), "License texts".to_string(), "=".repeat(13)]);
    for section in &report.sections {
        lines.extend([String::new(), rule.clone()]);
        lines.push(format!("License: {}", section.license));
        lines.extend(wrap("Used by:", &section.packages));
        lines.push(String::new());
        lines.extend(section.text.lines().map(str::to_string));
    }
    if !report.notices.is_empty() {
        lines.extend([String::new(), "Notices".to_string(), "=".repeat(7)]);
        for notice in &report.notices {
            lines.extend([String::new(), rule.clone()]);
            lines.extend(wrap("From:", &notice.sources));
            lines.push(String::new());
            lines.extend(notice.text.lines().map(str::to_string));
        }
    }
    format!("{}\n", lines.join("\n").trim_end())
}

// Tests

#[test]
fn third_party_licenses_are_up_to_date() {
    let report = build_report(shipped_packages())
        .unwrap_or_else(|error| panic!("the license check failed:\n{error}"));
    let expected = render(&report);
    let path = root().join(DOCUMENT);
    if env::var(UPDATE_VARIABLE).is_ok_and(|value| value == "1") {
        fs::write(&path, &expected).expect("writing THIRD-PARTY-LICENSES.txt failed");
    }
    let actual = fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!("{DOCUMENT} is missing. Run the tests with {UPDATE_VARIABLE}=1.")
        })
        .replace("\r\n", "\n");
    assert!(
        actual == expected,
        "{DOCUMENT} is outdated. Run the tests with {UPDATE_VARIABLE}=1."
    );
}

#[test]
fn packages_contain_the_third_party_licenses() {
    let installer =
        fs::read_to_string(root().join(INSTALLER)).expect("reading the installer failed");
    assert!(
        installer.contains(&format!("Source: \"..\\{DOCUMENT}\"; DestDir: \"{{app}}\"")),
        "{INSTALLER} does not install {DOCUMENT}"
    );
    let workflow = fs::read_to_string(root().join(RELEASE_WORKFLOW))
        .expect("reading the release workflow failed");
    for expected in [
        format!("cp LICENSE {DOCUMENT} README.md CHANGELOG.md \"dist/$name/\""),
        format!("for file in LICENSE {DOCUMENT} README.md CHANGELOG.md; do"),
        "--test third_party_licenses".to_string(),
    ] {
        assert!(
            workflow.contains(&expected),
            "{RELEASE_WORKFLOW} lacks {expected}"
        );
    }
}

#[test]
fn json_is_parsed() {
    let json = Json::parse(r#" {"a": [1, -2.5e3, true, null], "b": "x\"é😀\n", "c": {}} "#)
        .expect("valid JSON");
    assert_eq!(
        json.get("a"),
        Some(&Json::Array(vec![
            Json::Number("1".to_string()),
            Json::Number("-2.5e3".to_string()),
            Json::Bool(true),
            Json::Null,
        ]))
    );
    assert_eq!(json.get("b").and_then(Json::as_str), Some("x\"é😀\n"));
    assert_eq!(json.get("c"), Some(&Json::Object(BTreeMap::new())));
    assert!(Json::parse("[1,").is_err());
    assert!(Json::parse("{} x").is_err());
}

#[test]
fn preferred_licenses_are_chosen() {
    let cases: [(&str, &[&str]); 7] = [
        ("MIT OR Apache-2.0", &["MIT"]),
        ("Apache-2.0/MIT", &["MIT"]),
        ("Apache-2.0 OR GPL-2.0-only", &["Apache-2.0"]),
        ("Apache-2.0 AND ISC", &["Apache-2.0", "ISC"]),
        (
            "(MIT OR Apache-2.0) AND Unicode-3.0",
            &["MIT", "Unicode-3.0"],
        ),
        (
            "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
            &["MIT"],
        ),
        ("Unlicense OR MIT", &["MIT"]),
    ];
    for (expression, expected) in cases {
        assert_eq!(
            choose_licenses(expression).as_deref(),
            Ok(expected
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
                .as_slice()),
            "{expression}"
        );
    }
    assert!(choose_licenses("GPL-3.0-only").is_err());
    assert!(choose_licenses("(MIT").is_err());
}

#[test]
fn license_files_are_detected() {
    let mit = MIT_TEMPLATE.replace("{copyright}", "Someone");
    assert_eq!(
        detect_licenses("LICENSE-MIT", &mit),
        BTreeSet::from(["MIT"])
    );
    assert_eq!(
        detect_licenses("LICENSE", "Apache License\nVersion 2.0"),
        BTreeSet::from(["Apache-2.0"])
    );
    assert!(detect_licenses("LICENSE", "See LICENSE-other-bits.").is_empty());
    assert_eq!(normalize("\u{feff}a  \r\nb\r\n\r\n"), "a\nb\n");
}

#[test]
fn package_lists_are_wrapped() {
    let items: Vec<_> = (0..12)
        .map(|index| format!("crate-{index} 1.0.0"))
        .collect();
    let lines = wrap("Used by:", &items);
    assert!(lines.len() > 1);
    assert!(lines.iter().all(|line| line.len() <= RULE_WIDTH));
    assert!(lines[0].starts_with("Used by: crate-0 1.0.0,"));
    assert!(lines[1].starts_with("         crate-"));
}
