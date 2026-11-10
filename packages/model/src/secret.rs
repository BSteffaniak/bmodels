//! Secret values and references.
//!
//! Configuration never needs to hold a secret: any sensitive field accepts a [`SecretRef`]
//! (`{ vault = "notify/WEBHOOK" }`, `{ env = "VAR" }`, `{ cmd = "op read ..." }`) instead of a
//! literal. Resolved values travel as [`Secret`], which redacts itself in `Debug` and wipes its
//! memory on drop.

use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// A resolved secret value. `Debug` prints `<redacted>`; the buffer is wiped on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Wraps `value`, trimming surrounding whitespace.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        let mut value: String = value.into();
        let trimmed = value.trim();
        if trimmed.len() != value.len() {
            let owned = trimmed.to_owned();
            zeroize::Zeroize::zeroize(&mut value);
            value = owned;
        }
        Self(Zeroizing::new(value))
    }

    /// The plaintext. Keep the borrow short; never log it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Moves the value into a zeroizing string (for APIs that take one).
    #[must_use]
    pub fn into_zeroizing(self) -> Zeroizing<String> {
        self.0
    }
}

impl From<Zeroizing<String>> for Secret {
    fn from(value: Zeroizing<String>) -> Self {
        Self::new(value.as_str())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A vault entry: `profile/VAR`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretName {
    /// Vault profile (`openai`, `notify`).
    pub profile: String,
    /// Variable inside the profile (`API_KEY`, `WEBHOOK`).
    pub var: String,
}

/// Variable name of a source's API key inside its vault profile.
pub const API_KEY_VAR: &str = "API_KEY";

/// A malformed [`SecretName`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid secret name `{0}`: expected `profile/VAR` (profile: letters, digits, `-`, `_`, `.`; VAR: letters, digits, `_`)"
)]
pub struct InvalidSecretName(pub String);

impl SecretName {
    /// A name from parts.
    ///
    /// # Errors
    ///
    /// * [`InvalidSecretName`] if either part has disallowed characters or is empty
    pub fn new(profile: &str, var: &str) -> Result<Self, InvalidSecretName> {
        let profile_ok = !profile.is_empty()
            && profile
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        let var_ok = !var.is_empty() && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if profile_ok && var_ok {
            Ok(Self {
                profile: profile.to_owned(),
                var: var.to_owned(),
            })
        } else {
            Err(InvalidSecretName(format!("{profile}/{var}")))
        }
    }

    /// Parses `profile/VAR`.
    ///
    /// # Errors
    ///
    /// * [`InvalidSecretName`] if there is no `/` or a part is invalid
    pub fn parse(text: &str) -> Result<Self, InvalidSecretName> {
        let (profile, var) = text
            .split_once('/')
            .ok_or_else(|| InvalidSecretName(text.to_owned()))?;
        Self::new(profile, var).map_err(|_| InvalidSecretName(text.to_owned()))
    }

    /// The API key entry of source `id` (`<id>/API_KEY`).
    ///
    /// # Errors
    ///
    /// * [`InvalidSecretName`] if `id` is not a valid profile name
    pub fn api_key(id: &str) -> Result<Self, InvalidSecretName> {
        Self::new(id, API_KEY_VAR)
    }
}

impl fmt::Display for SecretName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.profile, self.var)
    }
}

/// Where a secret lives. Written in TOML as a one-key inline table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SecretRef {
    /// A bmodels vault entry, `profile/VAR` (`bmodels secret set` writes these).
    Vault(String),
    /// An environment variable.
    Env(String),
    /// A command whose trimmed stdout is the value (run through the platform shell).
    Cmd(String),
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vault(name) => write!(f, "vault:{name}"),
            Self::Env(var) => write!(f, "env:{var}"),
            Self::Cmd(_) => f.write_str("cmd"),
        }
    }
}

/// A configuration value that may be sensitive: a reference, or (discouraged) a literal.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SecretValue {
    /// Resolved at use time.
    Ref(SecretRef),
    /// Plaintext in the configuration file. `bmodels secret check` flags these.
    Literal(String),
}

impl SecretValue {
    /// The reference, if this is not a literal.
    #[must_use]
    pub const fn as_ref(&self) -> Option<&SecretRef> {
        match self {
            Self::Ref(r) => Some(r),
            Self::Literal(_) => None,
        }
    }

    /// Whether this is a plaintext literal.
    #[must_use]
    pub const fn is_literal(&self) -> bool {
        matches!(self, Self::Literal(_))
    }

    /// A copy safe to print: literals become `"<redacted>"`.
    #[must_use]
    pub fn redacted(&self) -> Self {
        match self {
            Self::Ref(r) => Self::Ref(r.clone()),
            Self::Literal(_) => Self::Literal("<redacted>".to_owned()),
        }
    }
}

impl From<&str> for SecretValue {
    fn from(value: &str) -> Self {
        Self::Literal(value.to_owned())
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ref(r) => write!(f, "Ref({r})"),
            Self::Literal(_) => f.write_str("Literal(<redacted>)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Doc {
        a: SecretValue,
        b: SecretValue,
        c: SecretValue,
    }

    #[test]
    fn decodes_references_and_literals() {
        let doc: Doc = toml::from_str(
            r#"
            a = "plain"
            b = { vault = "notify/WEBHOOK" }
            c = { env = "TOKEN" }
            "#,
        )
        .unwrap();
        assert_eq!(doc.a, SecretValue::Literal("plain".into()));
        assert_eq!(
            doc.b,
            SecretValue::Ref(SecretRef::Vault("notify/WEBHOOK".into()))
        );
        assert_eq!(doc.c, SecretValue::Ref(SecretRef::Env("TOKEN".into())));
        assert!(toml::from_str::<Doc>("a = { nope = 1 }\nb = \"x\"\nc = \"y\"").is_err());
    }

    #[test]
    fn debug_never_shows_values() {
        assert_eq!(format!("{:?}", Secret::new("sk-1")), "<redacted>");
        assert_eq!(
            format!("{:?}", SecretValue::from("sk-1")),
            "Literal(<redacted>)"
        );
        assert_eq!(Secret::new("  k \n").expose(), "k");
    }

    #[test]
    fn names_parse_and_validate() {
        let name = SecretName::parse("openai/API_KEY").unwrap();
        assert_eq!(name, SecretName::api_key("openai").unwrap());
        assert_eq!(name.to_string(), "openai/API_KEY");
        for bad in ["openai", "/K", "p/", "a b/K", "p/K-1", "p/K/x"] {
            assert!(SecretName::parse(bad).is_err(), "{bad}");
        }
    }
}
