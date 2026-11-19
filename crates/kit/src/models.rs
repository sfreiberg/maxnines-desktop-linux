//! What the server's API lists. Fields the app doesn't use are left out, and the
//! server may add more.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};

/// How bad an alert is, mildest first, so the worse of two is the greater.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Warning,
    Critical,
}

impl<'de> Deserialize<'de> for Severity {
    /// A severity added to the server later is at least a warning.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(if text == "critical" {
            Self::Critical
        } else {
            Self::Warning
        })
    }
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// An alert as the server lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ServerAlert {
    pub id: i64,
    pub rule_id: Option<i64>,
    pub rule_name: String,
    pub severity: Severity,
    pub host_id: String,
    pub hostname: String,
    /// Which of its rule's series it's about, such as a check's name. None for alerts
    /// about the host as a whole.
    #[serde(default)]
    pub series: Option<String>,
    /// What was wrong, as it said when it fired.
    pub summary: String,
    pub started_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// Where things stood as it cleared, such as "web-1 is back online". Missing for
    /// one closed for another reason, such as its rule being deleted, and from older
    /// servers.
    #[serde(default)]
    pub resolved_summary: Option<String>,
    pub acknowledged_by: Option<String>,
    #[serde(default)]
    pub silence_id: Option<i64>,
    #[serde(default)]
    pub maintenance_window_id: Option<i64>,
}

impl ServerAlert {
    pub fn is_acknowledged(&self) -> bool {
        self.acknowledged_by.is_some()
    }

    /// A silence for one alert names its rule, so one whose rule has been deleted has
    /// nothing to name.
    pub fn can_be_silenced(&self) -> bool {
        self.rule_id.is_some()
    }

    /// Whether a silence or a maintenance window is keeping its notifications back.
    pub fn is_silenced(&self) -> bool {
        self.silence_id.is_some() || self.maintenance_window_id.is_some()
    }

    /// What it says now: where things stood as it cleared once it has, and what's
    /// wrong until then, as the server's own notifications say.
    pub fn says_now(&self) -> &str {
        self.resolved_summary.as_deref().unwrap_or(&self.summary)
    }

    /// The page it opens: its host's.
    pub fn page(&self) -> String {
        format!("/hosts/{}", self.host_id)
    }
}

/// What `GET /api/v1/alerts` returns.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Alerts {
    #[serde(default)]
    pub open: Vec<ServerAlert>,
    #[serde(default)]
    pub resolved: Vec<ServerAlert>,
}

/// A silence: while it runs, the alerts it covers fire and show, but nobody is told
/// about them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Silence {
    pub id: i64,
    /// The alert rule it covers, or every rule.
    pub rule_id: Option<i64>,
    /// Which hosts it covers, such as `id=7f3c…` or `hostname=db*`. None for all.
    #[serde(default)]
    pub filter: Option<String>,
    /// One series alone, such as a check's name. None for all.
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub reason: String,
    pub ends_at: DateTime<Utc>,
}

/// What `GET /api/v1/silences` returns. Ones that have ended aren't used.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Silences {
    #[serde(default)]
    pub active: Vec<Silence>,
}

/// A check the server runs itself, as `GET /api/v1/remote-checks` lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RemoteCheck {
    pub id: i64,
    pub enabled: bool,
    /// How it last went. Missing until it's run since the server started, and from
    /// servers too old to say.
    #[serde(default)]
    pub latest: Option<Latest>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Latest {
    pub up: bool,
}

/// A host the server watches.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MonitoredHost {
    pub id: String,
    pub hostname: String,
    pub state: String,
    pub online: bool,
    /// `agent`, `agentless` or `built_in`.
    pub kind: String,
    /// How it is, such as `online`, `offline` or `not_read`.
    #[serde(default)]
    pub health: Option<String>,
}

impl MonitoredHost {
    /// Whether it's a host that could be online: approved, not one of the server's own
    /// built-in hosts, and not one with no agent that nothing is read for, which is
    /// neither online nor offline.
    pub fn is_counted(&self) -> bool {
        self.state == "approved"
            && self.health.as_deref() != Some("not_read")
            && self.kind != "built_in"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_servers_alerts_are_read() {
        let alerts: Alerts = serde_json::from_str(
            r#"{"open": [
                 {"id": 7, "rule_id": 1, "rule_name": "High CPU", "severity": "critical",
                  "host_id": "abc", "hostname": "web-1", "series": null, "value": 97.2,
                  "summary": "High CPU on web-1", "started_at": "2026-11-06T04:44:39.545Z",
                  "resolved_at": null, "resolve_reason": null, "acknowledged_by": null,
                  "silence_id": null, "maintenance_window_id": null},
                 {"id": 8, "rule_id": null, "rule_name": "Something new", "severity": "emergency",
                  "host_id": "abc", "hostname": "web-1", "summary": "Something new",
                  "started_at": "2026-11-06T04:41:40Z", "resolved_at": null, "acknowledged_by": "sam@example.com"}],
               "resolved": [
                 {"id": 5, "rule_id": 2, "rule_name": "Host offline", "severity": "critical",
                  "host_id": "def", "hostname": "db-1", "series": null,
                  "summary": "db-1 is offline", "started_at": "2026-11-06T03:33:20Z",
                  "resolved_at": "2026-11-06T04:23:20Z", "resolved_summary": "db-1 is back online",
                  "acknowledged_by": null, "silence_id": null, "maintenance_window_id": null}]}"#,
        )
        .unwrap();
        assert_eq!(alerts.open[0].severity, Severity::Critical);
        // An alert about the host as a whole has no series.
        assert_eq!(alerts.open[0].series, None);
        // A severity the app doesn't know is at least a warning, and fields older
        // servers leave out are empty.
        assert_eq!(alerts.open[1].severity, Severity::Warning);
        assert!(alerts.open[1].is_acknowledged());
        assert!(!alerts.open[1].can_be_silenced());
        assert_eq!(alerts.open[0].says_now(), "High CPU on web-1");
        assert_eq!(alerts.resolved[0].says_now(), "db-1 is back online");
        assert_eq!(alerts.resolved[0].page(), "/hosts/def");
    }

    #[test]
    fn times_are_read_with_milliseconds_and_without() {
        let ends = |ends_at: serde_json::Value| {
            serde_json::from_value::<Silence>(serde_json::json!({
                "id": 1, "rule_id": null, "ends_at": ends_at
            }))
            .map(|silence| silence.ends_at)
        };
        // To the millisecond, which is as precise as the server keeps them.
        assert_eq!(
            ends("2026-11-19T01:57:17.828Z".into()).unwrap(),
            DateTime::from_timestamp_millis(1_795_053_437_828).unwrap()
        );
        // The server leaves the fraction out when the milliseconds are zero.
        assert_eq!(
            ends("2026-11-19T02:30:00Z".into()).unwrap(),
            DateTime::from_timestamp(1_795_055_400, 0).unwrap()
        );
        // Milliseconds since 1970, as older servers sent, aren't a time.
        assert!(ends(1_795_053_437_828_i64.into()).is_err());
    }

    #[test]
    fn only_hosts_that_could_be_online_are_counted() {
        let host = |state: &str, kind: &str, health: Option<&str>| MonitoredHost {
            id: "a".into(),
            hostname: "a".into(),
            state: state.into(),
            online: true,
            kind: kind.into(),
            health: health.map(Into::into),
        };
        assert!(host("approved", "agent", Some("online")).is_counted());
        assert!(!host("pending", "agent", None).is_counted());
        assert!(!host("approved", "built_in", None).is_counted());
        assert!(!host("approved", "agentless", Some("not_read")).is_counted());
        assert!(host("approved", "agentless", Some("online")).is_counted());
    }
}
