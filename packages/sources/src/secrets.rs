//! API key resolution. Keys are read on demand and never stored, logged, or serialized.

use std::process::Command;

use bmodels_model::SourceSpec;

use crate::SourceError;

/// Looks up environment variables (injected so resolution is testable).
pub trait EnvLookup: Send + Sync {
    /// A non-empty variable.
    fn var(&self, name: &str) -> Option<String>;
}

/// The process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvLookup for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
    }
}

/// Resolves the API key of `spec`.
///
/// `api_key_cmd` wins over `api_key_env`. Returns `Ok(None)` when the source needs no key, or
/// when the key is optional and unavailable.
///
/// # Errors
///
/// * [`SourceError::MissingKey`] if a required key is not configured or empty
/// * [`SourceError::KeyCommand`] if `api_key_cmd` fails or prints nothing
pub fn resolve_key(spec: &SourceSpec, env: &dyn EnvLookup) -> Result<Option<String>, SourceError> {
    if let Some(cmd) = &spec.api_key_cmd {
        let key = run_key_command(cmd)?;
        return Ok(Some(key));
    }
    let Some(var) = &spec.api_key_env else {
        return Ok(None);
    };
    match env.var(var) {
        Some(key) => Ok(Some(key.trim().to_owned())),
        None if spec.api_key_optional => Ok(None),
        None => Err(SourceError::MissingKey(var.clone())),
    }
}

fn run_key_command(cmd: &str) -> Result<String, SourceError> {
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
        .map_err(|e| SourceError::KeyCommand(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SourceError::KeyCommand(format!(
            "exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if key.is_empty() {
        return Err(SourceError::KeyCommand("printed nothing".to_owned()));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bmodels_model::SourceKind;

    use super::*;

    struct MapEnv(BTreeMap<&'static str, &'static str>);

    impl EnvLookup for MapEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).map(|v| (*v).to_owned())
        }
    }

    fn spec(env: Option<&str>, optional: bool) -> SourceSpec {
        let mut spec = SourceSpec::new(SourceKind::OpenaiCompatible);
        spec.api_key_env = env.map(str::to_owned);
        spec.api_key_optional = optional;
        spec
    }

    #[test]
    fn env_key_required_and_optional() {
        let env = MapEnv(BTreeMap::from([("SET", " k ")]));
        assert_eq!(
            resolve_key(&spec(Some("SET"), false), &env).unwrap(),
            Some("k".into())
        );
        assert!(matches!(
            resolve_key(&spec(Some("UNSET"), false), &env),
            Err(SourceError::MissingKey(v)) if v == "UNSET"
        ));
        assert_eq!(resolve_key(&spec(Some("UNSET"), true), &env).unwrap(), None);
        assert_eq!(resolve_key(&spec(None, false), &env).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn key_command() {
        let env = MapEnv(BTreeMap::new());
        let mut s = spec(Some("UNSET"), false);
        s.api_key_cmd = Some("printf 'secret\\n'".into());
        assert_eq!(resolve_key(&s, &env).unwrap(), Some("secret".into()));
        s.api_key_cmd = Some("exit 3".into());
        assert!(matches!(
            resolve_key(&s, &env),
            Err(SourceError::KeyCommand(_))
        ));
    }
}
