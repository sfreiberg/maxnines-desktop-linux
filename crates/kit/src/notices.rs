//! What's worth a notification, between what the server listed last time and now.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::models::{Alerts, RESOLVED_LISTED, RESOLVED_MARK, ServerAlert, Severity};
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

    /// Whether alerts of `severity` are wanted.
    fn wants(&self, severity: Severity) -> bool {
        match severity {
            Severity::Critical => self.critical,
            Severity::Warning => self.warnings,
        }
    }
}

/// One notification to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Unique to the change, as it names when it was noticed, so one never replaces
    /// an earlier notification, even about the same alert, from another run of the
    /// app or about another server.
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

/// Works out what's worth a notification each time the server lists its alerts, and
/// remembers who's been told what.
#[derive(Debug, Clone, Default)]
pub struct Notifier {
    /// The alerts firing when the server last listed them. Nothing until the first
    /// answer, so what was already firing when the app started isn't news.
    last: Option<Vec<ServerAlert>>,
    /// The firing alerts the person has been told have started. Told about one, they
    /// still hear when it ends, even if it's been silenced since, as the server's own
    /// notifications do.
    told: HashSet<i64>,
}

impl Notifier {
    /// What's worth a notification in `new`, since the last listing.
    pub fn notices(&mut self, new: &Alerts, when: NotifyWhen, now: DateTime<Utc>) -> Vec<Notice> {
        let notices = match &self.last {
            Some(old) => worked_out(old, new, when, now, &mut self.told),
            None => Vec::new(),
        };
        self.last = Some(new.open.clone());
        notices
    }
}

/// What's worth a notification between `old`, what was firing, and `new`. Silenced
/// alerts stay quiet, as they do everywhere else in MaxNines, except for the end of one
/// the person was already told about. `told` is kept up to date.
fn worked_out(
    old: &[ServerAlert],
    new: &Alerts,
    when: NotifyWhen,
    now: DateTime<Utc>,
    told: &mut HashSet<i64>,
) -> Vec<Notice> {
    let before: HashMap<i64, _> = old.iter().rev().map(|alert| (alert.id, alert)).collect();
    let still: HashSet<i64> = new.open.iter().map(|alert| alert.id).collect();
    let noticed = now.timestamp_millis();

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
        if !when.wants(alert.severity) {
            continue;
        }
        worst = worst.max(alert.severity);
        told.insert(alert.id);
        let level = if was.is_none() || unsilenced {
            match alert.severity {
                Severity::Critical => "Critical",
                Severity::Warning => "Warning",
            }
        } else {
            "Now critical"
        };
        started.push(Notice {
            id: format!("alert-{}-{}-{noticed}", alert.id, alert.severity.as_str()),
            title: format!("{} {level}: {}", alert.severity.mark(), alert.rule_name),
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
        // The server lists only the latest ended alerts, newest started first, so one
        // that started before all of a full list may have ended without being listed.
        let listed_since = (new.resolved.len() >= RESOLVED_LISTED)
            .then(|| new.resolved.iter().map(|alert| alert.started_at).min())
            .flatten();
        // Told it started, they hear it end, even if it's been silenced since.
        // Otherwise it's only news if it isn't silenced and its severity is wanted,
        // so nobody hears the end of something they chose not to hear start.
        for alert in old.iter().filter(|alert| {
            !still.contains(&alert.id)
                && (told.contains(&alert.id)
                    || (!alert.is_silenced() && when.wants(alert.severity)))
        }) {
            // From when the problem began, as the server's pages say it.
            let (lasted, outcome) = match resolved.get(&alert.id) {
                Some(done) => (
                    age(done.breached_at, done.resolved_at.unwrap_or(now)),
                    // Where things stand now, so it doesn't read as the problem again.
                    // One closed for another reason has only what was wrong, said as
                    // past.
                    done.resolved_summary
                        .clone()
                        .unwrap_or_else(|| format!("Was: {}", done.summary)),
                ),
                // Ended, but too long after it started to be listed: it ended since the
                // last listing, so about now.
                None if listed_since.is_some_and(|since| alert.started_at <= since) => (
                    age(alert.breached_at, now),
                    format!("Was: {}", alert.summary),
                ),
                // Gone, as when its host was deleted, which the server doesn't call
                // resolved, so it isn't news worth telling.
                None => continue,
            };
            ended.push(Notice {
                id: format!("alert-{}-resolved-{noticed}", alert.id),
                title: format!("{RESOLVED_MARK} Resolved: {}", alert.rule_name),
                body: format!("{outcome}\nLasted {lasted}"),
                acknowledges: None,
                path: alert.page(),
            });
        }
    }
    // What's ended is no longer anyone's news.
    told.retain(|id| still.contains(id));

    let started_title = format!("{} {} alerts started", worst.mark(), started.len());
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
    use chrono::TimeDelta;

    use super::*;
    use crate::models::ServerAlert;

    /// Just after the alerts these tests make.
    pub(crate) fn now() -> DateTime<Utc> {
        "2026-11-06T04:45:00Z".parse().unwrap()
    }

    pub(crate) fn alert(id: i64, severity: Severity) -> ServerAlert {
        ServerAlert {
            id,
            rule_id: Some(1),
            rule_name: format!("Rule {id}"),
            severity,
            host_id: format!("h{id}"),
            hostname: format!("host-{id}"),
            series: None,
            summary: format!("Alert {id}"),
            started_at: now() - TimeDelta::minutes(5),
            // Its rule waits two minutes before it fires.
            breached_at: now() - TimeDelta::minutes(7),
            resolved_at: None,
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

        pub(crate) fn resolved(mut self, at: DateTime<Utc>, summary: Option<&str>) -> Self {
            self.resolved_at = Some(at);
            self.resolved_summary = summary.map(Into::into);
            self
        }
    }

    const EVERYTHING: NotifyWhen = NotifyWhen {
        critical: true,
        warnings: true,
        resolved: true,
    };

    /// What's worth a notification from `old` to `new`, told about nothing before.
    fn notices(old: &Alerts, new: &Alerts, when: NotifyWhen, now: DateTime<Utc>) -> Vec<Notice> {
        worked_out(&old.open, new, when, now, &mut HashSet::new())
    }

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
        let all = notices(&old, &new, EVERYTHING, now());
        // Not the one already firing, nor the silenced one.
        assert_eq!(titles(&all), ["🔴 Critical: Rule 2", "🟡 Warning: Rule 3"]);
        assert_eq!(all[0].body, "Alert 2");
        assert_eq!(all[0].acknowledges, Some(2));
        assert_eq!(all[0].path, "/alerts/2");

        let critical_only = NotifyWhen {
            critical: true,
            warnings: false,
            resolved: false,
        };
        assert_eq!(
            titles(&notices(&old, &new, critical_only, now())),
            ["🔴 Critical: Rule 2"]
        );
        let nothing = NotifyWhen {
            critical: false,
            warnings: false,
            resolved: false,
        };
        assert!(notices(&old, &new, nothing, now()).is_empty());
    }

    #[test]
    fn an_alert_that_gets_worse_is_notified() {
        let old = open(vec![alert(1, Severity::Warning)]);
        let new = open(vec![alert(1, Severity::Critical).acknowledged()]);
        let all = notices(&old, &new, EVERYTHING, now());
        assert_eq!(titles(&all), ["🔴 Now critical: Rule 1"]);
        // Someone's already on it.
        assert_eq!(all[0].acknowledges, None);
        // Getting better isn't news.
        assert!(notices(&new, &old, EVERYTHING, now()).is_empty());
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
            titles(&notices(&old, &new, EVERYTHING, now())),
            ["🔴 Critical: Rule 1", "🔴 Critical: Rule 2"]
        );
        // Still silenced, it stays quiet.
        assert!(notices(&old, &old, EVERYTHING, now()).is_empty());
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
                    .resolved(now() - TimeDelta::minutes(2), Some("host-1 is back online")),
                alert(3, Severity::Critical)
                    .resolved(now() - TimeDelta::minutes(2), Some("host-3 is back online")),
                alert(4, Severity::Warning).resolved(now() - TimeDelta::minutes(4), None),
            ],
        };
        let all = notices(&old, &new, EVERYTHING, now());
        // The second just went, which the server doesn't call resolved, and the third
        // was silenced.
        assert_eq!(titles(&all), ["🟢 Resolved: Rule 1", "🟢 Resolved: Rule 4"]);
        // Not what was wrong, which would read as the problem all over again. It
        // lasted from when the problem began, before the alert fired.
        assert_eq!(all[0].body, "host-1 is back online\nLasted 5m");
        // Closed for another reason, it has only what was wrong, said as past.
        assert_eq!(all[1].body, "Was: Alert 4\nLasted 3m");
        assert_eq!(all[0].acknowledges, None);
        let no_resolved = NotifyWhen {
            resolved: false,
            ..EVERYTHING
        };
        assert!(notices(&old, &new, no_resolved, now()).is_empty());
    }

    #[test]
    fn someone_told_an_alert_started_hears_it_end_though_its_been_silenced_since() {
        let mut notifier = Notifier::default();
        let first = open(vec![alert(2, Severity::Critical).silenced()]);
        // What was firing when the app started isn't news.
        assert!(notifier.notices(&first, EVERYTHING, now()).is_empty());
        let firing = open(vec![
            alert(1, Severity::Critical),
            alert(2, Severity::Critical).silenced(),
        ]);
        assert_eq!(
            titles(&notifier.notices(&firing, EVERYTHING, now())),
            ["🔴 Critical: Rule 1"]
        );
        // Silenced from the app after the notification.
        let silenced = open(vec![
            alert(1, Severity::Critical).silenced(),
            alert(2, Severity::Critical).silenced(),
        ]);
        assert!(notifier.notices(&silenced, EVERYTHING, now()).is_empty());
        let over = Alerts {
            open: vec![],
            resolved: vec![
                alert(1, Severity::Critical).resolved(now(), None),
                alert(2, Severity::Critical).resolved(now(), None),
            ],
        };
        // The second was silenced all along, so nobody was told it started.
        assert_eq!(
            titles(&notifier.notices(&over, EVERYTHING, now())),
            ["🟢 Resolved: Rule 1"]
        );
        // Once it's ended, it's forgotten.
        assert!(notifier.told.is_empty());
    }

    #[test]
    fn each_change_has_a_notification_of_its_own() {
        let mut notifier = Notifier::default();
        let mut ids = Vec::new();
        let mut at = now();
        let mut listed = |alerts: Vec<ServerAlert>, notifier: &mut Notifier| {
            at += TimeDelta::seconds(15);
            let notices = notifier.notices(&open(alerts), EVERYTHING, at);
            ids.extend(notices.into_iter().map(|notice| notice.id));
        };
        listed(vec![], &mut notifier);
        // Critical, silenced, its silence over, a warning, critical again.
        listed(vec![alert(7, Severity::Critical)], &mut notifier);
        listed(vec![alert(7, Severity::Critical).silenced()], &mut notifier);
        listed(vec![alert(7, Severity::Critical)], &mut notifier);
        listed(vec![alert(7, Severity::Warning)], &mut notifier);
        listed(vec![alert(7, Severity::Critical)], &mut notifier);
        assert_eq!(ids.len(), 3, "{ids:?}");
        // None replaces another, which a desktop may do without showing it again.
        let unique: HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), 3, "{ids:?}");
        assert!(ids[0].starts_with("alert-7-critical-"), "{ids:?}");
    }

    #[test]
    fn the_end_of_an_alert_isnt_news_if_its_start_wasnt_wanted() {
        let old = open(vec![
            alert(1, Severity::Critical),
            alert(2, Severity::Warning),
        ]);
        let new = Alerts {
            open: vec![],
            resolved: vec![
                alert(1, Severity::Critical).resolved(now(), None),
                alert(2, Severity::Warning).resolved(now(), None),
            ],
        };
        let warnings_and_resolved = NotifyWhen {
            critical: false,
            ..EVERYTHING
        };
        assert_eq!(
            titles(&notices(&old, &new, warnings_and_resolved, now())),
            ["🟢 Resolved: Rule 2"]
        );
        // Told it started, before critical ones were turned off, they hear it end.
        let mut told = HashSet::from([1]);
        let all = worked_out(&old.open, &new, warnings_and_resolved, now(), &mut told);
        assert_eq!(titles(&all), ["🟢 Resolved: Rule 1", "🟢 Resolved: Rule 2"]);
    }

    #[test]
    fn an_alert_that_ends_too_long_after_it_started_to_be_listed_is_notified() {
        // The server lists only the latest ended alerts to have started, so a long
        // one can end without being listed.
        let full: Vec<ServerAlert> = (1000..)
            .take(RESOLVED_LISTED)
            .map(|id| alert(id, Severity::Warning).resolved(now(), None))
            .collect();
        let mut long = alert(1, Severity::Critical);
        long.started_at -= TimeDelta::days(3);
        long.breached_at -= TimeDelta::days(3);
        // Started after the oldest listed, it would be listed had it ended, so it's
        // gone, as when its host was deleted.
        let mut gone = alert(2, Severity::Critical);
        gone.started_at += TimeDelta::minutes(1);
        let old = open(vec![long, gone]);
        let new = Alerts {
            open: vec![],
            resolved: full.clone(),
        };
        let all = notices(&old, &new, EVERYTHING, now());
        assert_eq!(titles(&all), ["🟢 Resolved: Rule 1"]);
        assert_eq!(all[0].body, "Was: Alert 1\nLasted 3d");
        // A list that isn't full has every alert that's ended.
        let short = Alerts {
            open: vec![],
            resolved: full[1..].to_vec(),
        };
        assert!(notices(&old, &short, EVERYTHING, now()).is_empty());
    }

    #[test]
    fn a_storm_becomes_one_notification() {
        let old = Alerts::default();
        let new = open((1..=5).map(|id| alert(id, Severity::Critical)).collect());
        let all = notices(&old, &new, EVERYTHING, now());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "🔴 5 alerts started");
        assert_eq!(all[0].body, "Alert 1\nAlert 2\nAlert 3\nand 2 more");
        assert_eq!(all[0].path, "/alerts");
        assert_eq!(all[0].acknowledges, None);
        // Only warnings say so.
        let warnings = open((1..=5).map(|id| alert(id, Severity::Warning)).collect());
        assert_eq!(
            notices(&old, &warnings, EVERYTHING, now())[0].title,
            "🟡 5 alerts started"
        );
        let over = Alerts {
            open: vec![],
            resolved: (1..=5)
                .map(|id| {
                    alert(id, Severity::Critical).resolved(now() - TimeDelta::minutes(2), None)
                })
                .collect(),
        };
        assert_eq!(
            titles(&notices(&new, &over, EVERYTHING, now())),
            ["🟢 5 alerts resolved"]
        );
        // Three at once are each shown.
        let three = open((1..=3).map(|id| alert(id, Severity::Critical)).collect());
        assert_eq!(notices(&old, &three, EVERYTHING, now()).len(), 3);
    }
}
