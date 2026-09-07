//! One retry policy for the fetches behind the usage line, so a rate-limited endpoint is asked
//! again when it said it would answer, and a failing one is asked less and less often.

use std::time::Duration;

pub const LADDER_FIRST: Duration = Duration::from_secs(2 * 60);
pub const LADDER_MAX: Duration = Duration::from_secs(10 * 60);
/// Equal to the default fetch interval, so an honored wait never polls faster than the
/// default; a configured interval above 60 seconds can still be undercut by a short
/// Retry-After.
pub const RETRY_AFTER_MIN: Duration = Duration::from_secs(60);
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(60 * 60);

/// The wait a Retry-After header asks for: delay-seconds or an RFC 7231 date, clamped to
/// [RETRY_AFTER_MIN, RETRY_AFTER_MAX]. None for an absent or unreadable header, and for a date
/// that is not in the future: a past date carries no schedule, and a skewed clock would
/// otherwise turn it into the floor.
pub fn retry_after(header: Option<&str>, now_epoch_s: i64) -> Option<Duration> {
    let raw = header?.trim();
    let seconds = match raw.parse::<u64>() {
        Ok(seconds) => seconds,
        // An HTTP date names its zone GMT, which the RFC 2822 parser accepts and the RFC 3339
        // parser does not.
        Err(_) => {
            let at = chrono::DateTime::parse_from_rfc2822(raw).ok()?.timestamp();
            if at <= now_epoch_s {
                return None;
            }
            u64::try_from(at - now_epoch_s).unwrap_or(0)
        }
    };
    Some(Duration::from_secs(seconds).clamp(RETRY_AFTER_MIN, RETRY_AFTER_MAX))
}

/// The next rung of the ladder after a failure without a Retry-After: twice the previous
/// backoff, at least LADDER_FIRST, at most LADDER_MAX.
pub fn next_backoff(previous: Option<Duration>) -> Duration {
    previous
        .map_or(LADDER_FIRST, |p| p.saturating_mul(2))
        .clamp(LADDER_FIRST, LADDER_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sun, 06 Nov 1994 08:49:37 GMT, the example date of RFC 7231.
    const NOW_S: i64 = 784_111_777;

    #[test]
    fn retry_after_reads_seconds_and_http_dates() {
        assert_eq!(
            retry_after(Some("120"), NOW_S),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            retry_after(Some(" 120 "), NOW_S),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            retry_after(Some("Sun, 06 Nov 1994 08:54:37 GMT"), NOW_S),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn retry_after_ignores_an_absent_or_unreadable_header() {
        assert!(retry_after(None, NOW_S).is_none());
        for bad in ["", "soon", "-5", "1994-11-06T08:54:37Z"] {
            assert!(
                retry_after(Some(bad), NOW_S).is_none(),
                "{bad:?} must read as unreadable"
            );
        }
    }

    #[test]
    fn retry_after_clamps_both_ends() {
        assert_eq!(retry_after(Some("0"), NOW_S), Some(RETRY_AFTER_MIN));
        assert_eq!(retry_after(Some("1"), NOW_S), Some(RETRY_AFTER_MIN));
        assert_eq!(retry_after(Some("99999"), NOW_S), Some(RETRY_AFTER_MAX));
        // A date already in the past carries no schedule, so the ladder applies instead of
        // an instant retry at the floor.
        assert!(retry_after(Some("Sun, 06 Nov 1994 08:00:00 GMT"), NOW_S).is_none());
    }

    #[test]
    fn next_backoff_doubles_from_the_first_rung_to_the_cap() {
        assert_eq!(next_backoff(None), LADDER_FIRST);
        assert_eq!(next_backoff(Some(LADDER_FIRST)), Duration::from_secs(240));
        assert_eq!(
            next_backoff(Some(Duration::from_secs(240))),
            Duration::from_secs(480)
        );
        assert_eq!(next_backoff(Some(Duration::from_secs(480))), LADDER_MAX);
        assert_eq!(next_backoff(Some(LADDER_MAX)), LADDER_MAX);
        // A rung below the first (a hand-edited cache) restarts the ladder.
        assert_eq!(next_backoff(Some(Duration::from_secs(1))), LADDER_FIRST);
    }
}
