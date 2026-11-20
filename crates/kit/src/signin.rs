//! Signing in through the browser, as the server's "Signing in apps" documentation
//! describes: the person approves the app on a MaxNines page, and the browser brings
//! an approval back to this computer, which the app swaps for tokens.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;

use crate::Error;
use crate::client::ServerAddress;
use crate::http::{Body, Http, Request};

/// What the server knows this app as.
pub const CLIENT_ID: &str = "maxnines-desktop-linux";

/// The most the app asks for: enough to acknowledge and silence. The person can
/// choose less.
pub const ASKED_ROLE: &str = "responder";

/// How long a sign-in endpoint waits for its answer. Short, so a refresh that gets no
/// answer can be tried again while the server still takes the refresh token: see
/// `tokens::RETRY_WINDOW`.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10);

/// How long someone has to approve the app in the browser.
pub const BROWSER_WAIT: Duration = Duration::from_secs(5 * 60);

const CALLBACK_PATH: &str = "/callback";

/// What the server gives for an approval or a refresh token.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    /// Seconds the access token works for.
    pub expires_in: u64,
    pub refresh_token: String,
    /// What the app acts as, which the person may have chosen lower than asked.
    pub role: String,
    /// The email address of who signed in.
    pub user: String,
}

/// Random bytes in base64url, which uses only characters a URL leaves alone.
fn random_text(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    // The system's randomness only fails if the system is broken.
    SystemRandom::new()
        .fill(&mut buffer)
        .expect("the system's random number source failed");
    URL_SAFE_NO_PAD.encode(buffer)
}

/// One attempt at signing in: what's sent to the browser, and how to read what comes
/// back.
#[derive(Debug, Clone)]
pub struct BrowserSignIn {
    pub server: ServerAddress,
    /// Where the browser comes back to, on this computer.
    pub redirect_uri: String,
    /// The computer's name, shown when approving and in the server's list of
    /// signed-in apps.
    pub device: String,
    /// Kept by the app, and sent only when swapping the approval for tokens.
    verifier: String,
    /// Sent and given back unchanged, so an answer to some other request is turned
    /// away.
    state: String,
}

/// What the browser brought back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callback {
    Code(String),
    /// The person chose Deny.
    Denied,
    /// The server couldn't ask the person, saying why in OAuth's words.
    Failed(String),
}

impl BrowserSignIn {
    pub fn new(server: ServerAddress, port: u16, device: &str) -> Self {
        Self::with(server, port, device, random_text(32), random_text(16))
    }

    fn with(
        server: ServerAddress,
        port: u16,
        device: &str,
        verifier: String,
        state: String,
    ) -> Self {
        // The server takes up to 100 characters, with no control characters.
        let device: String = device
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(100)
            .collect();
        Self {
            server,
            redirect_uri: format!("http://127.0.0.1:{port}{CALLBACK_PATH}"),
            device,
            verifier,
            state,
        }
    }

    /// The SHA-256 of the verifier, in base64url without padding, as RFC 7636 says.
    fn challenge(&self) -> String {
        URL_SAFE_NO_PAD.encode(digest(&SHA256, self.verifier.as_bytes()))
    }

    /// The page the browser opens for the person to approve the app.
    pub fn authorize_url(&self) -> String {
        let challenge = self.challenge();
        let query = serde_urlencoded::to_string([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", self.state.as_str()),
            ("device", self.device.as_str()),
            ("role", ASKED_ROLE),
        ])
        .unwrap_or_default();
        format!("{}?{query}", self.server.page("oauth/authorize"))
    }

    /// What the browser brought back, from its request's target, such as
    /// `/callback?code=…&state=…`. Nothing for a request that isn't the answer to this
    /// sign-in: another path, or another `state`.
    pub fn callback(&self, target: &str) -> Option<Callback> {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != CALLBACK_PATH {
            return None;
        }
        let fields: Vec<(String, String)> = serde_urlencoded::from_str(query).unwrap_or_default();
        let field = |name: &str| {
            fields
                .iter()
                .find_map(|(key, value)| (key == name).then_some(value.as_str()))
        };
        if field("state") != Some(self.state.as_str()) {
            return None;
        }
        Some(match (field("code"), field("error")) {
            (Some(code), _) if !code.is_empty() => Callback::Code(code.to_string()),
            (_, Some("access_denied")) => Callback::Denied,
            (_, error) => Callback::Failed(error.unwrap_or("no code").to_string()),
        })
    }
}

/// The server's sign-in endpoints, which apps call with what they hold rather than a
/// token.
#[derive(Clone)]
pub struct SignInEndpoints {
    pub server: ServerAddress,
    http: Arc<dyn Http>,
}

#[derive(Deserialize)]
struct OAuthAnswer {
    error: String,
    error_description: Option<String>,
}

impl SignInEndpoints {
    pub fn new(server: ServerAddress, http: Arc<dyn Http>) -> Self {
        Self { server, http }
    }

    /// Swaps an approval for tokens.
    pub fn tokens(&self, code: &str, sign_in: &BrowserSignIn) -> Result<Tokens, Error> {
        self.issue(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("redirect_uri", &sign_in.redirect_uri),
            ("code_verifier", &sign_in.verifier),
        ])
    }

    /// Swaps a refresh token for new tokens. The old refresh token stops working.
    /// [`Error::SignedOut`] when the sign-in has run out or been revoked.
    pub fn refresh(&self, refresh_token: &str) -> Result<Tokens, Error> {
        match self.issue(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token),
        ]) {
            Err(Error::OAuth(error, _)) if error == "invalid_grant" => Err(Error::SignedOut),
            other => other,
        }
    }

    /// Signs the app out on the server. Either token works.
    pub fn revoke(&self, token: &str) -> Result<(), Error> {
        let answer = self.post(
            "oauth/revoke",
            &[("client_id", CLIENT_ID), ("token", token)],
        )?;
        if answer.status == 200 {
            Ok(())
        } else {
            Err(Error::Status(answer.status))
        }
    }

    fn issue(&self, fields: &[(&str, &str)]) -> Result<Tokens, Error> {
        let answer = self.post("oauth/token", fields)?;
        if answer.status != 200 {
            if let Ok(said) = serde_json::from_str::<OAuthAnswer>(&answer.text) {
                let description = said.error_description.unwrap_or_else(|| said.error.clone());
                return Err(Error::OAuth(said.error, description));
            }
            return Err(Error::Status(answer.status));
        }
        serde_json::from_str(&answer.text).map_err(|_| Error::NotMaxNines)
    }

    fn post(&self, path: &str, fields: &[(&str, &str)]) -> Result<crate::http::Answer, Error> {
        self.http.send(&Request {
            method: "POST",
            url: self.server.page(path),
            bearer: None,
            body: Body::Form(serde_urlencoded::to_string(fields).unwrap_or_default()),
            timeout: SIGN_IN_TIMEOUT,
        })
    }
}

/// The most connections read from at once while waiting for the browser. Past this,
/// the one that's waited longest is dropped, as the least likely to be the browser
/// coming back, so whatever else connects can't use up the app.
const MOST_WAITING: usize = 16;

/// How long a connection has to send its request.
const REQUEST_WAIT: Duration = Duration::from_secs(10);

/// The longest request read: a browser's headers are a few kilobytes.
const LONGEST_REQUEST: usize = 64 * 1024;

/// Signs in through the browser: listens on 127.0.0.1 for the browser to come back,
/// has `open` show the person the server's approval page, swaps the approval for
/// tokens, and only then tells the person in the browser how it went. Gives up after
/// `wait`, or once `cancelled` is set. It all happens on the calling thread, which it
/// keeps until then.
pub fn through_browser(
    endpoints: &SignInEndpoints,
    device: &str,
    open: &dyn Fn(&str),
    cancelled: &AtomicBool,
    wait: Duration,
) -> Result<Tokens, Error> {
    let local = |error: std::io::Error| Error::SignInFailed(error.to_string());
    let listener = TcpListener::bind("127.0.0.1:0").map_err(local)?;
    let port = listener.local_addr().map_err(local)?.port();
    let sign_in = BrowserSignIn::new(endpoints.server.clone(), port, device);
    open(&sign_in.authorize_url());
    listener.set_nonblocking(true).map_err(local)?;
    let deadline = Instant::now() + wait;
    // Every connection is read a little at a time, as it sends, so one a browser
    // opens and never uses holds nothing up.
    let mut waiting: VecDeque<Waiting> = VecDeque::new();
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // Not every system has it inherit the listener's.
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    if waiting.len() >= MOST_WAITING {
                        waiting.pop_front();
                    }
                    waiting.push_back(Waiting {
                        stream,
                        read: Vec::new(),
                        until: Instant::now() + REQUEST_WAIT,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                // One that gave up before it was taken.
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(local(error)),
            }
        }
        let Some((mut stream, target)) = next_request(&mut waiting, Instant::now()) else {
            if Instant::now() >= deadline {
                return Err(Error::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        // Answered all at once, rather than as the connection takes it.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_write_timeout(Some(REQUEST_WAIT));
        // Anything else the browser asks for, such as an icon, or an answer to some
        // other request, isn't the one being waited for.
        let Some(callback) = sign_in.callback(&target) else {
            reply(&mut stream, "404 Not Found", "Not found", "");
            continue;
        };
        let result = match callback {
            Callback::Denied => Err(Error::Denied),
            Callback::Failed(error) => Err(Error::SignInFailed(error)),
            Callback::Code(code) => endpoints.tokens(&code, &sign_in),
        };
        match &result {
            Ok(_) => reply(
                &mut stream,
                "200 OK",
                "Signed in to MaxNines",
                "You can close this tab.",
            ),
            Err(error) => reply(
                &mut stream,
                "200 OK",
                "Not signed in",
                &format!("{error}. You can close this tab."),
            ),
        }
        return result;
    }
}

/// A connection to the loopback port, and what it's sent so far.
struct Waiting {
    stream: TcpStream,
    read: Vec<u8>,
    /// When it's dropped if it hasn't sent a whole request.
    until: Instant,
}

/// How a connection's request is coming along.
enum Reading {
    /// Not all here yet.
    Waiting,
    /// Here: the target of its GET.
    Target(String),
    /// Closed, too slow, too long, or not a GET.
    Gone,
}

impl Waiting {
    /// Reads what's arrived, without waiting for more.
    fn read(&mut self, now: Instant) -> Reading {
        let mut chunk = [0u8; 4096];
        loop {
            // The headers have all arrived, as closing with some unread could cut the
            // page off.
            if self.read.windows(4).any(|window| window == b"\r\n\r\n") {
                return get_target(&self.read).map_or(Reading::Gone, Reading::Target);
            }
            if now >= self.until || self.read.len() > LONGEST_REQUEST {
                return Reading::Gone;
            }
            match self.stream.read(&mut chunk) {
                Ok(0) => return Reading::Gone,
                Ok(read) => self.read.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Reading::Waiting;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Reading::Gone,
            }
        }
    }
}

/// The first connection with its whole request here, taken from `waiting` with the
/// request's target. Ones that have gone are dropped on the way.
fn next_request(waiting: &mut VecDeque<Waiting>, now: Instant) -> Option<(TcpStream, String)> {
    let mut index = 0;
    while index < waiting.len() {
        match waiting[index].read(now) {
            Reading::Waiting => index += 1,
            Reading::Gone => {
                waiting.remove(index);
            }
            Reading::Target(target) => {
                let arrived = waiting.remove(index)?;
                return Some((arrived.stream, target));
            }
        }
    }
    None
}

/// The target of a GET request, from its head.
fn get_target(head: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(head);
    let mut parts = text.lines().next()?.split(' ');
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(target)) => Some(target.to_string()),
        _ => None,
    }
}

/// Answers the browser with a page, then closes the connection.
fn reply(stream: &mut TcpStream, status: &str, title: &str, detail: &str) {
    let page = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title>\
         <style>body{{margin:0;min-height:100vh;display:grid;place-items:center;\
         font:16px system-ui,sans-serif;background:#0f1214;color:#e6e9eb}}\
         main{{text-align:center;padding:24px}}h1{{font-size:22px;font-weight:600;margin:0 0 8px}}\
         p{{color:#9aa3a8;margin:0}}</style></head>\
         <body><main><h1>{title}</h1><p>{detail}</p></main></body></html>",
        title = escape(title),
        detail = escape(detail),
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::thread::JoinHandle;

    use super::*;
    use crate::http::fake::{Fake, answer};

    /// Plays the browser on a thread of its own, started when the sign-in opens the
    /// approval page, so what it checks fails the test once it's joined.
    struct Browser(RefCell<Option<JoinHandle<()>>>);

    impl Browser {
        fn new() -> Self {
            Self(RefCell::new(None))
        }

        fn start(&self, browse: impl FnOnce() + Send + 'static) {
            let started = self.0.replace(Some(std::thread::spawn(browse)));
            assert!(started.is_none(), "the approval page was opened twice");
        }

        /// Waits for it to finish, failing if anything it checked did.
        fn finished(&self) {
            let browser = self.0.take().expect("the approval page was never opened");
            if let Err(panic) = browser.join() {
                std::panic::resume_unwind(panic);
            }
        }
    }

    fn server() -> ServerAddress {
        ServerAddress::parse("https://monitor.example.com").unwrap()
    }

    fn sign_in() -> BrowserSignIn {
        BrowserSignIn::with(
            server(),
            8123,
            " office\tlaptop \n",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into(),
            "xyz".into(),
        )
    }

    #[test]
    fn the_approval_page_asks_for_this_app() {
        let url = sign_in().authorize_url();
        let (page, query) = url.split_once('?').unwrap();
        assert_eq!(page, "https://monitor.example.com/oauth/authorize");
        let fields: Vec<(String, String)> = serde_urlencoded::from_str(query).unwrap();
        let field = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(field("client_id"), "maxnines-desktop-linux");
        assert_eq!(field("redirect_uri"), "http://127.0.0.1:8123/callback");
        // RFC 7636's own example.
        assert_eq!(
            field("code_challenge"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(field("code_challenge_method"), "S256");
        assert_eq!(field("state"), "xyz");
        assert_eq!(field("device"), "officelaptop");
        assert_eq!(field("role"), "responder");
    }

    #[test]
    fn only_the_answer_to_this_sign_in_is_read() {
        let sign_in = sign_in();
        assert_eq!(
            sign_in.callback("/callback?code=a%2Bb&state=xyz"),
            Some(Callback::Code("a+b".into()))
        );
        assert_eq!(
            sign_in.callback("/callback?error=access_denied&state=xyz"),
            Some(Callback::Denied)
        );
        assert_eq!(
            sign_in.callback("/callback?error=invalid_request&state=xyz"),
            Some(Callback::Failed("invalid_request".into()))
        );
        assert_eq!(sign_in.callback("/callback?code=a&state=other"), None);
        assert_eq!(sign_in.callback("/favicon.ico"), None);
    }

    #[test]
    fn the_sign_in_endpoints_say_what_went_wrong() {
        let endpoints = |status, text: &'static str| {
            SignInEndpoints::new(server(), Arc::new(Fake::new(move |_| answer(status, text))))
        };
        assert_eq!(
            endpoints(
                400,
                r#"{"error": "invalid_grant", "error_description": "Gone"}"#
            )
            .refresh("r"),
            Err(Error::SignedOut)
        );
        assert_eq!(
            endpoints(400, r#"{"error": "invalid_client"}"#).refresh("r"),
            Err(Error::OAuth(
                "invalid_client".into(),
                "invalid_client".into()
            ))
        );
        assert_eq!(endpoints(502, "").refresh("r"), Err(Error::Status(502)));
        assert_eq!(
            endpoints(200, "<html>").refresh("r"),
            Err(Error::NotMaxNines)
        );
        let issued = endpoints(
            200,
            r#"{"access_token": "a", "expires_in": 900, "refresh_token": "r2",
                "role": "viewer", "user": "sam@example.com", "token_type": "Bearer"}"#,
        )
        .refresh("r")
        .unwrap();
        assert_eq!(issued.refresh_token, "r2");
        assert_eq!(issued.role, "viewer");
    }

    #[test]
    fn signing_in_through_the_browser_gets_tokens() {
        let fake = Arc::new(Fake::new(|_| {
            answer(
                200,
                r#"{"access_token": "a", "expires_in": 900, "refresh_token": "r",
                    "role": "responder", "user": "sam@example.com"}"#,
            )
        }));
        let endpoints = SignInEndpoints::new(server(), fake.clone());
        // Plays the browser: asks for an icon first, then comes back with the code.
        let browser = Browser::new();
        let open = |url: &str| {
            let query = url.split_once('?').unwrap().1;
            let fields: Vec<(String, String)> = serde_urlencoded::from_str(query).unwrap();
            let field = |name: &str| {
                fields
                    .iter()
                    .find(|(key, _)| key == name)
                    .unwrap()
                    .1
                    .clone()
            };
            let back = field("redirect_uri");
            let state = field("state");
            browser.start(move || {
                let address = back
                    .trim_start_matches("http://")
                    .trim_end_matches("/callback")
                    .to_string();
                let get = |target: &str| {
                    let mut stream = TcpStream::connect(&address).unwrap();
                    write!(stream, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
                    let mut page = String::new();
                    stream.read_to_string(&mut page).unwrap();
                    page
                };
                assert!(get("/favicon.ico").starts_with("HTTP/1.1 404"));
                let page = get(&format!("/callback?code=the-code&state={state}"));
                assert!(page.contains("Signed in to MaxNines"), "{page}");
            });
        };
        let tokens = through_browser(
            &endpoints,
            "laptop",
            &open,
            &AtomicBool::new(false),
            Duration::from_secs(10),
        )
        .unwrap();
        browser.finished();
        assert_eq!(tokens.user, "sam@example.com");
        let sent = fake.sent();
        let Body::Form(form) = &sent[0].body else {
            panic!("{:?}", sent[0].body)
        };
        assert!(form.contains("grant_type=authorization_code"), "{form}");
        assert!(form.contains("code=the-code"), "{form}");
        assert!(form.contains("code_verifier="), "{form}");
    }

    #[test]
    fn connections_that_never_send_dont_keep_the_browser_out() {
        let fake = Arc::new(Fake::new(|_| {
            answer(
                200,
                r#"{"access_token": "a", "expires_in": 900, "refresh_token": "r",
                    "role": "responder", "user": "sam@example.com"}"#,
            )
        }));
        let endpoints = SignInEndpoints::new(server(), fake);
        let browser = Browser::new();
        let open = |url: &str| {
            let query = url.split_once('?').unwrap().1;
            let fields: Vec<(String, String)> = serde_urlencoded::from_str(query).unwrap();
            let field = |name: &str| {
                fields
                    .iter()
                    .find(|(key, _)| key == name)
                    .unwrap()
                    .1
                    .clone()
            };
            let address = field("redirect_uri")
                .trim_start_matches("http://")
                .trim_end_matches("/callback")
                .to_string();
            let state = field("state");
            browser.start(move || {
                // Far more than are read at once, open and silent, and one that
                // starts a request and never finishes it.
                let idle: Vec<TcpStream> = (0..MOST_WAITING * 3)
                    .map(|_| TcpStream::connect(&address).unwrap())
                    .collect();
                let mut partial = TcpStream::connect(&address).unwrap();
                write!(partial, "GET /callback?code=x").unwrap();
                let mut stream = TcpStream::connect(&address).unwrap();
                write!(
                    stream,
                    "GET /callback?code=the-code&state={state} HTTP/1.1\r\nHost: x\r\n\r\n"
                )
                .unwrap();
                let mut page = String::new();
                stream.read_to_string(&mut page).unwrap();
                assert!(page.contains("Signed in to MaxNines"), "{page}");
                drop((idle, partial));
            });
        };
        let tokens = through_browser(
            &endpoints,
            "laptop",
            &open,
            &AtomicBool::new(false),
            Duration::from_secs(10),
        )
        .unwrap();
        browser.finished();
        assert_eq!(tokens.refresh_token, "r");
    }

    #[test]
    fn signing_in_can_be_cancelled_or_run_out() {
        let endpoints = SignInEndpoints::new(server(), Arc::new(Fake::new(|_| answer(500, ""))));
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            through_browser(
                &endpoints,
                "x",
                &|_| {},
                &cancelled,
                Duration::from_secs(10)
            ),
            Err(Error::Cancelled)
        );
        assert_eq!(
            through_browser(
                &endpoints,
                "x",
                &|_| {},
                &AtomicBool::new(false),
                Duration::ZERO
            ),
            Err(Error::TimedOut)
        );
    }
}
