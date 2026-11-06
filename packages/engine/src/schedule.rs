//! When each source is next due.
//!
//! A source is due `interval` after its last attempt. After a failure it retries sooner, with
//! exponential backoff starting at 5 minutes (5m, 10m, 20m, ...), never later than `interval`.

use std::collections::BTreeMap;

use bmodels_model::{Config, SourceState, Timestamp};

/// First retry delay after a failure.
pub const RETRY_BASE_SECS: i64 = 300;

/// When a source is next due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    /// Source id.
    pub source: String,
    /// Next due time (`<= now` means due now).
    pub next_at: Timestamp,
}

/// Next due time of every enabled source, soonest first.
#[must_use]
pub fn schedule(
    config: &Config,
    states: &BTreeMap<String, SourceState>,
    now: Timestamp,
) -> Vec<Due> {
    let mut out: Vec<Due> = config
        .enabled_sources()
        .map(|(id, spec)| {
            let interval = config
                .interval_of(spec)
                .map_or(21_600, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            let next_at = states
                .get(id)
                .and_then(|s| s.last_attempt_at.map(|at| (s, at)))
                .map_or(now, |(state, at)| {
                    let wait = if state.last_error.is_some() && state.failures > 0 {
                        let shift = state.failures.saturating_sub(1).min(20);
                        RETRY_BASE_SECS.saturating_mul(1_i64 << shift).min(interval)
                    } else {
                        interval
                    };
                    at.saturating_add(wait)
                });
            Due {
                source: id.clone(),
                next_at,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.next_at
            .cmp(&b.next_at)
            .then_with(|| a.source.cmp(&b.source))
    });
    out
}

/// Enabled sources due at `now`.
#[must_use]
pub fn due_now(
    config: &Config,
    states: &BTreeMap<String, SourceState>,
    now: Timestamp,
) -> Vec<String> {
    schedule(config, states, now)
        .into_iter()
        .filter(|d| d.next_at <= now)
        .map(|d| d.source)
        .collect()
}

#[cfg(test)]
mod tests {
    use bmodels_model::{SourceKind, SourceSpec};

    use super::*;

    fn config() -> Config {
        let mut config = Config::default();
        let mut fast = SourceSpec::new(SourceKind::Openrouter);
        fast.interval = Some("1h".into());
        config.sources.insert("fast".into(), fast);
        config
            .sources
            .insert("slow".into(), SourceSpec::new(SourceKind::ModelsDev));
        let mut off = SourceSpec::new(SourceKind::Feed);
        off.enabled = false;
        config.sources.insert("off".into(), off);
        config
    }

    fn state(at: Timestamp, failures: u32) -> SourceState {
        SourceState {
            last_attempt_at: Some(at),
            failures,
            last_error: (failures > 0).then(|| "boom".to_owned()),
            ..SourceState::default()
        }
    }

    #[test]
    fn never_synced_is_due_and_disabled_is_ignored() {
        assert_eq!(due_now(&config(), &BTreeMap::new(), 100), ["fast", "slow"]);
    }

    #[test]
    fn intervals_and_backoff() {
        let states = BTreeMap::from([
            ("fast".to_owned(), state(0, 0)),
            ("slow".to_owned(), state(0, 3)),
        ]);
        let s = schedule(&config(), &states, 0);
        // slow failed 3 times: retry after 5m * 2^2 = 20m; fast waits its 1h interval.
        assert_eq!(
            s[0],
            Due {
                source: "slow".into(),
                next_at: 1_200
            }
        );
        assert_eq!(
            s[1],
            Due {
                source: "fast".into(),
                next_at: 3_600
            }
        );
        assert_eq!(due_now(&config(), &states, 1_200), ["slow"]);
        // Backoff never exceeds the interval.
        let states = BTreeMap::from([("fast".to_owned(), state(0, 10))]);
        let fast = schedule(&config(), &states, 0)
            .into_iter()
            .find(|d| d.source == "fast")
            .unwrap();
        assert_eq!(fast.next_at, 3_600);
    }
}
