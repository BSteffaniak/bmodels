#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Model registry source adapters.
//!
//! [`fetch`] dispatches on [`SourceKind`] to one adapter module per kind. Adapters turn a
//! registry's response into normalized [`ModelRecord`]s; they never touch the store. All HTTP
//! goes through the [`Http`] trait so adapters are tested against recorded fixtures.

mod adapters;
pub mod http;
pub mod parse;
pub mod secrets;
pub mod testing;

use bmodels_model::{ModelRecord, SourceKind, SourceSpec};
use globset::{GlobSet, GlobSetBuilder};

pub use http::{Http, Request, Response, SwitchyHttp};
pub use secrets::{EnvLookup, ProcessEnv, resolve_key};

/// Errors fetching a source.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The API key environment variable is not set (the source is skipped, not failed).
    #[error("needs {0}")]
    MissingKey(String),
    /// `api_key_cmd` failed.
    #[error("api_key_cmd failed: {0}")]
    KeyCommand(String),
    /// Connection failure or timeout.
    #[error("request failed: {0}")]
    Http(String),
    /// Non-success status.
    #[error("HTTP {status} from {url}{}", if detail.is_empty() { String::new() } else { format!(": {detail}") })]
    Status {
        /// Status code.
        status: u16,
        /// Request URL without the query string.
        url: String,
        /// Start of the response body.
        detail: String,
    },
    /// The response did not have the expected shape.
    #[error("unexpected response: {0}")]
    Parse(String),
    /// The source configuration is unusable.
    #[error("invalid source configuration: {0}")]
    Config(String),
}

impl SourceError {
    /// Whether this means "skipped" rather than "failed".
    #[must_use]
    pub const fn is_skip(&self) -> bool {
        matches!(self, Self::MissingKey(_))
    }
}

/// HTTP cache validators from a previous full response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validators {
    /// `ETag`.
    pub etag: Option<String>,
    /// `Last-Modified`.
    pub last_modified: Option<String>,
}

/// Everything an adapter needs besides its spec.
pub struct FetchContext<'a> {
    /// HTTP client.
    pub http: &'a dyn Http,
    /// Resolved API key, if any.
    pub api_key: Option<String>,
    /// Validators for a conditional GET (single-request sources only).
    pub validators: Validators,
}

impl std::fmt::Debug for FetchContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchContext")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("validators", &self.validators)
            .finish_non_exhaustive()
    }
}

/// A successful fetch.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    /// Records after the source's `include` / `exclude` filters, de-duplicated by id.
    pub records: Vec<ModelRecord>,
    /// Whether `records` is the source's whole catalog. Windowed and scraped sources set
    /// `false`, so a missing record never counts as removed.
    pub complete: bool,
    /// Validators to send next time.
    pub validators: Validators,
    /// Non-fatal problems (unknown provider ids, skipped malformed entries).
    pub warnings: Vec<String>,
}

/// Result of [`fetch`].
#[derive(Debug, Clone, PartialEq)]
pub enum FetchOutcome {
    /// The server answered `304 Not Modified`.
    NotModified,
    /// New data.
    Fetched(Fetched),
}

/// Fetches and normalizes one source.
///
/// # Errors
///
/// * [`SourceError::Config`] if the spec lacks a required URL or has an invalid glob
/// * [`SourceError::Http`] / [`SourceError::Status`] on transport or HTTP failures
/// * [`SourceError::Parse`] if the response has an unexpected shape
pub async fn fetch(
    id: &str,
    spec: &SourceSpec,
    ctx: &FetchContext<'_>,
) -> Result<FetchOutcome, SourceError> {
    let filter = IdFilter::new(&spec.include, &spec.exclude)?;
    let outcome = match spec.kind {
        SourceKind::OpenaiCompatible => adapters::openai_compatible::fetch(spec, ctx).await?,
        SourceKind::Anthropic => adapters::anthropic::fetch(spec, ctx).await?,
        SourceKind::Gemini => adapters::gemini::fetch(spec, ctx).await?,
        SourceKind::Openrouter => adapters::openrouter::fetch(spec, ctx).await?,
        SourceKind::ModelsDev => adapters::models_dev::fetch(spec, ctx).await?,
        SourceKind::Litellm => adapters::litellm::fetch(spec, ctx).await?,
        SourceKind::Huggingface => adapters::huggingface::fetch(spec, ctx).await?,
        SourceKind::OllamaLibrary => adapters::ollama::fetch(spec, ctx).await?,
        SourceKind::Feed => adapters::feed::fetch(id, spec, ctx).await?,
    };
    Ok(match outcome {
        FetchOutcome::NotModified => FetchOutcome::NotModified,
        FetchOutcome::Fetched(mut fetched) => {
            let mut seen = std::collections::BTreeSet::new();
            fetched
                .records
                .retain(|r| filter.keep(&r.model_id) && seen.insert(r.model_id.clone()));
            if let Some(template) = &spec.model_url {
                #[allow(clippy::literal_string_with_formatting_args)]
                // `{id}` is our template syntax
                let placeholder = "{id}";
                for record in &mut fetched.records {
                    record.url = Some(template.replace(placeholder, &record.model_id));
                }
            }
            FetchOutcome::Fetched(fetched)
        }
    })
}

/// The default endpoint of kinds that have a well-known public one.
#[must_use]
pub const fn default_url(kind: SourceKind) -> Option<&'static str> {
    match kind {
        SourceKind::Anthropic => Some("https://api.anthropic.com/v1"),
        SourceKind::Gemini => Some("https://generativelanguage.googleapis.com/v1beta"),
        SourceKind::Openrouter => Some("https://openrouter.ai/api/v1/models"),
        SourceKind::ModelsDev => Some("https://models.dev/api.json"),
        SourceKind::Litellm => Some(
            "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json",
        ),
        SourceKind::Huggingface => Some("https://huggingface.co/api/models"),
        SourceKind::OllamaLibrary => Some("https://ollama.com/library?sort=newest"),
        SourceKind::OpenaiCompatible | SourceKind::Feed => None,
    }
}

/// `include` / `exclude` globs over model ids (case-insensitive).
#[derive(Debug, Clone)]
pub struct IdFilter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl IdFilter {
    /// Compiles the globs.
    ///
    /// # Errors
    ///
    /// * [`SourceError::Config`] if a glob is malformed
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self, SourceError> {
        Ok(Self {
            include: glob_set(include)?,
            exclude: glob_set(exclude)?,
        })
    }

    /// Whether `id` passes.
    #[must_use]
    pub fn keep(&self, id: &str) -> bool {
        self.include.as_ref().is_none_or(|g| g.is_match(id))
            && !self.exclude.as_ref().is_some_and(|g| g.is_match(id))
    }

    /// Whether any of `ids` is excluded.
    #[must_use]
    pub fn excludes_any(&self, ids: &[&str]) -> bool {
        self.exclude
            .as_ref()
            .is_some_and(|g| ids.iter().any(|id| g.is_match(id)))
    }
}

fn glob_set(patterns: &[String]) -> Result<Option<GlobSet>, SourceError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = globset::GlobBuilder::new(pattern)
            .case_insensitive(true)
            .literal_separator(false)
            .build()
            .map_err(|e| SourceError::Config(format!("invalid glob `{pattern}`: {e}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map(Some)
        .map_err(|e| SourceError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_filter() {
        let f = IdFilter::new(&[], &["ft:*".into(), "*-TTS*".into()]).unwrap();
        assert!(f.keep("gpt-4o"));
        assert!(!f.keep("ft:gpt-4o:org"));
        assert!(!f.keep("gpt-4o-mini-tts"));
        let f = IdFilter::new(&["openai/*".into()], &[]).unwrap();
        assert!(f.keep("openai/gpt-4o"));
        assert!(!f.keep("anthropic/claude"));
    }
}
