//! Configuration schema.
//!
//! The composed document (built-in catalog → global file → `--config`) decodes into [`Config`].
//! Sources are a keyed map so a user layer can tweak one field of a built-in source
//! (`[sources.openrouter] enabled = false`) or add a new one.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::duration::{DurationError, parse_duration};
use crate::state::EventKind;

/// Default sync interval for sources that do not set one.
pub const DEFAULT_INTERVAL: &str = "6h";
/// Default HTTP timeout.
pub const DEFAULT_TIMEOUT: &str = "30s";

/// The whole configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Global settings.
    pub settings: Settings,
    /// Sources keyed by id.
    pub sources: BTreeMap<String, SourceSpec>,
    /// Rules that map listings onto canonical models.
    pub canonical: CanonicalRules,
    /// Global filters applied to every source.
    pub filters: FilterSpec,
    /// Notification sinks.
    pub notify: NotifySpec,
}

impl Config {
    /// Enabled sources, in id order.
    pub fn enabled_sources(&self) -> impl Iterator<Item = (&String, &SourceSpec)> {
        self.sources.iter().filter(|(_, s)| s.enabled)
    }

    /// The effective sync interval of `source`.
    ///
    /// # Errors
    ///
    /// * [`DurationError`] if the source's or the global interval is malformed
    pub fn interval_of(&self, source: &SourceSpec) -> Result<Duration, DurationError> {
        parse_duration(
            source
                .interval
                .as_deref()
                .unwrap_or(&self.settings.interval),
        )
    }
}

/// `[settings]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Default sync interval (`"6h"`).
    pub interval: String,
    /// Maximum sources fetched concurrently.
    pub concurrency: usize,
    /// Per-request HTTP timeout (`"30s"`).
    pub timeout: String,
    /// Overrides the data directory holding the database.
    pub data_dir: Option<String>,
    /// Price moves smaller than this percentage update silently instead of emitting
    /// `CHANGED` (aggregators like OpenRouter float prices by a cent or two).
    pub price_change_percent: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            interval: DEFAULT_INTERVAL.to_owned(),
            concurrency: 6,
            timeout: DEFAULT_TIMEOUT.to_owned(),
            data_dir: None,
            price_change_percent: 5,
        }
    }
}

impl Settings {
    /// The parsed HTTP timeout.
    ///
    /// # Errors
    ///
    /// * [`DurationError`] if `timeout` is malformed
    pub fn timeout(&self) -> Result<Duration, DurationError> {
        parse_duration(&self.timeout)
    }

    /// [`Settings::price_change_percent`] as a fraction.
    #[must_use]
    pub fn price_tolerance(&self) -> f64 {
        f64::from(self.price_change_percent) / 100.0
    }
}

/// Adapter kind of a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SourceKind {
    /// `GET {url}/models` returning `{"data": [...]}` or a bare array (OpenAI, Mistral, xAI,
    /// Groq, DeepSeek, Together, Fireworks, Cerebras, self-hosted servers, ...).
    OpenaiCompatible,
    /// Anthropic `/v1/models` (paginated, `x-api-key`).
    Anthropic,
    /// Google Generative Language `/v1beta/models` (paginated, `x-goog-api-key`).
    Gemini,
    /// `openrouter.ai/api/v1/models`.
    Openrouter,
    /// `models.dev/api.json`, filtered by `providers`.
    ModelsDev,
    /// LiteLLM's `model_prices_and_context_window.json`, filtered by `providers`.
    Litellm,
    /// Hugging Face Hub newest models for each of `authors`.
    Huggingface,
    /// The ollama.com library page.
    OllamaLibrary,
    /// An RSS or Atom feed; items become announcements.
    Feed,
}

impl SourceKind {
    /// Every kind, in declaration order.
    pub const ALL: [Self; 9] = [
        Self::OpenaiCompatible,
        Self::Anthropic,
        Self::Gemini,
        Self::Openrouter,
        Self::ModelsDev,
        Self::Litellm,
        Self::Huggingface,
        Self::OllamaLibrary,
        Self::Feed,
    ];

    /// The configuration spelling (`"OPENAI_COMPATIBLE"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "OPENAI_COMPATIBLE",
            Self::Anthropic => "ANTHROPIC",
            Self::Gemini => "GEMINI",
            Self::Openrouter => "OPENROUTER",
            Self::ModelsDev => "MODELS_DEV",
            Self::Litellm => "LITELLM",
            Self::Huggingface => "HUGGINGFACE",
            Self::OllamaLibrary => "OLLAMA_LIBRARY",
            Self::Feed => "FEED",
        }
    }

    /// Parses the configuration spelling.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

const fn yes() -> bool {
    true
}

/// `[sources.<id>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // independent config switches, mirrored from TOML
pub struct SourceSpec {
    /// Adapter kind.
    pub kind: SourceKind,
    /// Human name (defaults to the id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether the source is synced.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Endpoint. Base URL for `OPENAI_COMPATIBLE` / `ANTHROPIC` / `GEMINI` (e.g.
    /// `https://api.openai.com/v1`); the full URL for every other kind. Kinds with a
    /// well-known public endpoint have a default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Vendor of every model in this source (`"openai"`), for direct provider sources. When
    /// unset the vendor comes from the model id (`vendor/model`) or the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// Environment variable holding the API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Command whose trimmed stdout is the API key (run through the platform shell). Takes
    /// precedence over `api_key_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_cmd: Option<String>,
    /// Sync without a key when none is configured (otherwise the source is skipped).
    #[serde(default)]
    pub api_key_optional: bool,
    /// Extra request headers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Sync interval (defaults to `settings.interval`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// Only keep models whose id matches one of these globs (all when empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Drop models whose id matches one of these globs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Record the first successful sync as already seen instead of new.
    #[serde(default = "yes")]
    pub baseline: bool,
    /// Provider ids to keep (`MODELS_DEV`, `LITELLM`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    /// Hub authors / organizations to watch (`HUGGINGFACE`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    /// Maximum items fetched per request or author, for windowed sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Link template for a model page; `{id}` is replaced with the model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_url: Option<String>,
    /// Whether events from this source notify.
    #[serde(default = "yes")]
    pub notify: bool,
}

impl SourceSpec {
    /// A spec of `kind` with every optional field at its default.
    #[must_use]
    pub const fn new(kind: SourceKind) -> Self {
        Self {
            kind,
            name: None,
            enabled: true,
            url: None,
            vendor: None,
            api_key_env: None,
            api_key_cmd: None,
            api_key_optional: false,
            headers: BTreeMap::new(),
            interval: None,
            include: Vec::new(),
            exclude: Vec::new(),
            baseline: true,
            providers: Vec::new(),
            authors: Vec::new(),
            limit: None,
            model_url: None,
            notify: true,
        }
    }

    /// Whether the source is configured to authenticate.
    #[must_use]
    pub const fn wants_key(&self) -> bool {
        self.api_key_env.is_some() || self.api_key_cmd.is_some()
    }

    /// Display name: `name`, else `id`.
    #[must_use]
    pub fn display_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.name.as_deref().unwrap_or(id)
    }
}

/// `[canonical]`: how listings map onto canonical models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CanonicalRules {
    /// Fold dated snapshots (`-2024-08-06`, `-20241022`, `-0613`, `-001`) into their base model.
    pub collapse_snapshots: bool,
    /// Suffixes removed from model ids before canonicalizing (`":free"`, `"-latest"`).
    pub strip_suffixes: Vec<String>,
    /// Vendor spellings mapped to one vendor (`gemini = "google"`, `meta-llama = "meta"`).
    pub vendor_aliases: BTreeMap<String, String>,
    /// Vendor inferred from a model id prefix when neither the source nor the id names one
    /// (`llama = "meta"`, `gpt = "openai"`). The longest matching prefix wins.
    pub family_vendors: BTreeMap<String, String>,
    /// Explicit canonical key remaps (`"openai/chatgpt-4o" = "openai/gpt-4o"`).
    pub aliases: BTreeMap<String, String>,
}

impl Default for CanonicalRules {
    fn default() -> Self {
        Self {
            collapse_snapshots: true,
            strip_suffixes: Vec::new(),
            vendor_aliases: BTreeMap::new(),
            family_vendors: BTreeMap::new(),
            aliases: BTreeMap::new(),
        }
    }
}

/// `[filters]`: applied to every source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilterSpec {
    /// Drop listings whose model id or canonical key matches one of these globs.
    pub exclude: Vec<String>,
}

/// `[notify]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifySpec {
    /// Show a desktop notification.
    pub desktop: bool,
    /// Run this command (argv) with the events as JSON on stdin.
    pub command: Vec<String>,
    /// POST the events as JSON to this URL.
    pub webhook: Option<String>,
    /// Event kinds that notify.
    pub kinds: Vec<EventKind>,
    /// Maximum models named in one desktop notification.
    pub max_items: usize,
}

impl Default for NotifySpec {
    fn default() -> Self {
        Self {
            desktop: true,
            command: Vec::new(),
            webhook: None,
            kinds: vec![EventKind::New],
            max_items: 6,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_source_decodes_with_defaults() {
        let config: Config = toml::from_str(
            r#"
            [sources.local]
            kind = "OPENAI_COMPATIBLE"
            url = "http://localhost:8080/v1"
            "#,
        )
        .unwrap();
        let spec = &config.sources["local"];
        assert_eq!(spec.kind, SourceKind::OpenaiCompatible);
        assert!(spec.enabled && spec.baseline && spec.notify);
        assert_eq!(config.interval_of(spec).unwrap().as_secs(), 21_600);
        assert_eq!(config.notify.kinds, vec![EventKind::New]);
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = toml::from_str::<Config>(
            r#"
            [sources.x]
            kind = "FEED"
            urll = "typo"
            "#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn kind_spelling_round_trips() {
        for kind in SourceKind::ALL {
            assert_eq!(SourceKind::parse(kind.as_str()), Some(kind));
        }
    }
}
