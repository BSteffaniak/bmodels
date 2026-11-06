//! Persisted state: canonical models, listings, events, and per-source sync state.
//!
//! # Seen semantics
//!
//! A *listing* is one `(source, model_id)` pair; a *canonical model* groups the listings of
//! the same model across sources. Both carry a seen status:
//!
//! * acking a canonical model acks it and all of its listings and events
//! * acking a listing acks it and its events; once every listing of a canonical model is
//!   seen, the canonical model is seen too
//! * acking an event acks just that event (and, for `NEW` / `NEW_LISTING`, its listing)
//!
//! The inbox is the set of unacknowledged events.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Timestamp;
use crate::config::SourceKind;
use crate::record::{FieldChange, ModelInfo, RecordKind};

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
        pub enum $name {
            $($(#[$vmeta])* $variant,)*
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// Storage / JSON spelling.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)*
                }
            }

            /// Parses the storage spelling.
            #[must_use]
            pub fn parse(s: &str) -> Option<Self> {
                match s {
                    $($text => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_enum! {
    /// User status of a canonical model.
    ModelStatus {
        /// Discovered, not yet acknowledged.
        Unseen = "UNSEEN",
        /// Acknowledged.
        Seen = "SEEN",
        /// Acknowledged and starred.
        Starred = "STARRED",
        /// Never surfaces or notifies again.
        Ignored = "IGNORED",
    }
}

string_enum! {
    /// User status of a listing.
    ListingStatus {
        /// Discovered, not yet acknowledged.
        Unseen = "UNSEEN",
        /// Acknowledged.
        Seen = "SEEN",
    }
}

string_enum! {
    /// What happened.
    EventKind {
        /// A model nobody has seen before.
        New = "NEW",
        /// A known model appeared in another source, or as a new snapshot.
        NewListing = "NEW_LISTING",
        /// Tracked metadata (pricing, context window, ...) changed.
        Changed = "CHANGED",
        /// A listing disappeared from its source.
        Removed = "REMOVED",
        /// A removed listing came back.
        Reappeared = "REAPPEARED",
    }
}

impl EventKind {
    /// Short label for tables (`"new"`, `"listing"`, ...).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::NewListing => "listing",
            Self::Changed => "changed",
            Self::Removed => "removed",
            Self::Reappeared => "back",
        }
    }
}

impl ModelStatus {
    /// Whether the model counts as acknowledged.
    #[must_use]
    pub const fn is_seen(self) -> bool {
        !matches!(self, Self::Unseen)
    }
}

/// Identifies a listing: `<source>:<model_id>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ListingKey {
    /// Source id.
    pub source: String,
    /// Model id as reported by the source.
    pub model_id: String,
}

impl ListingKey {
    /// Creates a key.
    #[must_use]
    pub fn new(source: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            model_id: model_id.into(),
        }
    }

    /// Parses `<source>:<model_id>` (the model id may itself contain `:`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let (source, model) = s.split_once(':')?;
        (!source.is_empty() && !model.is_empty() && !source.contains('/'))
            .then(|| Self::new(source, model))
    }
}

impl fmt::Display for ListingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source, self.model_id)
    }
}

/// A model across all sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalModel {
    /// `vendor/family`.
    pub key: String,
    /// Vendor (`"openai"`).
    pub vendor: String,
    /// Best known human name.
    pub display_name: String,
    /// Model or announcement.
    pub kind: RecordKind,
    /// When any source first reported it.
    pub first_seen_at: Timestamp,
    /// Earliest creation / release date reported by a source.
    pub created_at: Option<Timestamp>,
    /// User status.
    pub status: ModelStatus,
    /// When the user acknowledged it.
    pub acked_at: Option<Timestamp>,
    /// Free-form user note.
    pub note: Option<String>,
}

/// One `(source, model_id)` observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    /// Identity.
    pub key: ListingKey,
    /// Canonical model it belongs to.
    pub canonical_key: String,
    /// Model or announcement.
    pub kind: RecordKind,
    /// Human name reported by the source.
    pub display_name: Option<String>,
    /// Creation / release date reported by the source.
    pub created_at: Option<Timestamp>,
    /// Link to the model page.
    pub url: Option<String>,
    /// Normalized metadata as last observed.
    pub info: ModelInfo,
    /// When this listing was first observed.
    pub first_seen_at: Timestamp,
    /// When it disappeared (while still gone).
    pub removed_at: Option<Timestamp>,
    /// Recorded during a source's baseline sync.
    pub baseline: bool,
    /// User status.
    pub status: ListingStatus,
    /// When the user acknowledged it.
    pub acked_at: Option<Timestamp>,
}

impl Listing {
    /// Whether the listing is currently present in its source.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        self.removed_at.is_none()
    }
}

/// Something that happened to a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Store id.
    pub id: i64,
    /// When it was detected.
    pub at: Timestamp,
    /// What happened.
    pub kind: EventKind,
    /// Canonical model.
    pub canonical_key: String,
    /// Listing.
    pub listing: ListingKey,
    /// Tracked field changes, for [`EventKind::Changed`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<FieldChange>,
    /// When the user acknowledged it.
    pub acked_at: Option<Timestamp>,
    /// When a notification was sent for it.
    pub notified_at: Option<Timestamp>,
}

/// Persisted sync state of a source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceState {
    /// Source id.
    pub id: String,
    /// Kind at the last sync.
    pub kind: Option<SourceKind>,
    /// When the baseline sync completed (`None` until it has).
    pub baseline_at: Option<Timestamp>,
    /// Start of the last attempt.
    pub last_attempt_at: Option<Timestamp>,
    /// End of the last successful sync.
    pub last_success_at: Option<Timestamp>,
    /// Error of the last attempt, if it failed.
    pub last_error: Option<String>,
    /// Why the last attempt was skipped (missing key), if it was.
    pub skipped: Option<String>,
    /// Consecutive failed attempts.
    pub failures: u32,
    /// HTTP `ETag` of the last full response.
    pub etag: Option<String>,
    /// HTTP `Last-Modified` of the last full response.
    pub last_modified: Option<String>,
    /// Fingerprint of the source spec and canonical rules used by the last full sync; a
    /// mismatch forces a full fetch so configuration changes apply immediately.
    pub fingerprint: Option<String>,
    /// Listings currently present.
    pub listing_count: u64,
}

/// Health summary of a source, derived from its [`SourceState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SourceHealth {
    /// Never synced.
    Never,
    /// Last attempt succeeded.
    Ok,
    /// Last attempt was skipped (no API key).
    Skipped,
    /// Last attempt failed.
    Error,
}

impl SourceState {
    /// Health of the last attempt.
    #[must_use]
    pub const fn health(&self) -> SourceHealth {
        if self.last_error.is_some() {
            SourceHealth::Error
        } else if self.skipped.is_some() {
            SourceHealth::Skipped
        } else if self.last_success_at.is_some() {
            SourceHealth::Ok
        } else {
            SourceHealth::Never
        }
    }
}

/// One recorded sync of one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRun {
    /// Source id.
    pub source: String,
    /// Start time.
    pub started_at: Timestamp,
    /// End time.
    pub finished_at: Timestamp,
    /// Error, if it failed.
    pub error: Option<String>,
    /// Records fetched after filtering.
    pub fetched: u64,
    /// `NEW` events.
    pub new: u64,
    /// `NEW_LISTING` events.
    pub new_listings: u64,
    /// `CHANGED` events.
    pub changed: u64,
    /// `REMOVED` events.
    pub removed: u64,
}

/// What to acknowledge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AckTarget {
    /// A canonical model and all its listings and events.
    Model(String),
    /// One listing and its events.
    Listing(ListingKey),
    /// One event.
    Event(i64),
    /// Every unacknowledged event, optionally limited to one source or vendor.
    All {
        /// Only events of this source.
        source: Option<String>,
        /// Only events of models of this vendor.
        vendor: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_key_parses_colons_in_model_ids() {
        let key = ListingKey::parse("openrouter:openai/gpt-4o:free").unwrap();
        assert_eq!(key.source, "openrouter");
        assert_eq!(key.model_id, "openai/gpt-4o:free");
        assert_eq!(key.to_string(), "openrouter:openai/gpt-4o:free");
        assert!(ListingKey::parse("openai/gpt-4o").is_none());
        assert!(ListingKey::parse(":x").is_none());
    }

    #[test]
    fn enums_round_trip() {
        for kind in EventKind::ALL {
            assert_eq!(EventKind::parse(kind.as_str()), Some(*kind));
        }
        for status in ModelStatus::ALL {
            assert_eq!(ModelStatus::parse(status.as_str()), Some(*status));
        }
    }
}
