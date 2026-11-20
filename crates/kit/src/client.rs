//! Talking to one MaxNines server's API as whoever signed in.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::Error;
use crate::http::{Answer, Body, Http, Request};
use crate::models::{Alerts, MonitoredHost, RemoteCheck, ServerAlert, Silence, Silences};

/// How long an API request waits for its answer.
const API_TIMEOUT: Duration = Duration::from_secs(15);

/// Where a MaxNines server is: the root of the site, whatever page of it was pasted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAddress {
    /// Always ends in `/`.
    root: String,
    in_the_clear: bool,
}

impl ServerAddress {
    /// `typed` is what someone types: the address they open MaxNines at.
    pub fn parse(typed: &str) -> Result<Self, Error> {
        let mut url = url::Url::parse(typed.trim()).map_err(|_| Error::BadAddress)?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(Error::BadAddress);
        }
        url.set_path("/");
        url.set_query(None);
        url.set_fragment(None);
        let host = url.host_str().unwrap_or_default();
        let in_the_clear =
            url.scheme() == "http" && !matches!(host, "localhost" | "127.0.0.1" | "[::1]");
        Ok(Self {
            root: url.to_string(),
            in_the_clear,
        })
    }

    /// The root of the site, such as `https://monitor.example.com/`.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// A page of the web interface, such as `/alerts`.
    pub fn page(&self, path: &str) -> String {
        format!("{}{}", self.root, path.trim_start_matches('/'))
    }

    /// Whether what the app sends crosses the network unencrypted to somewhere other
    /// than this computer.
    pub fn is_in_the_clear(&self) -> bool {
        self.in_the_clear
    }
}

/// Everything the app shows, from one round of asking the server.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listed {
    pub alerts: Alerts,
    pub hosts: Vec<MonitoredHost>,
    pub extras: Extras,
}

/// What the server is asked for besides alerts and hosts. What one round listed is
/// handed to the next, which keeps it while asking again fails for now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extras {
    /// The server's own checks. None when it can't list them: one too old to, or a
    /// sign-in not allowed to.
    pub checks: Option<Vec<RemoteCheck>>,
    /// The silences running. None when the server can't list them.
    pub silences: Option<Vec<Silence>>,
}

/// Something that hands out access tokens: a `TokenHolder` in the app.
pub trait AccessTokens: Send + Sync {
    /// One that works, as far as is known.
    fn access_token(&self) -> Result<String, Error>;
    /// A newer one than `rejected`, which the server just turned down.
    fn access_token_replacing(&self, rejected: &str) -> Result<String, Error>;
}

#[derive(Clone)]
pub struct Client {
    pub server: ServerAddress,
    tokens: Arc<dyn AccessTokens>,
    http: Arc<dyn Http>,
}

impl Client {
    pub fn new(server: ServerAddress, tokens: Arc<dyn AccessTokens>, http: Arc<dyn Http>) -> Self {
        Self {
            server,
            tokens,
            http,
        }
    }

    fn request(&self, method: &'static str, path: &str, body: Body, token: String) -> Request {
        Request {
            method,
            url: self.server.page(&format!("api/v1/{path}")),
            bearer: Some(token),
            body,
            timeout: API_TIMEOUT,
        }
    }

    /// Sends a request with the access token. If the server turns the token down, it
    /// gets a newer one and tries once more.
    fn send(&self, method: &'static str, path: &str, body: Body) -> Result<String, Error> {
        let token = self.tokens.access_token()?;
        let mut answer =
            self.http
                .send(&self.request(method, path, body.clone(), token.clone()))?;
        if answer.status == 401 {
            let newer = self.tokens.access_token_replacing(&token)?;
            answer = self.http.send(&self.request(method, path, body, newer))?;
        }
        match answer.status {
            200..=299 => Ok(answer.text),
            401 => Err(Error::NotSignedIn),
            403 => Err(Error::NotAllowed(said(&answer))),
            status => Err(Error::Status(status)),
        }
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        let text = self.send("GET", path, Body::None)?;
        serde_json::from_str(&text).map_err(|_| Error::NotMaxNines)
    }

    pub fn alerts(&self) -> Result<Alerts, Error> {
        self.get("alerts")
    }

    pub fn hosts(&self) -> Result<Vec<MonitoredHost>, Error> {
        self.get("hosts")
    }

    pub fn silences(&self) -> Result<Silences, Error> {
        self.get("silences")
    }

    pub fn remote_checks(&self) -> Result<Vec<RemoteCheck>, Error> {
        self.get("remote-checks")
    }

    /// Everything the app shows, asked for all at once, so a slow or unreachable
    /// server takes as long as one request to answer or give up, not four. Checks and
    /// silences are extras: when asking for them fails for now, what `last` had is
    /// kept, so they don't seem to end for a round. A sign-in that's run out ends the
    /// round, whichever request found out.
    pub fn listed(&self, last: &Extras) -> Result<Listed, Error> {
        let (alerts, hosts, checks, silences) = std::thread::scope(|scope| {
            let hosts = alongside(scope, || self.hosts());
            let checks = alongside(scope, || self.remote_checks());
            let silences = alongside(scope, || self.silences());
            let alerts = self.alerts();
            (alerts, hosts.join(), checks.join(), silences.join())
        });
        let signed_out = [
            alerts.as_ref().err(),
            hosts.as_ref().err(),
            checks.as_ref().err(),
            silences.as_ref().err(),
        ]
        .contains(&Some(&Error::SignedOut));
        if signed_out {
            return Err(Error::SignedOut);
        }
        Ok(Listed {
            alerts: alerts?,
            hosts: hosts?,
            extras: Extras {
                checks: kept(checks, &last.checks),
                silences: kept(silences.map(|silences| silences.active), &last.silences),
            },
        })
    }

    /// Says whoever signed in is dealing with an alert.
    pub fn acknowledge(&self, alert_id: i64) -> Result<(), Error> {
        self.send(
            "POST",
            &format!("alerts/{alert_id}/acknowledge"),
            Body::None,
        )
        .map(drop)
    }

    /// Keeps an alert's notifications back for a while.
    pub fn silence(&self, alert: &ServerAlert, for_secs: u64) -> Result<(), Error> {
        let body = silence_body(alert, for_secs)?;
        self.send("POST", "silences", Body::Json(body.to_string()))
            .map(drop)
    }
}

/// A request of [`Client::listed`], on a thread of its own alongside the others, or
/// later on the thread that asked, if the system can't start another.
enum Alongside<'scope, T, F> {
    Running(std::thread::ScopedJoinHandle<'scope, T>),
    Later(F),
}

fn alongside<'scope, T: Send + 'scope, F: FnOnce() -> T + Send + Clone + 'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    request: F,
) -> Alongside<'scope, T, F> {
    match std::thread::Builder::new().spawn_scoped(scope, request.clone()) {
        Ok(thread) => Alongside::Running(thread),
        Err(_) => Alongside::Later(request),
    }
}

impl<T, F: FnOnce() -> T> Alongside<'_, T, F> {
    /// What it found. One whose thread panicked panics here too, as it would have had
    /// it run on this thread.
    fn join(self) -> T {
        match self {
            Self::Running(thread) => thread
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Self::Later(request) => request(),
        }
    }
}

/// What to show of an extra the server was asked for: what it listed; nothing when it
/// can't list it at all, being too old to or the sign-in not allowed to; or what it
/// listed last time, when asking failed only for now, as when a proxy in front of it
/// answers with a page of its own for a while.
fn kept<T: Clone>(fresh: Result<T, Error>, last: &Option<T>) -> Option<T> {
    match fresh {
        Ok(listed) => Some(listed),
        Err(Error::NotAllowed(_) | Error::Status(404)) => None,
        Err(_) => last.clone(),
    }
}

/// What the server said went wrong, in an answer like `{"error": "…"}`.
fn said(answer: &Answer) -> Option<String> {
    let json: Value = serde_json::from_str(&answer.text).ok()?;
    json.get("error")?.as_str().map(str::to_string)
}

/// A silence for this alert and nothing else: its rule, on its host, for its series.
/// An alert about a group of hosts belongs to no one host, so it has no filter, which
/// the server takes to mean it alone.
fn silence_body(alert: &ServerAlert, for_secs: u64) -> Result<Value, Error> {
    // Without a rule the silence would cover every rule.
    let rule = alert.rule_id.ok_or(Error::CantSilence)?;
    let mut body = json!({
        "reason": "Silenced from MaxNines Desktop",
        "duration_secs": for_secs,
        "rule_id": rule,
    });
    if alert.host_id != "fleet" {
        body["filter"] = json!(format!("id={}", alert.host_id));
    }
    if let Some(series) = &alert.series {
        body["series"] = json!(series);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::http::fake::{Fake, answer};
    use crate::models::Severity;
    use crate::notices::tests::alert;

    /// Hands out `token-1`, then `token-2` once that's turned down.
    struct Counting(Mutex<u32>);

    impl AccessTokens for Counting {
        fn access_token(&self) -> Result<String, Error> {
            Ok(format!("token-{}", *self.0.lock().unwrap()))
        }

        fn access_token_replacing(&self, _: &str) -> Result<String, Error> {
            let mut count = self.0.lock().unwrap();
            *count += 1;
            Ok(format!("token-{count}"))
        }
    }

    fn client(fake: Arc<Fake>) -> Client {
        Client::new(
            ServerAddress::parse("https://monitor.example.com/alerts?x=1#y").unwrap(),
            Arc::new(Counting(Mutex::new(1))),
            fake,
        )
    }

    #[test]
    fn an_address_is_the_root_of_whatever_page_was_pasted() {
        let server =
            ServerAddress::parse("  https://monitor.example.com:8443/hosts/abc?x=1 ").unwrap();
        assert_eq!(
            server.page("/alerts"),
            "https://monitor.example.com:8443/alerts"
        );
        assert_eq!(server.root(), "https://monitor.example.com:8443/");
        assert!(!server.is_in_the_clear());
        assert!(
            ServerAddress::parse("http://192.168.0.155:9000")
                .unwrap()
                .is_in_the_clear()
        );
        // This computer, however it's written, is never in the clear.
        for here in [
            "http://localhost:9000",
            "HTTP://LOCALHOST:9000",
            "http://127.0.0.1:9000",
            "http://[::1]:9000",
        ] {
            assert!(
                !ServerAddress::parse(here).unwrap().is_in_the_clear(),
                "{here}"
            );
        }
        for bad in [
            "",
            "monitor.example.com",
            "ftp://monitor.example.com",
            "https://",
        ] {
            assert_eq!(ServerAddress::parse(bad), Err(Error::BadAddress), "{bad}");
        }
    }

    #[test]
    fn a_turned_down_token_is_replaced_and_tried_once_more() {
        let fake = Arc::new(Fake::new(|request| match request.bearer.as_deref() {
            Some("token-2") => answer(200, r#"{"open": [], "resolved": []}"#),
            _ => answer(401, ""),
        }));
        assert_eq!(client(fake.clone()).alerts().unwrap(), Alerts::default());
        let sent = fake.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].url, "https://monitor.example.com/api/v1/alerts");
        assert_eq!(sent[1].bearer.as_deref(), Some("token-2"));

        let refused = Arc::new(Fake::new(|_| answer(401, "")));
        assert_eq!(client(refused).hosts(), Err(Error::NotSignedIn));
    }

    #[test]
    fn what_the_server_says_isnt_allowed_is_shown() {
        let fake = Arc::new(Fake::new(|_| {
            answer(403, r#"{"error": "Needs the responder role"}"#)
        }));
        assert_eq!(
            client(fake).acknowledge(7),
            Err(Error::NotAllowed(Some("Needs the responder role".into())))
        );
        let silent = Arc::new(Fake::new(|_| answer(403, "")));
        let error = client(silent).acknowledge(7).unwrap_err();
        assert!(error.to_string().contains("responder role"), "{error}");
        let other = Arc::new(Fake::new(|_| answer(200, "<html>")));
        assert_eq!(client(other).alerts(), Err(Error::NotMaxNines));
    }

    /// Answers each of the four lists by its path, from `answers`.
    fn lists(answers: [(u16, &'static str); 4]) -> Arc<Fake> {
        Arc::new(Fake::new(move |request| {
            let path = request.url.rsplit_once("/api/v1/").unwrap().1;
            let index = ["alerts", "hosts", "remote-checks", "silences"]
                .iter()
                .position(|known| *known == path)
                .unwrap();
            let (status, text) = answers[index];
            answer(status, text)
        }))
    }

    const ALERTS: (u16, &str) = (200, r#"{"open": [], "resolved": []}"#);
    const HOSTS: (u16, &str) = (200, "[]");
    const CHECKS: (u16, &str) = (200, r#"[{"id": 1, "enabled": true}]"#);
    const SILENCES: (u16, &str) = (
        200,
        r#"{"active": [{"id": 3, "rule_id": null, "ends_at": "2026-11-19T02:30:00Z"}]}"#,
    );

    #[test]
    fn everything_is_asked_for_at_once() {
        use std::sync::Condvar;
        // Each request waits for all four to have arrived, which only happens if
        // they're sent together. One at a time, the first gives up waiting.
        let arrived = Arc::new((Mutex::new(0), Condvar::new()));
        let fake = Arc::new(Fake::new(move |request| {
            let (count, all_here) = &*arrived;
            let mut count = count.lock().unwrap();
            *count += 1;
            all_here.notify_all();
            let (count, waited) = all_here
                .wait_timeout_while(count, Duration::from_secs(10), |count| *count < 4)
                .unwrap();
            drop(count);
            if waited.timed_out() {
                return answer(504, "");
            }
            let path = request.url.rsplit_once("/api/v1/").unwrap().1;
            match path {
                "alerts" => answer(ALERTS.0, ALERTS.1),
                "hosts" => answer(HOSTS.0, HOSTS.1),
                "remote-checks" => answer(CHECKS.0, CHECKS.1),
                _ => answer(SILENCES.0, SILENCES.1),
            }
        }));
        let listed = client(fake).listed(&Extras::default()).unwrap();
        assert_eq!(listed.extras.checks.unwrap().len(), 1);
        assert_eq!(listed.extras.silences.unwrap()[0].id, 3);
    }

    #[test]
    fn extras_that_cant_be_asked_for_now_are_kept_from_last_time() {
        let last = client(lists([ALERTS, HOSTS, CHECKS, SILENCES]))
            .listed(&Extras::default())
            .unwrap();
        // A server that's struggling keeps what it said last time, as does one behind
        // a proxy that answers with a page of its own for a while.
        let struggling = client(lists([ALERTS, HOSTS, (500, ""), (200, "<html>")]))
            .listed(&last.extras)
            .unwrap();
        assert_eq!(struggling, last);
        // One that can't list them at all, being too old or the sign-in not allowed
        // to, shows none.
        let cant = client(lists([
            ALERTS,
            HOSTS,
            (403, r#"{"error": "Not allowed"}"#),
            (404, ""),
        ]))
        .listed(&last.extras)
        .unwrap();
        assert_eq!(cant.extras, Extras::default());
        // Without the alerts or hosts there's nothing to show.
        assert_eq!(
            client(lists([(500, ""), HOSTS, CHECKS, SILENCES])).listed(&last.extras),
            Err(Error::Status(500))
        );
    }

    /// Hands out a token, but finds the sign-in has run out when it's turned down.
    struct RunOut;

    impl AccessTokens for RunOut {
        fn access_token(&self) -> Result<String, Error> {
            Ok("token".into())
        }

        fn access_token_replacing(&self, _: &str) -> Result<String, Error> {
            Err(Error::SignedOut)
        }
    }

    #[test]
    fn a_sign_in_found_to_have_run_out_ends_the_round() {
        let fake = lists([(500, ""), HOSTS, CHECKS, (401, "")]);
        let client = Client::new(
            ServerAddress::parse("https://monitor.example.com").unwrap(),
            Arc::new(RunOut),
            fake,
        );
        assert_eq!(client.listed(&Extras::default()), Err(Error::SignedOut));
    }

    #[test]
    fn a_silence_covers_the_one_alert() {
        let fake = Arc::new(Fake::new(|_| answer(201, "{}")));
        let mut checked = alert(7, Severity::Critical);
        checked.series = Some("https://example.com".into());
        client(fake.clone()).silence(&checked, 3600).unwrap();
        let sent = &fake.sent()[0];
        assert_eq!(sent.url, "https://monitor.example.com/api/v1/silences");
        let Body::Json(text) = &sent.body else {
            panic!("{:?}", sent.body)
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            body,
            json!({"reason": "Silenced from MaxNines Desktop", "duration_secs": 3600,
                   "rule_id": 1, "filter": "id=h7", "series": "https://example.com"})
        );
        let mut fleet = alert(8, Severity::Warning);
        fleet.host_id = "fleet".into();
        assert!(silence_body(&fleet, 60).unwrap().get("filter").is_none());
        let mut ruleless = alert(9, Severity::Warning);
        ruleless.rule_id = None;
        assert_eq!(silence_body(&ruleless, 60), Err(Error::CantSilence));
    }
}
