//! What's worth a notification, between what the server listed last time and now.

use std::collections::{HashMap, HashSet};

use crate::models::{Alerts, Severity};
use crate::times::age;

/// Which changes someone wants a notification about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifyWhen {
    pub critical: bool,
    pub warnings: bool,
    pub resolved: bool,
}

impl NotifyWhen {
    pub fn any(&self) -> bool {
        self.critical || self.warnings || self.resolved
    }
}

/// One notification to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Unique to the change, so the same one is never shown twice.
    pub id: String,
    pub title: String,
    pub body: String,
    /// The alert it's about, which can be acknowledged from it, if it's firing and
    /// nobody has yet.
    pub acknowledges: Option<i64>,
    /// The page it opens.
    pub path: String,
}

/// More than this many at once become one notification, so a storm doesn't bury the
/// screen.
const MOST: usize = 3;

/// The coloured circles the server's Discord messages use, so how bad it is, or that
/// it's over, shows before a word is read.
fn mark(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "🔴",
        Severity::Warning => "🟡",
    }
}

const RESOLVED_MARK: &str = "🟢";

/// What's worth a notification between `old` and `new`. The first time there's no
/// old, and nothing is: what was already firing when the app started isn't news.
/// Silenced alerts stay quiet, as they do everywhere else in MaxNines.
pub fn notices(old: &Alerts, new: &Alerts, when: NotifyWhen, now_ms: i64) -> Vec<Notice> {
    let before: HashMap<i64, _> = old
        .open
        .iter()
        .rev()
        .map(|alert| (alert.id, alert))
        .collect();
    let still: HashSet<i64> = new.open.iter().map(|alert| alert.id).collect();

    let mut started = Vec::new();
    let mut worst = Severity::Warning;
    for alert in new.open.iter().filter(|alert| !alert.is_silenced()) {
        let was = before.get(&alert.id);
        // A new alert, one whose silence ended while it was still firing, as the
        // server tells people about then, or one that's got worse.
        let unsilenced = was.is_some_and(|was| was.is_silenced());
        let worse = was.is_some_and(|was| was.severity == Severity::Warning)
            && alert.severity == Severity::Critical;
        if was.is_some() && !unsilenced && !worse {
            continue;
        }
        let wanted = match alert.severity {
            Severity::Critical => when.critical,
            Severity::Warning => when.warnings,
        };
        if !wanted {
            continue;
        }
        worst = worst.max(alert.severity);
        let level = if was.is_none() || unsilenced {
            match alert.severity {
                Severity::Critical => "Critical",
                Severity::Warning => "Warning",
            }
        } else {
            "Now critical"
        };
        started.push(Notice {
            id: format!("alert-{}-{}", alert.id, alert.severity.as_str()),
            title: format!("{} {level}: {}", mark(alert.severity), alert.rule_name),
            body: alert.summary.clone(),
            acknowledges: (!alert.is_acknowledged()).then_some(alert.id),
            path: alert.page(),
        });
    }

    let mut ended = Vec::new();
    if when.resolved {
        let resolved: HashMap<i64, _> = new
            .resolved
            .iter()
            .rev()
            .map(|alert| (alert.id, alert))
            .collect();
        for alert in old
            .open
            .iter()
            .filter(|alert| !still.contains(&alert.id) && !alert.is_silenced())
        {
            // Only what the server says has resolved: one that's just gone, as when
            // its host was deleted, isn't news worth telling.
            let Some(done) = resolved.get(&alert.id) else {
                continue;
            };
            let lasted = age(done.started_ms, done.resolved_ms.unwrap_or(now_ms));
            // Where things stand now, so it doesn't read as the problem again. One
            // closed for another reason has only what was wrong, said as past.
            let outcome = done
                .resolved_summary
                .clone()
                .unwrap_or_else(|| format!("Was: {}", done.summary));
            ended.push(Notice {
                id: format!("alert-{}-resolved", alert.id),
                title: format!("{RESOLVED_MARK} Resolved: {}", alert.rule_name),
                body: format!("{outcome}\nLasted {lasted}"),
                acknowledges: None,
                path: alert.page(),
            });
        }
    }

    let started_title = format!("{} {} alerts started", mark(worst), started.len());
    let ended_title = format!("{RESOLVED_MARK} {} alerts resolved", ended.len());
    let mut all = gathered(started, started_title);
    all.extend(gathered(ended, ended_title));
    all
}

/// Many at once become one, which opens the alerts page.
fn gathered(notices: Vec<Notice>, title: String) -> Vec<Notice> {
    if notices.len() <= MOST {
        return notices;
    }
    let mut lines: Vec<String> = notices
        .iter()
        .take(MOST)
        .map(|notice| notice.body.lines().next().unwrap_or("").to_string())
        .collect();
    lines.push(format!("and {} more", notices.len() - MOST));
    vec![Notice {
        id: notices
            .iter()
            .map(|notice| notice.id.as_str())
            .collect::<Vec<_>>()
            .join(","),
        title,
        body: lines.join("\n"),
        acknowledges: None,
        path: "/alerts".to_string(),
    }]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::models::ServerAlert;

    const NOW: i64 = 1_793_940_300_000;

    pub(crate) fn alert(id: i64, severity: Severity) -> ServerAlert {
        ServerAlert {
            id,
            rule_id: Some(1),
            rule_name: format!("Rule {id}"),
            severity,
            host_id: format!("h{id}"),
            hostname: format!("host-{id}"),
            series: String::new(),
            summary: format!("Alert {id}"),
            started_ms: 1_793_940_000_000,
            resolved_ms: None,
            resolved_summary: None,
            acknowledged_by: None,
            silence_id: None,
            maintenance_window_id: None,
        }
    }

    impl ServerAlert {
        pub(crate) fn silenced(mut self) -> Self {
            self.silence_id = Some(1);
            self
        }

        pub(crate) fn acknowledged(mut self) -> Self {
            self.acknowledged_by = Some("sam@example.com".into());
            self
        }

        pub(crate) fn resolved(mut self, at_ms: i64, summary: Option<&str>) -> Self {
            self.resolved_ms = Some(at_ms);
            self.resolved_summary = summary.map(Into::into);
            self
        }
    }

    const EVERYTHING: NotifyWhen = NotifyWhen {
        critical: true,
        warnings: true,
        resolved: true,
    };

    fn titles(notices: &[Notice]) -> Vec<&str> {
        notices.iter().map(|notice| notice.title.as_str()).collect()
    }

    fn open(alerts: Vec<ServerAlert>) -> Alerts {
        Alerts {
            open: alerts,
            resolved: vec![],
        }
    }

    #[test]
    fn new_alerts_are_notified_as_wanted() {
        let old = open(vec![alert(1, Severity::Critical)]);
        let new = open(vec![
            alert(1, Severity::Critical),
            alert(2, Severity::Critical),
            alert(3, Severity::Warning),
            alert(4, Severity::Critical).silenced(),
        ]);
        let all = notices(&old, &new, EVERYTHING, NOW);
        // Not the one already firing, nor the silenced one.
        assert_eq!(titles(&all), ["🔴 Critical: Rule 2", "🟡 Warning: Rule 3"]);
        assert_eq!(all[0].body, "Alert 2");
        assert_eq!(all[0].acknowledges, Some(2));
        assert_eq!(all[0].path, "/hosts/h2");

        let critical_only = NotifyWhen {
            critical: true,
            warnings: false,
            resolved: false,
        };
        assert_eq!(
            titles(&notices(&old, &new, critical_only, NOW)),
            ["🔴 Critical: Rule 2"]
        );
        let nothing = NotifyWhen {
            critical: false,
            warnings: false,
            resolved: false,
        };
        assert!(notices(&old, &new, nothing, NOW).is_empty());
    }

    #[test]
    fn an_alert_that_gets_worse_is_notified() {
        let old = open(vec![alert(1, Severity::Warning)]);
        let new = open(vec![alert(1, Severity::Critical).acknowledged()]);
        let all = notices(&old, &new, EVERYTHING, NOW);
        assert_eq!(titles(&all), ["🔴 Now critical: Rule 1"]);
        // Someone's already on it.
        assert_eq!(all[0].acknowledges, None);
        // Getting better isn't news.
        assert!(notices(&new, &old, EVERYTHING, NOW).is_empty());
    }

    #[test]
    fn an_alert_still_firing_when_its_silence_ends_is_notified() {
        let old = open(vec![
            alert(1, Severity::Critical).silenced(),
            alert(2, Severity::Warning).silenced(),
        ]);
        let new = open(vec![
            alert(1, Severity::Critical),
            alert(2, Severity::Critical),
        ]);
        // As the server tells people then, and as what it is now, not how it changed.
        assert_eq!(
            titles(&notices(&old, &new, EVERYTHING, NOW)),
            ["🔴 Critical: Rule 1", "🔴 Critical: Rule 2"]
        );
        // Still silenced, it stays quiet.
        assert!(notices(&old, &old, EVERYTHING, NOW).is_empty());
    }

    #[test]
    fn resolved_alerts_say_where_things_stand_and_how_long_they_lasted() {
        let old = open(vec![
            alert(1, Severity::Critical),
            alert(2, Severity::Warning),
            alert(3, Severity::Critical).silenced(),
            alert(4, Severity::Warning),
        ]);
        let new = Alerts {
            open: vec![],
            resolved: vec![
                alert(1, Severity::Critical)
                    .resolved(1_793_940_180_000, Some("host-1 is back online")),
                alert(3, Severity::Critical)
                    .resolved(1_793_940_180_000, Some("host-3 is back online")),
                alert(4, Severity::Warning).resolved(1_793_940_060_000, None),
            ],
        };
        let all = notices(&old, &new, EVERYTHING, NOW);
        // The second just went, which the server doesn't call resolved, and the third
        // was silenced.
        assert_eq!(titles(&all), ["🟢 Resolved: Rule 1", "🟢 Resolved: Rule 4"]);
        // Not what was wrong, which would read as the problem all over again.
        assert_eq!(all[0].body, "host-1 is back online\nLasted 3m");
        // Closed for another reason, it has only what was wrong, said as past.
        assert_eq!(all[1].body, "Was: Alert 4\nLasted 1m");
        assert_eq!(all[0].acknowledges, None);
        let no_resolved = NotifyWhen {
            resolved: false,
            ..EVERYTHING
        };
        assert!(notices(&old, &new, no_resolved, NOW).is_empty());
    }

    #[test]
    fn a_storm_becomes_one_notification() {
        let old = Alerts::default();
        let new = open((1..=5).map(|id| alert(id, Severity::Critical)).collect());
        let all = notices(&old, &new, EVERYTHING, NOW);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "🔴 5 alerts started");
        assert_eq!(all[0].body, "Alert 1\nAlert 2\nAlert 3\nand 2 more");
        assert_eq!(all[0].path, "/alerts");
        assert_eq!(all[0].acknowledges, None);
        // Only warnings say so.
        let warnings = open((1..=5).map(|id| alert(id, Severity::Warning)).collect());
        assert_eq!(
            notices(&old, &warnings, EVERYTHING, NOW)[0].title,
            "🟡 5 alerts started"
        );
        let over = Alerts {
            open: vec![],
            resolved: (1..=5)
                .map(|id| alert(id, Severity::Critical).resolved(1_793_940_180_000, None))
                .collect(),
        };
        assert_eq!(
            titles(&notices(&new, &over, EVERYTHING, NOW)),
            ["🟢 5 alerts resolved"]
        );
        // Three at once are each shown.
        let three = open((1..=3).map(|id| alert(id, Severity::Critical)).collect());
        assert_eq!(notices(&old, &three, EVERYTHING, NOW).len(), 3);
    }
}
