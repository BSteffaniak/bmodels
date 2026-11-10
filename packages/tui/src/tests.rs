use std::sync::{Arc, Mutex};

use bmodels_model::{
    AckTarget, CanonicalModel, Event, EventKind, FieldChange, Listing, ListingKey, ListingStatus,
    ModelInfo, ModelStatus, RecordKind, SourceKind, SourceState,
};
use bmux_keyboard::{KeyCode, KeyStroke, Modifiers};

use super::*;

const NOW: i64 = 1_800_000_000;

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Command>>>);

impl Recorder {
    fn take(&self) -> Vec<Command> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

impl Backend for Recorder {
    fn start(&mut self, _sink: EventSink) {}

    fn request(&mut self, command: Command) {
        self.0.lock().unwrap().push(command);
    }
}

fn model(key: &str, name: &str, status: ModelStatus, first_seen: i64) -> CanonicalModel {
    CanonicalModel {
        key: key.into(),
        vendor: key.split('/').next().unwrap().into(),
        display_name: name.into(),
        kind: RecordKind::Model,
        first_seen_at: first_seen,
        created_at: None,
        status,
        acked_at: None,
        note: None,
    }
}

fn listing(source: &str, id: &str, canonical: &str, price: f64) -> Listing {
    Listing {
        key: ListingKey::new(source, id),
        canonical_key: canonical.into(),
        kind: RecordKind::Model,
        display_name: None,
        created_at: None,
        url: Some(format!("https://example.test/{id}")),
        info: ModelInfo {
            context_window: Some(400_000),
            input_price: Some(price),
            output_price: Some(price * 4.0),
            description: Some("A very capable model.".into()),
            ..ModelInfo::default()
        },
        first_seen_at: NOW - 3_600,
        removed_at: None,
        baseline: false,
        status: ListingStatus::Unseen,
        acked_at: None,
    }
}

fn source(id: &str, name: &str, health: &SourceHealthLike) -> SourceRow {
    let mut state = SourceState {
        id: id.into(),
        listing_count: 42,
        ..SourceState::default()
    };
    match health {
        SourceHealthLike::Ok => state.last_success_at = Some(NOW - 600),
        SourceHealthLike::Skipped => state.skipped = Some("needs OPENAI_API_KEY".into()),
        SourceHealthLike::Error => state.last_error = Some("HTTP 500".into()),
    }
    SourceRow {
        id: id.into(),
        name: name.into(),
        kind: SourceKind::Openrouter,
        enabled: true,
        vendor: (id == "openai").then(|| "openai".into()),
        interval: "6h".into(),
        state,
        next_due: Some(NOW + 3_600),
        takes_key: id == "openai",
        key_url: (id == "openai").then(|| "https://keys.test".into()),
    }
}

enum SourceHealthLike {
    Ok,
    Skipped,
    Error,
}

fn data() -> Arc<Data> {
    let events = vec![
        Event {
            id: 1,
            at: NOW - 3_600,
            kind: EventKind::New,
            canonical_key: "openai/gpt-6".into(),
            listing: ListingKey::new("openrouter", "openai/gpt-6"),
            changes: Vec::new(),
            acked_at: None,
            notified_at: None,
        },
        Event {
            id: 2,
            at: NOW - 60,
            kind: EventKind::Changed,
            canonical_key: "anthropic/claude-x".into(),
            listing: ListingKey::new("openrouter", "anthropic/claude-x"),
            changes: vec![FieldChange {
                field: "input_price".into(),
                old: Some("$3.00".into()),
                new: Some("$1.50".into()),
            }],
            acked_at: None,
            notified_at: None,
        },
        Event {
            id: 3,
            at: NOW - 86_400,
            kind: EventKind::Removed,
            canonical_key: "openai/gpt-3".into(),
            listing: ListingKey::new("openai", "gpt-3"),
            changes: Vec::new(),
            acked_at: Some(NOW - 80_000),
            notified_at: None,
        },
    ];
    Arc::new(Data::new(
        NOW,
        vec![
            model("openai/gpt-6", "GPT-6", ModelStatus::Unseen, NOW - 3_600),
            model(
                "anthropic/claude-x",
                "Claude X",
                ModelStatus::Seen,
                NOW - 86_400 * 30,
            ),
            model(
                "openai/gpt-3",
                "GPT-3",
                ModelStatus::Ignored,
                NOW - 86_400 * 900,
            ),
        ],
        vec![
            listing("openrouter", "openai/gpt-6", "openai/gpt-6", 1.25),
            listing("openai", "gpt-6", "openai/gpt-6", 1.0),
            listing(
                "openrouter",
                "anthropic/claude-x",
                "anthropic/claude-x",
                1.5,
            ),
        ],
        events,
        vec![
            source("openai", "OpenAI", &SourceHealthLike::Skipped),
            source("openrouter", "OpenRouter", &SourceHealthLike::Ok),
            source("broken", "Broken", &SourceHealthLike::Error),
        ],
    ))
}

fn app() -> (App<Recorder>, Recorder) {
    let recorder = Recorder::default();
    let (sink, _rx) = EventSink::channel();
    let mut app = App::new(recorder.clone(), sink);
    app.apply(BackendEvent::Data(data()));
    (app, recorder)
}

fn press(app: &mut App<Recorder>, c: char) -> Handled {
    let (key, shift) = if c.is_ascii_uppercase() {
        (KeyCode::Char(c.to_ascii_lowercase()), true)
    } else {
        (KeyCode::Char(c), false)
    };
    app.handle_key(KeyStroke {
        key,
        modifiers: Modifiers {
            shift,
            ..Modifiers::NONE
        },
    })
}

fn key(app: &mut App<Recorder>, code: KeyCode) -> Handled {
    app.handle_key(KeyStroke::simple(code))
}

fn screen(app: &mut App<Recorder>, width: u16) -> String {
    render_to_text(app, width, 24).join("\n")
}

#[test]
fn inbox_shows_unacked_events_newest_first_with_details() {
    let (mut app, _) = app();
    let text = screen(&mut app, 140);
    assert!(text.contains("Inbox (2)"), "{text}");
    assert!(text.contains("Models (3)"), "{text}");
    let changed = text.find("CHANGED").unwrap();
    let new = text.find("NEW ").unwrap();
    assert!(changed < new, "newest first:\n{text}");
    assert!(text.contains("input price $3.00→$1.50"), "{text}");
    assert!(
        !text.contains("REMOVED"),
        "acked events stay out of the inbox"
    );
    // Details pane for the selected (CHANGED) row.
    assert!(text.contains("Details (i)"), "{text}");
    assert!(text.contains("Listings (1)"), "{text}");
    assert!(
        text.contains("1 unseen · 3 models · 1/3 sources ok"),
        "{text}"
    );
}

#[test]
fn ack_and_ack_all() {
    let (mut app, rec) = app();
    press(&mut app, 'a');
    assert_eq!(rec.take(), [Command::Ack(AckTarget::Event(2))]);

    press(&mut app, 'A');
    assert!(matches!(app.modal, Some(Modal::ConfirmAckAll { ref ids }) if ids == &[2, 1]));
    press(&mut app, 'y');
    assert_eq!(
        rec.take(),
        [
            Command::Ack(AckTarget::Event(2)),
            Command::Ack(AckTarget::Event(1))
        ]
    );
    assert_eq!(app.modal, None);
}

#[test]
fn models_tab_listing_view_and_status_keys() {
    let (mut app, rec) = app();
    press(&mut app, '2');
    assert_eq!(app.tab, Tab::Models);
    assert_eq!(app.rows().len(), 2, "ignored hidden by default");
    let text = screen(&mut app, 140);
    assert!(
        text.contains("GPT-6") && text.contains("openai/gpt-6"),
        "{text}"
    );
    assert!(
        text.contains("$1.00 / $4.00"),
        "direct vendor listing wins:\n{text}"
    );

    press(&mut app, 'a');
    assert_eq!(
        rec.take(),
        [Command::Ack(AckTarget::Model("openai/gpt-6".into()))]
    );
    press(&mut app, 's');
    assert_eq!(
        rec.take(),
        [Command::SetStatus {
            key: "openai/gpt-6".into(),
            status: ModelStatus::Starred
        }]
    );

    press(&mut app, 'I');
    assert_eq!(app.rows().len(), 3);
    press(&mut app, 'v');
    assert_eq!(app.rows().len(), 3);
    assert!(matches!(app.rows()[0], Row::Listing(_)));
    press(&mut app, 'a');
    assert!(matches!(
        rec.take().as_slice(),
        [Command::Ack(AckTarget::Listing(_))]
    ));
}

#[test]
fn filter_narrows_rows_and_esc_clears() {
    let (mut app, _) = app();
    press(&mut app, '2');
    press(&mut app, '/');
    for c in "claude".chars() {
        press(&mut app, c);
    }
    assert_eq!(app.rows().len(), 1);
    assert!(screen(&mut app, 140).contains("filter: claude"));
    key(&mut app, KeyCode::Enter);
    assert!(!app.filter_editing);
    key(&mut app, KeyCode::Escape);
    assert_eq!(app.rows().len(), 2);
}

#[test]
fn sources_tab_sync_and_open() {
    let (mut app, rec) = app();
    press(&mut app, '3');
    let text = screen(&mut app, 140);
    assert!(text.contains("needs OPENAI"), "{text}");
    assert!(text.contains("HTTP 500"), "{text}");
    key(&mut app, KeyCode::Down);
    press(&mut app, 'R');
    let cmds = rec.take();
    assert_eq!(cmds.len(), 1);
    assert!(matches!(&cmds[0], Command::Sync(ids) if ids.len() == 1));
    assert!(app.sync.is_some());
    press(&mut app, 'r');
    assert_eq!(rec.take(), [], "one sync at a time");

    app.apply(BackendEvent::SyncStarted(vec!["openrouter".into()]));
    app.apply(BackendEvent::SourceStarted("openrouter".into()));
    assert!(screen(&mut app, 140).contains("syncing 0/1"));
    app.apply(BackendEvent::SourceFinished {
        source: "openrouter".into(),
        ok: true,
        summary: "ok".into(),
    });
    app.apply(BackendEvent::SyncFinished("1 new".into()));
    assert!(app.sync.is_none());

    press(&mut app, '1');
    press(&mut app, 'o');
    assert!(
        matches!(rec.take().as_slice(), [Command::Open(url)] if url.starts_with("https://example.test/"))
    );
}

#[test]
fn selection_survives_reload_and_narrow_terminals_use_a_modal() {
    let (mut app, _) = app();
    key(&mut app, KeyCode::Down);
    assert_eq!(app.selected(), Some(Row::Event(1)));
    let id_before = match app.selected() {
        Some(Row::Event(i)) => app.data.events[i].id,
        _ => unreachable!(),
    };
    app.apply(BackendEvent::Data(data()));
    let id_after = match app.selected() {
        Some(Row::Event(i)) => app.data.events[i].id,
        _ => unreachable!(),
    };
    assert_eq!(id_before, id_after);

    let text = screen(&mut app, 80);
    assert!(!text.contains("Details (i)"), "no docked pane when narrow");
    press(&mut app, 'i');
    assert!(matches!(app.modal, Some(Modal::Details { .. })));
    assert!(screen(&mut app, 80).contains("Details — esc to close"));
    key(&mut app, KeyCode::Escape);
    press(&mut app, '?');
    assert!(screen(&mut app, 80).contains("Seen state"));
    assert_eq!(
        press(&mut app, 'q'),
        Handled::Redraw,
        "q closes the modal first"
    );
    assert_eq!(press(&mut app, 'q'), Handled::Quit);
}

#[test]
fn empty_states() {
    let recorder = Recorder::default();
    let (sink, _rx) = EventSink::channel();
    let mut app = App::new(recorder, sink);
    assert!(screen(&mut app, 100).contains("loading…"));
    app.apply(BackendEvent::Data(Arc::new(Data::default())));
    assert!(screen(&mut app, 100).contains("nothing synced yet"));
}
