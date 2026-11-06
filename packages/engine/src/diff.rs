//! The pure heart of a sync: given what a source returned and what the store knows, decide
//! which listings, canonical models, and events to write.
//!
//! Rules for a record not seen before in this source:
//!
//! * **baseline**: on the source's first successful sync (when `baseline = true`), and for
//!   windowed sources' records created before the baseline, record it silently as seen
//! * otherwise, if its canonical model is new: create it unseen and emit `NEW`
//! * otherwise emit `NEW_LISTING`, pre-acknowledged when the model is still unseen (its `NEW`
//!   is already in the inbox) or ignored
//!
//! For a known listing: `REAPPEARED` if it had been removed, `CHANGED` if tracked fields
//! differ, silent update for everything else. Listings missing from a *complete* response
//! are `REMOVED`. Events of ignored models are always pre-acknowledged.

use std::collections::{BTreeMap, BTreeSet};

use bmodels_model::{
    CanonicalModel, EventKind, FieldChange, Listing, ListingKey, ListingStatus, ModelRecord,
    ModelStatus, SourceSpec, SourceState, Timestamp,
};
use bmodels_sources::{Fetched, IdFilter};
use bmodels_store::{NewEvent, SyncWrite};

use crate::canonical::{Canonical, Canonicalizer};

/// Windowed-source records created this long before the baseline still count as baseline
/// (registries report creation dates loosely).
pub const WINDOW_GRACE_SECS: i64 = 86_400;

/// Inputs of [`plan`].
#[derive(Debug, Clone, Copy)]
pub struct PlanInput<'a> {
    /// Source id.
    pub source_id: &'a str,
    /// Source spec.
    pub spec: &'a SourceSpec,
    /// Time stamped on new rows and events.
    pub now: Timestamp,
    /// The source's state before this sync.
    pub state: &'a SourceState,
    /// What the source returned (already filtered by the source's include/exclude).
    pub fetched: &'a Fetched,
    /// Canonicalization rules.
    pub canonicalizer: &'a Canonicalizer,
    /// The source's include/exclude filter (to leave filtered-out stored listings alone).
    pub source_filter: &'a IdFilter,
    /// Global `[filters]`, matched against model ids and canonical keys.
    pub global_filter: &'a IdFilter,
    /// Every stored listing of this source.
    pub existing: &'a [Listing],
    /// Stored canonical models, at least those of [`canonical_keys`] and of `existing`.
    pub models: &'a BTreeMap<String, CanonicalModel>,
    /// Relative price move below which a price change is silent (`0.05` = 5%).
    pub price_tolerance: f64,
}

/// What a sync found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Records kept after all filters.
    pub fetched: u64,
    /// `NEW` events.
    pub new: u64,
    /// `NEW_LISTING` events.
    pub new_listings: u64,
    /// `CHANGED` events.
    pub changed: u64,
    /// `REMOVED` events.
    pub removed: u64,
    /// `REAPPEARED` events.
    pub reappeared: u64,
    /// Records recorded silently as baseline.
    pub baseline: u64,
    /// Listings present after the sync.
    pub present: u64,
}

/// Output of [`plan`]: the writes (without source state and run record) and counts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// Writes; `state` and `run` are left for the caller.
    pub write: SyncWrite,
    /// Counts.
    pub counts: Counts,
}

/// Errors that make a response untrustworthy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// A complete source returned nothing while listings are present.
    #[error("returned no models; refusing to mark {0} listing(s) removed")]
    EmptyResponse(usize),
}

/// Canonical keys of `records` (to preload their models).
#[must_use]
pub fn canonical_keys(
    canonicalizer: &Canonicalizer,
    source_id: &str,
    spec: &SourceSpec,
    records: &[ModelRecord],
) -> BTreeSet<String> {
    records
        .iter()
        .map(|r| canonicalizer.canonicalize(source_id, spec, r).key)
        .collect()
}

/// Plans one source's writes.
///
/// # Errors
///
/// * [`PlanError::EmptyResponse`] if a complete response is empty while listings are present
pub fn plan(input: &PlanInput<'_>) -> Result<Plan, PlanError> {
    let PlanInput {
        source_id,
        spec,
        now,
        state,
        fetched,
        canonicalizer,
        source_filter,
        global_filter,
        existing,
        models,
        price_tolerance,
    } = *input;

    let untouched = |l: &Listing| {
        !source_filter.keep(&l.key.model_id)
            || global_filter.excludes_any(&[&l.key.model_id, &l.canonical_key])
    };
    let by_id: BTreeMap<&str, &Listing> = existing
        .iter()
        .map(|l| (l.key.model_id.as_str(), l))
        .collect();

    let records: Vec<(&ModelRecord, Canonical)> = fetched
        .records
        .iter()
        .map(|r| (r, canonicalizer.canonicalize(source_id, spec, r)))
        .filter(|(r, c)| !global_filter.excludes_any(&[&r.model_id, &c.key]))
        .collect();

    let watched_present = existing
        .iter()
        .filter(|l| l.is_present() && !untouched(l))
        .count();
    if fetched.complete && records.is_empty() && watched_present > 0 {
        return Err(PlanError::EmptyResponse(watched_present));
    }

    let mut planner = Planner {
        source_id,
        spec,
        now,
        models,
        price_tolerance,
        dirty: BTreeMap::new(),
        listings: Vec::new(),
        events: Vec::new(),
        moved: Vec::new(),
        counts: Counts {
            fetched: records.len() as u64,
            ..Counts::default()
        },
    };
    let first_sync = state.baseline_at.is_none() && spec.baseline;
    let mut seen_ids = BTreeSet::new();
    for (record, canonical) in &records {
        seen_ids.insert(record.model_id.as_str());
        match by_id.get(record.model_id.as_str()) {
            None => {
                let silent = first_sync
                    || (!fetched.complete
                        && state.baseline_at.is_some_and(|b| {
                            record.created_at.is_some_and(|c| c < b - WINDOW_GRACE_SECS)
                        }));
                planner.add(record, canonical, silent);
            }
            Some(old) => planner.update(old, record, canonical),
        }
    }
    if fetched.complete {
        for old in existing {
            if old.is_present() && !untouched(old) && !seen_ids.contains(old.key.model_id.as_str())
            {
                planner.remove(old);
            }
        }
    }

    // Present after the sync: every stored listing overlaid with the rewritten ones.
    let mut present: BTreeMap<&str, bool> = existing
        .iter()
        .map(|l| (l.key.model_id.as_str(), l.is_present()))
        .collect();
    for l in &planner.listings {
        present.insert(l.key.model_id.as_str(), l.is_present());
    }
    planner.counts.present = present.values().filter(|p| **p).count() as u64;

    let counts = planner.counts;
    Ok(Plan {
        write: SyncWrite {
            canonical_models: planner.dirty.into_values().collect(),
            listings: planner.listings,
            events: planner.events,
            moved: planner.moved,
            ..SyncWrite::default()
        },
        counts,
    })
}

/// Whether a name is just an id (`gpt-4o`, `mistral-large-2512`) rather than a human name.
fn looks_like_id(name: &str) -> bool {
    !name.chars().any(|c| c.is_whitespace() || c.is_uppercase())
}

struct Planner<'a> {
    source_id: &'a str,
    spec: &'a SourceSpec,
    now: Timestamp,
    models: &'a BTreeMap<String, CanonicalModel>,
    price_tolerance: f64,
    /// Canonical models created or changed by this plan.
    dirty: BTreeMap<String, CanonicalModel>,
    listings: Vec<Listing>,
    events: Vec<NewEvent>,
    moved: Vec<(ListingKey, String)>,
    counts: Counts,
}

impl Planner<'_> {
    fn model(&self, key: &str) -> Option<&CanonicalModel> {
        self.dirty.get(key).or_else(|| self.models.get(key))
    }

    fn event(
        &mut self,
        kind: EventKind,
        canonical_key: &str,
        listing: &ListingKey,
        changes: Vec<FieldChange>,
        acked: bool,
    ) {
        let ignored = self
            .model(canonical_key)
            .is_some_and(|m| m.status == ModelStatus::Ignored);
        self.events.push(NewEvent {
            at: self.now,
            kind,
            canonical_key: canonical_key.to_owned(),
            listing: listing.clone(),
            changes,
            acked_at: (acked || ignored).then_some(self.now),
        });
        let counter = match kind {
            EventKind::New => &mut self.counts.new,
            EventKind::NewListing => &mut self.counts.new_listings,
            EventKind::Changed => &mut self.counts.changed,
            EventKind::Removed => &mut self.counts.removed,
            EventKind::Reappeared => &mut self.counts.reappeared,
        };
        *counter += 1;
    }

    /// Creates the canonical model if missing; refreshes its name and creation date.
    fn touch_model(
        &mut self,
        record: &ModelRecord,
        canonical: &Canonical,
        status: ModelStatus,
    ) -> bool {
        let direct = self.spec.vendor.is_some();
        if let Some(existing) = self.model(&canonical.key).cloned() {
            let mut model = existing.clone();
            if canonical.exact
                && let Some(name) = &record.display_name
                && (direct || (looks_like_id(&model.display_name) && !looks_like_id(name)))
            {
                model.display_name.clone_from(name);
            }
            if let Some(created) = record.created_at
                && model.created_at.is_none_or(|c| created < c)
            {
                model.created_at = Some(created);
            }
            if model != existing {
                self.dirty.insert(model.key.clone(), model);
            }
            return false;
        }
        let fallback = record
            .model_id
            .rsplit('/')
            .next()
            .unwrap_or(&record.model_id);
        self.dirty.insert(
            canonical.key.clone(),
            CanonicalModel {
                key: canonical.key.clone(),
                vendor: canonical.vendor.clone(),
                display_name: record
                    .display_name
                    .clone()
                    .unwrap_or_else(|| fallback.to_owned()),
                kind: record.kind,
                first_seen_at: self.now,
                created_at: record.created_at,
                status,
                acked_at: status.is_seen().then_some(self.now),
                note: None,
            },
        );
        true
    }

    fn add(&mut self, record: &ModelRecord, canonical: &Canonical, silent: bool) {
        let key = ListingKey::new(self.source_id, &record.model_id);
        let ignored = self
            .model(&canonical.key)
            .is_some_and(|m| m.status == ModelStatus::Ignored);
        let seen = silent || ignored;
        let created = self.touch_model(
            record,
            canonical,
            if silent {
                ModelStatus::Seen
            } else {
                ModelStatus::Unseen
            },
        );
        self.listings.push(Listing {
            key: key.clone(),
            canonical_key: canonical.key.clone(),
            kind: record.kind,
            display_name: record.display_name.clone(),
            created_at: record.created_at,
            url: record.url.clone(),
            info: record.info.clone(),
            first_seen_at: self.now,
            removed_at: None,
            baseline: silent,
            status: if seen {
                ListingStatus::Seen
            } else {
                ListingStatus::Unseen
            },
            acked_at: seen.then_some(self.now),
        });
        if silent {
            self.counts.baseline += 1;
        } else if created {
            self.event(EventKind::New, &canonical.key, &key, Vec::new(), false);
        } else {
            let model_unseen = self
                .model(&canonical.key)
                .is_some_and(|m| m.status == ModelStatus::Unseen);
            self.event(
                EventKind::NewListing,
                &canonical.key,
                &key,
                Vec::new(),
                model_unseen,
            );
        }
    }

    fn update(&mut self, old: &Listing, record: &ModelRecord, canonical: &Canonical) {
        let status = if old.status == ListingStatus::Seen {
            ModelStatus::Seen
        } else {
            ModelStatus::Unseen
        };
        self.touch_model(record, canonical, status);
        let updated = Listing {
            canonical_key: canonical.key.clone(),
            kind: record.kind,
            display_name: record
                .display_name
                .clone()
                .or_else(|| old.display_name.clone()),
            created_at: record.created_at.or(old.created_at),
            url: record.url.clone().or_else(|| old.url.clone()),
            info: record.info.clone(),
            removed_at: None,
            ..old.clone()
        };
        if old.canonical_key != canonical.key {
            self.moved
                .push((old.key.clone(), old.canonical_key.clone()));
        }
        if old.removed_at.is_some() {
            self.event(
                EventKind::Reappeared,
                &canonical.key,
                &old.key,
                Vec::new(),
                false,
            );
        }
        let changes = record.info.diff(&old.info, self.price_tolerance);
        if !changes.is_empty() {
            self.event(EventKind::Changed, &canonical.key, &old.key, changes, false);
        }
        if updated != *old {
            self.listings.push(updated);
        }
    }

    fn remove(&mut self, old: &Listing) {
        self.listings.push(Listing {
            removed_at: Some(self.now),
            ..old.clone()
        });
        self.event(
            EventKind::Removed,
            &old.canonical_key.clone(),
            &old.key,
            Vec::new(),
            false,
        );
    }
}

#[cfg(test)]
mod tests;
