//! Plaintext secrets left in configuration.

use bmodels_model::{Config, SecretName};

/// A sensitive value written in plaintext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaintextFinding {
    /// Dotted configuration key (`notify.webhook`, `sources.local.headers.x-api-key`).
    pub key: String,
    /// Suggested vault entry to move it to.
    pub suggestion: SecretName,
}

impl PlaintextFinding {
    /// The TOML that replaces the plaintext value.
    #[must_use]
    pub fn replacement(&self) -> String {
        format!("{{ vault = \"{}\" }}", self.suggestion)
    }
}

/// Header names that carry credentials.
const SENSITIVE_HEADER_PARTS: [&str; 7] = [
    "auth", "key", "token", "secret", "cookie", "session", "password",
];

/// Finds plaintext webhooks and credential-looking headers.
#[must_use]
pub fn plaintext_secrets(config: &Config) -> Vec<PlaintextFinding> {
    let mut findings = Vec::new();
    for (id, spec) in &config.sources {
        for (header, value) in &spec.headers {
            let lower = header.to_ascii_lowercase();
            if value.is_literal() && SENSITIVE_HEADER_PARTS.iter().any(|p| lower.contains(p)) {
                let var: String = header
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() {
                            c.to_ascii_uppercase()
                        } else {
                            '_'
                        }
                    })
                    .collect();
                if let Ok(suggestion) = SecretName::new(id, &var) {
                    findings.push(PlaintextFinding {
                        key: format!("sources.{id}.headers.{header}"),
                        suggestion,
                    });
                }
            }
        }
    }
    if config
        .notify
        .webhook
        .as_ref()
        .is_some_and(bmodels_model::SecretValue::is_literal)
        && let Ok(suggestion) = SecretName::new("notify", "WEBHOOK")
    {
        findings.push(PlaintextFinding {
            key: "notify.webhook".to_owned(),
            suggestion,
        });
    }
    findings
}

#[cfg(test)]
mod tests {
    use bmodels_model::{SecretRef, SecretValue, SourceKind, SourceSpec};

    use super::*;

    #[test]
    fn flags_credential_headers_and_webhooks_only() {
        let mut config = Config::default();
        let mut local = SourceSpec::new(SourceKind::OpenaiCompatible);
        local.headers.insert("X-Api-Key".into(), "k".into());
        local.headers.insert("User-Agent".into(), "me".into());
        local.headers.insert(
            "Authorization".into(),
            SecretValue::Ref(SecretRef::Vault("local/AUTHORIZATION".into())),
        );
        config.sources.insert("local".into(), local);
        config.notify.webhook = Some("https://hooks.slack.com/x".into());
        let findings = plaintext_secrets(&config);
        assert_eq!(
            findings
                .iter()
                .map(|f| (f.key.as_str(), f.replacement()))
                .collect::<Vec<_>>(),
            [
                (
                    "sources.local.headers.X-Api-Key",
                    "{ vault = \"local/X_API_KEY\" }".to_owned()
                ),
                (
                    "notify.webhook",
                    "{ vault = \"notify/WEBHOOK\" }".to_owned()
                ),
            ]
        );
    }
}
