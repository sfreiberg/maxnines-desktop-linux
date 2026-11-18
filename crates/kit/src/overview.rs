//! What the app shows, worked out from what the server lists.

use std::collections::HashMap;

use crate::models::{Alerts, MonitoredHost, RemoteCheck, ServerAlert, Severity, Silence};

/// How far back resolved alerts are shown.
const RECENTLY_MS: i64 = 3_600_000;
/// More than this many resolved alerts is a list nobody reads.
const MOST_RESOLVED: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub struct Overview {
    /// Firing now, worst first and then newest first.
    pub firing: Vec<ServerAlert>,
    /// What ended in the last hour, newest first.
    pub resolved: Vec<ServerAlert>,
    pub hosts_online: usize,
    pub hosts: usize,
    /// The server's own checks, unless it has none or couldn't say.
    pub checks: Option<CheckCount>,
    /// Silences running now, ending soonest first.
    pub silences: Vec<SilenceRow>,
}

impl Overview {
    pub fn new(
        alerts: &Alerts,
        hosts: &[MonitoredHost],
        checks: Option<&[RemoteCheck]>,
        silences: &[Silence],
        now_ms: i64,
    ) -> Self {
        let mut firing = alerts.open.clone();
        firing.sort_by(|left, right| {
            right
                .severity
                .cmp(&left.severity)
                .then(right.started_ms.cmp(&left.started_ms))
        });
        let since = now_ms - RECENTLY_MS;
        let mut resolved: Vec<ServerAlert> = alerts
            .resolved
            .iter()
            .filter(|alert| alert.resolved_ms.unwrap_or(0) >= since)
            .cloned()
            .collect();
        resolved.sort_by(|left, right| {
            right
                .resolved_ms
                .unwrap_or(0)
                .cmp(&left.resolved_ms.unwrap_or(0))
        });
        resolved.truncate(MOST_RESOLVED);
        let counted: Vec<&MonitoredHost> = hosts.iter().filter(|host| host.is_counted()).collect();
        // Alerts are the only place this app learns a rule's name.
        let mut rules = HashMap::new();
        for alert in alerts.open.iter().chain(&alerts.resolved) {
            if let Some(rule) = alert.rule_id {
                rules.insert(rule, alert.rule_name.clone());
            }
        }
        let mut silences = silences.to_vec();
        silences.sort_by_key(|silence| silence.ends_ms);
        Self {
            firing,
            resolved,
            hosts: counted.len(),
            hosts_online: counted.iter().filter(|host| host.online).count(),
            checks: checks.and_then(CheckCount::of),
            silences: silences
                .iter()
                .map(|silence| SilenceRow::new(silence, &rules, hosts))
                .collect(),
        }
    }

    pub fn critical(&self) -> usize {
        self.count(Severity::Critical)
    }

    pub fn warnings(&self) -> usize {
        self.count(Severity::Warning)
    }

    fn count(&self, severity: Severity) -> usize {
        self.firing
            .iter()
            .filter(|alert| alert.severity == severity)
            .count()
    }

    /// The worst thing firing, which is what the icon says.
    pub fn worst(&self) -> Option<Severity> {
        self.firing.iter().map(|alert| alert.severity).max()
    }

    /// The whole state in a few words, such as `2 critical, 1 warning`.
    pub fn headline(&self) -> String {
        let mut parts = Vec::new();
        if self.critical() > 0 {
            parts.push(format!("{} critical", self.critical()));
        }
        match self.warnings() {
            0 => {}
            1 => parts.push("1 warning".to_string()),
            many => parts.push(format!("{many} warnings")),
        }
        if parts.is_empty() {
            "Nothing firing".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// How the server's own checks are doing. Paused ones aren't counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckCount {
    pub passing: usize,
    /// The ones that have run since the server started, which is all that can be
    /// passing or not.
    pub run: usize,
    pub enabled: usize,
}

impl CheckCount {
    /// Nothing, when there are no checks to count.
    fn of(checks: &[RemoteCheck]) -> Option<Self> {
        let enabled: Vec<&RemoteCheck> = checks.iter().filter(|check| check.enabled).collect();
        if enabled.is_empty() {
            return None;
        }
        Some(Self {
            enabled: enabled.len(),
            run: enabled
                .iter()
                .filter(|check| check.latest.is_some())
                .count(),
            passing: enabled
                .iter()
                .filter(|check| check.latest.as_ref().is_some_and(|latest| latest.up))
                .count(),
        })
    }

    pub fn failing(&self) -> usize {
        self.run - self.passing
    }
}

/// A silence as the app lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilenceRow {
    pub id: i64,
    pub reason: String,
    /// What it keeps quiet, such as `High CPU on web-1`.
    pub covers: String,
    pub ends_ms: i64,
}

impl SilenceRow {
    fn new(silence: &Silence, rules: &HashMap<i64, String>, hosts: &[MonitoredHost]) -> Self {
        let mut what = Vec::new();
        if let Some(rule) = silence.rule_id {
            what.push(
                rules
                    .get(&rule)
                    .cloned()
                    .unwrap_or_else(|| "One alert rule".to_string()),
            );
        }
        if !silence.series.is_empty() {
            what.push(silence.series.clone());
        }
        let filter = silence.filter.trim();
        let host = filter
            .strip_prefix("id=")
            .and_then(|id| hosts.iter().find(|host| host.id == id));
        let place = if filter.is_empty() {
            "every host".to_string()
        } else {
            host.map_or_else(|| filter.to_string(), |host| host.hostname.clone())
        };
        let covers = if what.is_empty() {
            if filter.is_empty() {
                "Every alert".to_string()
            } else {
                format!("Everything on {place}")
            }
        } else {
            format!("{} on {place}", what.join(" "))
        };
        Self {
            id: silence.id,
            reason: silence.reason.clone(),
            covers,
            ends_ms: silence.ends_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Latest;
    use crate::notices::tests::alert;

    const NOW: i64 = 1_793_940_300_000;

    fn host(id: &str, online: bool) -> MonitoredHost {
        MonitoredHost {
            id: id.into(),
            hostname: format!("host-{id}"),
            public_key: "k".into(),
            state: "approved".into(),
            online,
            kind: Some("agent".into()),
            health: None,
        }
    }

    #[test]
    fn the_worst_comes_first_and_old_news_goes() {
        let mut older = alert(1, Severity::Critical);
        older.started_ms -= 60_000;
        let alerts = Alerts {
            open: vec![
                older,
                alert(2, Severity::Warning),
                alert(3, Severity::Critical),
            ],
            resolved: vec![
                alert(4, Severity::Critical).resolved(NOW - 60_000, None),
                alert(5, Severity::Critical).resolved(NOW - 2 * RECENTLY_MS, None),
                alert(6, Severity::Warning).resolved(NOW - 10_000, None),
            ],
        };
        let hosts = [host("a", true), host("b", false)];
        let overview = Overview::new(&alerts, &hosts, None, &[], NOW);
        let ids = |alerts: &[ServerAlert]| alerts.iter().map(|alert| alert.id).collect::<Vec<_>>();
        assert_eq!(ids(&overview.firing), [3, 1, 2]);
        assert_eq!(ids(&overview.resolved), [6, 4]);
        assert_eq!((overview.hosts_online, overview.hosts), (1, 2));
        assert_eq!(overview.headline(), "2 critical, 1 warning");
        assert_eq!(overview.worst(), Some(Severity::Critical));
        let quiet = Overview::new(&Alerts::default(), &hosts, None, &[], NOW);
        assert_eq!(quiet.headline(), "Nothing firing");
        assert_eq!(quiet.worst(), None);
    }

    #[test]
    fn only_checks_that_arent_paused_are_counted() {
        let check = |id, enabled, latest: Option<bool>| RemoteCheck {
            id,
            enabled,
            latest: latest.map(|up| Latest { up }),
        };
        let checks = [
            check(1, true, Some(true)),
            check(2, true, Some(false)),
            check(3, true, None),
            check(4, false, Some(false)),
        ];
        let count = CheckCount::of(&checks).unwrap();
        assert_eq!((count.passing, count.run, count.enabled), (1, 2, 3));
        assert_eq!(count.failing(), 1);
        assert_eq!(CheckCount::of(&[check(4, false, None)]), None);
    }

    #[test]
    fn silences_say_what_they_keep_quiet() {
        let silence = |id, rule_id, filter: &str, series: &str| Silence {
            id,
            rule_id,
            filter: filter.into(),
            series: series.into(),
            reason: "deploy".into(),
            ends_ms: NOW + id * 1000,
        };
        let alerts = Alerts {
            open: vec![alert(1, Severity::Critical)],
            resolved: vec![],
        };
        let overview = Overview::new(
            &alerts,
            &[host("a", true)],
            None,
            &[
                silence(5, None, "", ""),
                silence(4, None, "hostname=db*", ""),
                silence(3, Some(99), "", ""),
                silence(2, Some(1), "id=a", "/var"),
            ],
            NOW,
        );
        let covers: Vec<&str> = overview
            .silences
            .iter()
            .map(|row| row.covers.as_str())
            .collect();
        assert_eq!(
            covers,
            [
                "Rule 1 /var on host-a",
                "One alert rule on every host",
                "Everything on hostname=db*",
                "Every alert",
            ]
        );
    }
}
