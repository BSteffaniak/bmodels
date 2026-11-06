#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Notification sinks: desktop, command hook, and webhook.
//!
//! One sync produces one [`Digest`]; each sink receives the whole digest at once (a desktop
//! notification summarizing up to `max_items` models, the JSON payload for the command and the
//! webhook).

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use bmodels_model::{EventKind, FieldChange, NotifySpec, Timestamp};
use serde::Serialize;

/// Time allowed for each sink.
pub const SINK_TIMEOUT: Duration = Duration::from_secs(15);

/// One announced event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotifyItem {
    /// What happened.
    pub kind: EventKind,
    /// Canonical key (`openai/gpt-6`).
    pub key: String,
    /// Display name.
    pub name: String,
    /// Vendor.
    pub vendor: String,
    /// Source id.
    pub source: String,
    /// Source display name.
    pub source_name: String,
    /// Model id within the source.
    pub model_id: String,
    /// Link, if known.
    pub url: Option<String>,
    /// Tracked changes, for `CHANGED`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<FieldChange>,
    /// Detection time.
    pub at: Timestamp,
}

/// Everything one sync announces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Digest {
    /// Events, newest first.
    pub items: Vec<NotifyItem>,
}

/// A sink failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotifyError {
    /// The desktop notification failed.
    #[error("desktop notification failed: {0}")]
    Desktop(String),
    /// The command hook failed.
    #[error("notify command failed: {0}")]
    Command(String),
    /// The webhook failed.
    #[error("webhook failed: {0}")]
    Webhook(String),
}

impl Digest {
    /// Desktop title and body, naming at most `max` models.
    #[must_use]
    pub fn summary(&self, max: usize) -> (String, String) {
        let count = |k: EventKind| self.items.iter().filter(|i| i.kind == k).count();
        let new = count(EventKind::New);
        let title = if new == self.items.len() {
            format!("{new} new model{}", plural(new))
        } else {
            let mut parts = Vec::new();
            for (kind, label) in [
                (EventKind::New, "new"),
                (EventKind::NewListing, "new listing"),
                (EventKind::Changed, "changed"),
                (EventKind::Removed, "removed"),
                (EventKind::Reappeared, "back"),
            ] {
                let n = count(kind);
                if n > 0 {
                    parts.push(format!("{n} {label}"));
                }
            }
            parts.join(", ")
        };
        let mut lines: Vec<String> = self
            .items
            .iter()
            .take(max.max(1))
            .map(|i| match i.kind {
                EventKind::New => format!("{} ({})", i.name, i.vendor),
                EventKind::NewListing => format!("{} on {}", i.name, i.source_name),
                EventKind::Changed => format!(
                    "{}: {}",
                    i.name,
                    i.changes
                        .iter()
                        .map(|c| c.field.replace('_', " "))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                EventKind::Removed => format!("{} gone from {}", i.name, i.source_name),
                EventKind::Reappeared => format!("{} back on {}", i.name, i.source_name),
            })
            .collect();
        let rest = self.items.len().saturating_sub(max.max(1));
        if rest > 0 {
            lines.push(format!("…and {rest} more"));
        }
        (format!("bmodels: {title}"), lines.join("\n"))
    }

    /// The JSON payload sent to the command hook and the webhook.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"items\":[]}".to_owned())
    }
}

const fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Delivers `digest` to every configured sink, returning one error per failed sink.
pub async fn deliver(spec: &NotifySpec, digest: &Digest) -> Vec<NotifyError> {
    if digest.items.is_empty() {
        return Vec::new();
    }
    let mut errors = Vec::new();
    if spec.desktop {
        let (title, body) = digest.summary(spec.max_items);
        if let Err(e) = desktop(&title, &body) {
            errors.push(e);
        }
    }
    if !spec.command.is_empty()
        && let Err(e) = command(&spec.command, digest.to_json().as_bytes())
    {
        errors.push(e);
    }
    if let Some(url) = &spec.webhook
        && let Err(e) = webhook(url, digest.to_json()).await
    {
        errors.push(e);
    }
    errors
}

/// Shows a desktop notification (with a timeout: some platforms block on a missing daemon).
///
/// # Errors
///
/// * [`NotifyError::Desktop`] if the platform rejects it or it times out
pub fn desktop(title: &str, body: &str) -> Result<(), NotifyError> {
    let (title, body) = (title.to_owned(), body.to_owned());
    with_timeout(SINK_TIMEOUT, move || {
        notify_rust::Notification::new()
            .appname("bmodels")
            .summary(&title)
            .body(&body)
            .show()
            .map(|_| ())
            .map_err(|e| NotifyError::Desktop(e.to_string()))
    })
    .unwrap_or_else(|| Err(NotifyError::Desktop("timed out".to_owned())))
}

/// Runs `argv` with `payload` on stdin.
///
/// # Errors
///
/// * [`NotifyError::Command`] if it cannot start, exits non-zero, or times out
pub fn command(argv: &[String], payload: &[u8]) -> Result<(), NotifyError> {
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| NotifyError::Command("empty command".to_owned()))?;
    let mut child = Command::new(program)
        .args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| NotifyError::Command(format!("{program}: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A hook that ignores stdin closes the pipe early; that is not a failure.
        let _ = stdin.write_all(payload);
    }
    let output = with_timeout(SINK_TIMEOUT, move || child.wait_with_output())
        .ok_or_else(|| NotifyError::Command("timed out".to_owned()))?
        .map_err(|e| NotifyError::Command(e.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(NotifyError::Command(format!(
            "exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// POSTs `json` to `url`.
///
/// # Errors
///
/// * [`NotifyError::Webhook`] on transport failures, non-2xx statuses, or timeouts
pub async fn webhook(url: &str, json: String) -> Result<(), NotifyError> {
    let request = switchy_http::Client::new()
        .post(url)
        .header("content-type", "application/json")
        .header("user-agent", concat!("bmodels/", env!("CARGO_PKG_VERSION")))
        .body(json.into_bytes().into())
        .send();
    let response = tokio::time::timeout(SINK_TIMEOUT, request)
        .await
        .map_err(|_| NotifyError::Webhook("timed out".to_owned()))?
        .map_err(|e| NotifyError::Webhook(e.to_string()))?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(NotifyError::Webhook(format!("HTTP {}", status.as_u16())))
    }
}

/// Runs `f` on a helper thread, giving up after `timeout`.
fn with_timeout<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: EventKind, name: &str) -> NotifyItem {
        NotifyItem {
            kind,
            key: format!("openai/{name}"),
            name: name.to_owned(),
            vendor: "openai".into(),
            source: "openrouter".into(),
            source_name: "OpenRouter".into(),
            model_id: name.to_owned(),
            url: None,
            changes: if kind == EventKind::Changed {
                vec![FieldChange {
                    field: "input_price".into(),
                    old: Some("$2.50".into()),
                    new: Some("$1.25".into()),
                }]
            } else {
                Vec::new()
            },
            at: 1,
        }
    }

    #[test]
    fn summary_titles_and_truncation() {
        let digest = Digest {
            items: vec![item(EventKind::New, "gpt-6"), item(EventKind::New, "gpt-7")],
        };
        let (title, body) = digest.summary(1);
        assert_eq!(title, "bmodels: 2 new models");
        assert_eq!(body, "gpt-6 (openai)\n…and 1 more");

        let mixed = Digest {
            items: vec![item(EventKind::New, "a"), item(EventKind::Changed, "b")],
        };
        let (title, body) = mixed.summary(5);
        assert_eq!(title, "bmodels: 1 new, 1 changed");
        assert!(body.contains("b: input price"));
    }

    #[test]
    fn json_payload() {
        let digest = Digest {
            items: vec![item(EventKind::New, "gpt-6")],
        };
        let json: serde_json::Value = serde_json::from_str(&digest.to_json()).unwrap();
        assert_eq!(json["items"][0]["kind"], "NEW");
        assert_eq!(json["items"][0]["key"], "openai/gpt-6");
    }

    #[cfg(unix)]
    #[test]
    fn command_hook_gets_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("payload.json");
        let argv = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("cat > '{}'", out.display()),
        ];
        command(&argv, b"{\"items\":[]}").unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "{\"items\":[]}");
        assert!(matches!(
            command(&["sh".into(), "-c".into(), "exit 2".into()], b""),
            Err(NotifyError::Command(_))
        ));
    }
}
