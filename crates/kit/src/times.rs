//! How long, said briefly, as the other MaxNines Desktop apps say it.

use chrono::{DateTime, Utc};

/// How long ago something was, in the largest whole unit, such as `4m` or `2h`.
pub fn age(since: DateTime<Utc>, now: DateTime<Utc>) -> String {
    span((now - since).num_seconds())
}

/// How long until something happens, in the same units as [`age`]. Part of a second
/// counts as a whole one, so what hasn't happened yet never says `0s`.
pub fn time_left(until: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let left = until - now;
    span(left.num_seconds() + i64::from(left.subsec_nanos() > 0))
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
    use chrono::TimeDelta;

    use super::*;

    #[test]
    fn spans_are_said_in_their_largest_whole_unit() {
        let now: DateTime<Utc> = "2026-11-06T04:40:00Z".parse().unwrap();
        let ago = |secs: i64| age(now - TimeDelta::seconds(secs), now);
        assert_eq!(ago(5), "5s");
        assert_eq!(ago(90), "1m");
        assert_eq!(ago(2 * 3600 + 5), "2h");
        assert_eq!(ago(3 * 86_400), "3d");
        assert_eq!(ago(-10), "0s");
        let left = |secs: i64| time_left(now + TimeDelta::seconds(secs), now);
        assert_eq!(left(3 * 3600), "3h");
        assert_eq!(left(-10), "0s");
        assert_eq!(time_left(now + TimeDelta::milliseconds(1), now), "1s");
    }
}
