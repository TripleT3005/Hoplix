use std::io::{BufRead, IsTerminal, Write};

use hoplix_core::{BoxFuture, PromptRequest, Prompter};

// Types

pub struct TerminalPrompter;

// Prompting

impl Prompter for TerminalPrompter {
    fn answer(&self, request: PromptRequest) -> BoxFuture<'_, Option<Vec<String>>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || ask(&request))
                .await
                .ok()
                .flatten()
        })
    }
}

pub fn is_available() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub fn heading(request: &PromptRequest) -> Vec<String> {
    let mut lines = vec![if request.jump {
        format!("The jump host {} asks:", request.host)
    } else {
        format!("{} asks:", request.host)
    }];
    lines.extend(
        [&request.name, &request.instruction]
            .into_iter()
            .filter(|text| !text.is_empty())
            .cloned(),
    );
    lines
}

fn ask(request: &PromptRequest) -> Option<Vec<String>> {
    if !is_available() {
        return None;
    }
    let mut output = std::io::stderr();
    for line in heading(request) {
        let _ = writeln!(output, "{line}");
    }
    let mut answers = Vec::with_capacity(request.fields.len());
    for field in &request.fields {
        let label = if field.text.is_empty() {
            "> ".to_string()
        } else if field.text.ends_with(char::is_whitespace) {
            field.text.clone()
        } else {
            format!("{} ", field.text)
        };
        let _ = write!(output, "{label}");
        let _ = output.flush();
        let answer = if field.echo {
            read_line()
        } else {
            let echo = Echo::hide();
            let answer = read_line();
            drop(echo);
            let _ = writeln!(output);
            answer
        };
        answers.push(answer?);
    }
    Some(answers)
}

fn read_line() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
    }
}

// Echo

struct Echo {
    #[cfg(windows)]
    mode: Option<u32>,
    #[cfg(unix)]
    hidden: bool,
}

#[cfg(unix)]
impl Echo {
    fn hide() -> Self {
        let hidden = std::process::Command::new("stty")
            .arg("-echo")
            .stdin(std::process::Stdio::inherit())
            .status()
            .is_ok_and(|status| status.success());
        Self { hidden }
    }
}

#[cfg(unix)]
impl Drop for Echo {
    fn drop(&mut self) {
        if self.hidden {
            let _ = std::process::Command::new("stty")
                .arg("echo")
                .stdin(std::process::Stdio::inherit())
                .status();
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
impl Echo {
    fn hide() -> Self {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
        };
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Self { mode: None };
        }
        if unsafe { SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT) } == 0 {
            return Self { mode: None };
        }
        Self { mode: Some(mode) }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
impl Drop for Echo {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode};
        if let Some(mode) = self.mode {
            let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            unsafe { SetConsoleMode(handle, mode) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hoplix_core::PromptField;

    #[test]
    fn headings_name_the_host_and_the_server_text() {
        let mut request = PromptRequest {
            host: "demo@example.test".to_string(),
            jump: false,
            name: String::new(),
            instruction: "Enter the code from your app".to_string(),
            fields: vec![PromptField {
                text: "Code:".to_string(),
                echo: false,
            }],
        };
        assert_eq!(
            heading(&request),
            ["demo@example.test asks:", "Enter the code from your app"]
        );
        request.jump = true;
        request.name = "otp".to_string();
        assert_eq!(
            heading(&request)[0],
            "The jump host demo@example.test asks:"
        );
        assert_eq!(heading(&request).len(), 3);
    }
}
