//! API keys other tools already keep on this machine.
//!
//! [`discover`] returns metadata only. A value is read by [`Discovered::read`] when the user
//! explicitly imports it. Only static API keys qualify: OAuth sessions (Claude Code's
//! subscription login, Codex's `ChatGPT` login) are never read or copied, and key helper
//! commands are suggested as `api_key_cmd` rather than copied, because their output rotates.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use bmodels_model::{Config, Secret, SourceKind};
use serde_json::Value;

/// Files larger than this are not credential files.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Where a discovered key lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryOrigin {
    /// An environment variable of this process.
    Env(String),
    /// The `OPENAI_API_KEY` field of Codex's `auth.json` (set by Codex's API-key login).
    CodexAuth(PathBuf),
    /// A variable in the `env` block of a Claude Code settings file.
    ClaudeSettingsEnv {
        /// Settings file.
        path: PathBuf,
        /// Variable name.
        var: String,
    },
    /// Claude Code's `apiKeyHelper` command. Not importable: use it as `api_key_cmd`.
    ClaudeKeyHelper {
        /// Settings file.
        path: PathBuf,
        /// The command.
        command: String,
    },
}

impl DiscoveryOrigin {
    /// Whether [`Discovered::read`] can copy this into the vault.
    #[must_use]
    pub const fn importable(&self) -> bool {
        !matches!(self, Self::ClaudeKeyHelper { .. })
    }
}

impl fmt::Display for DiscoveryOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env(var) => write!(f, "environment variable {var}"),
            Self::CodexAuth(path) => write!(f, "Codex API key in {}", path.display()),
            Self::ClaudeSettingsEnv { path, var } => {
                write!(f, "Claude Code setting env.{var} in {}", path.display())
            }
            Self::ClaudeKeyHelper { path, .. } => {
                write!(f, "Claude Code apiKeyHelper in {}", path.display())
            }
        }
    }
}

/// A key found for a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// Source the key belongs to.
    pub source: String,
    /// Where it lives.
    pub origin: DiscoveryOrigin,
}

/// Errors reading a discovered key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoverError {
    /// The value is gone or empty.
    #[error("{0}: no value found (it may have changed since discovery)")]
    Missing(String),
    /// The file is unsafe or unreadable.
    #[error("{0}")]
    File(String),
    /// The origin cannot be imported.
    #[error("{0} is a command; set it as `api_key_cmd` instead of importing")]
    NotImportable(String),
}

impl Discovered {
    /// Reads the value.
    ///
    /// # Errors
    ///
    /// * [`DiscoverError::NotImportable`] for key helper commands
    /// * [`DiscoverError::File`] if the file is a symlink, too large, or unreadable
    /// * [`DiscoverError::Missing`] if the value is no longer there
    pub fn read(&self, env: &dyn Fn(&str) -> Option<String>) -> Result<Secret, DiscoverError> {
        let missing = || DiscoverError::Missing(self.origin.to_string());
        let value = match &self.origin {
            DiscoveryOrigin::Env(var) => env(var).map(Secret::new),
            DiscoveryOrigin::CodexAuth(path) => codex_key(&read_json(path)?),
            DiscoveryOrigin::ClaudeSettingsEnv { path, var } => claude_env(&read_json(path)?, var),
            DiscoveryOrigin::ClaudeKeyHelper { .. } => {
                return Err(DiscoverError::NotImportable(self.origin.to_string()));
            }
        };
        value.filter(|s| !s.is_empty()).ok_or_else(missing)
    }
}

/// Finds keys for `config`'s keyed sources. `home` is the user's home directory; `env` reads
/// environment variables. Unreadable or malformed files are skipped silently.
#[must_use]
pub fn discover(
    config: &Config,
    home: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<Discovered> {
    let mut found = Vec::new();
    let sources_with_env = |var: &str| -> Vec<String> {
        config
            .sources
            .iter()
            .filter(|(_, s)| s.api_key_env.as_deref() == Some(var))
            .map(|(id, _)| id.clone())
            .collect()
    };
    for (id, spec) in &config.sources {
        if let Some(var) = &spec.api_key_env
            && env(var).is_some()
        {
            found.push(Discovered {
                source: id.clone(),
                origin: DiscoveryOrigin::Env(var.clone()),
            });
        }
    }
    let Some(home) = home else {
        return found;
    };

    let codex = env("CODEX_HOME")
        .map_or_else(|| home.join(".codex"), PathBuf::from)
        .join("auth.json");
    if let Ok(json) = read_json(&codex)
        && codex_key(&json).is_some()
    {
        for source in sources_with_env("OPENAI_API_KEY") {
            found.push(Discovered {
                source,
                origin: DiscoveryOrigin::CodexAuth(codex.clone()),
            });
        }
    }

    let claude = env("CLAUDE_CONFIG_DIR").map_or_else(|| home.join(".claude"), PathBuf::from);
    for file in ["settings.json", "settings.local.json"] {
        let path = claude.join(file);
        let Ok(json) = read_json(&path) else {
            continue;
        };
        if let Some(vars) = json.get("env").and_then(Value::as_object) {
            for var in vars.keys() {
                if claude_env(&json, var).is_none() {
                    continue;
                }
                for source in sources_with_env(var) {
                    found.push(Discovered {
                        source,
                        origin: DiscoveryOrigin::ClaudeSettingsEnv {
                            path: path.clone(),
                            var: var.clone(),
                        },
                    });
                }
            }
        }
        if let Some(command) = json
            .get("apiKeyHelper")
            .and_then(Value::as_str)
            .filter(|c| !c.trim().is_empty())
        {
            for (id, _) in config
                .sources
                .iter()
                .filter(|(_, s)| s.kind == SourceKind::Anthropic)
            {
                found.push(Discovered {
                    source: id.clone(),
                    origin: DiscoveryOrigin::ClaudeKeyHelper {
                        path: path.clone(),
                        command: command.to_owned(),
                    },
                });
            }
        }
    }
    found
}

fn codex_key(json: &Value) -> Option<Secret> {
    json.get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(Secret::new)
        .filter(|s| !s.is_empty())
}

fn claude_env(json: &Value, var: &str) -> Option<Secret> {
    json.get("env")?
        .get(var)?
        .as_str()
        .map(Secret::new)
        .filter(|s| !s.is_empty())
}

/// Reads a small regular file as JSON, refusing symlinks. The raw text is wiped after parsing.
fn read_json(path: &Path) -> Result<Value, DiscoverError> {
    let fail = |detail: &str| DiscoverError::File(format!("{}: {detail}", path.display()));
    let meta = fs::symlink_metadata(path).map_err(|e| fail(&e.to_string()))?;
    if meta.file_type().is_symlink() {
        return Err(fail("is a symlink; not following it"));
    }
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Err(fail("not a small regular file"));
    }
    let text = zeroize::Zeroizing::new(fs::read_to_string(path).map_err(|e| fail(&e.to_string()))?);
    serde_json::from_str(&text).map_err(|_| fail("not valid JSON"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bmodels_model::SourceSpec;

    use super::*;

    fn config() -> Config {
        let mut config = Config::default();
        let mut openai = SourceSpec::new(SourceKind::OpenaiCompatible);
        openai.api_key_env = Some("OPENAI_API_KEY".into());
        config.sources.insert("openai".into(), openai);
        let mut anthropic = SourceSpec::new(SourceKind::Anthropic);
        anthropic.api_key_env = Some("ANTHROPIC_API_KEY".into());
        config.sources.insert("anthropic".into(), anthropic);
        config
    }

    fn home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".codex")).unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        dir
    }

    #[test]
    fn finds_static_keys_and_skips_oauth_sessions() {
        let home = home();
        // ChatGPT login: tokens but no API key.
        fs::write(
            home.path().join(".codex/auth.json"),
            r#"{"OPENAI_API_KEY":null,"tokens":{"access_token":"oauth"}}"#,
        )
        .unwrap();
        fs::write(
            home.path().join(".claude/settings.json"),
            r#"{"env":{"ANTHROPIC_API_KEY":"sk-ant-file","OTHER":"x"},"apiKeyHelper":"op read x"}"#,
        )
        .unwrap();
        let env = BTreeMap::from([("OPENAI_API_KEY", "sk-env")]);
        let env = |n: &str| env.get(n).map(|v| (*v).to_owned());
        let found = discover(&config(), Some(home.path()), &env);
        let origins: Vec<_> = found.iter().map(|d| (&*d.source, &d.origin)).collect();
        assert_eq!(found.len(), 3, "{origins:?}");
        assert!(matches!(&found[0].origin, DiscoveryOrigin::Env(v) if v == "OPENAI_API_KEY"));
        assert_eq!(found[0].read(&env).unwrap().expose(), "sk-env");
        let file = found
            .iter()
            .find(|d| matches!(d.origin, DiscoveryOrigin::ClaudeSettingsEnv { .. }))
            .unwrap();
        assert_eq!(file.source, "anthropic");
        assert_eq!(file.read(&env).unwrap().expose(), "sk-ant-file");
        let helper = found.iter().find(|d| !d.origin.importable()).unwrap();
        assert!(matches!(
            helper.read(&env),
            Err(DiscoverError::NotImportable(_))
        ));
    }

    #[test]
    fn codex_api_key_login_is_found() {
        let home = home();
        fs::write(
            home.path().join(".codex/auth.json"),
            r#"{"OPENAI_API_KEY":"sk-codex"}"#,
        )
        .unwrap();
        let found = discover(&config(), Some(home.path()), &|_| None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, "openai");
        assert_eq!(found[0].read(&|_| None).unwrap().expose(), "sk-codex");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_files_are_not_followed() {
        let home = home();
        let real = home.path().join("real.json");
        fs::write(&real, r#"{"OPENAI_API_KEY":"sk-codex"}"#).unwrap();
        std::os::unix::fs::symlink(&real, home.path().join(".codex/auth.json")).unwrap();
        assert_eq!(
            discover(&config(), Some(home.path()), &|_| None),
            Vec::new()
        );
    }
}
