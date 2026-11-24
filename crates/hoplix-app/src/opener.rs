use std::ffi::OsString;
use std::path::Path;
use std::process::{Child, Command};

use hoplix_core::Error;

// Constants

const FILE_PLACEHOLDER: &str = "%1";

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCommand {
    pub program: String,
    pub arguments: Vec<String>,
}

// Command line

impl EditorCommand {
    pub fn parse(line: &str, is_file: impl Fn(&str) -> bool) -> Option<Self> {
        let (program, rest) = split_program(line.trim(), is_file)?;
        Some(Self {
            program,
            arguments: split_arguments(rest),
        })
    }

    pub fn file_arguments(&self, path: &Path) -> Vec<OsString> {
        let mut arguments: Vec<OsString> = self
            .arguments
            .iter()
            .map(|argument| substitute(argument, path))
            .collect();
        if !self
            .arguments
            .iter()
            .any(|argument| argument.contains(FILE_PLACEHOLDER))
        {
            arguments.push(path.into());
        }
        arguments
    }
}

fn split_program(line: &str, is_file: impl Fn(&str) -> bool) -> Option<(String, &str)> {
    let (program, rest) = if let Some(quoted) = line.strip_prefix('"') {
        quoted.split_once('"').unwrap_or((quoted, ""))
    } else {
        let boundaries = line
            .char_indices()
            .filter(|(_, character)| character.is_whitespace())
            .map(|(index, _)| index);
        let first = boundaries.clone().next().unwrap_or(line.len());
        let end = boundaries
            .chain(std::iter::once(line.len()))
            .find(|&end| is_file(&line[..end]))
            .unwrap_or(first);
        line.split_at(end)
    };
    let program = program.trim();
    (!program.is_empty()).then(|| (program.to_string(), rest))
}

fn split_arguments(text: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    for character in text.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            character if character.is_whitespace() && !quoted => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            character => {
                current.push(character);
                started = true;
            }
        }
    }
    if started {
        arguments.push(current);
    }
    arguments
}

fn substitute(argument: &str, path: &Path) -> OsString {
    let mut result = OsString::new();
    for (index, part) in argument.split(FILE_PLACEHOLDER).enumerate() {
        if index > 0 {
            result.push(path);
        }
        result.push(part);
    }
    result
}

fn is_absolute_file(candidate: &str) -> bool {
    let path = Path::new(candidate);
    path.is_absolute() && path.is_file()
}

// Opening

pub fn open(path: &Path, editor: &str) -> Result<(), Error> {
    match EditorCommand::parse(editor, is_absolute_file) {
        Some(command) => spawn(
            Command::new(&command.program).args(command.file_arguments(path)),
            &command.program,
        ),
        None => open_with_default(path),
    }
}

fn spawn(command: &mut Command, program: &str) -> Result<(), Error> {
    let child = command
        .spawn()
        .map_err(|error| Error::from_io(&error, program))?;
    reap(child);
    Ok(())
}

#[cfg(unix)]
fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

#[cfg(not(unix))]
fn reap(_child: Child) {}

// Platform

#[cfg(windows)]
#[allow(unsafe_code)]
fn open_with_default(path: &Path) -> Result<(), Error> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::{SE_ERR_NOASSOC, ShellExecuteW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    let file = wide(path.as_os_str());
    let execute = |verb: Option<&str>| {
        let verb = verb.map(|verb| wide(OsStr::new(verb)));
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ref().map_or(std::ptr::null(), |verb| verb.as_ptr()),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        result as usize
    };
    let mut code = execute(None);
    if code == SE_ERR_NOASSOC as usize {
        code = execute(Some("openas"));
    }
    if code > 32 {
        Ok(())
    } else {
        Err(Error::new(
            hoplix_core::ErrorKind::Other,
            path.to_string_lossy(),
        ))
    }
}

#[cfg(not(windows))]
fn open_with_default(path: &Path) -> Result<(), Error> {
    spawn(Command::new("xdg-open").arg(path), "xdg-open")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Option<EditorCommand> {
        EditorCommand::parse(line, |_| false)
    }

    fn command(program: &str, arguments: &[&str]) -> Option<EditorCommand> {
        Some(EditorCommand {
            program: program.to_string(),
            arguments: arguments.iter().map(ToString::to_string).collect(),
        })
    }

    // Command line

    #[test]
    fn programs_and_arguments_are_split() {
        assert_eq!(parse("code --wait %1"), command("code", &["--wait", "%1"]));
        assert_eq!(parse("  vim  "), command("vim", &[]));
        assert_eq!(
            parse(r#""C:\Program Files\Notepad++\notepad++.exe" -multiInst %1"#),
            command(
                r"C:\Program Files\Notepad++\notepad++.exe",
                &["-multiInst", "%1"]
            )
        );
        assert_eq!(
            parse(r#""C:\Tools\"-n "two words"  "" end"#),
            command(r"C:\Tools\", &["-n", "two words", "", "end"])
        );
        assert_eq!(
            parse(r#"edit --title="My file" %1"#),
            command("edit", &["--title=My file", "%1"])
        );
        assert_eq!(
            parse(r#""unterminated path"#),
            command("unterminated path", &[])
        );
    }

    #[test]
    fn empty_lines_use_the_default_program() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("   "), None);
        assert_eq!(parse(r#""""#), None);
        assert_eq!(parse(r#"" " %1"#), None);
    }

    #[test]
    fn unquoted_paths_with_spaces_are_found() {
        let program = r"C:\Program Files\My Editor\editor.exe";
        let is_file = |candidate: &str| candidate == program;
        assert_eq!(
            EditorCommand::parse(&format!("{program} -n %1"), is_file),
            command(program, &["-n", "%1"])
        );
        assert_eq!(
            EditorCommand::parse(program, is_file),
            command(program, &[])
        );
        assert_eq!(
            EditorCommand::parse(r"C:\Other Place\editor.exe -n", is_file),
            command(r"C:\Other", &[r"Place\editor.exe", "-n"])
        );
    }

    #[test]
    fn the_file_is_passed_as_its_own_argument() {
        let path = Path::new("/tmp/a b; rm -rf ~/x.txt");
        let file = OsString::from(path);
        let arguments = |line: &str| parse(line).expect("no command").file_arguments(path);
        assert_eq!(
            arguments("code --wait %1"),
            [OsString::from("--wait"), file.clone()]
        );
        assert_eq!(
            arguments("code --wait"),
            [OsString::from("--wait"), file.clone()]
        );
        assert_eq!(arguments("code"), [file.clone()]);
        assert_eq!(arguments(r#"code "%1""#), [file.clone()]);
        assert_eq!(
            arguments(r#"edit "--open=%1" -x"#),
            [
                OsString::from("--open=/tmp/a b; rm -rf ~/x.txt"),
                OsString::from("-x")
            ]
        );
        assert_eq!(arguments("diff %1 %1"), [file.clone(), file]);
    }

    // Opening

    #[test]
    fn missing_editors_are_reported() {
        let error = open(
            Path::new("notes.txt"),
            " \"hoplix-missing-editor-program\" --flag %1",
        )
        .expect_err("a missing editor started");
        assert_eq!(error.kind(), hoplix_core::ErrorKind::NotFound);
        assert_eq!(error.detail(), "hoplix-missing-editor-program");
    }
}
