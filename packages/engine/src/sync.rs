//! Sync orchestration: resolve keys, fetch sources concurrently, plan, and persist.

use std::collections::{BTreeMap, BTreeSet};

use bmodels_model::{Config, SourceSpec, SourceState, SyncRun, Timestamp};
use bmodels_sources::{
    FetchContext, FetchOutcome, Http, IdFilter, SecretLookup, SourceError, Validators,
};
use bmodels_store::{Store, StoreError};
use futures_util::StreamExt as _;

use crate::canonical::Canonicalizer;
use crate::diff::{Counts, PlanInput, canonical_keys, plan};
use crate::schedule;

/// Errors that abort a whole sync (per-source failures are reported, not raised).
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A requested source id is not configured.
    #[error("unknown source `{0}` (see `bmodels sources`)")]
    UnknownSource(String),
    /// The configuration is unusable.
    #[error("invalid configuration: {0}")]
    Config(String),
}

/// Which sources to sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Every enabled source.
    Enabled,
    /// These sources (even if disabled).
    Ids(Vec<String>),
    /// Enabled sources whose interval has elapsed.
    Due,
}

/// How one source's sync ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceOutcome {
    /// Fetched and diffed.
    Synced(Counts),
    /// The server said nothing changed.
    NotModified,
    /// Not attempted (no API key).
    Skipped(String),
    /// Failed.
    Failed(String),
}

/// Result of one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReport {
    /// Source id.
    pub source: String,
    /// Display name.
    pub name: String,
    /// Outcome.
    pub outcome: SourceOutcome,
    /// Non-fatal problems.
    pub warnings: Vec<String>,
    /// Start time.
    pub started_at: Timestamp,
    /// End time.
    pub finished_at: Timestamp,
}

/// Result of [`Engine::sync`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Per-source results, in completion order.
    pub sources: Vec<SourceReport>,
}

impl SyncReport {
    /// Sum of all synced sources' counts.
    #[must_use]
    pub fn totals(&self) -> Counts {
        let mut t = Counts::default();
        for s in &self.sources {
            if let SourceOutcome::Synced(c) = &s.outcome {
                t.fetched += c.fetched;
                t.new += c.new;
                t.new_listings += c.new_listings;
                t.changed += c.changed;
                t.removed += c.removed;
                t.reappeared += c.reappeared;
                t.baseline += c.baseline;
                t.present += c.present;
            }
        }
        t
    }

    /// Sources that failed.
    pub fn failures(&self) -> impl Iterator<Item = &SourceReport> {
        self.sources
            .iter()
            .filter(|s| matches!(s.outcome, SourceOutcome::Failed(_)))
    }
}

/// Progress callbacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// The sources about to be synced.
    Planned(Vec<String>),
    /// A source started.
    Started(String),
    /// A source finished.
    Finished(SourceReport),
}

/// Syncs sources into the store.
pub struct Engine<'a> {
    config: &'a Config,
    store: &'a Store,
    http: &'a dyn Http,
    secrets: &'a dyn SecretLookup,
    clock: fn() -> Timestamp,
}

impl std::fmt::Debug for Engine<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}

impl<'a> Engine<'a> {
    /// Creates an engine.
    #[must_use]
    pub fn new(
        config: &'a Config,
        store: &'a Store,
        http: &'a dyn Http,
        secrets: &'a dyn SecretLookup,
    ) -> Self {
        Self {
            config,
            store,
            http,
            secrets,
            clock: bmodels_model::now,
        }
    }

    /// Replaces the clock (tests).
    #[must_use]
    pub const fn with_clock(mut self, clock: fn() -> Timestamp) -> Self {
        self.clock = clock;
        self
    }

    /// Resolves a selection to source ids.
    ///
    /// # Errors
    ///
    /// * [`EngineError::UnknownSource`] for an unconfigured id
    /// * [`EngineError::Store`] if source states cannot be read
    pub async fn resolve(&self, selection: &Selection) -> Result<Vec<String>, EngineError> {
        Ok(match selection {
            Selection::Enabled => self
                .config
                .enabled_sources()
                .map(|(id, _)| id.clone())
                .collect(),
            Selection::Ids(ids) => {
                for id in ids {
                    if !self.config.sources.contains_key(id) {
                        return Err(EngineError::UnknownSource(id.clone()));
                    }
                }
                ids.iter()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            }
            Selection::Due => {
                let states = self.store.source_states().await?;
                schedule::due_now(self.config, &states, (self.clock)())
            }
        })
    }

    /// Syncs the selected sources, `settings.concurrency` at a time, persisting each as soon
    /// as it finishes.
    ///
    /// # Errors
    ///
    /// * [`EngineError::UnknownSource`] for an unconfigured id
    /// * [`EngineError::Config`] if the global filter is invalid
    /// * [`EngineError::Store`] if persisting fails (sources already persisted stay)
    pub async fn sync(
        &self,
        selection: &Selection,
        progress: &(dyn Fn(&Progress) + Sync),
    ) -> Result<SyncReport, EngineError> {
        let ids = self.resolve(selection).await?;
        progress(&Progress::Planned(ids.clone()));
        let global_filter = IdFilter::new(&[], &self.config.filters.exclude)
            .map_err(|e| EngineError::Config(format!("filters.exclude: {e}")))?;
        let canonicalizer = Canonicalizer::new(&self.config.canonical);
        let states = self.store.source_states().await?;

        let jobs = ids.into_iter().filter_map(|id| {
            let spec = self.config.sources.get(&id)?;
            let state = states.get(&id).cloned().unwrap_or_else(|| SourceState {
                id: id.clone(),
                ..SourceState::default()
            });
            Some((id, spec, state))
        });
        let mut fetches = futures_util::stream::iter(jobs)
            .map(|(id, spec, state)| async move {
                progress(&Progress::Started(id.clone()));
                let started = (self.clock)();
                let result = self.fetch_one(&id, spec, &state).await;
                (id, spec, state, started, result)
            })
            .buffer_unordered(self.config.settings.concurrency.max(1));

        let mut report = SyncReport::default();
        while let Some((id, spec, state, started, result)) = fetches.next().await {
            let source_report = self
                .persist(
                    &id,
                    spec,
                    state,
                    started,
                    result,
                    &canonicalizer,
                    &global_filter,
                )
                .await?;
            progress(&Progress::Finished(source_report.clone()));
            report.sources.push(source_report);
        }
        Ok(report)
    }

    /// Fingerprint of everything that shapes a source's stored listings. Headers are left out:
    /// they may hold secrets and do not shape listings.
    fn fingerprint(&self, spec: &SourceSpec) -> String {
        let mut spec = spec.clone();
        spec.headers.clear();
        let text = serde_json::to_string(&(&spec, &self.config.canonical, &self.config.filters))
            .unwrap_or_default();
        format!("{:016x}", crate::canonical::fnv1a(&text))
    }

    async fn fetch_one(
        &self,
        id: &str,
        spec: &SourceSpec,
        state: &SourceState,
    ) -> Result<FetchOutcome, SourceError> {
        let api_key = bmodels_sources::resolve_key(id, spec, self.secrets)?.map(|k| k.secret);
        let headers = spec
            .headers
            .iter()
            .map(|(name, value)| {
                bmodels_sources::resolve_value(value, self.secrets)
                    .map(|secret| (name.clone(), secret))
                    .map_err(|e| match e {
                        SourceError::Secret(detail) => {
                            SourceError::Secret(format!("headers.{name} ({detail})"))
                        }
                        other => other,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let validators = if state.fingerprint.as_deref() == Some(self.fingerprint(spec).as_str()) {
            Validators {
                etag: state.etag.clone(),
                last_modified: state.last_modified.clone(),
            }
        } else {
            Validators::default()
        };
        let ctx = FetchContext {
            http: self.http,
            api_key,
            headers,
            validators,
        };
        log::debug!("fetching {id}");
        let result = bmodels_sources::fetch(id, spec, &ctx).await;
        let secrets: Vec<_> = ctx
            .api_key
            .iter()
            .chain(ctx.headers.iter().map(|(_, s)| s))
            .collect();
        result.map_err(|e| match e {
            SourceError::Status { status, .. }
                if (status == 401 || status == 403) && ctx.api_key.is_some() =>
            {
                SourceError::KeyRejected {
                    source_id: id.to_owned(),
                    status,
                }
            }
            other => other.redacted(&secrets),
        })
    }

    #[allow(clippy::too_many_arguments)] // internal step; grouping would only add a struct
    async fn persist(
        &self,
        id: &str,
        spec: &SourceSpec,
        mut state: SourceState,
        started: Timestamp,
        result: Result<FetchOutcome, SourceError>,
        canonicalizer: &Canonicalizer,
        global_filter: &IdFilter,
    ) -> Result<SourceReport, EngineError> {
        let now = (self.clock)();
        let name = spec.display_name(id).to_owned();
        state.id = id.to_owned();
        state.kind = Some(spec.kind);
        state.last_attempt_at = Some(started);
        let mut report = SourceReport {
            source: id.to_owned(),
            name,
            outcome: SourceOutcome::NotModified,
            warnings: Vec::new(),
            started_at: started,
            finished_at: now,
        };
        let fetched = match result {
            Err(e) if e.is_skip() => {
                state.skipped = Some(e.to_string());
                state.last_error = None;
                self.store.record_attempt(&state, None).await?;
                report.outcome = SourceOutcome::Skipped(e.to_string());
                return Ok(report);
            }
            Err(e) => return self.fail(state, report, e.to_string()).await,
            Ok(FetchOutcome::NotModified) => {
                succeed(&mut state, now);
                self.store
                    .record_attempt(
                        &state,
                        Some(&run(id, started, now, None, &Counts::default())),
                    )
                    .await?;
                return Ok(report);
            }
            Ok(FetchOutcome::Fetched(fetched)) => fetched,
        };
        report.warnings.clone_from(&fetched.warnings);

        let existing = self.store.listings_of_source(id).await?;
        let mut keys = canonical_keys(canonicalizer, id, spec, &fetched.records);
        keys.extend(existing.iter().map(|l| l.canonical_key.clone()));
        let models: BTreeMap<_, _> = self.store.models_by_key(&keys).await?;
        let source_filter = match bmodels_sources::IdFilter::new(&spec.include, &spec.exclude) {
            Ok(f) => f,
            Err(e) => return self.fail(state, report, e.to_string()).await,
        };
        let planned = plan(&PlanInput {
            source_id: id,
            spec,
            now,
            state: &state,
            fetched: &fetched,
            canonicalizer,
            source_filter: &source_filter,
            global_filter,
            existing: &existing,
            models: &models,
            price_tolerance: self.config.settings.price_tolerance(),
        });
        let planned = match planned {
            Ok(p) => p,
            Err(e) => return self.fail(state, report, e.to_string()).await,
        };
        let mut write = planned.write;
        succeed(&mut state, now);
        state.baseline_at.get_or_insert(now);
        state.etag = fetched.validators.etag;
        state.last_modified = fetched.validators.last_modified;
        state.fingerprint = Some(self.fingerprint(spec));
        state.listing_count = planned.counts.present;
        write.state = state;
        write.run = Some(run(id, started, now, None, &planned.counts));
        self.store.apply_sync(&write).await?;
        report.outcome = SourceOutcome::Synced(planned.counts);
        Ok(report)
    }

    async fn fail(
        &self,
        mut state: SourceState,
        mut report: SourceReport,
        error: String,
    ) -> Result<SourceReport, EngineError> {
        log::info!("{}: {error}", report.source);
        state.last_error = Some(error.clone());
        state.skipped = None;
        state.failures = state.failures.saturating_add(1);
        let failed = run(
            &report.source,
            report.started_at,
            report.finished_at,
            Some(error.clone()),
            &Counts::default(),
        );
        self.store.record_attempt(&state, Some(&failed)).await?;
        report.outcome = SourceOutcome::Failed(error);
        Ok(report)
    }
}

fn succeed(state: &mut SourceState, now: Timestamp) {
    state.last_success_at = Some(now);
    state.last_error = None;
    state.skipped = None;
    state.failures = 0;
}

fn run(
    source: &str,
    started: Timestamp,
    finished: Timestamp,
    error: Option<String>,
    c: &Counts,
) -> SyncRun {
    SyncRun {
        source: source.to_owned(),
        started_at: started,
        finished_at: finished,
        error,
        fetched: c.fetched,
        new: c.new,
        new_listings: c.new_listings,
        changed: c.changed,
        removed: c.removed,
    }
}
