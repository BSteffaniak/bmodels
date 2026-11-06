//! Human formatting shared by the CLI and the TUI.

use crate::Timestamp;

pub use crate::record::{format_price, format_tokens};

/// `"just now"` or `"5m ago"` (falls back to the date for old timestamps).
#[must_use]
pub fn format_ago(now: Timestamp, ts: Timestamp) -> String {
    match format_age(now, ts) {
        age if age == "now" => "just now".to_owned(),
        age if age.contains('-') => age,
        age => format!("{age} ago"),
    }
}

/// `YYYY-MM-DD` (UTC) of a Unix timestamp.
#[must_use]
pub fn format_date(ts: Timestamp) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM-DD HH:MM` (UTC) of a Unix timestamp.
#[must_use]
pub fn format_datetime(ts: Timestamp) -> String {
    let secs = ts.rem_euclid(86_400);
    format!(
        "{} {:02}:{:02}",
        format_date(ts),
        secs / 3_600,
        (secs % 3_600) / 60
    )
}

/// Compact age of `ts` relative to `now`: `now`, `5m`, `3h`, `2d`, `5w`, then the date.
#[must_use]
pub fn format_age(now: Timestamp, ts: Timestamp) -> String {
    let delta = now.saturating_sub(ts);
    match delta {
        i64::MIN..60 => "now".to_owned(),
        60..3_600 => format!("{}m", delta / 60),
        3_600..86_400 => format!("{}h", delta / 3_600),
        86_400..1_209_600 => format!("{}d", delta / 86_400),
        1_209_600..7_776_000 => format!("{}w", delta / 604_800),
        _ => format_date(ts),
    }
}

/// Days since 1970-01-01 → (year, month, day) in the proleptic Gregorian calendar
/// (Howard Hinnant's `civil_from_days`).
const fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 1..=31 and 1..=12
    let (m, d) = (m as u32, d as u32);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(1_722_902_400), "2024-08-06");
        assert_eq!(format_date(951_782_400), "2000-02-29");
        assert_eq!(format_datetime(1_722_902_400 + 3_661), "2024-08-06 01:01");
        assert_eq!(format_date(-86_400), "1969-12-31");
    }

    #[test]
    fn ages() {
        let now = 1_722_902_400;
        assert_eq!(format_age(now, now - 5), "now");
        assert_eq!(format_age(now, now - 300), "5m");
        assert_eq!(format_age(now, now - 7_200), "2h");
        assert_eq!(format_age(now, now - 3 * 86_400), "3d");
        assert_eq!(format_age(now, now - 21 * 86_400), "3w");
        assert_eq!(format_age(now, now - 200 * 86_400), "2024-01-19");
        assert_eq!(format_age(now, now + 50), "now");
        assert_eq!(format_ago(now, now), "just now");
        assert_eq!(format_ago(now, now - 300), "5m ago");
        assert_eq!(format_ago(now, now - 200 * 86_400), "2024-01-19");
    }
}
