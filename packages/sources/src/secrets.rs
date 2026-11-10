//! Secret resolution. Values are read on demand and never stored, logged, or serialized.
//!
//! An API key is looked up in this order, first hit wins:
//!
//! 1. `api_key_cmd` (explicit in config)
//! 2. the vault entry `<source id>/API_KEY` (`bmodels auth login <source>`)
//! 3. the `api_key_env` environment variable

use std::fmt;
use std::process::Command;

use bmodels_model::{Secret, SecretName, SecretRef, SecretValue, SourceSpec};

use crate::SourceError;

/// Looks up secrets: environment variables and (optionally) the bmodels vault. Injected so
/// resolution is testable and `bmodels_sources` stays free of storage code.
pub trait SecretLookup: Send + Sync {
    /// A non-empty environment variable.
    fn var(&self, name: &str) -> Option<String>;

    /// A vault entry. Lookups without a vault return `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Returns a description when the vault exists but cannot be read.
    fn vault(&self, name: &SecretName) -> Result<Option<Secret>, String> {
        let _ = name;
        Ok(None)
    }
}

/// The process environment, without a vault.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl SecretLookup for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
    }
}

/// Where a resolved API key came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOrigin {
    /// `api_key_cmd`.
    Command,
    /// A vault entry.
    Vault(SecretName),
    /// An environment variable.
    Env(String),
}

impl fmt::Display for KeyOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command => f.write_str("api_key_cmd"),
            Self::Vault(name) => write!(f, "vault ({name})"),
            Self::Env(var) => write!(f, "env ({var})"),
        }
    }
}

/// A resolved API key and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKey {
    /// Where it came from.
    pub origin: KeyOrigin,
    /// The key.
    pub secret: Secret,
}

/// Resolves the API key of source `id`.
///
/// Returns `Ok(None)` when the source needs no key, or when the key is optional and
/// unavailable. A vault that cannot be read only matters for sources that want a key and
/// have no environment fallback.
///
/// # Errors
///
/// * [`SourceError::MissingKey`] if a required key is not configured anywhere
/// * [`SourceError::KeyCommand`] if `api_key_cmd` fails or prints nothing
/// * [`SourceError::VaultUnavailable`] if the vault cannot be read and nothing else has a key
pub fn resolve_key(
    id: &str,
    spec: &SourceSpec,
    lookup: &dyn SecretLookup,
) -> Result<Option<ResolvedKey>, SourceError> {
    if let Some(cmd) = &spec.api_key_cmd {
        return Ok(Some(ResolvedKey {
            origin: KeyOrigin::Command,
            secret: run_secret_command(cmd).map_err(SourceError::KeyCommand)?,
        }));
    }
    let mut vault_error = None;
    if let Ok(name) = SecretName::api_key(id) {
        match lookup.vault(&name) {
            Ok(Some(secret)) if !secret.is_empty() => {
                return Ok(Some(ResolvedKey {
                    origin: KeyOrigin::Vault(name),
                    secret,
                }));
            }
            Ok(_) => {}
            Err(e) => vault_error = Some(e),
        }
    }
    if let Some(var) = &spec.api_key_env
        && let Some(value) = lookup.var(var)
    {
        return Ok(Some(ResolvedKey {
            origin: KeyOrigin::Env(var.clone()),
            secret: Secret::new(value),
        }));
    }
    if !spec.wants_key() || spec.api_key_optional {
        return Ok(None);
    }
    Err(vault_error.map_or_else(
        || SourceError::MissingKey {
            source_id: id.to_owned(),
            env: spec.api_key_env.clone(),
        },
        SourceError::VaultUnavailable,
    ))
}

/// Resolves a configuration value that may reference a secret.
///
/// # Errors
///
/// * [`SourceError::Secret`] if a referenced vault entry or variable is missing, the vault
///   cannot be read, the name is malformed, or a command fails
pub fn resolve_value(
    value: &SecretValue,
    lookup: &dyn SecretLookup,
) -> Result<Secret, SourceError> {
    match value {
        SecretValue::Literal(text) => Ok(Secret::new(text.as_str())),
        SecretValue::Ref(reference) => resolve_ref(reference, lookup),
    }
}

/// Resolves a [`SecretRef`].
///
/// # Errors
///
/// * as [`resolve_value`]
pub fn resolve_ref(
    reference: &SecretRef,
    lookup: &dyn SecretLookup,
) -> Result<Secret, SourceError> {
    let fail = |detail: String| SourceError::Secret(format!("{reference}: {detail}"));
    match reference {
        SecretRef::Vault(text) => {
            let name = SecretName::parse(text).map_err(|e| fail(e.to_string()))?;
            match lookup.vault(&name).map_err(fail)? {
                Some(secret) if !secret.is_empty() => Ok(secret),
                _ => Err(fail(format!(
                    "not in the vault (run `bmodels secret set {name}`)"
                ))),
            }
        }
        SecretRef::Env(var) => lookup
            .var(var)
            .map(Secret::new)
            .ok_or_else(|| fail("not set".to_owned())),
        SecretRef::Cmd(cmd) => run_secret_command(cmd).map_err(fail),
    }
}

/// Runs `cmd` through the platform shell and returns its trimmed stdout.
///
/// # Errors
///
/// Returns a description if the command cannot start, exits non-zero, or prints nothing.
pub fn run_secret_command(cmd: &str) -> Result<Secret, String> {
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    };
    let output = command
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let stdout = zeroize::Zeroizing::new(output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("exited with {}: {}", output.status, stderr.trim()));
    }
    let secret = Secret::new(String::from_utf8_lossy(&stdout).into_owned());
    if secret.is_empty() {
        return Err("printed nothing".to_owned());
    }
    Ok(secret)
}

/// Replaces every occurrence of a secret in `text` with `<redacted>`. Use on any error or
/// response text that may echo a request (URLs, headers) before it is stored or shown.
#[must_use]
pub fn redact(text: &str, secrets: &[&Secret]) -> String {
    let mut out = text.to_owned();
    for secret in secrets {
        // Very short values would redact unrelated text; real credentials are longer.
        if secret.expose().len() >= 6 {
            out = out.replace(secret.expose(), "<redacted>");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bmodels_model::SourceKind;

    use super::*;

    #[derive(Default)]
    struct Fake {
        env: BTreeMap<&'static str, &'static str>,
        vault: BTreeMap<String, &'static str>,
        vault_error: bool,
    }

    impl SecretLookup for Fake {
        fn var(&self, name: &str) -> Option<String> {
            self.env.get(name).map(|v| (*v).to_owned())
        }

        fn vault(&self, name: &SecretName) -> Result<Option<Secret>, String> {
            if self.vault_error {
                return Err("locked".to_owned());
            }
            Ok(self.vault.get(&name.to_string()).map(|v| Secret::new(*v)))
        }
    }

    fn spec(env: Option<&str>, optional: bool) -> SourceSpec {
        let mut spec = SourceSpec::new(SourceKind::OpenaiCompatible);
        spec.api_key_env = env.map(str::to_owned);
        spec.api_key_optional = optional;
        spec
    }

    fn key(r: Result<Option<ResolvedKey>, SourceError>) -> Option<(KeyOrigin, String)> {
        r.unwrap().map(|k| (k.origin, k.secret.expose().to_owned()))
    }

    #[test]
    fn env_key_required_and_optional() {
        let env = Fake {
            env: BTreeMap::from([("SET", " k ")]),
            ..Fake::default()
        };
        assert_eq!(
            key(resolve_key("x", &spec(Some("SET"), false), &env)),
            Some((KeyOrigin::Env("SET".into()), "k".into()))
        );
        assert!(matches!(
            resolve_key("x", &spec(Some("UNSET"), false), &env),
            Err(SourceError::MissingKey { source_id, env: Some(v) }) if source_id == "x" && v == "UNSET"
        ));
        assert_eq!(
            key(resolve_key("x", &spec(Some("UNSET"), true), &env)),
            None
        );
        assert_eq!(key(resolve_key("x", &spec(None, false), &env)), None);
    }

    #[test]
    fn vault_wins_over_env_and_serves_keyless_sources() {
        let lookup = Fake {
            env: BTreeMap::from([("SET", "env-key")]),
            vault: BTreeMap::from([("x/API_KEY".to_owned(), "vault-key")]),
            ..Fake::default()
        };
        let name = SecretName::api_key("x").unwrap();
        assert_eq!(
            key(resolve_key("x", &spec(Some("SET"), false), &lookup)),
            Some((KeyOrigin::Vault(name.clone()), "vault-key".into()))
        );
        assert_eq!(
            key(resolve_key("x", &spec(None, false), &lookup)),
            Some((KeyOrigin::Vault(name), "vault-key".into()))
        );
    }

    #[test]
    fn unreadable_vault_falls_back_or_skips() {
        let lookup = Fake {
            env: BTreeMap::from([("SET", "env-key")]),
            vault_error: true,
            ..Fake::default()
        };
        assert_eq!(
            key(resolve_key("x", &spec(Some("SET"), false), &lookup)),
            Some((KeyOrigin::Env("SET".into()), "env-key".into()))
        );
        let err = resolve_key("x", &spec(Some("UNSET"), false), &lookup).unwrap_err();
        assert!(
            matches!(&err, SourceError::VaultUnavailable(_)) && err.is_skip(),
            "{err}"
        );
        assert_eq!(key(resolve_key("x", &spec(None, false), &lookup)), None);
    }

    #[cfg(unix)]
    #[test]
    fn key_command_wins() {
        let lookup = Fake {
            vault: BTreeMap::from([("x/API_KEY".to_owned(), "vault-key")]),
            ..Fake::default()
        };
        let mut s = spec(Some("UNSET"), false);
        s.api_key_cmd = Some("printf 'secret\\n'".into());
        assert_eq!(
            key(resolve_key("x", &s, &lookup)),
            Some((KeyOrigin::Command, "secret".into()))
        );
        s.api_key_cmd = Some("exit 3".into());
        assert!(matches!(
            resolve_key("x", &s, &lookup),
            Err(SourceError::KeyCommand(_))
        ));
    }

    #[test]
    fn values_resolve_from_every_reference_kind() {
        let lookup = Fake {
            env: BTreeMap::from([("TOKEN", "t0ken")]),
            vault: BTreeMap::from([("notify/WEBHOOK".to_owned(), "https://hook")]),
            ..Fake::default()
        };
        let get = |v: SecretValue| {
            resolve_value(&v, &lookup)
                .map(|s| s.expose().to_owned())
                .map_err(|e| e.to_string())
        };
        assert_eq!(get("lit".into()).unwrap(), "lit");
        assert_eq!(
            get(SecretValue::Ref(SecretRef::Vault("notify/WEBHOOK".into()))).unwrap(),
            "https://hook"
        );
        assert_eq!(
            get(SecretValue::Ref(SecretRef::Env("TOKEN".into()))).unwrap(),
            "t0ken"
        );
        let missing = get(SecretValue::Ref(SecretRef::Vault("notify/OTHER".into()))).unwrap_err();
        assert!(
            missing.contains("bmodels secret set notify/OTHER"),
            "{missing}"
        );
        assert!(get(SecretValue::Ref(SecretRef::Vault("bad".into()))).is_err());
        assert!(get(SecretValue::Ref(SecretRef::Env("NOPE".into()))).is_err());
    }

    #[test]
    fn redacts_secrets_from_text() {
        let s = Secret::new("sk-abcdef");
        let short = Secret::new("ab");
        assert_eq!(
            redact("GET https://x/?key=sk-abcdef failed (ab)", &[&s, &short]),
            "GET https://x/?key=<redacted> failed (ab)"
        );
    }
}
