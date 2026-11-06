//! Selects which new events deserve a notification.
//!
//! Every unnotified event is considered exactly once: it either becomes a [`Notice`] or is
//! marked notified silently (already acknowledged, kind not in `notify.kinds`, source has
//! `notify = false`, or older than [`STALE_SECS`]). The caller marks all returned ids after
//! delivering.

use std::collections::{BTreeMap, BTreeSet};

use bmodels_model::{CanonicalModel, Config, Event, Listing, Timestamp};
use bmodels_store::{EventQuery, Store, StoreError};

/// Unnotified events older than this are dropped rather than announced late.
pub const STALE_SECS: i64 = 86_400;

/// One event to announce.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    /// The event.
    pub event: Event,
    /// Its canonical model.
    pub model: Option<CanonicalModel>,
    /// Its listing.
    pub listing: Option<Listing>,
    /// Display name of the listing's source.
    pub source_name: String,
}

/// Pending notices plus every event id to mark notified afterwards.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pending {
    /// Events to announce, newest first.
    pub notices: Vec<Notice>,
    /// Ids of every considered event (announced or not).
    pub ids: Vec<i64>,
}

/// Collects pending notices.
///
/// # Errors
///
/// * [`StoreError`] on read failures
pub async fn pending(
    config: &Config,
    store: &Store,
    now: Timestamp,
) -> Result<Pending, StoreError> {
    let events = store
        .events(&EventQuery {
            unnotified: true,
            ..EventQuery::default()
        })
        .await?;
    let ids = events.iter().map(|e| e.id).collect();
    let wanted: Vec<Event> = events
        .into_iter()
        .filter(|e| {
            e.acked_at.is_none()
                && e.at >= now - STALE_SECS
                && config.notify.kinds.contains(&e.kind)
                && config
                    .sources
                    .get(&e.listing.source)
                    .is_none_or(|s| s.notify)
        })
        .collect();
    let keys: BTreeSet<String> = wanted.iter().map(|e| e.canonical_key.clone()).collect();
    let models: BTreeMap<String, CanonicalModel> = store.models_by_key(&keys).await?;
    let mut notices = Vec::with_capacity(wanted.len());
    for event in wanted {
        let listing = store.listing(&event.listing).await?;
        let source_name = config.sources.get(&event.listing.source).map_or_else(
            || event.listing.source.clone(),
            |s| s.display_name(&event.listing.source).to_owned(),
        );
        notices.push(Notice {
            model: models.get(&event.canonical_key).cloned(),
            listing,
            source_name,
            event,
        });
    }
    Ok(Pending { notices, ids })
}
