use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use hoplix_core::key_value::{self, Section};
use hoplix_core::{BoxFuture, Entry, Error, ErrorKind, FileSystem, Reader, Result, Writer};

use crate::preview::{
    DEFAULT_WIDTH, MAX_WIDTH as PREVIEW_MAX_WIDTH, MIN_WIDTH as PREVIEW_MIN_WIDTH,
};

// Constants

pub const SESSION_FILE: &str = "session.conf";
const TAB_SECTION: &str = "tab";
const MIN_WIDTH: f32 = 640.0;
const MIN_HEIGHT: f32 = 400.0;

// Types

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabOrigin {
    Local,
    Site(String),
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTab {
    pub pane: usize,
    pub origin: TabOrigin,
    pub path: String,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewLayout {
    pub open: bool,
    pub width: f32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Session {
    pub window: Option<WindowState>,
    pub focused_pane: usize,
    pub preview: Option<PreviewLayout>,
    pub tabs: Vec<SavedTab>,
}

#[derive(Debug)]
pub struct Offline {
    name: String,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1100.0,
            height: 700.0,
            x: None,
            y: None,
            maximized: false,
        }
    }
}

// Serialization

impl Session {
    pub fn parse(text: &str) -> Self {
        let sections = key_value::parse_sections(text);
        let mut session = Self::default();
        if let Some(top) = sections.first() {
            session.window = window_from(top);
            session.focused_pane = number(top, "focused_pane").unwrap_or(0);
            session.preview = preview_from(top);
        }
        session.tabs = sections
            .iter()
            .filter(|section| section.name == TAB_SECTION)
            .filter_map(tab_from)
            .collect();
        session
    }

    pub fn to_text(&self) -> String {
        let mut pairs = Vec::new();
        if let Some(window) = &self.window {
            pairs.push(("window.width", format!("{:.0}", window.width)));
            pairs.push(("window.height", format!("{:.0}", window.height)));
            if let (Some(x), Some(y)) = (window.x, window.y) {
                pairs.push(("window.x", format!("{x:.0}")));
                pairs.push(("window.y", format!("{y:.0}")));
            }
            pairs.push(("window.maximized", window.maximized.to_string()));
        }
        pairs.push(("focused_pane", self.focused_pane.to_string()));
        if let Some(preview) = &self.preview {
            pairs.push(("preview.open", preview.open.to_string()));
            pairs.push(("preview.width", format!("{:.0}", preview.width)));
        }
        let mut text = key_value::write(&pairs);
        for tab in &self.tabs {
            let (kind, site) = match &tab.origin {
                TabOrigin::Local => ("local", String::new()),
                TabOrigin::Site(id) => ("site", id.clone()),
                TabOrigin::Direct => continue,
            };
            text.push_str(&format!("\n[{TAB_SECTION}]\n"));
            text.push_str(&key_value::write(&[
                ("pane", tab.pane.to_string()),
                ("kind", kind.to_string()),
                ("site", site),
                ("path", tab.path.clone()),
                ("active", tab.active.to_string()),
            ]));
        }
        text
    }

    // Storage

    pub fn load(directory: &Path) -> Self {
        fs::read_to_string(directory.join(SESSION_FILE))
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn save(&self, directory: &Path) -> io::Result<()> {
        fs::create_dir_all(directory)?;
        let temporary = directory.join(format!("{SESSION_FILE}.tmp"));
        fs::write(&temporary, self.to_text())?;
        fs::rename(temporary, directory.join(SESSION_FILE))
    }
}

// Window

impl WindowState {
    pub fn usable_position(&self) -> Option<(f32, f32)> {
        let (x, y) = (self.x?, self.y?);
        is_on_screen(x, y).then_some((x, y))
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn is_on_screen(x: f32, y: f32) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    let (left, top, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let (left, top) = (left as f32, top as f32);
    let (right, bottom) = (left + width as f32, top + height as f32);
    width > 0
        && height > 0
        && x >= left - 8.0
        && y >= top - 8.0
        && x < right - 80.0
        && y < bottom - 40.0
}

#[cfg(not(windows))]
fn is_on_screen(_x: f32, _y: f32) -> bool {
    false
}

// Offline tabs

impl Offline {
    pub fn shared(name: &str) -> Arc<dyn FileSystem> {
        Arc::new(Self {
            name: name.to_string(),
        })
    }

    fn error(&self) -> Error {
        Error::new(ErrorKind::ConnectionFailed, self.name.clone())
    }
}

impl FileSystem for Offline {
    fn label(&self) -> Option<String> {
        Some(self.name.clone())
    }

    fn home(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async { Err(self.error()) })
    }

    fn list(&self, _path: &str) -> BoxFuture<'_, Result<Vec<Entry>>> {
        Box::pin(async { Err(self.error()) })
    }

    fn metadata(&self, _path: &str) -> BoxFuture<'_, Result<Entry>> {
        Box::pin(async { Err(self.error()) })
    }

    fn create_directory(&self, _path: &str) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(self.error()) })
    }

    fn remove_file(&self, _path: &str) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(self.error()) })
    }

    fn remove_directory(&self, _path: &str) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(self.error()) })
    }

    fn rename(&self, _from: &str, _to: &str) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(self.error()) })
    }

    fn open_read(&self, _path: &str, _offset: u64) -> BoxFuture<'_, Result<Reader>> {
        Box::pin(async { Err(self.error()) })
    }

    fn open_write(&self, _path: &str, _offset: u64) -> BoxFuture<'_, Result<Writer>> {
        Box::pin(async { Err(self.error()) })
    }

    fn join(&self, base: &str, name: &str) -> String {
        format!("{}/{name}", base.trim_end_matches('/'))
    }

    fn parent(&self, _path: &str) -> Option<String> {
        None
    }
}

// Helpers

fn number<T: std::str::FromStr>(section: &Section, key: &str) -> Option<T> {
    section.get(key).and_then(|value| value.parse().ok())
}

fn window_from(section: &Section) -> Option<WindowState> {
    let width: f32 = number(section, "window.width")?;
    let height: f32 = number(section, "window.height")?;
    if !(width.is_finite() && height.is_finite()) {
        return None;
    }
    Some(WindowState {
        width: width.max(MIN_WIDTH),
        height: height.max(MIN_HEIGHT),
        x: number::<f32>(section, "window.x").filter(|x| x.is_finite()),
        y: number::<f32>(section, "window.y").filter(|y| y.is_finite()),
        maximized: section.get("window.maximized") == Some("true"),
    })
}

fn preview_from(section: &Section) -> Option<PreviewLayout> {
    let open = section.get("preview.open")? == "true";
    let width = number::<f32>(section, "preview.width")
        .filter(|width| width.is_finite())
        .map_or(DEFAULT_WIDTH, |width| {
            width.clamp(PREVIEW_MIN_WIDTH, PREVIEW_MAX_WIDTH)
        });
    Some(PreviewLayout { open, width })
}

fn tab_from(section: &Section) -> Option<SavedTab> {
    let origin = match section.get("kind")? {
        "local" => TabOrigin::Local,
        "site" => TabOrigin::Site(section.get("site").filter(|id| !id.is_empty())?.to_string()),
        _ => return None,
    };
    Some(SavedTab {
        pane: number(section, "pane").unwrap_or(0),
        origin,
        path: section.get("path").unwrap_or_default().to_string(),
        active: section.get("active") == Some("true"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_round_trip_without_direct_connections() {
        let session = Session {
            window: Some(WindowState {
                width: 1280.0,
                height: 800.0,
                x: Some(-1910.0),
                y: Some(40.0),
                maximized: true,
            }),
            focused_pane: 1,
            preview: Some(PreviewLayout {
                open: true,
                width: 420.0,
            }),
            tabs: vec![
                SavedTab {
                    pane: 0,
                    origin: TabOrigin::Local,
                    path: "C:\\Data\\new\nline".to_string(),
                    active: false,
                },
                SavedTab {
                    pane: 1,
                    origin: TabOrigin::Direct,
                    path: "/tmp".to_string(),
                    active: false,
                },
                SavedTab {
                    pane: 1,
                    origin: TabOrigin::Site("abc123".to_string()),
                    path: "/var/www".to_string(),
                    active: true,
                },
            ],
        };
        let parsed = Session::parse(&session.to_text());
        assert_eq!(parsed.window, session.window);
        assert_eq!(parsed.focused_pane, 1);
        assert_eq!(parsed.preview, session.preview);
        assert_eq!(parsed.tabs.len(), 2);
        assert_eq!(parsed.tabs[0], session.tabs[0]);
        assert_eq!(parsed.tabs[1], session.tabs[2]);
    }

    #[test]
    fn broken_values_are_ignored() {
        let parsed = Session::parse(
            "window.width = wide\nfocused_pane = x\n[tab]\nkind = ftp\n[tab]\nkind = site\n[tab]\nkind = local\n",
        );
        assert_eq!(parsed.window, None);
        assert_eq!(parsed.focused_pane, 0);
        assert_eq!(parsed.tabs.len(), 1);
        assert_eq!(parsed.tabs[0].origin, TabOrigin::Local);
        let tiny = Session::parse("window.width = 10\nwindow.height = 20\n");
        let window = tiny.window.expect("window missing");
        assert_eq!((window.width, window.height), (MIN_WIDTH, MIN_HEIGHT));
        let preview = Session::parse("preview.open = true\npreview.width = 5\n").preview;
        assert_eq!(
            preview,
            Some(PreviewLayout {
                open: true,
                width: PREVIEW_MIN_WIDTH
            })
        );
        let preview = Session::parse("preview.open = no\npreview.width = wide\n").preview;
        assert_eq!(
            preview,
            Some(PreviewLayout {
                open: false,
                width: DEFAULT_WIDTH
            })
        );
        assert_eq!(parsed.preview, None);
    }

    #[test]
    fn far_away_positions_are_not_used() {
        let window = WindowState {
            x: Some(-32000.0),
            y: Some(-32000.0),
            ..WindowState::default()
        };
        assert_eq!(window.usable_position(), None);
        assert_eq!(WindowState::default().usable_position(), None);
    }

    #[tokio::test]
    async fn offline_tabs_refuse_every_operation() {
        let offline = Offline::shared("Server");
        assert_eq!(offline.label().as_deref(), Some("Server"));
        let error = offline.list("/").await.expect_err("listing worked");
        assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
        assert_eq!(offline.join("/a/", "b"), "/a/b");
    }
}
