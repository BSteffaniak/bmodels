//! Application state machine: everything except drawing.
//!
//! [`App`] owns the [`Backend`], applies [`BackendEvent`]s, and handles input. It is testable
//! without a terminal; `program.rs` forwards runtime events to it and `render.rs` draws it.

use std::collections::BTreeSet;
use std::sync::Arc;

use bmodels_model::{AckTarget, ModelStatus};
use bmux_keyboard::{KeyCode, KeyStroke};
use bmux_tui::event::{Event, MouseEventKind};
use bmux_tui::geometry::Rect;
use bmux_tui_components::uniform_list::{UniformList, UniformListOutcome, UniformListState};

use crate::backend::{Backend, BackendEvent, Command, Data, EventSink};
use crate::view::{Row, Tab, ViewOptions, row_identity, row_model_key, rows};

/// Ticks between automatic reloads (picks up writes from a background `bmodels watch`).
const RELOAD_TICKS: u64 = 250;

/// Result of handling input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handled {
    /// Exit the UI.
    Quit,
    /// State changed; redraw.
    Redraw,
    /// Nothing changed.
    Ignored,
}

/// Severity of the transient status message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLevel {
    /// Neutral.
    Info,
    /// Something worked.
    Success,
    /// Something failed.
    Error,
}

/// A modal dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// Key reference.
    Help {
        /// Scroll offset.
        scroll: usize,
    },
    /// Confirm acknowledging every visible inbox event.
    ConfirmAckAll {
        /// Event ids.
        ids: Vec<i64>,
    },
    /// Details of the selection (narrow terminals).
    Details {
        /// Scroll offset.
        scroll: usize,
    },
}

/// Progress of a running sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncState {
    /// Sources in this sync.
    pub total: usize,
    /// Sources finished.
    pub done: usize,
    /// Sources running.
    pub running: BTreeSet<String>,
    /// Sources that failed.
    pub failed: usize,
}

/// Application state.
#[allow(clippy::struct_excessive_bools)] // independent UI toggles
pub struct App<B: Backend> {
    backend: B,
    /// Current snapshot.
    pub data: Arc<Data>,
    /// Whether any snapshot has arrived.
    pub loaded: bool,
    /// Active tab.
    pub tab: Tab,
    states: [UniformListState; 4],
    /// Rows visible in the list (set by the renderer).
    pub viewport_rows: usize,
    /// Where the list's rows were painted, in terminal coordinates (set by the renderer).
    pub list_area: Rect,
    /// Row-set options.
    pub view: ViewOptions,
    /// Typing into the filter.
    pub filter_editing: bool,
    /// Details pane open.
    pub details: bool,
    /// Running sync.
    pub sync: Option<SyncState>,
    /// Summary of the last finished sync.
    pub last_sync: Option<String>,
    /// Transient message.
    pub message: Option<(String, MessageLevel)>,
    /// Open modal.
    pub modal: Option<Modal>,
    /// Animation counter.
    pub tick: u64,
    /// The terminal is too narrow for a details pane (set by the renderer).
    pub narrow: bool,
    rows: Vec<Row>,
}

impl<B: Backend> App<B> {
    /// Creates the app and starts the backend.
    pub fn new(mut backend: B, sink: EventSink) -> Self {
        backend.start(sink);
        let mut app = Self {
            backend,
            data: Arc::new(Data::default()),
            loaded: false,
            tab: Tab::Inbox,
            states: [UniformListState::new(); 4],
            viewport_rows: 10,
            list_area: Rect::new(0, 0, 0, 0),
            view: ViewOptions::default(),
            filter_editing: false,
            details: true,
            sync: None,
            last_sync: None,
            message: None,
            modal: None,
            tick: 0,
            narrow: false,
            rows: Vec::new(),
        };
        app.refresh_rows(None);
        app
    }

    /// Rows of the active tab.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Selection state of the active tab.
    #[must_use]
    pub const fn state(&self) -> UniformListState {
        self.states[self.tab.index()]
    }

    pub(crate) const fn state_mut(&mut self) -> &mut UniformListState {
        &mut self.states[self.tab.index()]
    }

    /// The selected row.
    #[must_use]
    pub fn selected(&self) -> Option<Row> {
        self.rows.get(self.state().selected).copied()
    }

    /// Recomputes rows, keeping the selection on `keep` (a row identity) when possible.
    fn refresh_rows(&mut self, keep: Option<String>) {
        self.rows = rows(&self.data, self.tab, &self.view);
        if let Some(identity) = keep
            && let Some(i) = self
                .rows
                .iter()
                .position(|r| row_identity(&self.data, *r) == identity)
        {
            self.state_mut().selected = i;
        }
        let (len, viewport) = (self.rows.len(), self.viewport_rows);
        self.state_mut().clamp(len, viewport);
    }

    fn selected_identity(&self) -> Option<String> {
        self.selected().map(|r| row_identity(&self.data, r))
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), MessageLevel::Info));
    }

    /// Applies a backend event.
    pub fn apply(&mut self, event: BackendEvent) {
        match event {
            BackendEvent::Data(data) => {
                let keep = self.selected_identity();
                self.data = data;
                self.loaded = true;
                // Rows of other tabs index into the old data; reset their selection safely.
                for (i, state) in self.states.iter_mut().enumerate() {
                    if i != self.tab.index() {
                        state.clamp(0, 1);
                    }
                }
                self.refresh_rows(keep);
            }
            BackendEvent::SyncStarted(ids) => {
                self.sync = Some(SyncState {
                    total: ids.len(),
                    ..SyncState::default()
                });
            }
            BackendEvent::SourceStarted(id) => {
                if let Some(sync) = &mut self.sync {
                    sync.running.insert(id);
                }
            }
            BackendEvent::SourceFinished {
                source,
                ok,
                summary,
            } => {
                if let Some(sync) = &mut self.sync {
                    sync.running.remove(&source);
                    sync.done += 1;
                    if !ok {
                        sync.failed += 1;
                    }
                }
                if !ok {
                    self.message = Some((summary, MessageLevel::Error));
                }
            }
            BackendEvent::SyncFinished(summary) => {
                let failed = self.sync.as_ref().is_some_and(|s| s.failed > 0);
                self.sync = None;
                self.message = Some((
                    summary.clone(),
                    if failed {
                        MessageLevel::Error
                    } else {
                        MessageLevel::Success
                    },
                ));
                self.last_sync = Some(summary);
            }
            BackendEvent::Info(text) => self.message = Some((text, MessageLevel::Success)),
            BackendEvent::Error(text) => self.message = Some((text, MessageLevel::Error)),
        }
    }

    /// Advances animations and periodic reloads; returns whether a redraw is useful.
    pub fn on_tick(&mut self) -> bool {
        self.tick = self.tick.wrapping_add(1);
        if self.tick.is_multiple_of(RELOAD_TICKS) && self.sync.is_none() {
            self.backend.request(Command::Reload);
        }
        self.sync.is_some()
    }

    /// Handles a terminal event.
    pub fn handle_event(&mut self, event: &Event) -> Handled {
        match event {
            Event::Key(stroke) => self.handle_key(*stroke),
            Event::Mouse(mouse) => {
                if self.modal.is_none() {
                    let (list, area) = (UniformList::new("rows", self.rows.len()), self.list_area);
                    return match list.handle_event(area, self.state_mut(), event) {
                        UniformListOutcome::Ignored => Handled::Ignored,
                        _ => Handled::Redraw,
                    };
                }
                let delta = match mouse.kind {
                    MouseEventKind::ScrollUp => -3,
                    MouseEventKind::ScrollDown => 3,
                    _ => return Handled::Ignored,
                };
                match &mut self.modal {
                    Some(Modal::Help { scroll } | Modal::Details { scroll }) => {
                        *scroll = scroll.saturating_add_signed(delta);
                        Handled::Redraw
                    }
                    Some(Modal::ConfirmAckAll { .. }) | None => Handled::Ignored,
                }
            }
            Event::Resize(_) => {
                let (len, viewport) = (self.rows.len(), self.viewport_rows);
                self.state_mut().clamp(len, viewport);
                Handled::Redraw
            }
            Event::Paste(text) if self.filter_editing => {
                self.view.filter.push_str(text.trim());
                self.refresh_rows(None);
                Handled::Redraw
            }
            _ => Handled::Ignored,
        }
    }

    /// Handles one key stroke.
    pub fn handle_key(&mut self, stroke: KeyStroke) -> Handled {
        let key = Key::from(stroke);
        if key.ctrl && key.code == KeyCode::Char('c') {
            return Handled::Quit;
        }
        self.message = None;
        if self.filter_editing {
            return self.handle_filter_key(key);
        }
        if self.modal.is_some() {
            return self.handle_modal_key(key);
        }
        let (len, viewport) = (self.rows.len(), self.viewport_rows);
        match key.code {
            KeyCode::Up => self.state_mut().move_by(-1, len, viewport),
            KeyCode::Down => self.state_mut().move_by(1, len, viewport),
            KeyCode::PageUp => self.state_mut().page(false, len, viewport),
            KeyCode::PageDown => self.state_mut().page(true, len, viewport),
            KeyCode::Home => self.state_mut().top(len, viewport),
            KeyCode::End => self.state_mut().bottom(len, viewport),
            KeyCode::Tab => self.set_tab(self.tab.cycle(key.shift)),
            KeyCode::Enter => self.toggle_details(),
            KeyCode::Escape => {
                if self.view.filter.is_empty() {
                    return Handled::Ignored;
                }
                self.view.filter.clear();
                self.refresh_rows(self.selected_identity());
            }
            _ => return self.handle_char(key, len, viewport),
        }
        Handled::Redraw
    }

    fn handle_char(&mut self, key: Key, len: usize, viewport: usize) -> Handled {
        let Some(c) = key.ch() else {
            if key.ctrl {
                match key.code {
                    KeyCode::Char('d') => self.state_mut().page(true, len, viewport),
                    KeyCode::Char('u') => self.state_mut().page(false, len, viewport),
                    _ => return Handled::Ignored,
                }
                return Handled::Redraw;
            }
            return Handled::Ignored;
        };
        match c {
            'q' => return Handled::Quit,
            'j' => self.state_mut().move_by(1, len, viewport),
            'k' => self.state_mut().move_by(-1, len, viewport),
            'g' => self.state_mut().top(len, viewport),
            'G' => self.state_mut().bottom(len, viewport),
            '1' => self.set_tab(Tab::Inbox),
            '2' => self.set_tab(Tab::Models),
            '3' => self.set_tab(Tab::Sources),
            '4' => self.set_tab(Tab::History),
            'i' => self.toggle_details(),
            '?' => self.modal = Some(Modal::Help { scroll: 0 }),
            '/' => self.filter_editing = true,
            'a' => self.ack_selected(),
            'A' => self.confirm_ack_all(),
            's' => self.toggle_status(ModelStatus::Starred),
            'x' => self.toggle_status(ModelStatus::Ignored),
            'u' => self.set_selected_status(ModelStatus::Unseen, "marked unseen"),
            'v' if self.tab == Tab::Models => {
                self.view.listing_view = !self.view.listing_view;
                self.refresh_rows(None);
                self.info(if self.view.listing_view {
                    "showing listings"
                } else {
                    "showing models"
                });
            }
            'I' => {
                self.view.show_ignored = !self.view.show_ignored;
                self.refresh_rows(self.selected_identity());
                self.info(if self.view.show_ignored {
                    "showing ignored models"
                } else {
                    "hiding ignored models"
                });
            }
            'r' => self.start_sync(Vec::new()),
            'R' => match self.selected() {
                Some(Row::Source(i)) => {
                    let id = self.data.sources[i].id.clone();
                    self.start_sync(vec![id]);
                }
                _ => self.info("select a source (tab 3) to sync just it"),
            },
            'o' => self.open_selected(),
            _ => return Handled::Ignored,
        }
        Handled::Redraw
    }

    fn handle_filter_key(&mut self, key: Key) -> Handled {
        match key.code {
            KeyCode::Enter => self.filter_editing = false,
            KeyCode::Escape => {
                self.filter_editing = false;
                self.view.filter.clear();
                self.refresh_rows(self.selected_identity());
            }
            KeyCode::Backspace => {
                self.view.filter.pop();
                self.refresh_rows(None);
            }
            KeyCode::Up | KeyCode::Down => {
                let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                let (len, viewport) = (self.rows.len(), self.viewport_rows);
                self.state_mut().move_by(delta, len, viewport);
            }
            _ => match key.ch() {
                Some(c) if !key.ctrl && !key.alt => {
                    self.view.filter.push(c);
                    self.state_mut().selected = 0;
                    self.refresh_rows(None);
                }
                _ => return Handled::Ignored,
            },
        }
        Handled::Redraw
    }

    fn handle_modal_key(&mut self, key: Key) -> Handled {
        let close = key.code == KeyCode::Escape || key.ch() == Some('q');
        match self.modal.clone() {
            Some(Modal::Help { scroll }) => {
                if close || key.ch() == Some('?') || key.code == KeyCode::Enter {
                    self.modal = None;
                } else {
                    self.scroll_modal(key, scroll);
                }
            }
            Some(Modal::Details { scroll }) => {
                if close || key.ch() == Some('i') || key.code == KeyCode::Enter {
                    self.modal = None;
                } else if key.ch() == Some('a') || key.ch() == Some('o') {
                    self.modal = None;
                    return self.handle_key(KeyStroke::simple(key.code));
                } else {
                    self.scroll_modal(key, scroll);
                }
            }
            Some(Modal::ConfirmAckAll { ids }) => {
                if key.code == KeyCode::Enter || key.ch() == Some('y') {
                    for id in &ids {
                        self.backend.request(Command::Ack(AckTarget::Event(*id)));
                    }
                    self.info(format!("acknowledged {} event(s)", ids.len()));
                }
                self.modal = None;
            }
            None => {}
        }
        Handled::Redraw
    }

    const fn scroll_modal(&mut self, key: Key, scroll: usize) {
        let next = match (key.code, key.ch()) {
            (KeyCode::Up, _) | (_, Some('k')) => scroll.saturating_sub(1),
            (KeyCode::Down, _) | (_, Some('j')) => scroll + 1,
            (KeyCode::PageUp, _) => scroll.saturating_sub(10),
            (KeyCode::PageDown, _) => scroll + 10,
            _ => return,
        };
        if let Some(Modal::Help { scroll } | Modal::Details { scroll }) = &mut self.modal {
            *scroll = next;
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.refresh_rows(None);
        }
    }

    fn toggle_details(&mut self) {
        if self.narrow {
            if self.selected().is_some() {
                self.modal = Some(Modal::Details { scroll: 0 });
            }
        } else {
            self.details = !self.details;
        }
    }

    fn ack_selected(&mut self) {
        let target = match self.selected() {
            Some(Row::Event(i)) => AckTarget::Event(self.data.events[i].id),
            Some(Row::Model(i)) => AckTarget::Model(self.data.models[i].key.clone()),
            Some(Row::Listing(i)) => AckTarget::Listing(self.data.listings[i].key.clone()),
            Some(Row::Source(_)) | None => return,
        };
        // Keep the cursor in place: the acknowledged inbox row disappears on reload.
        self.backend.request(Command::Ack(target));
    }

    fn confirm_ack_all(&mut self) {
        if self.tab != Tab::Inbox {
            self.info("A acknowledges everything visible in the inbox (tab 1)");
            return;
        }
        let ids: Vec<i64> = self
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Event(i) => Some(self.data.events[*i].id),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            self.info("inbox is empty");
        } else {
            self.modal = Some(Modal::ConfirmAckAll { ids });
        }
    }

    fn toggle_status(&mut self, status: ModelStatus) {
        let Some(key) = self.selected().and_then(|r| row_model_key(&self.data, r)) else {
            return;
        };
        let current = self.data.model(key).map(|m| m.status);
        let (next, label) = if current == Some(status) {
            (ModelStatus::Seen, "cleared")
        } else if status == ModelStatus::Starred {
            (status, "starred")
        } else {
            (status, "ignored")
        };
        self.set_selected_status(next, label);
    }

    fn set_selected_status(&mut self, status: ModelStatus, label: &str) {
        let Some(key) = self
            .selected()
            .and_then(|r| row_model_key(&self.data, r))
            .map(str::to_owned)
        else {
            return;
        };
        self.info(format!("{key} {label}"));
        self.backend.request(Command::SetStatus { key, status });
    }

    fn start_sync(&mut self, ids: Vec<String>) {
        if self.sync.is_some() {
            self.info("a sync is already running");
            return;
        }
        self.sync = Some(SyncState::default());
        self.backend.request(Command::Sync(ids));
    }

    fn open_selected(&mut self) {
        let url = match self.selected() {
            Some(Row::Listing(i)) => self.data.listings[i].url.clone(),
            Some(Row::Event(i)) => {
                let e = &self.data.events[i];
                self.data
                    .listings
                    .iter()
                    .find(|l| l.key == e.listing)
                    .and_then(|l| l.url.clone())
                    .or_else(|| self.first_url(&e.canonical_key.clone()))
            }
            Some(Row::Model(i)) => self.first_url(&self.data.models[i].key.clone()),
            Some(Row::Source(_)) | None => None,
        };
        match url {
            Some(url) => {
                self.info(format!("opening {url}"));
                self.backend.request(Command::Open(url));
            }
            None => self.info("no link for this row"),
        }
    }

    fn first_url(&self, key: &str) -> Option<String> {
        self.data
            .listings_of(key)
            .into_iter()
            .find_map(|l| l.url.clone())
    }
}

/// Normalized key: bmux reports letters lowercase with `shift`; this restores the character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    code: KeyCode,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

impl From<KeyStroke> for Key {
    fn from(stroke: KeyStroke) -> Self {
        Self {
            code: stroke.key,
            ctrl: stroke.modifiers.ctrl,
            alt: stroke.modifiers.alt,
            shift: stroke.modifiers.shift,
        }
    }
}

impl Key {
    const fn ch(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(c) if self.shift && c.is_ascii_lowercase() => {
                Some(c.to_ascii_uppercase())
            }
            KeyCode::Char(c) if !self.ctrl && !self.alt => Some(c),
            KeyCode::Space if !self.ctrl && !self.alt => Some(' '),
            _ => None,
        }
    }
}
