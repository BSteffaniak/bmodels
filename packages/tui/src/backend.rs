//! The contract between the TUI and the rest of bmodels.
//!
//! The TUI never touches the store or the network. Everything goes through a [`Backend`]:
//! requests are fire-and-forget [`Command`]s, and results come back asynchronously as
//! [`BackendEvent`]s through the [`EventSink`] handed to [`Backend::start`]. The CLI implements
//! the backend with a worker thread that owns the store and runs syncs.

use std::collections::BTreeMap;
use std::sync::Arc;

use bmodels_model::{
    AckTarget, CanonicalModel, Event, Listing, ModelInfo, ModelStatus, Secret, SourceKind,
    SourceState, Timestamp,
};

/// Source of data and executor of actions for the UI. Methods must not block.
pub trait Backend: Send + 'static {
    /// Called once at start-up. Keep `sink` and push a [`BackendEvent::Data`] soon.
    fn start(&mut self, sink: EventSink);

    /// Requests an action. Answer with events (usually a fresh [`BackendEvent::Data`]).
    fn request(&mut self, command: Command);
}

impl<B: Backend + ?Sized> Backend for Box<B> {
    fn start(&mut self, sink: EventSink) {
        (**self).start(sink);
    }

    fn request(&mut self, command: Command) {
        (**self).request(command);
    }
}

/// An action requested by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Re-read the store.
    Reload,
    /// Sync these sources now (empty: every enabled source).
    Sync(Vec<String>),
    /// Acknowledge something.
    Ack(AckTarget),
    /// Set a canonical model's status.
    SetStatus {
        /// Canonical key.
        key: String,
        /// New status.
        status: ModelStatus,
    },
    /// Open a URL in the browser.
    Open(String),
    /// Store a source's API key in the vault, then sync that source.
    SaveKey {
        /// Source id.
        source: String,
        /// The key.
        key: Secret,
    },
}

/// Something the backend reports.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    /// A fresh snapshot of everything.
    Data(Arc<Data>),
    /// A sync of these sources started.
    SyncStarted(Vec<String>),
    /// One source started syncing.
    SourceStarted(String),
    /// One source finished.
    SourceFinished {
        /// Source id.
        source: String,
        /// Whether it succeeded (or was skipped / not modified).
        ok: bool,
        /// One-line summary.
        summary: String,
    },
    /// The whole sync finished.
    SyncFinished(String),
    /// An informational message.
    Info(String),
    /// An error message.
    Error(String),
}

/// Pushes [`BackendEvent`]s to the UI. Cheap to clone, usable from any thread.
#[derive(Debug, Clone)]
pub struct EventSink {
    tx: tokio::sync::mpsc::UnboundedSender<BackendEvent>,
}

/// Receiving end of an [`EventSink`].
#[derive(Debug)]
pub struct EventReceiver {
    rx: tokio::sync::mpsc::UnboundedReceiver<BackendEvent>,
}

impl EventSink {
    /// Creates a connected pair.
    #[must_use]
    pub fn channel() -> (Self, EventReceiver) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx }, EventReceiver { rx })
    }

    /// Pushes one event. Returns `false` once the UI is gone.
    #[allow(clippy::must_use_candidate)] // fire-and-forget is the common use
    pub fn send(&self, event: BackendEvent) -> bool {
        self.tx.send(event).is_ok()
    }
}

impl EventReceiver {
    /// Waits for the next event (`None` once every sink is dropped).
    pub async fn recv(&mut self) -> Option<BackendEvent> {
        self.rx.recv().await
    }

    /// Takes an event if one is ready.
    pub fn try_recv(&mut self) -> Option<BackendEvent> {
        self.rx.try_recv().ok()
    }
}

/// A configured source with its sync state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    /// Source id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Adapter kind.
    pub kind: SourceKind,
    /// Whether it is synced automatically.
    pub enabled: bool,
    /// Vendor of a direct provider source.
    pub vendor: Option<String>,
    /// Effective interval (`"6h"`).
    pub interval: String,
    /// Persisted state.
    pub state: SourceState,
    /// Next scheduled sync.
    pub next_due: Option<Timestamp>,
    /// Whether the source takes an API key.
    pub takes_key: bool,
    /// Where to create one.
    pub key_url: Option<String>,
}

/// Everything the UI shows. Built once per reload; indexes are precomputed.
#[derive(Debug, Clone, Default)]
pub struct Data {
    /// When the snapshot was taken.
    pub now: Timestamp,
    /// Canonical models.
    pub models: Vec<CanonicalModel>,
    /// Listings.
    pub listings: Vec<Listing>,
    /// Unacknowledged plus recent events, newest first.
    pub events: Vec<Event>,
    /// Configured sources, in id order.
    pub sources: Vec<SourceRow>,
    model_index: BTreeMap<String, usize>,
    listings_by_model: BTreeMap<String, Vec<usize>>,
    source_index: BTreeMap<String, usize>,
}

impl Data {
    /// Builds a snapshot and its indexes.
    #[must_use]
    pub fn new(
        now: Timestamp,
        models: Vec<CanonicalModel>,
        listings: Vec<Listing>,
        mut events: Vec<Event>,
        sources: Vec<SourceRow>,
    ) -> Self {
        events.sort_by_key(|e| std::cmp::Reverse((e.at, e.id)));
        let model_index = models
            .iter()
            .enumerate()
            .map(|(i, m)| (m.key.clone(), i))
            .collect();
        let mut listings_by_model: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, l) in listings.iter().enumerate() {
            listings_by_model
                .entry(l.canonical_key.clone())
                .or_default()
                .push(i);
        }
        let source_index = sources
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.clone(), i))
            .collect();
        Self {
            now,
            models,
            listings,
            events,
            sources,
            model_index,
            listings_by_model,
            source_index,
        }
    }

    /// A canonical model by key.
    #[must_use]
    pub fn model(&self, key: &str) -> Option<&CanonicalModel> {
        self.model_index.get(key).map(|i| &self.models[*i])
    }

    /// Listings of a canonical model, direct-vendor sources first, then present before
    /// removed.
    #[must_use]
    pub fn listings_of(&self, key: &str) -> Vec<&Listing> {
        let mut out: Vec<&Listing> = self
            .listings_by_model
            .get(key)
            .map(|ix| ix.iter().map(|i| &self.listings[*i]).collect())
            .unwrap_or_default();
        let vendor = self.model(key).map(|m| m.vendor.as_str());
        out.sort_by_key(|l| {
            let direct = self
                .source(&l.key.source)
                .and_then(|s| s.vendor.as_deref())
                .is_some_and(|v| Some(v) == vendor);
            (!direct, !l.is_present(), l.key.source.clone())
        });
        out
    }

    /// A source by id.
    #[must_use]
    pub fn source(&self, id: &str) -> Option<&SourceRow> {
        self.source_index.get(id).map(|i| &self.sources[*i])
    }

    /// Display name of a source (its id if unknown).
    #[must_use]
    pub fn source_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.source(id).map_or(id, |s| s.name.as_str())
    }

    /// Metadata merged across a model's present listings (direct vendor first).
    #[must_use]
    pub fn merged_info(&self, key: &str) -> ModelInfo {
        let listings = self.listings_of(key);
        let present: Vec<&Listing> = listings
            .iter()
            .copied()
            .filter(|l| l.is_present())
            .collect();
        let pool = if present.is_empty() {
            listings
        } else {
            present
        };
        merge_info(pool.iter().map(|l| &l.info))
    }

    /// Number of unacknowledged events.
    #[must_use]
    pub fn unacked(&self) -> usize {
        self.events.iter().filter(|e| e.acked_at.is_none()).count()
    }
}

/// Fills each field from the first `info` that has it.
#[must_use]
pub fn merge_info<'a>(infos: impl Iterator<Item = &'a ModelInfo>) -> ModelInfo {
    let mut out = ModelInfo::default();
    for info in infos {
        macro_rules! fill {
            ($($field:ident),*) => {
                $(if out.$field.is_none() { out.$field.clone_from(&info.$field); })*
            };
        }
        fill!(
            description,
            owned_by,
            context_window,
            max_output,
            input_price,
            output_price,
            deprecation_date,
            knowledge_cutoff,
            open_weights
        );
        if out.input_modalities.is_empty() {
            out.input_modalities.clone_from(&info.input_modalities);
        }
        if out.output_modalities.is_empty() {
            out.output_modalities.clone_from(&info.output_modalities);
        }
        if out.capabilities.is_empty() {
            out.capabilities.clone_from(&info.capabilities);
        }
        if out.variants.is_empty() {
            out.variants.clone_from(&info.variants);
        }
    }
    out
}
