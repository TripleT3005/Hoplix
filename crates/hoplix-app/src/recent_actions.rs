use hoplix_core::RecentList;
use iced::Task;

use crate::app::{Hoplix, Message, Page};

// Messages

impl Hoplix {
    pub(crate) fn update_recent(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::RecentSelected(index) => return self.use_recent(index, false),
            Message::RecentRemoved(index) => {
                if self.recent.remove(index).is_some() {
                    self.save_recent();
                }
            }
            Message::RecentCleared => {
                self.recent.clear();
                self.save_recent();
            }
            Message::RememberRecent(enabled) => {
                self.settings.remember_recent = enabled;
                self.save_settings();
                if !enabled {
                    self.recent.clear();
                    self.save_recent();
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub(crate) fn use_recent(&mut self, index: usize, connect: bool) -> Task<Message> {
        let Some(entry) = self.recent.entries.get(index).cloned() else {
            return Task::none();
        };
        self.connect_form.fill_from_recent(&entry);
        if connect && self.connect_form.ready_to_connect() {
            return self.connect(None);
        }
        self.page = Page::Connect;
        Task::none()
    }

    pub(crate) fn remember_connection(&mut self) {
        if !self.settings.remember_recent || self.connect_form.site.is_some() {
            return;
        }
        let Ok(params) = self.connect_form.to_params() else {
            return;
        };
        let entry = self.connect_form.recent_connection(&params);
        self.recent.record(entry);
        self.save_recent();
    }

    pub(crate) fn load_recent(&mut self) {
        self.recent = match &self.config_dir {
            Some(directory) if self.settings.remember_recent => {
                RecentList::load(directory).unwrap_or_default()
            }
            _ => RecentList::default(),
        };
    }

    fn save_recent(&mut self) {
        let Some(directory) = &self.config_dir else {
            return;
        };
        if let Err(error) = self.recent.save(directory) {
            self.notice = Some(self.strings.format(
                "connect.recent.save_failed",
                &[("detail", &error.to_string())],
            ));
        }
    }
}
