mod browser;
mod compare;
mod connect;
mod edit;
mod fakes;
mod file_list;
mod history;
mod palette;
mod queue;
mod routes;
mod secrets;
mod settings;
mod site_manager;
mod sites;
mod updates;

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hoplix_core::{Keyring, TransferEvent};
use iced::keyboard::{self, Key, Modifiers, key};
use iced::widget::scrollable::AbsoluteOffset;
use iced::{Event, Point, Rectangle, Size, mouse, window};
use iced_test::core::renderer::Headless;
use iced_test::core::widget::{self, operation::Outcome};
use iced_test::core::{Font, Pixels, clipboard};
use iced_test::futures::futures::StreamExt;
use iced_test::renderer::Renderer;
use iced_test::runtime::user_interface::{Cache, UserInterface};
use iced_test::runtime::{Action, task};
use iced_test::selector::{Candidate, Selector};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{Hoplix, Message, Page, Services};
use crate::settings::{Layout, Settings};
use crate::transfers::{cursor_event, input_event};
use crate::ui::SETTINGS_SCROLL_ID;

// Constants

const VIEWPORT: Size = Size::new(1280.0, 1000.0);
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: f32 = 1.3;
const PICK_LIST_PADDING: f32 = 10.0;
const LABEL_OFFSET: f32 = 160.0 + 12.0 + 24.0;
const REVEAL_MARGIN: f32 = 120.0;

// Passwords

static PASSWORD_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn test_password() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(PASSWORD_COUNTER.fetch_add(1, Ordering::Relaxed));
    let first = hasher.finish();
    hasher.write_u64(first);
    let second = hasher.finish();
    [first, second]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .map(|nibble| char::from(b'a' + nibble))
        .collect()
}

// Test folders

pub struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("hoplix-ui-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        for folder in ["config", "data", "remote", "temp"] {
            fs::create_dir_all(path.join(folder)).expect("creating the test folders failed");
        }
        Self { path }
    }

    pub fn config(&self) -> PathBuf {
        self.path.join("config")
    }

    pub fn data(&self) -> PathBuf {
        self.path.join("data")
    }

    pub fn remote(&self) -> PathBuf {
        self.path.join("remote")
    }

    pub fn temp(&self) -> PathBuf {
        self.path.join("temp")
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn text_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn write_files(directory: &Path, names: &[&str]) {
    for name in names {
        let path = directory.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("creating a folder failed");
        }
        if name.ends_with('/') {
            fs::create_dir_all(&path).expect("creating a folder failed");
        } else {
            fs::write(&path, name.as_bytes()).expect("writing a file failed");
        }
    }
}

// Harness

pub struct Harness {
    pub app: Hoplix,
    pub root: TestRoot,
    renderer: Renderer,
    cache: Option<Cache>,
    cursor: Point,
    window: window::Id,
    runtime: tokio::runtime::Runtime,
    transfer_events: UnboundedReceiver<TransferEvent>,
    driving: bool,
    pub opened: Arc<Mutex<Vec<(PathBuf, String)>>>,
    pub updates: Arc<fakes::Updates>,
}

impl Harness {
    pub fn new(name: &str) -> Self {
        Self::start(name, Layout::Modern, |_| {})
    }

    pub fn start(name: &str, layout: Layout, prepare: impl FnOnce(&TestRoot)) -> Self {
        Self::launch(name, layout, prepare, fakes::Updates::up_to_date())
    }

    pub fn launch(
        name: &str,
        layout: Layout,
        prepare: impl FnOnce(&TestRoot),
        updates: Arc<fakes::Updates>,
    ) -> Self {
        let root = TestRoot::new(name);
        Settings {
            language: Some("en".to_string()),
            layout,
            ..Settings::default()
        }
        .save(&root.config())
        .expect("saving the test settings failed");
        prepare(&root);
        let opened = Arc::new(Mutex::new(Vec::new()));
        let services = Services {
            keyring: Keyring::in_memory(),
            connector: fakes::connector(root.remote()),
            local: fakes::Folder::local(&root.data()),
            opener: fakes::opener(Arc::clone(&opened)),
            temp_dir: root.temp(),
            update_source: fakes::update_source(Arc::clone(&updates)),
            update_delay: Duration::ZERO,
        };
        let (app, task) = Hoplix::load(Some(root.config()), services);
        let transfer_events = app.transfers.subscribe();
        let renderer = iced_test::futures::futures::executor::block_on(Renderer::new(
            Font::with_name("Fira Sans"),
            Pixels(FONT_SIZE),
            Some("tiny-skia"),
        ))
        .expect("creating the headless renderer failed");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("creating the test runtime failed");
        let mut harness = Self {
            app,
            root,
            renderer,
            cache: None,
            cursor: Point::ORIGIN,
            window: window::Id::unique(),
            runtime,
            transfer_events,
            driving: false,
            opened,
            updates,
        };
        harness.run(task);
        harness.open_window();
        harness
    }

    pub fn restart(&mut self) {
        let services = self.app.services.clone();
        let (app, task) = Hoplix::load(Some(self.root.config()), services);
        self.app = app;
        self.cache = None;
        self.driving = false;
        self.transfer_events = self.app.transfers.subscribe();
        self.run(task);
        self.open_window();
    }

    fn open_window(&mut self) {
        self.send(Message::Window(
            self.window,
            window::Event::Opened {
                position: None,
                size: VIEWPORT,
            },
        ));
    }

    // Messages and tasks

    pub fn send(&mut self, message: Message) {
        let task = self.app.update(message);
        self.run(task);
        self.drain_transfers();
    }

    fn run(&mut self, task: iced::Task<Message>) {
        let Some(mut stream) = task::into_stream(task) else {
            return;
        };
        loop {
            let next = self
                .runtime
                .block_on(async { tokio::time::timeout(STEP_TIMEOUT, stream.next()).await })
                .expect("a task did not finish in time");
            match next {
                None => break,
                Some(Action::Output(message)) => {
                    let task = self.app.update(message);
                    self.run(task);
                }
                Some(Action::Widget(operation)) => self.operate(operation),
                Some(_) => {}
            }
        }
    }

    fn operate(&mut self, mut operation: Box<dyn widget::Operation>) {
        loop {
            let cache = self.cache.take().unwrap_or_default();
            let mut interface =
                UserInterface::build(self.app.view(), VIEWPORT, cache, &mut self.renderer);
            interface.operate(&self.renderer, operation.as_mut());
            self.cache = Some(interface.into_cache());
            match operation.finish() {
                Outcome::Chain(next) => operation = next,
                Outcome::None | Outcome::Some(()) => break,
            }
        }
    }

    // Transfers

    fn drain_transfers(&mut self) {
        while let Ok(event) = self.transfer_events.try_recv() {
            let task = self.app.update(Message::TransferEvent(event));
            self.run(task);
        }
    }

    pub fn drive_transfers_until(&mut self, done: impl Fn(&Hoplix) -> bool) {
        if !self.driving {
            self.runtime.spawn(self.app.transfers.clone().drive());
            self.driving = true;
        }
        while !done(&self.app) {
            let event = self
                .runtime
                .block_on(async {
                    tokio::time::timeout(STEP_TIMEOUT, self.transfer_events.recv()).await
                })
                .expect("the transfers did not finish in time")
                .expect("the transfer queue stopped");
            let task = self.app.update(Message::TransferEvent(event));
            self.run(task);
        }
    }

    // Events

    pub fn event(&mut self, event: Event) {
        let cache = self.cache.take().unwrap_or_default();
        let mut messages = Vec::new();
        let mut interface =
            UserInterface::build(self.app.view(), VIEWPORT, cache, &mut self.renderer);
        let (_, statuses) = interface.update(
            std::slice::from_ref(&event),
            mouse::Cursor::Available(self.cursor),
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        self.cache = Some(interface.into_cache());
        let status = statuses
            .first()
            .copied()
            .unwrap_or(iced::event::Status::Ignored);
        let mut subscribed: Vec<Message> = input_event(event.clone(), status, self.window)
            .into_iter()
            .chain(crate::file_list::mouse_event(
                event.clone(),
                status,
                self.window,
            ))
            .collect();
        if self.app.drag.is_some() {
            subscribed.extend(cursor_event(event, status, self.window));
        }
        for message in messages.into_iter().chain(subscribed) {
            self.send(message);
        }
    }

    pub fn redraw(&mut self) {
        self.event(Event::Window(window::Event::RedrawRequested(
            iced::time::Instant::now(),
        )));
    }

    pub fn move_to(&mut self, position: Point) {
        self.cursor = position;
        self.event(Event::Mouse(mouse::Event::CursorMoved { position }));
    }

    pub fn press(&mut self) {
        self.event(Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        )));
    }

    pub fn release(&mut self) {
        self.event(Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        )));
    }

    pub fn click_at(&mut self, position: Point) {
        self.move_to(position);
        self.press();
        self.release();
    }

    pub fn click(&mut self, text: &str) {
        let position = self.center(text);
        self.click_at(position);
    }

    pub fn click_nth(&mut self, text: &str, index: usize) {
        let bounds = self.find_all(text);
        let target = bounds
            .get(index)
            .unwrap_or_else(|| panic!("{text:?} #{index} not found in {:?}", self.texts()));
        self.click_at(target.center());
    }

    pub fn click_with(&mut self, text: &str, modifiers: Modifiers) {
        let position = self.center(text);
        self.move_to(position);
        self.set_modifiers(modifiers);
        self.press();
        self.release();
        self.set_modifiers(Modifiers::default());
    }

    pub fn double_click(&mut self, text: &str) {
        let position = self.center(text);
        self.click_at(position);
        self.press();
        self.release();
    }

    pub fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.event(Event::Keyboard(keyboard::Event::ModifiersChanged(
            modifiers,
        )));
    }

    pub fn tap(&mut self, key: Key, modifiers: Modifiers) {
        if !modifiers.is_empty() {
            self.set_modifiers(modifiers);
        }
        let text = match &key {
            Key::Character(character) if !modifiers.command() => Some(character.clone()),
            Key::Named(key::Named::Space) => Some(" ".into()),
            _ => None,
        };
        self.event(Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text,
            repeat: false,
        }));
        self.event(Event::Keyboard(keyboard::Event::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
        }));
        if !modifiers.is_empty() {
            self.set_modifiers(Modifiers::default());
        }
    }

    pub fn key(&mut self, named: key::Named) {
        self.tap(Key::Named(named), Modifiers::default());
    }

    pub fn shortcut(&mut self, character: &str, modifiers: Modifiers) {
        self.tap(Key::Character(character.into()), modifiers);
    }

    pub fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.tap(
                Key::Character(character.to_string().as_str().into()),
                Modifiers::default(),
            );
        }
    }

    pub fn fill_at(&mut self, position: Point, text: &str) {
        self.click_at(position);
        self.shortcut("a", Modifiers::CTRL);
        self.key(key::Named::Backspace);
        self.type_text(text);
    }

    pub fn fill(&mut self, label: &str, text: &str) {
        let row = self.bounds(label);
        self.fill_at(Point::new(row.x + LABEL_OFFSET, row.center_y()), text);
    }

    pub fn fill_input(&mut self, index: usize, text: &str) {
        let inputs = self.inputs();
        let input = inputs
            .get(index)
            .unwrap_or_else(|| panic!("input #{index} not found among {}", inputs.len()));
        self.fill_at(input.center(), text);
    }

    pub fn inputs(&mut self) -> Vec<Rectangle> {
        self.search(
            |candidate: Candidate<'_>| match candidate {
                Candidate::TextInput { visible_bounds, .. } => visible_bounds,
                _ => None,
            },
            true,
        )
    }

    pub fn pick(&mut self, label: &str, option: usize) {
        self.pick_of(label, option, 0);
    }

    pub fn pick_of(&mut self, label: &str, option: usize, count: usize) {
        if self.app.page == Page::Settings {
            self.reveal(label);
        }
        let row = self.bounds(label);
        let target = Point::new(row.x + LABEL_OFFSET, row.center_y());
        self.pick_at(target, option, count, FONT_SIZE);
    }

    pub fn pick_at(&mut self, target: Point, option: usize, count: usize, text_size: f32) {
        self.click_at(target);
        let height = text_size * LINE_HEIGHT + PICK_LIST_PADDING;
        let top = target.y - height / 2.0;
        let bottom = target.y + height / 2.0;
        let first = if VIEWPORT.height - bottom > top {
            bottom + 1.0
        } else {
            top - 1.0 - height * count as f32
        };
        let position = Point::new(target.x, first + height * (option as f32 + 0.5));
        self.move_to(position);
        self.press();
        self.release();
    }

    // Queries

    fn search<T: Clone + Send>(
        &mut self,
        selector: impl Selector<Output = T> + Send,
        all: bool,
    ) -> Vec<T> {
        let cache = self.cache.take().unwrap_or_default();
        let mut interface =
            UserInterface::build(self.app.view(), VIEWPORT, cache, &mut self.renderer);
        let found = if all {
            let mut operation = selector.find_all();
            interface.operate(
                &self.renderer,
                &mut widget::operation::black_box(&mut operation),
            );
            match widget::Operation::finish(&operation) {
                Outcome::Some(found) => found,
                _ => Vec::new(),
            }
        } else {
            let mut operation = selector.find();
            interface.operate(
                &self.renderer,
                &mut widget::operation::black_box(&mut operation),
            );
            match widget::Operation::finish(&operation) {
                Outcome::Some(found) => found.into_iter().collect(),
                _ => Vec::new(),
            }
        };
        self.cache = Some(interface.into_cache());
        found
    }

    pub fn texts(&mut self) -> Vec<String> {
        self.search(
            |candidate: Candidate<'_>| match candidate {
                Candidate::Text { content, .. } => Some(content.to_string()),
                _ => None,
            },
            true,
        )
    }

    pub fn find_all(&mut self, text: &str) -> Vec<Rectangle> {
        let found = self.find_texts(text);
        let visible = |found: &[(Rectangle, Option<Rectangle>)]| -> Vec<Rectangle> {
            found.iter().filter_map(|(_, visible)| *visible).collect()
        };
        let complete = found.iter().any(|(bounds, visible)| {
            visible.is_some_and(|visible| visible.height + 0.5 >= bounds.height)
        });
        if complete || self.app.page != Page::Settings {
            return visible(&found);
        }
        if found.is_empty() {
            return Vec::new();
        }
        self.reveal(text);
        visible(&self.find_texts(text))
    }

    fn reveal(&mut self, text: &str) {
        if let Some((bounds, _)) = self.find_texts(text).first().copied() {
            self.scroll_settings((bounds.y - REVEAL_MARGIN).max(0.0));
        }
    }

    pub fn scroll_settings(&mut self, offset: f32) {
        self.scroll(SETTINGS_SCROLL_ID, offset);
    }

    pub fn scroll(&mut self, id: &'static str, offset: f32) {
        self.run(iced::widget::operation::scroll_to(
            id,
            AbsoluteOffset { x: 0.0, y: offset },
        ));
    }

    fn find_texts(&mut self, text: &str) -> Vec<(Rectangle, Option<Rectangle>)> {
        let wanted = text.to_string();
        self.search(
            move |candidate: Candidate<'_>| match candidate {
                Candidate::Text {
                    content,
                    bounds,
                    visible_bounds,
                    ..
                } if content == wanted => Some((bounds, visible_bounds)),
                _ => None,
            },
            true,
        )
    }

    pub fn has(&mut self, text: &str) -> bool {
        !self.find_all(text).is_empty()
    }

    pub fn contains(&mut self, part: &str) -> bool {
        self.texts().iter().any(|text| text.contains(part))
    }

    pub fn bounds(&mut self, text: &str) -> Rectangle {
        match self.find_all(text).first() {
            Some(bounds) => *bounds,
            None => panic!("{text:?} not found in {:?}", self.texts()),
        }
    }

    pub fn input_bounds(&mut self, id: &str) -> Rectangle {
        let wanted = widget::Id::from(id.to_string());
        let found = self.search(
            move |candidate: Candidate<'_>| match candidate {
                Candidate::TextInput {
                    id: Some(id),
                    visible_bounds,
                    ..
                } if *id == wanted => visible_bounds,
                _ => None,
            },
            false,
        );
        *found
            .first()
            .unwrap_or_else(|| panic!("input {id:?} not found"))
    }

    fn center(&mut self, text: &str) -> Point {
        self.bounds(text).center()
    }

    pub fn keyring_secret(&self, site: &str) -> Option<String> {
        self.keyring_named(&hoplix_core::secret_name(site))
    }

    pub fn keyring_named(&self, name: &str) -> Option<String> {
        let keyring = self.app.services.keyring.clone();
        let name = name.to_string();
        self.runtime
            .block_on(async move { keyring.get(&name).await })
            .expect("reading the test keyring failed")
    }

    pub fn store_keyring_secret(&self, site: &str, secret: &str) {
        self.store_keyring_named(&hoplix_core::secret_name(site), secret);
    }

    pub fn store_keyring_named(&self, name: &str, secret: &str) {
        let keyring = self.app.services.keyring.clone();
        let name = name.to_string();
        let secret = secret.to_string();
        self.runtime
            .block_on(async move { keyring.set(&name, &secret).await })
            .expect("writing the test keyring failed");
    }

    pub fn tab(&self) -> &crate::app::Tab {
        self.app.focused_tab().expect("no focused tab")
    }

    pub fn names(&self) -> Vec<String> {
        self.tab()
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    pub fn selected(&self) -> Vec<String> {
        self.tab()
            .selected_entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }
}
