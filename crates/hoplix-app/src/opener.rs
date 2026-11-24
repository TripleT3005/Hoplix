use std::path::Path;
use std::process::{Child, Command};

use hoplix_core::Error;

// Opening

pub fn open(path: &Path, editor: &str) -> Result<(), Error> {
    let editor = editor.trim().trim_matches('"').trim();
    if editor.is_empty() {
        open_with_default(path)
    } else {
        spawn(Command::new(editor).arg(path), editor)
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

    // Opening

    #[test]
    fn missing_editors_are_reported() {
        let error = open(
            Path::new("notes.txt"),
            " \"hoplix-missing-editor-program\" ",
        )
        .expect_err("a missing editor started");
        assert_eq!(error.kind(), hoplix_core::ErrorKind::NotFound);
        assert_eq!(error.detail(), "hoplix-missing-editor-program");
    }
}
