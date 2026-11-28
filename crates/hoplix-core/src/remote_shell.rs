use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Entry, EntryKind, Error, ErrorKind, Properties, parse_permission_text};

// Constants

pub(crate) const LOCALE: &str = "LC_ALL=C; export LC_ALL; ";
const SECONDS_PER_DAY: u64 = 86_400;

// Types

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CommandOutput {
    pub status: Option<u32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listed {
    pub name: String,
    pub kind: char,
    pub mode: Option<u32>,
    pub owner: String,
    pub group: String,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

// Quoting

pub(crate) fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for character in text.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

pub(crate) fn directory_argument(path: &str) -> String {
    if path.ends_with('/') {
        quote(path)
    } else {
        quote(&format!("{path}/"))
    }
}

pub(crate) fn entry_argument(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    quote(if trimmed.is_empty() { "/" } else { trimmed })
}

// Commands

pub(crate) fn list_command(path: &str, follow: bool) -> String {
    let flags = if follow { "-lanbL" } else { "-lanb" };
    format!(
        "{LOCALE}ls {flags} --time-style=+%s -- {}",
        directory_argument(path)
    )
}

pub(crate) fn stat_command(path: &str, follow: bool) -> String {
    let flags = if follow { "-ldnbL" } else { "-ldnb" };
    format!(
        "{LOCALE}ls {flags} --time-style=+%s -- {}",
        entry_argument(path)
    )
}

pub(crate) fn rename_command(from: &str, to: &str) -> String {
    let target = entry_argument(to);
    format!(
        "{LOCALE}if [ -e {target} ] || [ -L {target} ]; then echo 'File exists' >&2; exit 1; fi; mv -- {} {target}",
        entry_argument(from)
    )
}

pub(crate) fn touch_command(path: &str, modified: SystemTime) -> Option<String> {
    let seconds = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(format!(
        "{LOCALE}TZ=UTC0 touch -m -t {} -- {}",
        touch_time(seconds),
        entry_argument(path)
    ))
}

pub(crate) fn simple_command(program: &str, path: &str) -> String {
    format!("{LOCALE}{program} -- {}", entry_argument(path))
}

// Times

fn touch_time(seconds: u64) -> String {
    let days = seconds / SECONDS_PER_DAY;
    let rest = seconds % SECONDS_PER_DAY;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}.{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

// Listings

pub(crate) fn unescape(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte != b'\\' || index + 1 >= bytes.len() {
            result.push(byte);
            index += 1;
            continue;
        }
        let next = bytes[index + 1];
        let octal = bytes
            .get(index + 1..index + 4)
            .filter(|digits| digits.iter().all(|digit| (b'0'..=b'7').contains(digit)));
        if let Some(digits) = octal {
            let value = digits
                .iter()
                .fold(0u32, |value, digit| value * 8 + u32::from(digit - b'0'));
            result.push(u8::try_from(value).unwrap_or(b'?'));
            index += 4;
            continue;
        }
        result.push(match next {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'v' => 0x0b,
            other => other,
        });
        index += 2;
    }
    result
}

fn split_fields(line: &str, count: usize) -> Option<(Vec<&str>, &str)> {
    let mut fields = Vec::with_capacity(count);
    let mut rest = line;
    for _ in 0..count {
        rest = rest.trim_start_matches(' ');
        let end = rest.find(' ').unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    Some((fields, rest.strip_prefix(' ')?))
}

fn separate_target(name: &str) -> &str {
    let bytes = name.as_bytes();
    let mut index = 0;
    while index + 4 <= bytes.len() {
        if bytes[index] == b'\\' {
            index += 2;
            continue;
        }
        if &bytes[index..index + 4] == b" -> " {
            return &name[..index];
        }
        index += 1;
    }
    name
}

pub(crate) fn parse_line(line: &str) -> Option<Listed> {
    let line = line.trim_end_matches('\r');
    let (head, _) = split_fields(line, 1)?;
    let permissions = head[0];
    let kind = permissions.chars().next()?;
    let device = matches!(kind, 'c' | 'b');
    let count = if device { 7 } else { 6 };
    let (fields, rest) = split_fields(line, count)?;
    let mode = permissions
        .get(1..10)
        .and_then(parse_permission_text)
        .map(|mode| mode | type_bits(kind));
    let size = if device { None } else { fields[4].parse().ok() };
    let modified = fields[count - 1]
        .parse::<i64>()
        .ok()
        .and_then(|seconds| u64::try_from(seconds).ok())
        .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds));
    let name_text = if kind == 'l' {
        separate_target(rest)
    } else {
        rest
    };
    let name = String::from_utf8_lossy(&unescape(name_text)).into_owned();
    if name.is_empty() {
        return None;
    }
    Some(Listed {
        name,
        kind,
        mode,
        owner: fields[2].to_string(),
        group: fields[3].to_string(),
        size,
        modified,
    })
}

fn type_bits(kind: char) -> u32 {
    match kind {
        'd' => 0o040_000,
        'l' => 0o120_000,
        '-' => 0o100_000,
        _ => 0,
    }
}

pub(crate) fn parse_listing(output: &[u8]) -> Vec<Listed> {
    String::from_utf8_lossy(output)
        .lines()
        .filter(|line| !line.starts_with("total "))
        .filter_map(parse_line)
        .filter(|listed| listed.name != "." && listed.name != "..")
        .collect()
}

impl Listed {
    pub fn is_link(&self) -> bool {
        self.kind == 'l'
    }

    pub fn entry(&self, followed: Option<&Self>, name: &str) -> Entry {
        let resolved = followed.unwrap_or(self);
        let is_directory = resolved.kind == 'd';
        Entry {
            name: name.to_string(),
            kind: if is_directory {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            size: resolved.size.filter(|_| !is_directory),
            modified: resolved.modified,
            is_link: self.is_link(),
            permissions: resolved.mode.map(|mode| mode & crate::PERMISSION_BITS),
            owner: Some(resolved.owner.clone()),
        }
    }

    pub fn properties(&self, followed: Option<&Self>, name: &str) -> Properties {
        let resolved = followed.unwrap_or(self);
        let mut properties = Properties::from_entry(self.entry(followed, name));
        properties.permissions = resolved.mode.map(|mode| mode & crate::PERMISSION_BITS);
        properties.owner = Some(resolved.owner.clone());
        properties.group = Some(resolved.group.clone());
        properties
    }
}

// Errors

pub(crate) fn error_kind(message: &str) -> ErrorKind {
    const KINDS: [(&str, ErrorKind); 7] = [
        ("No such file or directory", ErrorKind::NotFound),
        ("Permission denied", ErrorKind::PermissionDenied),
        ("Operation not permitted", ErrorKind::PermissionDenied),
        ("Not a directory", ErrorKind::NotADirectory),
        ("File exists", ErrorKind::AlreadyExists),
        ("Directory not empty", ErrorKind::DirectoryNotEmpty),
        ("not found", ErrorKind::Unsupported),
    ];
    KINDS
        .iter()
        .find(|(text, _)| message.contains(text))
        .map_or(ErrorKind::Other, |(_, kind)| *kind)
}

pub(crate) fn message_error(message: &str, path: &str) -> Error {
    let message = message.trim();
    let kind = error_kind(message);
    match kind {
        ErrorKind::Other => Error::new(kind, format!("{path}: {message}")),
        ErrorKind::Unsupported => Error::new(kind, format!("{path}: {message}")),
        _ => Error::new(kind, path),
    }
}

pub(crate) fn io_error(message: &str) -> io::Error {
    let kind = match error_kind(message) {
        ErrorKind::NotFound => io::ErrorKind::NotFound,
        ErrorKind::PermissionDenied => io::ErrorKind::PermissionDenied,
        ErrorKind::AlreadyExists => io::ErrorKind::AlreadyExists,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, message.trim().to_string())
}

impl CommandOutput {
    pub fn succeeded(&self) -> bool {
        self.status == Some(0)
    }

    pub fn error(&self, path: &str) -> Error {
        let message = String::from_utf8_lossy(&self.stderr);
        let message = message.lines().find(|line| !line.trim().is_empty());
        match message {
            Some(message) => message_error(message, path),
            None => Error::new(
                ErrorKind::Other,
                format!(
                    "{path}: exit status {}",
                    self.status
                        .map_or_else(|| "?".to_string(), |s| s.to_string())
                ),
            ),
        }
    }

    pub fn check(self, path: &str) -> crate::Result<Self> {
        if self.succeeded() {
            Ok(self)
        } else {
            Err(self.error(path))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Quoting

    #[test]
    fn quoting_survives_the_shell() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("with space"), "'with space'");
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote("''"), "''\\'''\\'''");
        assert_eq!(quote("a\nb"), "'a\nb'");
        assert_eq!(
            quote("$HOME `id` $(id) \\ \" ; | & * ?"),
            "'$HOME `id` $(id) \\ \" ; | & * ?'"
        );
        assert_eq!(quote("-rf"), "'-rf'");
        assert_eq!(quote(""), "''");
        for text in ["x'; rm -rf / #", "'", "a''b", "\n'\n"] {
            let quoted = quote(text);
            let inner = &quoted[1..quoted.len() - 1];
            assert_eq!(inner.replace("'\\''", "'"), text);
            assert!(!inner.replace("'\\''", "").contains('\''), "{quoted}");
        }
    }

    #[test]
    fn commands_quote_every_path() {
        assert_eq!(
            list_command("/srv/a b", false),
            "LC_ALL=C; export LC_ALL; ls -lanb --time-style=+%s -- '/srv/a b/'"
        );
        assert_eq!(
            list_command("/", true),
            "LC_ALL=C; export LC_ALL; ls -lanbL --time-style=+%s -- '/'"
        );
        assert_eq!(
            stat_command("/srv/x/", false),
            "LC_ALL=C; export LC_ALL; ls -ldnb --time-style=+%s -- '/srv/x'"
        );
        assert_eq!(
            simple_command("rmdir", "/tmp/it's"),
            "LC_ALL=C; export LC_ALL; rmdir -- '/tmp/it'\\''s'"
        );
        assert_eq!(
            rename_command("/a", "/b $x"),
            "LC_ALL=C; export LC_ALL; if [ -e '/b $x' ] || [ -L '/b $x' ]; then echo 'File exists' >&2; exit 1; fi; mv -- '/a' '/b $x'"
        );
        let time = UNIX_EPOCH + Duration::from_secs(1_773_579_600);
        assert_eq!(
            touch_command("/f", time).expect("no command"),
            "LC_ALL=C; export LC_ALL; TZ=UTC0 touch -m -t 202603151300.00 -- '/f'"
        );
    }

    #[test]
    fn dates_convert_to_the_calendar() {
        assert_eq!(touch_time(0), "197001010000.00");
        assert_eq!(touch_time(951_782_400), "200002290000.00");
        assert_eq!(touch_time(1_773_579_600 + 59), "202603151300.59");
        assert_eq!(touch_time(4_107_542_399), "210002282359.59");
    }

    // Listings

    #[test]
    fn listings_are_parsed_with_escapes() {
        let output = b"total 16\n\
drwxr-xr-x 4 1000 1000 4096 1773579600 .\n\
drwxr-xr-x 3 0 0 4096 1773579600 ..\n\
-rw-r--r-- 1 1000 33 12 1773579600 a\\ b.txt\n\
-rw-r----- 1 1000 1000 0 1773579600 Gr\\303\\274\\303\\237e\\nzwei\n\
drwxr-sr-x 2 1000 1000 4096 1773576000 dir\n\
lrwxrwxrwx 1 1000 1000 11 1773579600 link\\ ->\\ x -> target\\ y\n\
-rw-r--r-- 1 1000 1000 7 1773579600 it's\\\\$`q`\"\n\
crw-rw-rw- 1 0 0 1, 3 1773579600 null\n\
-rwsr-xr-t 1 0 0 5 -60 old\n";
        let listed = parse_listing(output);
        let names: Vec<&str> = listed.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "a b.txt",
                "Grüße\nzwei",
                "dir",
                "link -> x",
                "it's\\$`q`\"",
                "null",
                "old"
            ]
        );
        assert_eq!(listed[0].size, Some(12));
        assert_eq!(listed[0].mode, Some(0o100_644));
        assert_eq!(listed[0].group, "33");
        assert_eq!(
            listed[0].modified,
            Some(UNIX_EPOCH + Duration::from_secs(1_773_579_600))
        );
        assert_eq!(listed[2].kind, 'd');
        assert_eq!(listed[2].mode, Some(0o042_755));
        assert!(listed[3].is_link());
        assert_eq!(listed[5].size, None);
        assert_eq!(listed[6].mode, Some(0o105_755));
        assert_eq!(listed[6].modified, None);

        let directory = listed[2].entry(None, "dir");
        assert!(directory.is_directory() && directory.size.is_none());
        let link = listed[3].entry(Some(&listed[2]), "link -> x");
        assert!(link.is_directory() && link.is_link);
        assert_eq!(directory.permissions, Some(0o2755));
        assert_eq!(directory.owner.as_deref(), Some("1000"));
        let properties = listed[0].properties(None, "a b.txt");
        assert_eq!(properties.permissions, Some(0o644));
        assert_eq!(properties.owner.as_deref(), Some("1000"));
    }

    #[test]
    fn broken_lines_are_skipped() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("-rw-r--r-- 1 0 0 12"), None);
        assert_eq!(parse_line("-rw-r--r-- 1 0 0 12 1773579600 "), None);
        assert_eq!(unescape("trailing\\"), b"trailing\\");
        assert_eq!(unescape("\\377\\t\\q"), b"\xff\tq");
    }

    // Errors

    #[test]
    fn messages_map_to_error_kinds() {
        let cases = [
            (
                "ls: cannot access '/x/': No such file or directory",
                ErrorKind::NotFound,
            ),
            (
                "mkdir: cannot create directory '/x': File exists",
                ErrorKind::AlreadyExists,
            ),
            (
                "rmdir: failed to remove '/x': Directory not empty",
                ErrorKind::DirectoryNotEmpty,
            ),
            (
                "scp: /root/x: Permission denied",
                ErrorKind::PermissionDenied,
            ),
            (
                "ls: cannot access '/f/': Not a directory",
                ErrorKind::NotADirectory,
            ),
            ("sh: 1: scp: not found", ErrorKind::Unsupported),
            ("something odd", ErrorKind::Other),
        ];
        for (message, kind) in cases {
            assert_eq!(message_error(message, "/x").kind(), kind, "{message}");
        }
        assert_eq!(message_error("odd\n", "/x").detail(), "/x: odd");
        assert_eq!(
            io_error("scp: /x: No such file or directory").kind(),
            io::ErrorKind::NotFound
        );

        let output = CommandOutput {
            status: Some(2),
            stdout: Vec::new(),
            stderr: b"\nls: cannot access '/x/': Permission denied\n".to_vec(),
        };
        assert_eq!(output.error("/x").kind(), ErrorKind::PermissionDenied);
        let silent = CommandOutput {
            status: Some(3),
            ..CommandOutput::default()
        };
        assert_eq!(
            silent.clone().check("/x").expect_err("ok").detail(),
            "/x: exit status 3"
        );
        let done = CommandOutput {
            status: Some(0),
            ..CommandOutput::default()
        };
        assert!(done.check("/x").is_ok());
    }
}
