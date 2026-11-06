//! Row decoding.

use bmodels_model::{
    CanonicalModel, Event, EventKind, FieldChange, Listing, ListingKey, ListingStatus, ModelInfo,
    ModelStatus, RecordKind, SourceKind, SourceState, SyncRun,
};
use switchy_database::{DatabaseValue, Row};

use crate::StoreError;

const fn invalid(msg: String) -> StoreError {
    StoreError::InvalidData(msg)
}

pub fn opt_int(row: &Row, column: &str) -> Result<Option<i64>, StoreError> {
    match row.get(column) {
        None
        | Some(
            DatabaseValue::Null | DatabaseValue::Int64Opt(None) | DatabaseValue::Int32Opt(None),
        ) => Ok(None),
        Some(DatabaseValue::Int64(v) | DatabaseValue::Int64Opt(Some(v))) => Ok(Some(v)),
        Some(DatabaseValue::Int32(v) | DatabaseValue::Int32Opt(Some(v))) => Ok(Some(i64::from(v))),
        Some(DatabaseValue::UInt64(v)) => Ok(Some(i64::try_from(v).unwrap_or(i64::MAX))),
        Some(other) => Err(invalid(format!(
            "column {column} is not an integer: {other:?}"
        ))),
    }
}

pub fn int(row: &Row, column: &str) -> Result<i64, StoreError> {
    opt_int(row, column)?.ok_or_else(|| invalid(format!("column {column} is null")))
}

pub fn opt_text(row: &Row, column: &str) -> Result<Option<String>, StoreError> {
    match row.get(column) {
        None | Some(DatabaseValue::Null | DatabaseValue::StringOpt(None)) => Ok(None),
        Some(DatabaseValue::String(v) | DatabaseValue::StringOpt(Some(v))) => Ok(Some(v)),
        Some(other) => Err(invalid(format!("column {column} is not text: {other:?}"))),
    }
}

pub fn text(row: &Row, column: &str) -> Result<String, StoreError> {
    opt_text(row, column)?.ok_or_else(|| invalid(format!("column {column} is null")))
}

pub const fn bool_int(v: bool) -> i64 {
    if v { 1 } else { 0 }
}

fn count(row: &Row, column: &str) -> Result<u64, StoreError> {
    Ok(u64::try_from(int(row, column)?).unwrap_or(0))
}

pub fn source_from_row(row: &Row) -> Result<SourceState, StoreError> {
    Ok(SourceState {
        id: text(row, "id")?,
        kind: opt_text(row, "kind")?.and_then(|k| SourceKind::parse(&k)),
        baseline_at: opt_int(row, "baseline_at")?,
        last_attempt_at: opt_int(row, "last_attempt_at")?,
        last_success_at: opt_int(row, "last_success_at")?,
        last_error: opt_text(row, "last_error")?,
        skipped: opt_text(row, "skipped")?,
        failures: u32::try_from(int(row, "failures")?).unwrap_or(0),
        etag: opt_text(row, "etag")?,
        last_modified: opt_text(row, "last_modified")?,
        fingerprint: opt_text(row, "fingerprint")?,
        listing_count: count(row, "listing_count")?,
    })
}

pub fn canonical_from_row(row: &Row) -> Result<CanonicalModel, StoreError> {
    let status = text(row, "status")?;
    Ok(CanonicalModel {
        key: text(row, "key")?,
        vendor: text(row, "vendor")?,
        display_name: text(row, "display_name")?,
        kind: RecordKind::parse(&text(row, "kind")?),
        first_seen_at: int(row, "first_seen_at")?,
        created_at: opt_int(row, "created_at")?,
        status: ModelStatus::parse(&status)
            .ok_or_else(|| invalid(format!("unknown model status {status}")))?,
        acked_at: opt_int(row, "acked_at")?,
        note: opt_text(row, "note")?,
    })
}

pub fn listing_from_row(row: &Row) -> Result<Listing, StoreError> {
    let status = text(row, "status")?;
    let info: ModelInfo = serde_json::from_str(&text(row, "info")?)
        .map_err(|e| invalid(format!("listing info: {e}")))?;
    Ok(Listing {
        key: ListingKey::new(text(row, "source_id")?, text(row, "model_id")?),
        canonical_key: text(row, "canonical_key")?,
        kind: RecordKind::parse(&text(row, "kind")?),
        display_name: opt_text(row, "display_name")?,
        created_at: opt_int(row, "created_at")?,
        url: opt_text(row, "url")?,
        info,
        first_seen_at: int(row, "first_seen_at")?,
        removed_at: opt_int(row, "removed_at")?,
        baseline: int(row, "baseline")? != 0,
        status: ListingStatus::parse(&status)
            .ok_or_else(|| invalid(format!("unknown listing status {status}")))?,
        acked_at: opt_int(row, "acked_at")?,
    })
}

pub fn event_from_row(row: &Row) -> Result<Event, StoreError> {
    let kind = text(row, "kind")?;
    let changes: Vec<FieldChange> = serde_json::from_str(&text(row, "changes")?)
        .map_err(|e| invalid(format!("event changes: {e}")))?;
    Ok(Event {
        id: int(row, "id")?,
        at: int(row, "at")?,
        kind: EventKind::parse(&kind).ok_or_else(|| invalid(format!("unknown event {kind}")))?,
        canonical_key: text(row, "canonical_key")?,
        listing: ListingKey::new(text(row, "source_id")?, text(row, "model_id")?),
        changes,
        acked_at: opt_int(row, "acked_at")?,
        notified_at: opt_int(row, "notified_at")?,
    })
}

pub fn sync_run_from_row(row: &Row) -> Result<SyncRun, StoreError> {
    Ok(SyncRun {
        source: text(row, "source_id")?,
        started_at: int(row, "started_at")?,
        finished_at: int(row, "finished_at")?,
        error: opt_text(row, "error")?,
        fetched: count(row, "fetched")?,
        new: count(row, "new")?,
        new_listings: count(row, "new_listings")?,
        changed: count(row, "changed")?,
        removed: count(row, "removed")?,
    })
}
