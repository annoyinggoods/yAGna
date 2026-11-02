//! The visible pauses: the waits a long run takes when a rate limit or
//! a queue backoff says so, counted down through the caller's sink so
//! the status line always says what the run is waiting for.
//!
//! The shape: `ensure_headroom` — the free `/rate_limit` probe before a
//! burst, pausing until the reset when the budget is under the floor;
//! `wait_until` — the pause itself, one line per tick, heartbeat kept
//! alive; `RATE_LIMIT_FLOOR` — the budget a burst refuses to start
//! under.
//!
//! Items: `RATE_LIMIT_FLOOR` — the floor; `wait_until` — the visible
//! pause; `ensure_headroom` — the pre-burst probe; `human_wait` — the
//! countdown's clock.
//! Depends on: yagna_forge (the probe through `ForgeHost`),
//! yagna_core (the report's reset), chrono.
//! Called by: crate::sync (the sweep), crate::enrich (the queue
//! runners), crate::prefetch (its burst).

use std::time::Duration;

use chrono::{DateTime, Utc};
use yagna_core::model::RateLimitReport;
use yagna_forge::ForgeHost;

/// The core budget a burst refuses to run under: remaining requests
/// below this mean pause until the reset first — one sweep page or a
/// handful of enrichment items would burn it and die mid-run.
pub(crate) const RATE_LIMIT_FLOOR: u64 = 25;

/// The countdown's tick: one status line per this much waiting.
const PAUSE_TICK: Duration = Duration::from_secs(30);

/// The longest rate-limit pause honored whole: past it something is
/// wrong (clock skew, a reset days away) and the caller decides — the
/// run proceeds on its own retry policy rather than hanging forever.
pub(crate) const MAX_PAUSE: Duration = Duration::from_secs(90 * 60);

/// The pause itself: sleep until `until`, one countdown line per tick
/// through `sink`, the heartbeat kept alive across the sleep (a
/// claimed queue row must not lose its lease while the run waits).
/// `false` when the wait exceeds `cap` — the caller moves on.
pub(crate) fn wait_until(
    until: DateTime<Utc>,
    what: &str,
    heartbeat: &mut dyn FnMut(),
    sink: &mut impl FnMut(String),
    cap: Duration,
) -> bool {
    loop {
        let Ok(remaining) = (until - Utc::now()).to_std() else {
            return true;
        };
        if remaining > cap {
            return false;
        }
        sink(format!("paused: {what} in {}", human_wait(remaining)));
        let tick = remaining.min(PAUSE_TICK);
        std::thread::sleep(tick);
        heartbeat();
    }
}

/// The pre-burst probe: one free `/rate_limit` request; under the
/// floor, the run pauses until the reset (visibly) before its first
/// request. `false` when the wait was refused — the caller proceeds on
/// its own policy. A probe that cannot be made is silence, not an
/// error: the run's own retries speak when the limit really bites.
pub(crate) fn ensure_headroom(
    host: &ForgeHost,
    token: &str,
    sink: &mut impl FnMut(String),
) -> bool {
    let Ok(report) = host.rate_limit(Some(token)) else {
        return true;
    };
    if report.core_remaining >= RATE_LIMIT_FLOOR {
        return true;
    }
    sink(format!(
        "rate limit low: {} of {} core requests left — checking the reset",
        report.core_remaining, report.core_limit
    ));
    match_reset(report, sink)
}

/// The page's ride-along budget: under the floor, the same visible
/// pause as the probe's, driven from the sweep page's own
/// `rateLimit { remaining resetAt }` fields.
pub(crate) fn ensure_page_headroom(
    remaining: Option<u64>,
    reset: Option<DateTime<Utc>>,
    sink: &mut impl FnMut(String),
) -> bool {
    let Some(remaining) = remaining else {
        return true;
    };
    if remaining >= RATE_LIMIT_FLOOR {
        return true;
    }
    sink(format!(
        "rate limit low: {remaining} core requests left — checking the reset"
    ));
    match Some(reset) {
        Some(Some(until)) => wait_until(until, "rate limit resets", &mut || {}, sink, MAX_PAUSE),
        _ => {
            sink(
                "rate limit low: the page named no reset — the retry ladder speaks when it bites"
                    .to_string(),
            );
            true
        }
    }
}

/// Waits out the report's reset, when it is worth waiting for.
fn match_reset(report: RateLimitReport, sink: &mut impl FnMut(String)) -> bool {
    match report.core_reset {
        Some(until) => wait_until(until, "rate limit resets", &mut || {}, sink, MAX_PAUSE),
        None => {
            sink(
                "rate limit low: the endpoint named no reset — proceeding on the retry ladder"
                    .to_string(),
            );
            true
        }
    }
}

/// The countdown's clock: minutes when they carry weight, seconds
/// otherwise.
pub(crate) fn human_wait(remaining: Duration) -> String {
    let seconds = remaining.as_secs();
    let minutes = seconds / 60;
    if minutes > 0 {
        format!("{minutes}m{}s", seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_countdown_speaks_minutes_when_they_carry_weight() {
        assert_eq!(human_wait(Duration::from_secs(840)), "14m0s");
        assert_eq!(human_wait(Duration::from_secs(45)), "45s");
        assert_eq!(human_wait(Duration::from_secs(3600)), "60m0s");
    }

    /// A wait already past is no wait at all.
    #[test]
    fn a_wait_in_the_past_ends_immediately() {
        let mut lines = Vec::new();
        let mut heartbeat_count = 0u32;
        let waited = wait_until(
            Utc::now() - chrono::Duration::seconds(5),
            "rate limit resets",
            &mut || heartbeat_count += 1,
            &mut |line| lines.push(line),
            MAX_PAUSE,
        );
        assert!(waited);
        assert!(lines.is_empty(), "no countdown for a past instant");
        assert_eq!(heartbeat_count, 0);
    }

    /// A wait past the cap is refused — the caller moves on.
    #[test]
    fn a_wait_past_its_cap_is_refused() {
        let mut lines = Vec::new();
        let waited = wait_until(
            Utc::now() + chrono::Duration::hours(2),
            "rate limit resets",
            &mut || {},
            &mut |line| lines.push(line),
            Duration::from_secs(60),
        );
        assert!(!waited, "two hours will not be waited out");
        assert!(
            lines.is_empty(),
            "a wait that is refused is not announced as a pause"
        );
    }

    /// Under the floor with a reset named, the probe asks for the
    /// pause; over the floor, it is silence.
    #[test]
    fn the_floor_decides_whether_the_pause_is_asked_for() {
        let mut lines = Vec::new();
        assert!(ensure_page_headroom(
            Some(RATE_LIMIT_FLOOR),
            None,
            &mut |line| lines.push(line)
        ));
        assert!(lines.is_empty(), "at the floor the burst may run");
        assert!(
            ensure_page_headroom(Some(RATE_LIMIT_FLOOR - 1), None, &mut |line| lines
                .push(line)),
            "no reset named: the run proceeds on its ladder"
        );
        assert!(
            lines.iter().any(|line| line.contains("rate limit low")),
            "the low budget is said out loud"
        );
        assert!(ensure_page_headroom(None, None, &mut |_| {}));
    }
}
