#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Layered bmodels configuration loading via `bmux_config_composition`.
//!
//! Layers, lowest first (later layers override earlier ones field by field; tables merge
//! recursively, arrays and scalars replace):
//!
//! 1. the built-in catalog (`bmodels_catalog`), one in-memory text layer per file
//! 2. the global user file: `$BMODELS_CONFIG` if set, else `<config dir>/config.toml`
//!    (`~/.config/bmodels/config.toml` on macOS/Linux, `%APPDATA%\bmodels\config.toml` on
//!    Windows)
//! 3. an explicit `--config` file
//!
//! Because sources are a keyed map, a user layer containing `[sources.openrouter]
//! enabled = false` overrides only that field of the built-in source.

pub mod paths;

use std::fmt;
use std::path::PathBuf;

use bmodels_model::Config;
use bmux_config_composition::{
    DecodeDetail, ErrorKind, ExplicitMode, Layer, LoadContext, Loader, LoaderBuilder, PathSegment,
    Requirement, SourceId, TextLocation, TomlCodec,
};

pub use paths::{APP_NAME, AppDirs, DATABASE_FILE, Environment, NoHome, Platform};

/// Environment variable overriding the global configuration file path.
pub const CONFIG_ENV: &str = "BMODELS_CONFIG";
/// Environment variable overriding the data directory.
pub const DATA_DIR_ENV: &str = "BMODELS_DATA_DIR";

/// Layer-name prefix of catalog layers.
const CATALOG_LAYER_PREFIX: &str = "catalog/";

/// Which kind of layer a source came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LayerKind {
    /// Built-in schema defaults.
    Defaults,
    /// A built-in catalog file.
    Catalog,
    /// The global user file.
    Global,
    /// An explicit `--config` file.
    Explicit,
}

/// One configuration source that contributed to the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSource {
    /// Layer kind.
    pub kind: LayerKind,
    /// Layer name (`"global"`, `"catalog/providers"`, ...).
    pub layer: String,
    /// File path, for file layers.
    pub path: Option<PathBuf>,
}

impl fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            Some(path) => write!(f, "{} ({})", path.display(), self.layer),
            None => f.write_str(&self.layer),
        }
    }
}

/// Error loading configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The working directory could not be determined.
    #[error("cannot determine the working directory: {0}")]
    CurrentDir(#[source] std::io::Error),
    /// The home directory is needed but unknown.
    #[error(transparent)]
    NoHome(#[from] NoHome),
    /// A file named by `--config` or `$BMODELS_CONFIG` does not exist.
    #[error("configuration file {} does not exist", path.display())]
    MissingFile {
        /// The missing file.
        path: PathBuf,
    },
    /// Reading, parsing, or composing a layer failed.
    #[error("{}{kind:?}{}", source_prefix(.origin.as_ref()), location_suffix(*.location))]
    Layer {
        /// Failure category.
        kind: ErrorKind,
        /// Offending source, when known.
        origin: Option<ConfigSource>,
        /// Line/column of a syntax error, when known.
        location: Option<TextLocation>,
    },
    /// The composed document does not match the configuration schema.
    #[error("{}invalid value at `{key}`: {detail}", source_prefix(.origin.as_ref()))]
    Decode {
        /// Dotted key path of the offending value (`sources.openai.interval`).
        key: String,
        /// What was expected.
        detail: String,
        /// Layer that supplied the offending value, when known.
        origin: Option<ConfigSource>,
    },
    /// A value decoded but is semantically invalid (bad interval, unknown glob syntax, ...).
    #[error("invalid configuration at `{key}`: {detail}")]
    Invalid {
        /// Dotted key path.
        key: String,
        /// What is wrong.
        detail: String,
    },
}

fn source_prefix(source: Option<&ConfigSource>) -> String {
    source.map(|s| format!("{s}: ")).unwrap_or_default()
}

fn location_suffix(location: Option<TextLocation>) -> String {
    location
        .map(|l| format!(" at line {}, column {}", l.line, l.column))
        .unwrap_or_default()
}

/// Options controlling which layers are loaded.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Working directory (absolute), for relative paths.
    pub cwd: PathBuf,
    /// Environment snapshot (including `HOME`).
    pub env: Environment,
    /// Include the built-in catalog.
    pub catalog: bool,
    /// Include the global user file.
    pub global: bool,
    /// Explicit `--config` file, applied last (must exist).
    pub explicit: Option<PathBuf>,
}

impl LoadOptions {
    /// Default options for the current process (all layers, no explicit file).
    ///
    /// # Errors
    ///
    /// * [`ConfigError::CurrentDir`] if the working directory cannot be determined
    pub fn from_process() -> Result<Self, ConfigError> {
        Ok(Self::new(
            std::env::current_dir().map_err(ConfigError::CurrentDir)?,
            Environment::from_process(),
        ))
    }

    /// Default options (all layers, no explicit file) for an explicit context.
    #[must_use]
    pub const fn new(cwd: PathBuf, env: Environment) -> Self {
        Self {
            cwd,
            env,
            catalog: true,
            global: true,
            explicit: None,
        }
    }

    /// Sets the explicit `--config` file.
    #[must_use]
    pub fn with_explicit(mut self, path: Option<PathBuf>) -> Self {
        self.explicit = path;
        self
    }

    fn absolute(&self, path: PathBuf) -> PathBuf {
        if path.is_absolute() {
            path
        } else {
            self.cwd.join(path)
        }
    }

    /// The application directories for this environment.
    ///
    /// # Errors
    ///
    /// * [`ConfigError::NoHome`] if the home directory is unknown
    pub fn app_dirs(&self) -> Result<AppDirs, ConfigError> {
        Ok(self.env.app_dirs()?)
    }

    /// The global user file that will be consulted: `$BMODELS_CONFIG` if set, else the platform
    /// default.
    ///
    /// # Errors
    ///
    /// * [`ConfigError::NoHome`] if the home directory is unknown
    pub fn global_path(&self) -> Result<PathBuf, ConfigError> {
        if let Some(path) = self.env.var(CONFIG_ENV) {
            return Ok(self.absolute(self.env.expand_tilde(path)?));
        }
        Ok(self.app_dirs()?.config_file())
    }
}

/// A loaded configuration plus provenance.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// The composed configuration.
    pub config: Config,
    /// Sources that were present, lowest layer first (schema defaults excluded).
    pub sources: Vec<ConfigSource>,
    /// The global file path that was consulted (present or not).
    pub global_path: PathBuf,
    /// The data directory (`$BMODELS_DATA_DIR`, else `settings.data_dir`, else the platform
    /// default).
    pub data_dir: PathBuf,
}

impl LoadedConfig {
    /// The database file inside [`LoadedConfig::data_dir`].
    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join(DATABASE_FILE)
    }
}

/// Loads configuration for the current process with default options.
///
/// # Errors
///
/// * Any error from [`LoadOptions::from_process`] or [`load`]
pub fn load_default(explicit: Option<PathBuf>) -> Result<LoadedConfig, ConfigError> {
    load(&LoadOptions::from_process()?.with_explicit(explicit))
}

fn layers(
    options: &LoadOptions,
    global_path: &std::path::Path,
) -> Result<LoaderBuilder, ConfigError> {
    let mut builder = Loader::builder();
    if options.catalog {
        for file in bmodels_catalog::FILES {
            builder = builder.layer(
                format!("{CATALOG_LAYER_PREFIX}{}", file.name),
                Layer::Text(file.source.to_owned()),
            );
        }
    }
    if options.global {
        let requirement = if options.env.var(CONFIG_ENV).is_some() {
            if !global_path.is_file() {
                return Err(ConfigError::MissingFile {
                    path: global_path.to_path_buf(),
                });
            }
            Requirement::Required
        } else {
            Requirement::Optional
        };
        builder = builder.layer(
            "global",
            Layer::File {
                path: global_path.to_path_buf(),
                requirement,
            },
        );
    }
    if let Some(explicit) = &options.explicit {
        let path = options.absolute(explicit.clone());
        if !path.is_file() {
            return Err(ConfigError::MissingFile { path });
        }
        builder = builder.explicit_file(path, Requirement::Required, ExplicitMode::Additive);
    }
    Ok(builder)
}

/// Loads, composes, and validates all configured layers.
///
/// # Errors
///
/// * [`ConfigError::MissingFile`] if the explicit file or `$BMODELS_CONFIG` does not exist
/// * [`ConfigError::Layer`] if a file cannot be read or parsed (with line/column for syntax
///   errors)
/// * [`ConfigError::Decode`] if the composed document does not match the schema (with the
///   dotted key path and the layer that supplied the offending value)
/// * [`ConfigError::Invalid`] if an interval or timeout is malformed
/// * [`ConfigError::NoHome`] if the home directory is needed but unknown
pub fn load(options: &LoadOptions) -> Result<LoadedConfig, ConfigError> {
    let global_path = options.global_path()?;
    let builder = layers(options, &global_path)?;
    let layer_err =
        |error: bmux_config_composition::Error, sources: &[ConfigSource]| ConfigError::Layer {
            kind: error.kind,
            origin: error.source.and_then(|id| sources.get(id.0).cloned()),
            location: error.location,
        };

    let loader = builder.build().map_err(|e| layer_err(e, &[]))?;
    let context = LoadContext::new(options.cwd.clone(), options.env.vars().clone())
        .map_err(|e| layer_err(e, &[]))?;
    let captured = loader.capture(&context).map_err(|e| layer_err(e, &[]))?;
    let sources: Vec<ConfigSource> = captured
        .sources()
        .iter()
        .map(|s| ConfigSource {
            kind: layer_kind(&s.layer),
            layer: s.layer.clone(),
            path: s.path.clone(),
        })
        .collect();
    let resolved = captured
        .resolve(&TomlCodec)
        .map_err(|e| layer_err(e, &sources))?;
    let config: Config = resolved
        .decode_located()
        .map_err(|failure| ConfigError::Decode {
            key: dotted(&failure.path),
            detail: describe(&failure.detail),
            origin: failure
                .error
                .source
                .and_then(|id| sources.get(id.0).cloned()),
        })?;
    validate(&config)?;

    let data_dir = if let Some(dir) = options.env.var(DATA_DIR_ENV) {
        options.absolute(options.env.expand_tilde(dir)?)
    } else if let Some(dir) = &config.settings.data_dir {
        options.absolute(options.env.expand_tilde(dir)?)
    } else {
        options.app_dirs()?.data
    };

    Ok(LoadedConfig {
        config,
        sources: sources
            .into_iter()
            .filter(|s| s.kind != LayerKind::Defaults)
            .collect(),
        global_path,
        data_dir,
    })
}

/// Parses a single configuration document (no layering, no catalog).
///
/// # Errors
///
/// * [`ConfigError::Layer`] with a line/column on TOML syntax errors
/// * [`ConfigError::Decode`] with the dotted key path on schema errors
/// * [`ConfigError::Invalid`] if an interval or timeout is malformed
pub fn parse_str(text: &str) -> Result<Config, ConfigError> {
    let value = TomlCodec::parse(text).map_err(|e| ConfigError::Layer {
        kind: e.kind,
        origin: None,
        location: e.location,
    })?;
    let config = bmux_config_composition::Document::new(value, SourceId(0))
        .decode_located()
        .map_err(|failure| ConfigError::Decode {
            key: dotted(&failure.path),
            detail: describe(&failure.detail),
            origin: None,
        })?;
    validate(&config)?;
    Ok(config)
}

/// Checks values serde cannot: interval and timeout syntax, and that every enabled source
/// that needs one has a URL.
fn validate(config: &Config) -> Result<(), ConfigError> {
    let invalid = |key: String, detail: String| ConfigError::Invalid { key, detail };
    bmodels_model::parse_duration(&config.settings.interval)
        .map_err(|e| invalid("settings.interval".to_owned(), e.to_string()))?;
    config
        .settings
        .timeout()
        .map_err(|e| invalid("settings.timeout".to_owned(), e.to_string()))?;
    if config.settings.concurrency == 0 {
        return Err(invalid(
            "settings.concurrency".to_owned(),
            "must be at least 1".to_owned(),
        ));
    }
    for (id, spec) in &config.sources {
        if let Some(interval) = &spec.interval {
            bmodels_model::parse_duration(interval)
                .map_err(|e| invalid(format!("sources.{id}.interval"), e.to_string()))?;
        }
        let needs_url = matches!(
            spec.kind,
            bmodels_model::SourceKind::OpenaiCompatible | bmodels_model::SourceKind::Feed
        );
        if spec.enabled && needs_url && spec.url.is_none() {
            return Err(invalid(
                format!("sources.{id}.url"),
                format!("required for kind {}", spec.kind),
            ));
        }
    }
    Ok(())
}

fn layer_kind(layer: &str) -> LayerKind {
    match layer {
        "defaults" => LayerKind::Defaults,
        "global" => LayerKind::Global,
        l if l.starts_with(CATALOG_LAYER_PREFIX) => LayerKind::Catalog,
        _ => LayerKind::Explicit,
    }
}

fn dotted(path: &[PathSegment]) -> String {
    let mut out = String::new();
    for segment in path {
        match segment {
            PathSegment::Key(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            PathSegment::Index(i) => {
                out.push('[');
                out.push_str(&i.to_string());
                out.push(']');
            }
        }
    }
    if out.is_empty() {
        out.push_str("<root>");
    }
    out
}

fn describe(detail: &DecodeDetail) -> String {
    match detail {
        DecodeDetail::UnknownField { field, expected } => {
            format!("unknown field `{field}`, expected one of {expected:?}")
        }
        DecodeDetail::MissingField { field } => format!("missing field `{field}`"),
        DecodeDetail::InvalidType { expected } => format!("invalid type, expected {expected}"),
        DecodeDetail::InvalidValue { expected } => format!("invalid value, expected {expected}"),
        DecodeDetail::InvalidLength { expected } => format!("invalid length, expected {expected}"),
        DecodeDetail::UnknownVariant { expected } => {
            format!("unknown variant, expected one of {expected:?}")
        }
        _ => "invalid value".to_owned(),
    }
}

/// A commented starter configuration written by `bmodels config init`.
pub const STARTER_CONFIG: &str = r#"# bmodels configuration. Built-in sources come from the catalog; anything here overrides
# them field by field. See `bmodels sources` for what is available.

[settings]
# interval = "6h"        # default sync interval
# concurrency = 6
# timeout = "30s"
# price_change_percent = 5   # smaller price moves update silently

# Turn a built-in source off:
# [sources.ollama]
# enabled = false

# Watch more Hugging Face organizations:
# [sources.huggingface]
# authors = ["meta-llama", "Qwen", "mistralai", "deepseek-ai", "google"]

# Add your own OpenAI-compatible endpoint:
# [sources.local]
# kind = "OPENAI_COMPATIBLE"
# url = "http://localhost:11434/v1"
# vendor = "local"

# Hide noise everywhere (globs match model ids and canonical keys):
# [filters]
# exclude = ["*embedding*", "*tts*", "*whisper*"]

[notify]
# desktop = true
# kinds = ["NEW"]                       # also: NEW_LISTING, CHANGED, REMOVED, REAPPEARED
# command = ["sh", "-c", "cat > /tmp/bmodels-events.json"]
# webhook = "https://example.com/hook"
"#;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use bmodels_model::SourceKind;

    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        home: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap().join("home");
        fs::create_dir_all(home.join(".config").join("bmodels")).unwrap();
        Fixture { _dir: dir, home }
    }

    fn options(f: &Fixture, extra: &[(&str, String)]) -> LoadOptions {
        let mut vars: BTreeMap<String, String> = extra
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        vars.insert(
            "XDG_CONFIG_HOME".to_owned(),
            f.home.join(".config").display().to_string(),
        );
        vars.insert(
            "APPDATA".to_owned(),
            f.home.join(".config").display().to_string(),
        );
        vars.insert("HOME".to_owned(), f.home.display().to_string());
        LoadOptions::new(f.home.clone(), Environment::new(vars))
    }

    fn global_file(f: &Fixture) -> PathBuf {
        f.home.join(".config").join("bmodels").join("config.toml")
    }

    #[test]
    fn catalog_alone_loads() {
        let f = fixture();
        let loaded = load(&options(&f, &[])).unwrap();
        assert!(loaded.config.sources.contains_key("openrouter"));
        assert!(loaded.config.canonical.collapse_snapshots);
        assert!(loaded.sources.iter().all(|s| s.kind == LayerKind::Catalog));
    }

    #[test]
    fn global_file_overrides_one_field_of_a_builtin_source() {
        let f = fixture();
        fs::write(
            global_file(&f),
            "[sources.openrouter]\nenabled = false\n\n[sources.mine]\nkind = \"FEED\"\nurl = \"https://example.com/rss\"\n",
        )
        .unwrap();
        let loaded = load(&options(&f, &[])).unwrap();
        let openrouter = &loaded.config.sources["openrouter"];
        assert!(!openrouter.enabled);
        assert_eq!(openrouter.kind, SourceKind::Openrouter);
        assert!(openrouter.model_url.is_some(), "catalog fields survive");
        assert_eq!(loaded.config.sources["mine"].kind, SourceKind::Feed);
        assert!(loaded.sources.iter().any(|s| s.kind == LayerKind::Global));
    }

    #[test]
    fn explicit_layer_wins_and_must_exist() {
        let f = fixture();
        fs::write(global_file(&f), "[settings]\ninterval = \"1h\"\n").unwrap();
        let explicit = f.home.join("extra.toml");
        fs::write(&explicit, "[settings]\ninterval = \"2h\"\n").unwrap();
        let loaded = load(&options(&f, &[]).with_explicit(Some(explicit))).unwrap();
        assert_eq!(loaded.config.settings.interval, "2h");

        let missing = load(&options(&f, &[]).with_explicit(Some(f.home.join("nope.toml"))));
        assert!(matches!(missing, Err(ConfigError::MissingFile { .. })));
    }

    #[test]
    fn env_overrides_global_path_and_data_dir() {
        let f = fixture();
        let custom = f.home.join("custom.toml");
        fs::write(&custom, "[settings]\nconcurrency = 2\n").unwrap();
        let loaded = load(&options(
            &f,
            &[
                (CONFIG_ENV, custom.display().to_string()),
                (DATA_DIR_ENV, "~/data".to_owned()),
            ],
        ))
        .unwrap();
        assert_eq!(loaded.config.settings.concurrency, 2);
        assert_eq!(loaded.global_path, custom);
        assert_eq!(
            loaded.database_path(),
            f.home.join("data").join(DATABASE_FILE)
        );
    }

    #[test]
    fn decode_errors_name_the_key_and_layer() {
        let f = fixture();
        fs::write(global_file(&f), "[sources.openai]\nenabled = \"yes\"\n").unwrap();
        let err = load(&options(&f, &[])).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("sources.openai.enabled"), "{text}");
        assert!(text.contains("global"), "{text}");
    }

    #[test]
    fn invalid_interval_is_rejected() {
        let err = parse_str("[settings]\ninterval = \"soon\"\n").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { ref key, .. } if key == "settings.interval"));
    }

    #[test]
    fn starter_config_is_valid() {
        parse_str(STARTER_CONFIG).unwrap();
    }
}
