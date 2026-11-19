//! Talking to one MaxNines server's API as whoever signed in.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::Error;
use crate::http::{Answer, Body, Http, Request};
use crate::models::{Alerts, MonitoredHost, RemoteCheck, ServerAlert, Silences};

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
