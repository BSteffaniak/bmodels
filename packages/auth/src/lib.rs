#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Where bmodels keeps secrets.
//!
//! * [`vault`]: the bmodels-owned sshenv vault ([`AuthVault`]) and a cached [`VaultLookup`] that
//!   feeds the engine's [`bmodels_sources::SecretLookup`]
//! * [`discover`]: API keys other tools already keep on this machine (metadata first; values
//!   only on explicit import)
//! * [`check`]: plaintext secrets left in configuration
//!
//! Secret values are only ever held as [`bmodels_model::Secret`]; nothing here logs, prints, or
//! serializes them.

pub mod check;
pub mod discover;
pub mod vault;

pub use check::{PlaintextFinding, plaintext_secrets};
pub use discover::{Discovered, DiscoveryOrigin, discover};
pub use sshenv_vault::app::{SecurityAction, SecurityReport};
pub use vault::{AuthVault, VaultError, VaultLookup};

use bmodels_model::{Config, SourceSpec};

/// Sources that take an API key, in id order: those naming an env var or command, plus any
/// with a stored vault key.
#[must_use]
pub fn keyed_sources<'a>(
    config: &'a Config,
    stored: &std::collections::BTreeSet<String>,
) -> Vec<(&'a String, &'a SourceSpec)> {
    config
        .sources
        .iter()
        .filter(|(id, spec)| spec.wants_key() || stored.contains(*id))
        .collect()
}
