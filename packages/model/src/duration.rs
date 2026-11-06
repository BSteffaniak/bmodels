//! Human interval strings: an integer followed by `s`, `m`, `h`, `d`, or `w` (`"90s"`, `"6h"`,
//! `"1d"`). Several terms may be concatenated (`"1h30m"`).

use std::time::Duration;

/// Error parsing an interval string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid duration `{input}`: expected e.g. `30s`, `15m`, `6h`, `1d`, `1h30m`")]
pub struct DurationError {
    /// The rejected input.
    pub input: String,
}

/// Parses an interval string.
///
/// # Errors
///
/// * [`DurationError`] if the string is empty, has a unit without a number, a number without a
///   unit, an unknown unit, or overflows
pub fn parse_duration(input: &str) -> Result<Duration, DurationError> {
    let err = || DurationError {
        input: input.to_owned(),
    };
    let text = input.trim();
    if text.is_empty() {
        return Err(err());
    }
    let mut total: u64 = 0;
    let mut number: Option<u64> = None;
    for c in text.chars() {
        if let Some(digit) = c.to_digit(10) {
            number = Some(
                number
                    .unwrap_or(0)
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(u64::from(digit)))
                    .ok_or_else(err)?,
            );
            continue;
        }
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            'w' => 604_800,
            _ => return Err(err()),
        };
        let n = number.take().ok_or_else(err)?;
        total = n
            .checked_mul(unit)
            .and_then(|v| total.checked_add(v))
            .ok_or_else(err)?;
    }
    if number.is_some() {
        return Err(err());
    }
    Ok(Duration::from_secs(total))
}

/// Formats a duration compactly using its two most significant units (`"6h"`, `"1d2h"`,
/// `"45s"`).
#[must_use]
pub fn format_duration(duration: Duration) -> String {
    let mut secs = duration.as_secs();
    if secs == 0 {
        return "0s".to_owned();
    }
    let mut out = String::new();
    let mut terms = 0;
    for (unit, label) in [(86_400, 'd'), (3_600, 'h'), (60, 'm'), (1, 's')] {
        if secs >= unit && terms < 2 {
            out.push_str(&(secs / unit).to_string());
            out.push(label);
            secs %= unit;
            terms += 1;
        } else if terms > 0 {
            // Stop after the first gap so "1d0h5m" never happens.
            terms = 2;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units_and_compounds() {
        assert_eq!(parse_duration("30s").unwrap().as_secs(), 30);
        assert_eq!(parse_duration("6h").unwrap().as_secs(), 21_600);
        assert_eq!(parse_duration("1d").unwrap().as_secs(), 86_400);
        assert_eq!(parse_duration("1h30m").unwrap().as_secs(), 5_400);
        assert_eq!(parse_duration(" 2w ").unwrap().as_secs(), 1_209_600);
    }

    #[test]
    fn rejects_malformed() {
        for bad in ["", "h", "10", "5x", "1.5h", "-1h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn formats_compactly() {
        assert_eq!(format_duration(secs(21_600)), "6h");
        assert_eq!(format_duration(secs(93_600)), "1d2h");
        assert_eq!(format_duration(secs(86_405)), "1d");
        assert_eq!(format_duration(secs(45)), "45s");
        assert_eq!(format_duration(Duration::ZERO), "0s");
    }
}
