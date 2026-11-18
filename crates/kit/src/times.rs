//! How long, said briefly, as the other MaxNines Desktop apps say it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Now, in milliseconds since 1970, as the server counts.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// How long ago something was, in the largest whole unit, such as `4m` or `2h`.
pub fn age(since_ms: i64, now_ms: i64) -> String {
    span(now_ms.saturating_sub(since_ms) / 1000)
}

/// How long until something happens, in the same units as [`age`].
pub fn time_left(until_ms: i64, now_ms: i64) -> String {
    span((until_ms.saturating_sub(now_ms) + 999) / 1000)
}

fn span(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_said_in_their_largest_whole_unit() {
        let now = 1_793_940_000_000;
        let ago = |secs: i64| age(now - secs * 1000, now);
        assert_eq!(ago(5), "5s");
        assert_eq!(ago(90), "1m");
        assert_eq!(ago(2 * 3600 + 5), "2h");
        assert_eq!(ago(3 * 86_400), "3d");
        assert_eq!(ago(-10), "0s");
        let left = |secs: i64| time_left(now + secs * 1000, now);
        assert_eq!(left(3 * 3600), "3h");
        assert_eq!(left(-10), "0s");
        assert_eq!(time_left(now + 1, now), "1s");
    }
}
