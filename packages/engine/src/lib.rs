#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Sync orchestration: fetch, normalize, filter, canonicalize, diff against the store, persist,
//! and emit events.
//!
//! * [`canonical`]: listing → canonical model mapping
//! * [`diff`]: the pure planner deciding listings, models, and events for one source
//! * [`sync`]: [`Engine`], which fetches sources concurrently and persists each plan
//! * [`schedule`]: when each source is next due (intervals + failure backoff)
//! * [`notices`]: which new events deserve a notification
//!
//! This is the only crate that combines `bmodels_sources` and `bmodels_store`.

pub mod canonical;
pub mod diff;
pub mod notices;
pub mod schedule;
pub mod sync;

pub use canonical::{Canonical, Canonicalizer};
pub use diff::{Counts, Plan, PlanError, PlanInput};
pub use notices::{Notice, Pending};
pub use schedule::{Due, due_now, schedule};
pub use sync::{Engine, EngineError, Progress, Selection, SourceOutcome, SourceReport, SyncReport};

#[cfg(test)]
mod tests;
