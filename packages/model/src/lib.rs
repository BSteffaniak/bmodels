#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Shared bmodels domain types: model records, source specs, change events, and user state.
//!
//! This crate is the contract between every other bmodels crate. It is free of I/O.
//!
//! * [`config`]: the configuration schema (`[settings]`, `[sources.<id>]`, `[canonical]`, ...)
//! * [`record`]: what a source adapter returns ([`ModelRecord`]) and the tracked-field diff
//! * [`state`]: what the store persists (canonical models, listings, events, source state)
//! * [`secret`]: secret values and references (`{ vault = "openai/API_KEY" }`)
//! * [`duration`]: `"30s"` / `"6h"` / `"1d"` interval parsing
//! * [`display`]: dates, ages, token counts, and prices for humans

pub mod config;
pub mod display;
pub mod duration;
pub mod record;
pub mod secret;
pub mod state;

pub use config::{
    AuthSpec, CanonicalRules, Config, FilterSpec, NotifySpec, Settings, SourceKind, SourceSpec,
};
pub use duration::{DurationError, format_duration, parse_duration};
pub use record::{FieldChange, ModelInfo, ModelRecord, RecordKind};
pub use secret::{API_KEY_VAR, InvalidSecretName, Secret, SecretName, SecretRef, SecretValue};
pub use state::{
    AckTarget, CanonicalModel, Event, EventKind, Listing, ListingKey, ListingStatus, ModelStatus,
    SourceHealth, SourceState, SyncRun,
};

/// Unix timestamp in seconds.
pub type Timestamp = i64;

/// Current Unix time in seconds.
#[must_use]
pub fn now() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
