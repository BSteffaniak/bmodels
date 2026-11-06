//! Tolerant JSON / date / HTML helpers shared by the adapters.
//!
//! Registries change shape often, so adapters read `serde_json::Value`s through these
//! accessors instead of strict structs: a missing or oddly typed optional field degrades to
//! `None` rather than failing the whole source.

use bmodels_model::Timestamp;
use serde_json::Value;

use crate::SourceError;

/// Parses a JSON body.
///
/// # Errors
///
/// * [`SourceError::Parse`] if the body is not JSON
pub fn json(body: &[u8]) -> Result<Value, SourceError> {
    serde_json::from_slice(body).map_err(|e| SourceError::Parse(format!("invalid JSON: {e}")))
}

/// A non-empty string field.
#[must_use]
pub fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// An owned non-empty string field.
#[must_use]
pub fn string_at(v: &Value, key: &str) -> Option<String> {
    str_at(v, key).map(str::to_owned)
}

/// A non-negative integer field, also accepting integral floats and numeric strings.
#[must_use]
pub fn u64_at(v: &Value, key: &str) -> Option<u64> {
    as_u64(v.get(key)?)
}

/// See [`u64_at`].
#[must_use]
pub fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| *f >= 0.0 && f.fract() == 0.0 && *f < 1.8e19)
                .map(f64_to_u64)
        }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // range checked by caller
const fn f64_to_u64(f: f64) -> u64 {
    f as u64
}

/// A finite float field, also accepting numeric strings.
#[must_use]
pub fn f64_at(v: &Value, key: &str) -> Option<f64> {
    as_f64(v.get(key)?)
}

/// See [`f64_at`].
#[must_use]
pub fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|f: &f64| f.is_finite())
}

/// A boolean field.
#[must_use]
pub fn bool_at(v: &Value, key: &str) -> Option<bool> {
    v.get(key)?.as_bool()
}

/// A string-array field (non-strings skipped).
#[must_use]
pub fn strings_at(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Converts a USD-per-token price to USD per million tokens. Negative prices (used for
/// "variable" by some registries) become `None`.
#[must_use]
pub fn per_million(per_token: Option<f64>) -> Option<f64> {
    per_token
        .filter(|p| *p >= 0.0)
        .map(|p| (p * 1_000_000.0 * 1e6).round() / 1e6)
}

/// Unix seconds from an RFC 3339 timestamp, an RFC 2822 date, `YYYY-MM-DD`, or `YYYY-MM`.
#[must_use]
pub fn timestamp(s: &str) -> Option<Timestamp> {
    let s = s.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) {
        return Some(dt.timestamp());
    }
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d"))
        .ok()?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp())
}

/// A Unix-seconds field (number) or a date string field.
#[must_use]
pub fn timestamp_at(v: &Value, key: &str) -> Option<Timestamp> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64().filter(|t| *t > 0),
        Value::String(s) => timestamp(s),
        _ => None,
    }
}

/// Decodes the handful of HTML entities registries actually emit.
#[must_use]
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        if let Some(c) = decoded {
            out.push(c);
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Removes HTML tags, decodes entities, collapses whitespace, and truncates to `max` chars.
#[must_use]
pub fn plain_text(html: &str, max: usize) -> String {
    let mut out = String::with_capacity(html.len().min(max * 2));
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let text = decode_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() <= max {
        text
    } else {
        let mut t: String = text.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_from_strings_and_floats() {
        let v: Value = serde_json::json!({"a": "128000", "b": 4096.0, "c": "0.0000025", "d": -1});
        assert_eq!(u64_at(&v, "a"), Some(128_000));
        assert_eq!(u64_at(&v, "b"), Some(4_096));
        assert_eq!(per_million(f64_at(&v, "c")), Some(2.5));
        assert_eq!(per_million(f64_at(&v, "d")), None);
    }

    #[test]
    fn dates() {
        assert_eq!(timestamp("2024-08-06"), Some(1_722_902_400));
        assert_eq!(timestamp("2024-08"), Some(1_722_470_400));
        assert_eq!(timestamp("2024-08-06T00:00:00Z"), Some(1_722_902_400));
        assert_eq!(
            timestamp("Tue, 06 Aug 2024 00:00:00 GMT"),
            Some(1_722_902_400)
        );
        assert_eq!(timestamp("soon"), None);
    }

    #[test]
    fn html_helpers() {
        assert_eq!(
            decode_entities("AutoTrust&#8217;s &amp; co &bogus"),
            "AutoTrust’s & co &bogus"
        );
        assert_eq!(plain_text("<p>Hello <b>world</b></p>", 100), "Hello world");
        assert_eq!(plain_text("abcdef", 4), "abc…");
    }
}
