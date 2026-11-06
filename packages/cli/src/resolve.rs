//! Turns user-typed model references into store targets.
//!
//! Accepted forms, tried in order:
//!
//! 1. `#12` or `12`: an event id
//! 2. `source:model_id`: a listing (when that listing exists)
//! 3. `vendor/family`: a canonical model key
//! 4. anything else: a case-insensitive match on the key's family or the display name; must be
//!    unambiguous

use bmodels_model::{AckTarget, CanonicalModel, ListingKey};
use bmodels_store::Store;

/// What a reference resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A canonical model key.
    Model(String),
    /// A listing.
    Listing(ListingKey, String),
    /// An event id.
    Event(i64),
}

impl Resolved {
    /// The canonical key, if the reference names a model or listing.
    pub fn model_key(&self) -> Option<&str> {
        match self {
            Self::Model(key) | Self::Listing(_, key) => Some(key),
            Self::Event(_) => None,
        }
    }

    /// The acknowledgement target.
    pub fn ack_target(&self) -> AckTarget {
        match self {
            Self::Model(key) => AckTarget::Model(key.clone()),
            Self::Listing(key, _) => AckTarget::Listing(key.clone()),
            Self::Event(id) => AckTarget::Event(*id),
        }
    }
}

/// Resolves `input`.
pub async fn resolve(store: &Store, input: &str) -> anyhow::Result<Resolved> {
    let input = input.trim();
    if let Some(id) = input
        .strip_prefix('#')
        .unwrap_or(input)
        .parse::<i64>()
        .ok()
        .filter(|_| input.starts_with('#') || input.chars().all(|c| c.is_ascii_digit()))
    {
        return Ok(Resolved::Event(id));
    }
    if let Some(key) = ListingKey::parse(input)
        && let Some(listing) = store.listing(&key).await?
    {
        return Ok(Resolved::Listing(key, listing.canonical_key));
    }
    if store.model(input).await?.is_some() {
        return Ok(Resolved::Model(input.to_owned()));
    }
    let needle = input.to_lowercase();
    let models = store.models().await?;
    let exact: Vec<&CanonicalModel> = models
        .iter()
        .filter(|m| {
            m.key.rsplit('/').next().is_some_and(|f| f == needle)
                || m.display_name.to_lowercase() == needle
        })
        .collect();
    let candidates = if exact.is_empty() {
        models
            .iter()
            .filter(|m| m.key.contains(&needle) || m.display_name.to_lowercase().contains(&needle))
            .collect()
    } else {
        exact
    };
    match candidates.as_slice() {
        [one] => Ok(Resolved::Model(one.key.clone())),
        [] => anyhow::bail!("no model matches `{input}` (try `bmodels list {input}`)"),
        many => {
            let mut keys: Vec<&str> = many.iter().map(|m| m.key.as_str()).collect();
            keys.sort_unstable();
            let shown = keys.iter().take(8).copied().collect::<Vec<_>>().join(", ");
            let more = if keys.len() > 8 {
                format!(" and {} more", keys.len() - 8)
            } else {
                String::new()
            };
            anyhow::bail!("`{input}` is ambiguous: {shown}{more}")
        }
    }
}
