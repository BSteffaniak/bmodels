//! The real TUI backend: a worker thread that owns the store and runs syncs.
//!
//! The worker runs a current-thread runtime with a `LocalSet`, so a long sync runs as its own
//! task while acknowledgements and reloads keep being served.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use bmodels_auth::VaultLookup;
use bmodels_engine::{Engine, Progress, Selection, SourceOutcome};
use bmodels_model::{Config, SourceState, Timestamp, format_duration};
use bmodels_sources::SwitchyHttp;
use bmodels_store::Store;
use bmodels_tui::{Backend, BackendEvent, Command, Data, EventSink, SourceRow};
use tokio::sync::mpsc;

use crate::context::{Context, now};
use crate::output;

/// Events kept in the snapshot besides the unacknowledged ones.
const HISTORY: usize = 500;

/// Configured sources joined with their state and schedule, in id order.
pub fn source_rows(
    config: &Config,
    states: &BTreeMap<String, SourceState>,
    now: Timestamp,
) -> Vec<SourceRow> {
    let due: BTreeMap<String, Timestamp> = bmodels_engine::schedule(config, states, now)
        .into_iter()
        .map(|d| (d.source, d.next_at))
        .collect();
    config
        .sources
        .iter()
        .map(|(id, spec)| SourceRow {
            id: id.clone(),
            name: spec.display_name(id).to_owned(),
            kind: spec.kind,
            enabled: spec.enabled,
            vendor: spec.vendor.clone(),
            interval: config
                .interval_of(spec)
                .map_or_else(|_| "?".to_owned(), format_duration),
            state: states.get(id).cloned().unwrap_or_else(|| SourceState {
                id: id.clone(),
                ..SourceState::default()
            }),
            next_due: due.get(id).copied(),
            takes_key: spec.wants_key(),
            key_url: spec.key_url.clone(),
        })
        .collect()
}

/// Reads everything the UI shows.
pub async fn load_data(ctx: &Context, store: &Store) -> anyhow::Result<Data> {
    let snapshot = store.snapshot(HISTORY).await?;
    let t = now();
    Ok(Data::new(
        t,
        snapshot.models,
        snapshot.listings,
        snapshot.events,
        source_rows(ctx.config(), &snapshot.sources, t),
    ))
}

/// [`Backend`] over the store and engine.
pub struct StoreBackend {
    ctx: Context,
    tx: Option<mpsc::UnboundedSender<Command>>,
}

impl StoreBackend {
    /// Creates a backend; the worker starts in [`Backend::start`].
    pub const fn new(ctx: Context) -> Self {
        Self { ctx, tx: None }
    }
}

impl Backend for StoreBackend {
    fn start(&mut self, sink: EventSink) {
        let (tx, rx) = mpsc::unbounded_channel();
        self.tx = Some(tx);
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("bmodels-backend".into())
            .spawn(move || worker(&ctx, &sink, rx))
            .map_or_else(
                |e| log::error!("cannot start the backend thread: {e}"),
                |_| (),
            );
    }

    fn request(&mut self, command: Command) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(command);
        }
    }
}

fn worker(ctx: &Context, sink: &EventSink, mut rx: mpsc::UnboundedReceiver<Command>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            sink.send(BackendEvent::Error(format!(
                "cannot start the async runtime: {e}"
            )));
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let store = match ctx.store().await {
            Ok(store) => Rc::new(store),
            Err(e) => {
                sink.send(BackendEvent::Error(format!("{e:#}")));
                return;
            }
        };
        send_data(ctx, &store, sink).await;
        let syncing = Rc::new(Cell::new(false));
        while let Some(command) = rx.recv().await {
            match command {
                Command::Reload => send_data(ctx, &store, sink).await,
                Command::Sync(ids) => {
                    if syncing.replace(true) {
                        continue;
                    }
                    let (ctx, store, sink, syncing) =
                        (ctx.clone(), store.clone(), sink.clone(), syncing.clone());
                    tokio::task::spawn_local(async move {
                        run_sync(&ctx, &store, &sink, ids).await;
                        syncing.set(false);
                        send_data(&ctx, &store, &sink).await;
                    });
                }
                Command::Ack(target) => {
                    if let Err(e) = store.ack(&target, now()).await {
                        sink.send(BackendEvent::Error(e.to_string()));
                    }
                    send_data(ctx, &store, sink).await;
                }
                Command::SetStatus { key, status } => {
                    if let Err(e) = store.set_status(&key, status, now()).await {
                        sink.send(BackendEvent::Error(e.to_string()));
                    }
                    send_data(ctx, &store, sink).await;
                }
                Command::Open(url) => {
                    if let Err(e) = open::that_detached(&url) {
                        sink.send(BackendEvent::Error(format!("cannot open {url}: {e}")));
                    }
                }
                Command::SaveKey { source, key } => {
                    let saved = bmodels_model::SecretName::api_key(&source)
                        .map_err(|e| e.to_string())
                        .and_then(|name| ctx.vault().set(&name, key).map_err(|e| e.to_string()));
                    if let Err(e) = saved {
                        sink.send(BackendEvent::Error(format!("saving the key: {e}")));
                        continue;
                    }
                    sink.send(BackendEvent::Info(format!(
                        "saved {source} key; syncing it"
                    )));
                    if syncing.replace(true) {
                        send_data(ctx, &store, sink).await;
                        continue;
                    }
                    let (ctx, store, sink, syncing) =
                        (ctx.clone(), store.clone(), sink.clone(), syncing.clone());
                    tokio::task::spawn_local(async move {
                        run_sync(&ctx, &store, &sink, vec![source]).await;
                        syncing.set(false);
                        send_data(&ctx, &store, &sink).await;
                    });
                }
            }
        }
    });
}

async fn send_data(ctx: &Context, store: &Store, sink: &EventSink) {
    match load_data(ctx, store).await {
        Ok(data) => sink.send(BackendEvent::Data(Arc::new(data))),
        Err(e) => sink.send(BackendEvent::Error(format!("{e:#}"))),
    };
}

async fn run_sync(ctx: &Context, store: &Store, sink: &EventSink, ids: Vec<String>) {
    let selection = if ids.is_empty() {
        Selection::Enabled
    } else {
        Selection::Ids(ids)
    };
    let http = match SwitchyHttp::new(ctx.timeout()) {
        Ok(http) => http,
        Err(e) => {
            sink.send(BackendEvent::SyncFinished(format!("sync failed: {e}")));
            return;
        }
    };
    let vault = ctx.vault();
    let secrets = VaultLookup::new(Some(&vault));
    let engine = Engine::new(ctx.config(), store, &http, &secrets);
    let progress = |p: &Progress| {
        let event = match p {
            Progress::Planned(ids) => BackendEvent::SyncStarted(ids.clone()),
            Progress::Started(id) => BackendEvent::SourceStarted(id.clone()),
            Progress::Finished(r) => BackendEvent::SourceFinished {
                source: r.source.clone(),
                ok: !matches!(r.outcome, SourceOutcome::Failed(_)),
                summary: format!("{}: {}", r.name, output::source_summary(r)),
            },
        };
        sink.send(event);
    };
    match engine.sync(&selection, &progress).await {
        Ok(report) => {
            // The user is looking at the inbox; don't let a later `watch` re-announce these.
            if let Ok(pending) = bmodels_engine::notices::pending(ctx.config(), store, now()).await
            {
                let _ = store.mark_notified(&pending.ids, now()).await;
            }
            let failed = report.failures().count();
            let mut summary = format!("sync done: {}", output::counts_summary(&report.totals()));
            if failed > 0 {
                let _ = write!(summary, ", {failed} source(s) failed (see Sources)");
            }
            sink.send(BackendEvent::SyncFinished(summary));
        }
        Err(e) => {
            sink.send(BackendEvent::SyncFinished(format!("sync failed: {e}")));
        }
    }
}
