#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Built-in sources and canonicalization rules, embedded as TOML.
//!
//! Each file under `packages/catalog/catalog/` is written in exactly the format of a user
//! configuration file and becomes one of the lowest configuration layers; `bmodels_config`
//! composes user layers on top with keyed-override semantics, so `[sources.openrouter]
//! enabled = false` in a user file tweaks only that field of the built-in source.

use bmodels_model::Config;

/// One embedded catalog file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogFile {
    /// File stem (`"providers"`).
    pub name: &'static str,
    /// Raw TOML source.
    pub source: &'static str,
}

macro_rules! catalog_files {
    ($($name:literal),* $(,)?) => {
        /// Every embedded catalog file, in layering order.
        pub const FILES: &[CatalogFile] = &[
            $(CatalogFile {
                name: $name,
                source: include_str!(concat!("../catalog/", $name, ".toml")),
            },)*
        ];
    };
}

catalog_files!["canonical", "providers", "registries"];

/// Error in a built-in catalog file (a bug in bmodels).
#[derive(Debug, thiserror::Error)]
#[error("built-in catalog file `{file}` is invalid: {source}")]
pub struct CatalogError {
    /// File stem.
    pub file: &'static str,
    /// Parse error.
    #[source]
    pub source: toml::de::Error,
}

/// Parses one catalog file on its own.
///
/// # Errors
///
/// * [`CatalogError`] if the file is not a valid configuration document
pub fn parse(file: &CatalogFile) -> Result<Config, CatalogError> {
    toml::from_str(file.source).map_err(|source| CatalogError {
        file: file.name,
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use bmodels_model::SourceKind;

    use super::*;

    #[test]
    fn every_file_parses() {
        for file in FILES {
            parse(file).unwrap();
        }
    }

    #[test]
    fn source_ids_are_unique_across_files() {
        let mut seen = BTreeSet::new();
        for file in FILES {
            for id in parse(file).unwrap().sources.keys() {
                assert!(seen.insert(id.clone()), "duplicate source `{id}`");
            }
        }
    }

    #[test]
    fn sources_needing_a_url_have_one() {
        for file in FILES {
            for (id, spec) in parse(file).unwrap().sources {
                if matches!(spec.kind, SourceKind::OpenaiCompatible | SourceKind::Feed) {
                    assert!(spec.url.is_some(), "`{id}` needs a url");
                }
                if let Some(template) = &spec.model_url {
                    assert!(template.starts_with("https://"), "`{id}` model_url");
                }
            }
        }
    }
}
