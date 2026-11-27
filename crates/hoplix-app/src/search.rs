use std::sync::Arc;

use hoplix_core::{Search, SearchBatch, SearchHit};
use iced::Task;
use iced::futures::SinkExt;

use crate::app::{Dialog, Hoplix, Message, TabId};
use crate::transfers::Drag;
use crate::ui;

// Constants

pub const MAX_RESULTS: usize = 10_000;
const BATCH_BUFFER: usize = 16;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchDialog {
    pub tab: TabId,
    pub pattern: String,
    pub depth: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResults {
    pub pattern: String,
    pub root: String,
    pub hits: Vec<SearchHit>,
    pub unreadable: usize,
    pub running: bool,
    pub truncated: bool,
    pub selected: Option<usize>,
    generation: u64,
}

#[derive(Debug, Clone)]
pub enum SearchMessage {
    Open,
    Pattern(String),
    Depth(String),
    Found(TabId, u64, SearchBatch),
    Finished(TabId, u64),
    Cancel(TabId),
    Close(TabId),
    Select(TabId, usize),
    Reveal(TabId, usize),
}

// Dialog

impl SearchDialog {
    pub fn depth_limit(&self) -> Result<Option<usize>, ()> {
        let text = self.depth.trim();
        if text.is_empty() {
            return Ok(None);
        }
        text.parse::<usize>()
            .ok()
            .filter(|depth| *depth > 0)
            .map(Some)
            .ok_or(())
    }

    pub fn can_start(&self) -> bool {
        self.depth_limit().is_ok()
    }
}

// Messages

impl Hoplix {
    pub(crate) fn update_search(&mut self, message: SearchMessage) -> Task<Message> {
        match message {
            SearchMessage::Open => return self.open_search(),
            SearchMessage::Pattern(pattern) => {
                if let Some(Dialog::Search(dialog)) = &mut self.dialog {
                    dialog.pattern = pattern;
                }
            }
            SearchMessage::Depth(depth) => {
                if let Some(Dialog::Search(dialog)) = &mut self.dialog {
                    dialog.depth = depth;
                }
            }
            SearchMessage::Found(id, generation, batch) => self.add_hits(id, generation, batch),
            SearchMessage::Finished(id, generation) => {
                if let Some(results) = self.results_mut(id, generation) {
                    results.running = false;
                }
                self.search_handles.remove(&id);
            }
            SearchMessage::Cancel(id) => {
                self.stop_search(id);
                if let Some(results) = self.tab_mut(id).and_then(|tab| tab.search.as_mut()) {
                    results.running = false;
                }
            }
            SearchMessage::Close(id) => {
                self.stop_search(id);
                if let Some(tab) = self.tab_mut(id) {
                    tab.search = None;
                }
            }
            SearchMessage::Select(id, index) => {
                if let Some((pane, position)) = self.locate(id) {
                    self.panes[pane].active = position;
                    self.focused_pane = pane;
                }
                if let Some(results) = self.tab_mut(id).and_then(|tab| tab.search.as_mut()) {
                    results.selected = Some(index);
                    self.drag = Some(Drag::new(id, None));
                }
            }
            SearchMessage::Reveal(id, index) => return self.reveal_hit(id, index),
        }
        Task::none()
    }

    fn open_search(&mut self) -> Task<Message> {
        let Some(tab) = self.focused_tab().filter(|tab| !tab.path.is_empty()) else {
            return Task::none();
        };
        let pattern = tab
            .search
            .as_ref()
            .map(|results| results.pattern.clone())
            .unwrap_or_else(|| tab.list.filter.clone());
        self.dialog = Some(Dialog::Search(Box::new(SearchDialog {
            tab: tab.id,
            pattern,
            depth: String::new(),
        })));
        ui::focus_input()
    }

    pub(crate) fn start_search(&mut self, dialog: &SearchDialog) -> Task<Message> {
        let Ok(depth) = dialog.depth_limit() else {
            return Task::none();
        };
        let id = dialog.tab;
        self.stop_search(id);
        self.search_generation += 1;
        let generation = self.search_generation;
        let Some(tab) = self.tab_mut(id) else {
            return Task::none();
        };
        let root = tab.path.clone();
        let pattern = dialog.pattern.trim().to_string();
        tab.search = Some(SearchResults {
            pattern: pattern.clone(),
            root: root.clone(),
            hits: Vec::new(),
            unreadable: 0,
            running: true,
            truncated: false,
            selected: None,
            generation,
        });
        let file_system = Arc::clone(&tab.file_system);
        let stream = iced::stream::channel(BATCH_BUFFER, async move |mut output| {
            let mut search = Search::new(file_system, &root, &pattern, depth);
            while let Some(batch) = search.next_batch().await {
                if batch.hits.is_empty() && batch.error.is_none() {
                    continue;
                }
                if output.send(batch).await.is_err() {
                    break;
                }
            }
        });
        let (task, handle) = Task::run(stream, move |batch| {
            Message::Search(SearchMessage::Found(id, generation, batch))
        })
        .chain(Task::done(Message::Search(SearchMessage::Finished(
            id, generation,
        ))))
        .abortable();
        self.search_handles.insert(id, handle);
        task
    }

    pub(crate) fn stop_search(&mut self, id: TabId) {
        if let Some(handle) = self.search_handles.remove(&id) {
            handle.abort();
        }
    }

    fn results_mut(&mut self, id: TabId, generation: u64) -> Option<&mut SearchResults> {
        self.tab_mut(id)
            .and_then(|tab| tab.search.as_mut())
            .filter(|results| results.generation == generation)
    }

    fn add_hits(&mut self, id: TabId, generation: u64, batch: SearchBatch) {
        let Some(results) = self.results_mut(id, generation) else {
            return;
        };
        if batch.error.is_some() {
            results.unreadable += 1;
        }
        let room = MAX_RESULTS.saturating_sub(results.hits.len());
        if batch.hits.len() > room {
            results.truncated = true;
        }
        results.hits.extend(batch.hits.into_iter().take(room));
        if results.truncated {
            results.running = false;
            self.stop_search(id);
        }
    }

    fn reveal_hit(&mut self, id: TabId, index: usize) -> Task<Message> {
        let Some(hit) = self
            .tab_by_id(id)
            .and_then(|tab| tab.search.as_ref())
            .and_then(|results| results.hits.get(index))
            .cloned()
        else {
            return Task::none();
        };
        self.stop_search(id);
        if let Some(tab) = self.tab_mut(id) {
            tab.search = None;
        }
        self.reveal_in(id, hit.directory, hit.entry.name)
    }
}
