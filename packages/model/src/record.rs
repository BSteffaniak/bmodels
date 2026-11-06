//! What source adapters return, and the tracked-field diff between two observations.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::Timestamp;

/// Whether a record is a model or an announcement (a feed item).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RecordKind {
    /// A model.
    #[default]
    Model,
    /// A changelog / news item.
    Announcement,
}

impl RecordKind {
    /// Storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Model => "MODEL",
            Self::Announcement => "ANNOUNCEMENT",
        }
    }

    /// Parses the storage spelling (unknown values read as [`RecordKind::Model`]).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        if s == "ANNOUNCEMENT" {
            Self::Announcement
        } else {
            Self::Model
        }
    }
}

/// One model (or announcement) as reported by a source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRecord {
    /// Identifier exactly as the source reports it (`"gpt-4o"`, `"openai/gpt-4o"`).
    pub model_id: String,
    /// Model or announcement.
    #[serde(default)]
    pub kind: RecordKind,
    /// Human name, when the source has one.
    pub display_name: Option<String>,
    /// Vendor the adapter inferred (`"openai"` for OpenRouter's `openai/gpt-4o`).
    pub vendor_hint: Option<String>,
    /// When the source says the model was created or released.
    pub created_at: Option<Timestamp>,
    /// Link to the model's page.
    pub url: Option<String>,
    /// Normalized metadata.
    pub info: ModelInfo,
}

impl ModelRecord {
    /// A record with only an id.
    #[must_use]
    pub fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            kind: RecordKind::Model,
            display_name: None,
            vendor_hint: None,
            created_at: None,
            url: None,
            info: ModelInfo::default(),
        }
    }
}

/// Normalized metadata. Only the *tracked* fields (see [`ModelInfo::diff`]) produce change
/// events; the rest are informational and update silently.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelInfo {
    /// Short description (untracked: descriptions churn).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Owner reported by the source (untracked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    /// Context window in tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Maximum output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
    /// USD per million input tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_price: Option<f64>,
    /// USD per million output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_price: Option<f64>,
    /// Input modalities (`text`, `image`, ...).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub input_modalities: Vec<String>,
    /// Output modalities.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub output_modalities: Vec<String>,
    /// Capability flags (`tools`, `reasoning`, `vision`, ...).
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub capabilities: BTreeSet<String>,
    /// Size / quantization variants (`8b`, `70b`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// Deprecation or shutdown date (`YYYY-MM-DD`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecation_date: Option<String>,
    /// Knowledge cutoff (untracked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub knowledge_cutoff: Option<String>,
    /// Open weights (untracked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_weights: Option<bool>,
}

/// One tracked field that changed between two observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldChange {
    /// Field name (`context_window`, `input_price`, ...).
    pub field: String,
    /// Previous value, rendered for display (`None` = absent).
    pub old: Option<String>,
    /// New value, rendered for display (`None` = absent).
    pub new: Option<String>,
}

fn fmt_list(v: &[String]) -> Option<String> {
    (!v.is_empty()).then(|| v.join(", "))
}

/// Formats a USD-per-million price (`$2.50`, `$0.075`, `free`).
#[must_use]
pub fn format_price(price: f64) -> String {
    if price == 0.0 {
        "free".to_owned()
    } else if price >= 0.1 {
        format!("${price:.2}")
    } else {
        let s = format!("${price:.4}");
        s.trim_end_matches('0').to_owned()
    }
}

/// Formats a token count (`128K`, `1M`, `1.05M`, `8192`).
#[must_use]
#[allow(clippy::cast_precision_loss)] // token counts are far below f64 precision limits
pub fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let m = tokens as f64 / 1_000_000.0;
        let s = format!("{m:.2}");
        format!("{}M", s.trim_end_matches('0').trim_end_matches('.'))
    } else if tokens >= 1_000 && (tokens.is_multiple_of(1_000) || tokens.is_multiple_of(1_024)) {
        let k = if tokens.is_multiple_of(1_000) {
            tokens / 1_000
        } else {
            tokens / 1_024
        };
        format!("{k}K")
    } else {
        tokens.to_string()
    }
}

impl ModelInfo {
    /// Tracked fields that differ from `old` to `self`, in a stable order.
    ///
    /// Prices count as changed only when they appear, disappear, or move by more than
    /// `price_tolerance` (a fraction: `0.05` = 5%), so aggregator price wobble stays quiet.
    #[must_use]
    pub fn diff(&self, old: &Self, price_tolerance: f64) -> Vec<FieldChange> {
        let mut out = Vec::new();
        let mut push = |field: &str, o: Option<String>, n: Option<String>| {
            if o != n {
                out.push(FieldChange {
                    field: field.to_owned(),
                    old: o,
                    new: n,
                });
            }
        };
        push(
            "context_window",
            old.context_window.map(format_tokens),
            self.context_window.map(format_tokens),
        );
        push(
            "max_output",
            old.max_output.map(format_tokens),
            self.max_output.map(format_tokens),
        );
        if !prices_equal(old.input_price, self.input_price, price_tolerance) {
            push(
                "input_price",
                old.input_price.map(format_price),
                self.input_price.map(format_price),
            );
        }
        if !prices_equal(old.output_price, self.output_price, price_tolerance) {
            push(
                "output_price",
                old.output_price.map(format_price),
                self.output_price.map(format_price),
            );
        }
        push(
            "input_modalities",
            fmt_list(&sorted(&old.input_modalities)),
            fmt_list(&sorted(&self.input_modalities)),
        );
        push(
            "output_modalities",
            fmt_list(&sorted(&old.output_modalities)),
            fmt_list(&sorted(&self.output_modalities)),
        );
        let caps = |c: &BTreeSet<String>| fmt_list(&c.iter().cloned().collect::<Vec<_>>());
        push(
            "capabilities",
            caps(&old.capabilities),
            caps(&self.capabilities),
        );
        push(
            "variants",
            fmt_list(&sorted(&old.variants)),
            fmt_list(&sorted(&self.variants)),
        );
        push(
            "deprecation_date",
            old.deprecation_date.clone(),
            self.deprecation_date.clone(),
        );
        out
    }

    /// `"$2.50 / $10.00"` (input / output per 1M tokens), if any price is known.
    #[must_use]
    pub fn price_summary(&self) -> Option<String> {
        match (self.input_price, self.output_price) {
            (None, None) => None,
            (i, o) => Some(format!(
                "{} / {}",
                i.map_or_else(|| "?".to_owned(), format_price),
                o.map_or_else(|| "?".to_owned(), format_price)
            )),
        }
    }
}

fn sorted(v: &[String]) -> Vec<String> {
    let mut v = v.to_vec();
    v.sort();
    v.dedup();
    v
}

fn prices_equal(a: Option<f64>, b: Option<f64>, tolerance: f64) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            (a - b).abs() <= 1e-9_f64.max(a.abs().max(b.abs()) * tolerance.max(1e-6))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_reports_only_tracked_changes() {
        let old = ModelInfo {
            description: Some("old".into()),
            context_window: Some(128_000),
            input_price: Some(2.5),
            input_modalities: vec!["text".into(), "image".into()],
            ..ModelInfo::default()
        };
        let mut new = old.clone();
        new.description = Some("new".into());
        new.input_modalities = vec!["image".into(), "text".into()];
        new.input_price = Some(2.500_000_000_1);
        assert_eq!(new.diff(&old, 0.0), Vec::new());
        // A 2% move is noise at 5% tolerance and a change at 0%.
        let mut wobble = old.clone();
        wobble.input_price = Some(2.45);
        assert_eq!(wobble.diff(&old, 0.05), Vec::new());
        assert_eq!(wobble.diff(&old, 0.0).len(), 1);

        new.context_window = Some(1_000_000);
        new.output_price = Some(10.0);
        let changes = new.diff(&old, 0.05);
        assert_eq!(
            changes,
            vec![
                FieldChange {
                    field: "context_window".into(),
                    old: Some("128K".into()),
                    new: Some("1M".into()),
                },
                FieldChange {
                    field: "output_price".into(),
                    old: None,
                    new: Some("$10.00".into()),
                },
            ]
        );
    }

    #[test]
    fn formats_tokens_and_prices() {
        assert_eq!(format_tokens(128_000), "128K");
        assert_eq!(format_tokens(131_072), "128K");
        assert_eq!(format_tokens(1_048_576), "1.05M");
        assert_eq!(format_tokens(2_000_000), "2M");
        assert_eq!(format_tokens(4_097), "4097");
        assert_eq!(format_price(2.5), "$2.50");
        assert_eq!(format_price(0.075), "$0.075");
        assert_eq!(format_price(0.0), "free");
    }
}
