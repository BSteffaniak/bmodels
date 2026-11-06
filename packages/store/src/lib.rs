#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Persistent storage (SQLite via `switchy_database`) for models, listings, events, and
//! per-source sync state.
//!
//! # Schema (v1)
//!
//! * `bmodels_schema(contract_id = 1, schema_version)`: the schema contract row
//! * `sources(id PK, kind, baseline_at, last_attempt_at, last_success_at, last_error, skipped,
//!   failures, etag, last_modified, fingerprint, listing_count)`
//! * `canonical_models(key PK, vendor, display_name, kind, first_seen_at, created_at, status,
//!   acked_at, note)`
//! * `listings(source_id, model_id, canonical_key, kind, display_name, created_at, url, info
//!   JSON, first_seen_at, removed_at, baseline, status, acked_at)`, keyed by
//!   `(source_id, model_id)`
//! * `events(id, at, kind, canonical_key, source_id, model_id, changes JSON, acked_at,
//!   notified_at)`
//! * `sync_runs(id, source_id, started_at, finished_at, error, fetched, new, new_listings,
//!   changed, removed)`
//!
//! A sync's writes ([`SyncWrite`]) and every acknowledgement ([`Store::ack`]) are applied in
//! one transaction each. The database runs in WAL mode so a background `bmodels watch` and the
//! TUI can share it.

mod rows;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use bmodels_model::{
    AckTarget, CanonicalModel, Event, EventKind, FieldChange, Listing, ListingKey, ListingStatus,
    ModelStatus, SourceState, SyncRun, Timestamp,
};
use switchy_database::query::{FilterableQuery as _, SortDirection};
use switchy_database::schema::{Column, DataType, create_index, create_table};
use switchy_database::{Database, DatabaseValue, SqliteJournalMode};
use switchy_database_connection::{SqliteAccess, SqliteOpenOptions, open_sqlite_rusqlite};

use crate::rows::{
    bool_int, canonical_from_row, event_from_row, int, listing_from_row, source_from_row,
    sync_run_from_row,
};

/// Latest schema version understood by this build.
pub const SCHEMA_VERSION: i64 = 1;

/// Errors from the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Opening or configuring the SQLite connection failed.
    #[error("cannot open database: {0}")]
    Open(#[from] switchy_database_connection::InitSqliteRusqliteError),
    /// Creating the data directory failed.
    #[error("cannot create data directory: {0}")]
    Io(#[from] std::io::Error),
    /// A database operation failed.
    #[error("database error: {0}")]
    Database(#[from] switchy_database::DatabaseError),
    /// The database was written by a newer bmodels.
    #[error("database schema version {found} is newer than supported version {supported}")]
    FutureSchema {
        /// Version found in the file.
        found: i64,
        /// Newest version this build supports.
        supported: i64,
    },
    /// A row did not have the expected shape.
    #[error("invalid stored data: {0}")]
    InvalidData(String),
    /// The target of an operation does not exist.
    #[error("not found: {0}")]
    NotFound(String),
}

/// An event to insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    /// Detection time.
    pub at: Timestamp,
    /// What happened.
    pub kind: EventKind,
    /// Canonical model.
    pub canonical_key: String,
    /// Listing.
    pub listing: ListingKey,
    /// Tracked changes ([`EventKind::Changed`]).
    pub changes: Vec<FieldChange>,
    /// Pre-acknowledged (events of ignored models).
    pub acked_at: Option<Timestamp>,
}

/// Everything one successful sync of one source writes, applied atomically.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncWrite {
    /// Canonical models to insert or update (display name / created date).
    pub canonical_models: Vec<CanonicalModel>,
    /// Listings to insert or replace.
    pub listings: Vec<Listing>,
    /// Events to insert.
    pub events: Vec<NewEvent>,
    /// The source's new state.
    pub state: SourceState,
    /// The run record.
    pub run: Option<SyncRun>,
    /// Listings whose canonical key changed (rules changed): `(listing, old key)`. Their
    /// events follow them, and old models left without listings are deleted.
    pub moved: Vec<(ListingKey, String)>,
}

/// Counts of what an acknowledgement changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AckOutcome {
    /// Events acknowledged.
    pub events: usize,
    /// Listings marked seen.
    pub listings: usize,
    /// Canonical models marked seen.
    pub models: usize,
}

impl AckOutcome {
    /// Whether anything changed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.events == 0 && self.listings == 0 && self.models == 0
    }
}

/// Filter for [`Store::events`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventQuery {
    /// Only unacknowledged events.
    pub unacked: bool,
    /// Only events never notified.
    pub unnotified: bool,
    /// Only events at or after this time.
    pub since: Option<Timestamp>,
    /// Only events of this canonical model.
    pub canonical_key: Option<String>,
    /// Only events of this source.
    pub source: Option<String>,
    /// Newest first, at most this many.
    pub limit: Option<usize>,
}

/// Everything the UI shows, read in one go.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Every canonical model.
    pub models: Vec<CanonicalModel>,
    /// Every listing.
    pub listings: Vec<Listing>,
    /// Unacknowledged events plus the most recent acknowledged ones, newest first.
    pub events: Vec<Event>,
    /// Sync state per source id.
    pub sources: BTreeMap<String, SourceState>,
}

/// Handle to the database.
#[derive(Debug)]
pub struct Store {
    db: Box<dyn Database>,
}

impl Store {
    /// Opens (creating it and its directory if needed) the database at `path`, enables WAL,
    /// and applies migrations.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Io`] if the parent directory cannot be created
    /// * [`StoreError::Open`] if the file cannot be opened or configured
    /// * [`StoreError::Database`] if a migration fails
    /// * [`StoreError::FutureSchema`] if the database was written by a newer version
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = open_connection(path)?;
        db.sqlite_journal_mode(SqliteJournalMode::Wal).await?;
        Self::from_database(Box::new(db)).await
    }

    /// Opens a private in-memory database (tests, dry runs).
    ///
    /// # Errors
    ///
    /// * [`StoreError::Open`] if SQLite fails to initialize
    /// * [`StoreError::Database`] if creating the schema fails
    pub async fn open_in_memory() -> Result<Self, StoreError> {
        Self::from_database(Box::new(open_connection(Path::new(":memory:"))?)).await
    }

    /// Wraps an existing connection and applies migrations.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] if a migration fails
    /// * [`StoreError::FutureSchema`] if the database was written by a newer version
    pub async fn from_database(db: Box<dyn Database>) -> Result<Self, StoreError> {
        migrate(&*db).await?;
        Ok(Self { db })
    }

    // ---------------------------------------------------------------- reads

    /// Sync state of every source that has ever been attempted.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn source_states(&self) -> Result<BTreeMap<String, SourceState>, StoreError> {
        let rows = self.db.select("sources").execute(&*self.db).await?;
        rows.iter()
            .map(|r| source_from_row(r).map(|s| (s.id.clone(), s)))
            .collect()
    }

    /// Sync state of one source (default state if never attempted).
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn source_state(&self, id: &str) -> Result<SourceState, StoreError> {
        let row = self
            .db
            .select("sources")
            .where_eq("id", id)
            .execute_first(&*self.db)
            .await?;
        row.map_or_else(
            || {
                Ok(SourceState {
                    id: id.to_owned(),
                    ..SourceState::default()
                })
            },
            |r| source_from_row(&r),
        )
    }

    /// Every listing of `source` (present and removed).
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn listings_of_source(&self, source: &str) -> Result<Vec<Listing>, StoreError> {
        let rows = self
            .db
            .select("listings")
            .where_eq("source_id", source)
            .execute(&*self.db)
            .await?;
        rows.iter().map(listing_from_row).collect()
    }

    /// Every listing.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn listings(&self) -> Result<Vec<Listing>, StoreError> {
        let rows = self.db.select("listings").execute(&*self.db).await?;
        rows.iter().map(listing_from_row).collect()
    }

    /// Listings of one canonical model.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn listings_of_model(&self, key: &str) -> Result<Vec<Listing>, StoreError> {
        let rows = self
            .db
            .select("listings")
            .where_eq("canonical_key", key)
            .execute(&*self.db)
            .await?;
        rows.iter().map(listing_from_row).collect()
    }

    /// One listing.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn listing(&self, key: &ListingKey) -> Result<Option<Listing>, StoreError> {
        let row = self
            .db
            .select("listings")
            .where_eq("source_id", key.source.as_str())
            .where_eq("model_id", key.model_id.as_str())
            .execute_first(&*self.db)
            .await?;
        row.as_ref().map(listing_from_row).transpose()
    }

    /// Every canonical model.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn models(&self) -> Result<Vec<CanonicalModel>, StoreError> {
        let rows = self
            .db
            .select("canonical_models")
            .execute(&*self.db)
            .await?;
        rows.iter().map(canonical_from_row).collect()
    }

    /// Canonical models with these keys.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn models_by_key(
        &self,
        keys: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, CanonicalModel>, StoreError> {
        let mut out = BTreeMap::new();
        let keys: Vec<&String> = keys.iter().collect();
        for chunk in keys.chunks(400) {
            let values: Vec<DatabaseValue> = chunk
                .iter()
                .map(|k| DatabaseValue::String((*k).clone()))
                .collect();
            let rows = self
                .db
                .select("canonical_models")
                .where_in("key", values)
                .execute(&*self.db)
                .await?;
            for row in &rows {
                let model = canonical_from_row(row)?;
                out.insert(model.key.clone(), model);
            }
        }
        Ok(out)
    }

    /// One canonical model.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn model(&self, key: &str) -> Result<Option<CanonicalModel>, StoreError> {
        let row = self
            .db
            .select("canonical_models")
            .where_eq("key", key)
            .execute_first(&*self.db)
            .await?;
        row.as_ref().map(canonical_from_row).transpose()
    }

    /// Events matching `query`, newest first.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn events(&self, query: &EventQuery) -> Result<Vec<Event>, StoreError> {
        let mut select = self
            .db
            .select("events")
            .sort("at", SortDirection::Desc)
            .sort("id", SortDirection::Desc);
        if query.unacked {
            select = select.where_eq("acked_at", DatabaseValue::Null);
        }
        if query.unnotified {
            select = select.where_eq("notified_at", DatabaseValue::Null);
        }
        if let Some(since) = query.since {
            select = select.where_gte("at", since);
        }
        if let Some(key) = &query.canonical_key {
            select = select.where_eq("canonical_key", key.as_str());
        }
        if let Some(source) = &query.source {
            select = select.where_eq("source_id", source.as_str());
        }
        if let Some(limit) = query.limit {
            select = select.limit(limit);
        }
        let rows = select.execute(&*self.db).await?;
        rows.iter().map(event_from_row).collect()
    }

    /// Recent sync runs, newest first.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn sync_runs(
        &self,
        source: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SyncRun>, StoreError> {
        let mut select = self
            .db
            .select("sync_runs")
            .sort("started_at", SortDirection::Desc)
            .sort("id", SortDirection::Desc)
            .limit(limit);
        if let Some(source) = source {
            select = select.where_eq("source_id", source);
        }
        let rows = select.execute(&*self.db).await?;
        rows.iter().map(sync_run_from_row).collect()
    }

    /// Everything the UI needs: all models and listings, every unacknowledged event plus the
    /// `history` most recent events, and all source states.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] / [`StoreError::InvalidData`] on read failures
    pub async fn snapshot(&self, history: usize) -> Result<Snapshot, StoreError> {
        let mut events = self
            .events(&EventQuery {
                unacked: true,
                ..EventQuery::default()
            })
            .await?;
        let ids: BTreeSet<i64> = events.iter().map(|e| e.id).collect();
        let recent = self
            .events(&EventQuery {
                limit: Some(history),
                ..EventQuery::default()
            })
            .await?;
        events.extend(recent.into_iter().filter(|e| !ids.contains(&e.id)));
        events.sort_by_key(|e| std::cmp::Reverse((e.at, e.id)));
        Ok(Snapshot {
            models: self.models().await?,
            listings: self.listings().await?,
            events,
            sources: self.source_states().await?,
        })
    }

    // ---------------------------------------------------------------- writes

    /// Applies one sync's writes in a transaction.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] if a write fails (nothing is applied)
    pub async fn apply_sync(&self, write: &SyncWrite) -> Result<(), StoreError> {
        let tx = self.db.begin_transaction().await?;
        let db: &dyn Database = &*tx;
        for model in &write.canonical_models {
            upsert_model(db, model).await?;
        }
        for listing in &write.listings {
            upsert_listing(db, listing).await?;
        }
        for event in &write.events {
            insert_event(db, event).await?;
        }
        for (listing, old_key) in &write.moved {
            let new_key = write
                .listings
                .iter()
                .find(|l| &l.key == listing)
                .map(|l| l.canonical_key.as_str());
            if let Some(new_key) = new_key {
                db.update("events")
                    .where_eq("source_id", listing.source.as_str())
                    .where_eq("model_id", listing.model_id.as_str())
                    .value("canonical_key", new_key)
                    .execute(db)
                    .await?;
            }
            let remaining = db
                .select("listings")
                .columns(&["model_id"])
                .where_eq("canonical_key", old_key.as_str())
                .limit(1)
                .execute(db)
                .await?;
            if remaining.is_empty() {
                db.delete("canonical_models")
                    .where_eq("key", old_key.as_str())
                    .execute(db)
                    .await?;
            }
        }
        upsert_source(db, &write.state).await?;
        if let Some(run) = &write.run {
            insert_run(db, run).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Records a failed or skipped attempt (state only, plus a run record for failures).
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] if a write fails
    pub async fn record_attempt(
        &self,
        state: &SourceState,
        run: Option<&SyncRun>,
    ) -> Result<(), StoreError> {
        let tx = self.db.begin_transaction().await?;
        upsert_source(&*tx, state).await?;
        if let Some(run) = run {
            insert_run(&*tx, run).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Acknowledges `target` at time `at`, cascading as described in
    /// [`bmodels_model::state`].
    ///
    /// # Errors
    ///
    /// * [`StoreError::NotFound`] if a named model, listing, or event does not exist
    /// * [`StoreError::Database`] if a write fails (nothing is applied)
    pub async fn ack(&self, target: &AckTarget, at: Timestamp) -> Result<AckOutcome, StoreError> {
        let tx = self.db.begin_transaction().await?;
        let db: &dyn Database = &*tx;
        let mut outcome = AckOutcome::default();
        match target {
            AckTarget::Model(key) => ack_model(db, key, at, &mut outcome).await?,
            AckTarget::Listing(listing) => ack_listing(db, listing, at, &mut outcome).await?,
            AckTarget::Event(id) => {
                let row = db
                    .select("events")
                    .where_eq("id", *id)
                    .execute_first(db)
                    .await?
                    .ok_or_else(|| StoreError::NotFound(format!("event {id}")))?;
                ack_event(db, &event_from_row(&row)?, at, &mut outcome).await?;
            }
            AckTarget::All { source, vendor } => {
                let mut select = db
                    .select("events")
                    .where_eq("acked_at", DatabaseValue::Null);
                if let Some(source) = source {
                    select = select.where_eq("source_id", source.as_str());
                }
                let rows = select.execute(db).await?;
                let events: Vec<Event> =
                    rows.iter().map(event_from_row).collect::<Result<_, _>>()?;
                let vendors = if vendor.is_some() {
                    vendor_of(db, events.iter().map(|e| e.canonical_key.clone()).collect()).await?
                } else {
                    BTreeMap::new()
                };
                for event in &events {
                    if let Some(v) = vendor
                        && vendors.get(&event.canonical_key) != Some(v)
                    {
                        continue;
                    }
                    ack_event(db, event, at, &mut outcome).await?;
                }
            }
        }
        tx.commit().await?;
        Ok(outcome)
    }

    /// Sets a canonical model's status. Setting `SEEN`/`STARRED`/`IGNORED` also acknowledges
    /// it like [`AckTarget::Model`]; `UNSEEN` marks it (and its listings) unseen again.
    ///
    /// # Errors
    ///
    /// * [`StoreError::NotFound`] if the model does not exist
    /// * [`StoreError::Database`] if a write fails
    pub async fn set_status(
        &self,
        key: &str,
        status: ModelStatus,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let tx = self.db.begin_transaction().await?;
        let db: &dyn Database = &*tx;
        if db
            .select("canonical_models")
            .where_eq("key", key)
            .execute_first(db)
            .await?
            .is_none()
        {
            return Err(StoreError::NotFound(format!("model {key}")));
        }
        if status == ModelStatus::Unseen {
            db.update("canonical_models")
                .where_eq("key", key)
                .value("status", status.as_str())
                .value("acked_at", DatabaseValue::Null)
                .execute(db)
                .await?;
            db.update("listings")
                .where_eq("canonical_key", key)
                .value("status", ListingStatus::Unseen.as_str())
                .value("acked_at", DatabaseValue::Null)
                .execute(db)
                .await?;
        } else {
            let mut outcome = AckOutcome::default();
            ack_model(db, key, at, &mut outcome).await?;
            db.update("canonical_models")
                .where_eq("key", key)
                .value("status", status.as_str())
                .execute(db)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Sets or clears a canonical model's note.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] if the write fails
    pub async fn set_note(&self, key: &str, note: Option<&str>) -> Result<(), StoreError> {
        self.db
            .update("canonical_models")
            .where_eq("key", key)
            .value("note", note.map(str::to_owned))
            .execute(&*self.db)
            .await?;
        Ok(())
    }

    /// Marks events as notified.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Database`] if a write fails
    pub async fn mark_notified(&self, ids: &[i64], at: Timestamp) -> Result<(), StoreError> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.db.begin_transaction().await?;
        for chunk in ids.chunks(400) {
            tx.update("events")
                .where_in("id", chunk.to_vec())
                .value("notified_at", at)
                .execute(&*tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

// -------------------------------------------------------------------- ack cascade

async fn ack_model(
    db: &dyn Database,
    key: &str,
    at: Timestamp,
    outcome: &mut AckOutcome,
) -> Result<(), StoreError> {
    let row = db
        .select("canonical_models")
        .where_eq("key", key)
        .execute_first(db)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("model {key}")))?;
    if canonical_from_row(&row)?.status == ModelStatus::Unseen {
        db.update("canonical_models")
            .where_eq("key", key)
            .value("status", ModelStatus::Seen.as_str())
            .value("acked_at", at)
            .execute(db)
            .await?;
        outcome.models += 1;
    }
    outcome.listings += update_count(
        db,
        &db.update("listings")
            .where_eq("canonical_key", key)
            .where_eq("status", ListingStatus::Unseen.as_str())
            .value("status", ListingStatus::Seen.as_str())
            .value("acked_at", at),
    )
    .await?;
    outcome.events += update_count(
        db,
        &db.update("events")
            .where_eq("canonical_key", key)
            .where_eq("acked_at", DatabaseValue::Null)
            .value("acked_at", at),
    )
    .await?;
    Ok(())
}

async fn ack_listing(
    db: &dyn Database,
    key: &ListingKey,
    at: Timestamp,
    outcome: &mut AckOutcome,
) -> Result<(), StoreError> {
    let row = db
        .select("listings")
        .where_eq("source_id", key.source.as_str())
        .where_eq("model_id", key.model_id.as_str())
        .execute_first(db)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("listing {key}")))?;
    let listing = listing_from_row(&row)?;
    if listing.status == ListingStatus::Unseen {
        db.update("listings")
            .where_eq("source_id", key.source.as_str())
            .where_eq("model_id", key.model_id.as_str())
            .value("status", ListingStatus::Seen.as_str())
            .value("acked_at", at)
            .execute(db)
            .await?;
        outcome.listings += 1;
    }
    outcome.events += update_count(
        db,
        &db.update("events")
            .where_eq("source_id", key.source.as_str())
            .where_eq("model_id", key.model_id.as_str())
            .where_eq("acked_at", DatabaseValue::Null)
            .value("acked_at", at),
    )
    .await?;
    settle_model(db, &listing.canonical_key, at, outcome).await
}

async fn ack_event(
    db: &dyn Database,
    event: &Event,
    at: Timestamp,
    outcome: &mut AckOutcome,
) -> Result<(), StoreError> {
    if event.acked_at.is_none() {
        db.update("events")
            .where_eq("id", event.id)
            .value("acked_at", at)
            .execute(db)
            .await?;
        outcome.events += 1;
    }
    if matches!(event.kind, EventKind::New | EventKind::NewListing) {
        match ack_listing(db, &event.listing, at, outcome).await {
            Ok(()) | Err(StoreError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Marks a canonical model seen once none of its listings is unseen.
async fn settle_model(
    db: &dyn Database,
    key: &str,
    at: Timestamp,
    outcome: &mut AckOutcome,
) -> Result<(), StoreError> {
    let unseen = db
        .select("listings")
        .columns(&["model_id"])
        .where_eq("canonical_key", key)
        .where_eq("status", ListingStatus::Unseen.as_str())
        .limit(1)
        .execute(db)
        .await?;
    if !unseen.is_empty() {
        return Ok(());
    }
    outcome.models += update_count(
        db,
        &db.update("canonical_models")
            .where_eq("key", key)
            .where_eq("status", ModelStatus::Unseen.as_str())
            .value("status", ModelStatus::Seen.as_str())
            .value("acked_at", at),
    )
    .await?;
    Ok(())
}

async fn vendor_of(
    db: &dyn Database,
    keys: BTreeSet<String>,
) -> Result<BTreeMap<String, String>, StoreError> {
    let mut out = BTreeMap::new();
    let keys: Vec<String> = keys.into_iter().collect();
    for chunk in keys.chunks(400) {
        let values: Vec<DatabaseValue> = chunk
            .iter()
            .map(|k| DatabaseValue::String(k.clone()))
            .collect();
        let rows = db
            .select("canonical_models")
            .columns(&["key", "vendor"])
            .where_in("key", values)
            .execute(db)
            .await?;
        for row in &rows {
            out.insert(rows::text(row, "key")?, rows::text(row, "vendor")?);
        }
    }
    Ok(out)
}

// -------------------------------------------------------------------- row writers

async fn upsert_model(db: &dyn Database, m: &CanonicalModel) -> Result<(), StoreError> {
    db.upsert("canonical_models")
        .where_eq("key", m.key.as_str())
        .value("key", m.key.as_str())
        .value("vendor", m.vendor.as_str())
        .value("display_name", m.display_name.as_str())
        .value("kind", m.kind.as_str())
        .value("first_seen_at", m.first_seen_at)
        .value("created_at", m.created_at)
        .value("status", m.status.as_str())
        .value("acked_at", m.acked_at)
        .value("note", m.note.clone())
        .execute(db)
        .await?;
    Ok(())
}

async fn upsert_listing(db: &dyn Database, l: &Listing) -> Result<(), StoreError> {
    let info = serde_json::to_string(&l.info)
        .map_err(|e| StoreError::InvalidData(format!("cannot encode listing info: {e}")))?;
    db.upsert("listings")
        .where_eq("source_id", l.key.source.as_str())
        .where_eq("model_id", l.key.model_id.as_str())
        .value("source_id", l.key.source.as_str())
        .value("model_id", l.key.model_id.as_str())
        .value("canonical_key", l.canonical_key.as_str())
        .value("kind", l.kind.as_str())
        .value("display_name", l.display_name.clone())
        .value("created_at", l.created_at)
        .value("url", l.url.clone())
        .value("info", info)
        .value("first_seen_at", l.first_seen_at)
        .value("removed_at", l.removed_at)
        .value("baseline", bool_int(l.baseline))
        .value("status", l.status.as_str())
        .value("acked_at", l.acked_at)
        .execute(db)
        .await?;
    Ok(())
}

async fn insert_event(db: &dyn Database, e: &NewEvent) -> Result<(), StoreError> {
    let changes = serde_json::to_string(&e.changes)
        .map_err(|err| StoreError::InvalidData(format!("cannot encode changes: {err}")))?;
    db.insert("events")
        .value("at", e.at)
        .value("kind", e.kind.as_str())
        .value("canonical_key", e.canonical_key.as_str())
        .value("source_id", e.listing.source.as_str())
        .value("model_id", e.listing.model_id.as_str())
        .value("changes", changes)
        .value("acked_at", e.acked_at)
        .value("notified_at", DatabaseValue::Null)
        .execute(db)
        .await?;
    Ok(())
}

async fn upsert_source(db: &dyn Database, s: &SourceState) -> Result<(), StoreError> {
    db.upsert("sources")
        .where_eq("id", s.id.as_str())
        .value("id", s.id.as_str())
        .value("kind", s.kind.map(|k| k.as_str().to_owned()))
        .value("baseline_at", s.baseline_at)
        .value("last_attempt_at", s.last_attempt_at)
        .value("last_success_at", s.last_success_at)
        .value("last_error", s.last_error.clone())
        .value("skipped", s.skipped.clone())
        .value("failures", i64::from(s.failures))
        .value("etag", s.etag.clone())
        .value("last_modified", s.last_modified.clone())
        .value("fingerprint", s.fingerprint.clone())
        .value(
            "listing_count",
            i64::try_from(s.listing_count).unwrap_or(i64::MAX),
        )
        .execute(db)
        .await?;
    Ok(())
}

async fn insert_run(db: &dyn Database, r: &SyncRun) -> Result<(), StoreError> {
    let n = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
    db.insert("sync_runs")
        .value("source_id", r.source.as_str())
        .value("started_at", r.started_at)
        .value("finished_at", r.finished_at)
        .value("error", r.error.clone())
        .value("fetched", n(r.fetched))
        .value("new", n(r.new))
        .value("new_listings", n(r.new_listings))
        .value("changed", n(r.changed))
        .value("removed", n(r.removed))
        .execute(db)
        .await?;
    Ok(())
}

async fn update_count(
    db: &dyn Database,
    statement: &switchy_database::query::UpdateStatement<'_>,
) -> Result<usize, StoreError> {
    Ok(usize::try_from(db.exec_update_count(statement).await?).unwrap_or(usize::MAX))
}

// -------------------------------------------------------------------- schema

fn open_connection(
    path: &Path,
) -> Result<switchy_database::rusqlite::RusqliteDatabase, StoreError> {
    let options = SqliteOpenOptions {
        access: SqliteAccess::ReadWriteCreate,
        allow_uri: false,
        busy_timeout: Duration::from_secs(10),
        foreign_keys: true,
        connections: std::num::NonZeroUsize::MIN,
    };
    Ok(open_sqlite_rusqlite(path, &options)?)
}

async fn schema_version(db: &dyn Database) -> Result<i64, StoreError> {
    if !db.table_exists("bmodels_schema").await? {
        return Ok(0);
    }
    let row = db
        .select("bmodels_schema")
        .where_eq("contract_id", 1_i64)
        .execute_first(db)
        .await?;
    row.map_or(Ok(0), |r| int(&r, "schema_version"))
}

async fn migrate(db: &dyn Database) -> Result<(), StoreError> {
    let version = schema_version(db).await?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::FutureSchema {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version < 1 {
        let tx = db.begin_transaction().await?;
        install_v1(&*tx).await?;
        tx.insert("bmodels_schema")
            .value("contract_id", 1_i64)
            .value("schema_version", 1_i64)
            .execute(&*tx)
            .await?;
        tx.commit().await?;
        log::debug!("bmodels schema created at v1");
    }
    // Future migrations: `if version < 2 { ...; update bmodels_schema.schema_version = 2 }`.
    Ok(())
}

fn column(name: &str, data_type: DataType, nullable: bool) -> Column {
    Column {
        name: name.into(),
        data_type,
        nullable,
        auto_increment: false,
        default: None,
    }
}

fn id_column() -> Column {
    Column {
        name: "id".into(),
        data_type: DataType::BigInt,
        nullable: false,
        auto_increment: true,
        default: None,
    }
}

async fn install_v1(db: &dyn Database) -> Result<(), StoreError> {
    use DataType::{BigInt, Int, Text};
    create_table("bmodels_schema")
        .if_not_exists(true)
        .column(column("contract_id", Int, false))
        .primary_key("contract_id")
        .check_integer_equals("contract_id", 1)
        .column(column("schema_version", Int, false))
        .execute(db)
        .await?;
    create_table("sources")
        .if_not_exists(true)
        .column(column("id", Text, false))
        .primary_key("id")
        .column(column("kind", Text, true))
        .column(column("baseline_at", BigInt, true))
        .column(column("last_attempt_at", BigInt, true))
        .column(column("last_success_at", BigInt, true))
        .column(column("last_error", Text, true))
        .column(column("skipped", Text, true))
        .column(column("failures", Int, false))
        .column(column("etag", Text, true))
        .column(column("last_modified", Text, true))
        .column(column("fingerprint", Text, true))
        .column(column("listing_count", BigInt, false))
        .execute(db)
        .await?;
    create_table("canonical_models")
        .if_not_exists(true)
        .column(column("key", Text, false))
        .primary_key("key")
        .column(column("vendor", Text, false))
        .column(column("display_name", Text, false))
        .column(column("kind", Text, false))
        .column(column("first_seen_at", BigInt, false))
        .column(column("created_at", BigInt, true))
        .column(column("status", Text, false))
        .column(column("acked_at", BigInt, true))
        .column(column("note", Text, true))
        .execute(db)
        .await?;
    create_table("listings")
        .if_not_exists(true)
        .column(column("source_id", Text, false))
        .column(column("model_id", Text, false))
        .column(column("canonical_key", Text, false))
        .column(column("kind", Text, false))
        .column(column("display_name", Text, true))
        .column(column("created_at", BigInt, true))
        .column(column("url", Text, true))
        .column(column("info", Text, false))
        .column(column("first_seen_at", BigInt, false))
        .column(column("removed_at", BigInt, true))
        .column(column("baseline", Int, false))
        .column(column("status", Text, false))
        .column(column("acked_at", BigInt, true))
        .primary_key_columns(["source_id", "model_id"])
        .execute(db)
        .await?;
    create_index("listings_canonical_key")
        .if_not_exists(true)
        .table("listings")
        .column("canonical_key")
        .execute(db)
        .await?;
    install_v1_history(db).await
}

async fn install_v1_history(db: &dyn Database) -> Result<(), StoreError> {
    use DataType::{BigInt, Text};
    create_table("events")
        .if_not_exists(true)
        .column(id_column())
        .primary_key("id")
        .column(column("at", BigInt, false))
        .column(column("kind", Text, false))
        .column(column("canonical_key", Text, false))
        .column(column("source_id", Text, false))
        .column(column("model_id", Text, false))
        .column(column("changes", Text, false))
        .column(column("acked_at", BigInt, true))
        .column(column("notified_at", BigInt, true))
        .execute(db)
        .await?;
    for (name, col) in [
        ("events_at", "at"),
        ("events_canonical_key", "canonical_key"),
        ("events_acked_at", "acked_at"),
    ] {
        create_index(name)
            .if_not_exists(true)
            .table("events")
            .column(col)
            .execute(db)
            .await?;
    }
    create_table("sync_runs")
        .if_not_exists(true)
        .column(id_column())
        .primary_key("id")
        .column(column("source_id", Text, false))
        .column(column("started_at", BigInt, false))
        .column(column("finished_at", BigInt, false))
        .column(column("error", Text, true))
        .column(column("fetched", BigInt, false))
        .column(column("new", BigInt, false))
        .column(column("new_listings", BigInt, false))
        .column(column("changed", BigInt, false))
        .column(column("removed", BigInt, false))
        .execute(db)
        .await?;
    create_index("sync_runs_started_at")
        .if_not_exists(true)
        .table("sync_runs")
        .column("started_at")
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
