//! Keeping one sign-in's tokens, and handing out an access token that works.
//!
//! The access token lives only in memory. The refresh token is handed to `save` each
//! time it changes, before the new access token is used, so what's kept is always the
//! newest: the server revokes the whole sign-in if a refresh token it has replaced is
//! used again a minute later. For the same reason only one refresh runs at a time, and
//! everything that needs a token meanwhile waits for it.
//!
//! If the new refresh token can't be kept, it's still used, as the old one no longer
//! works, and the holder remembers it isn't kept, so it can be tried again before
//! the app next starts and reads the old one.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::Error;
use crate::client::AccessTokens;
use crate::signin::{SignInEndpoints, Tokens};

/// How long before the access token runs out that a new one is fetched.
const EARLY: Duration = Duration::from_secs(60);

/// How long to wait before each new try when a refresh fails for any reason but being
/// signed out. They all fall within the minute the server still takes a refresh token
/// it has just replaced, so an answer lost on the way is asked for again in time,
/// rather than at the next check, when reusing the token would get the whole sign-in
/// revoked.
pub const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
];

/// How long after the first try a refresh may start another. The server takes a
/// refresh token it has just replaced for a minute from when it replaced it, which is
/// no earlier than the first try, so this leaves room for a slow network, however long
/// each try waited for its answer.
pub const RETRY_WINDOW: Duration = Duration::from_secs(45);

/// Keeps tokens, or forgets them when given nothing, saying why if it couldn't.
pub type Save = Box<dyn Fn(Option<&Tokens>) -> Result<(), String> + Send + Sync>;
type Clock = Box<dyn Fn() -> Instant + Send + Sync>;
type Sleep = Box<dyn Fn(Duration) + Send + Sync>;

struct State {
    refresh_token: Option<String>,
    access: Option<(String, Instant)>,
    /// Set once signing out starts, after which nothing refreshes.
    signing_out: bool,
    /// The newest tokens, while `save` couldn't keep them.
    unkept: Option<Tokens>,
}

pub struct TokenHolder {
    endpoints: SignInEndpoints,
    /// Keeps tokens, or forgets them when given nothing, as when signed out.
    save: Save,
    now: Clock,
    sleep: Sleep,
    retry_delays: Vec<Duration>,
    state: Mutex<State>,
    /// Held while a refresh runs, so there's only ever one.
    refreshing: Mutex<()>,
}

impl TokenHolder {
    /// Just signed in, with tokens that `save` already has, unless it couldn't keep
    /// them, when `kept` is false and [`Self::keep_again`] tries again.
    pub fn signed_in(tokens: &Tokens, kept: bool, endpoints: SignInEndpoints, save: Save) -> Self {
        let holder = Self::new(Some(tokens.refresh_token.clone()), endpoints, save);
        {
            let mut state = holder.state.lock().unwrap();
            state.access = Some((
                tokens.access_token.clone(),
                Instant::now() + Duration::from_secs(tokens.expires_in),
            ));
            state.unkept = (!kept).then(|| tokens.clone());
        }
        holder
    }

    /// Signed in earlier, with only the saved refresh token, so the first request
    /// refreshes.
    pub fn saved(refresh_token: String, endpoints: SignInEndpoints, save: Save) -> Self {
        Self::new(Some(refresh_token), endpoints, save)
    }

    fn new(refresh_token: Option<String>, endpoints: SignInEndpoints, save: Save) -> Self {
        Self {
            endpoints,
            save,
            now: Box::new(Instant::now),
            sleep: Box::new(std::thread::sleep),
            retry_delays: RETRY_DELAYS.to_vec(),
            state: Mutex::new(State {
                refresh_token,
                access: None,
                signing_out: false,
                unkept: None,
            }),
            refreshing: Mutex::new(()),
        }
    }

    /// Signs out: tells the server, and forgets the tokens whether or not it could be
    /// told. An error when the server couldn't be told, so the person can be asked to
    /// revoke it there.
    pub fn sign_out(&self) -> Result<(), Error> {
        self.state.lock().unwrap().signing_out = true;
        // The server only revokes by its newest tokens, so a refresh running now is
        // let finish, and what it brings back is revoked.
        let _waited = self.refreshing.lock().unwrap();
        let token = {
            let mut state = self.state.lock().unwrap();
            let token = state
                .refresh_token
                .take()
                .or_else(|| state.access.take().map(|(token, _)| token));
            state.access = None;
            state.unkept = None;
            token
        };
        // Forgotten whether or not that works: the server is told next, after which
        // the token is no use to anyone.
        let _ = (self.save)(None);
        match token {
            Some(token) => self.endpoints.revoke(&token),
            None => Ok(()),
        }
    }

    /// Whether the newest refresh token is kept, so the app would start with it.
    pub fn is_kept(&self) -> bool {
        self.state.lock().unwrap().unkept.is_none()
    }

    /// Tries again to keep the newest tokens, after `save` couldn't. Ok once they're
    /// kept, or if there's nothing left to keep.
    pub fn keep_again(&self) -> Result<(), String> {
        // No refresh brings newer ones meanwhile, which this would overwrite.
        let _only_one = self.refreshing.lock().unwrap();
        let tokens = {
            let state = self.state.lock().unwrap();
            match &state.unkept {
                Some(tokens) if !state.signing_out => tokens.clone(),
                _ => return Ok(()),
            }
        };
        (self.save)(Some(&tokens))?;
        self.state.lock().unwrap().unkept = None;
        Ok(())
    }

    /// The access token, if it isn't about to run out.
    fn current(&self) -> Option<String> {
        let state = self.state.lock().unwrap();
        let (token, expires) = state.access.as_ref()?;
        (expires.saturating_duration_since((self.now)()) > EARLY).then(|| token.clone())
    }

    /// Refreshes, unless another refresh brought a token other than `rejected` while
    /// this one waited its turn.
    fn refreshed(&self, rejected: Option<&str>) -> Result<String, Error> {
        let _only_one = self.refreshing.lock().unwrap();
        if let Some(token) = self.current()
            && Some(token.as_str()) != rejected
        {
            return Ok(token);
        }
        let refresh_token = {
            let state = self.state.lock().unwrap();
            if state.signing_out {
                return Err(Error::SignedOut);
            }
            state.refresh_token.clone().ok_or(Error::SignedOut)?
        };
        let tokens = match self.refresh_trying(&refresh_token) {
            Ok(tokens) => tokens,
            Err(Error::SignedOut) => {
                // It's run out or been revoked, and won't work again.
                let mut state = self.state.lock().unwrap();
                state.refresh_token = None;
                state.access = None;
                state.unkept = None;
                drop(state);
                // If it can't be forgotten, the app finds it no longer works next time.
                let _ = (self.save)(None);
                return Err(Error::SignedOut);
            }
            Err(error) => return Err(error),
        };
        // Kept before it's used, so a crash now can't leave the old one saved. If it
        // can't be kept, it's used all the same, as the old one no longer works, and
        // remembered, so it can be kept later.
        let kept = (self.save)(Some(&tokens));
        let mut state = self.state.lock().unwrap();
        state.unkept = kept.is_err().then(|| tokens.clone());
        state.refresh_token = Some(tokens.refresh_token.clone());
        state.access = Some((
            tokens.access_token.clone(),
            (self.now)() + Duration::from_secs(tokens.expires_in),
        ));
        Ok(tokens.access_token)
    }

    /// Swaps the refresh token, trying again soon if the answer doesn't come.
    fn refresh_trying(&self, refresh_token: &str) -> Result<Tokens, Error> {
        let started = (self.now)();
        let mut delays = self.retry_delays.iter();
        loop {
            match self.endpoints.refresh(refresh_token) {
                Err(Error::SignedOut) => return Err(Error::SignedOut),
                Err(error) => {
                    let signing_out = self.state.lock().unwrap().signing_out;
                    let Some(&delay) = delays.next() else {
                        return Err(error);
                    };
                    let waited = (self.now)().saturating_duration_since(started);
                    if signing_out || waited + delay >= RETRY_WINDOW {
                        return Err(error);
                    }
                    (self.sleep)(delay);
                }
                ok => return ok,
            }
        }
    }
}

impl AccessTokens for TokenHolder {
    fn access_token(&self) -> Result<String, Error> {
        match self.current() {
            Some(token) => Ok(token),
            None => self.refreshed(None),
        }
    }

    fn access_token_replacing(&self, rejected: &str) -> Result<String, Error> {
        // Another request may have fetched a new one already.
        match self.current() {
            Some(token) if token != rejected => Ok(token),
            _ => self.refreshed(Some(rejected)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::client::ServerAddress;
    use crate::http::Body;
    use crate::http::fake::{Fake, answer};

    fn issued(n: usize) -> String {
        format!(
            r#"{{"access_token": "access-{n}", "expires_in": 900, "refresh_token": "refresh-{n}",
                 "role": "responder", "user": "sam@example.com"}}"#
        )
    }

    fn endpoints(fake: Arc<Fake>) -> SignInEndpoints {
        SignInEndpoints::new(ServerAddress::parse("https://m.example.com").unwrap(), fake)
    }

    /// What was saved, in order: the refresh token, or nothing for signed out.
    fn saving() -> (Arc<Mutex<Vec<Option<String>>>>, Save) {
        let saved = Arc::new(Mutex::new(Vec::new()));
        let into = saved.clone();
        let save: Save = Box::new(move |tokens: Option<&Tokens>| {
            into.lock()
                .unwrap()
                .push(tokens.map(|tokens| tokens.refresh_token.clone()));
            Ok(())
        });
        (saved, save)
    }

    fn sent_refresh_tokens(fake: &Fake) -> Vec<String> {
        fake.sent()
            .iter()
            .filter_map(|request| match &request.body {
                Body::Form(form) => form
                    .split('&')
                    .find_map(|field| field.strip_prefix("refresh_token="))
                    .map(str::to_string),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_saved_sign_in_refreshes_first_and_keeps_the_new_token_before_using_it() {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        let fake = Arc::new(Fake::new(move |_| {
            answer(200, &issued(counted.fetch_add(1, Ordering::SeqCst) + 1))
        }));
        let (saved, save) = saving();
        let holder = TokenHolder::saved("refresh-0".into(), endpoints(fake.clone()), save);
        assert_eq!(holder.access_token().unwrap(), "access-1");
        // Fresh, so it's used as it is.
        assert_eq!(holder.access_token().unwrap(), "access-1");
        assert_eq!(*saved.lock().unwrap(), [Some("refresh-1".to_string())]);
        // Turned down, it's replaced with the newest refresh token.
        assert_eq!(
            holder.access_token_replacing("access-1").unwrap(),
            "access-2"
        );
        assert_eq!(sent_refresh_tokens(&fake), ["refresh-0", "refresh-1"]);
    }

    #[test]
    fn a_refresh_token_that_couldnt_be_kept_is_used_and_kept_later() {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        let fake = Arc::new(Fake::new(move |_| {
            answer(200, &issued(counted.fetch_add(1, Ordering::SeqCst) + 1))
        }));
        let saved = Arc::new(Mutex::new(Vec::new()));
        let working = Arc::new(AtomicUsize::new(0));
        let (into, works) = (saved.clone(), working.clone());
        let save: Save = Box::new(move |tokens: Option<&Tokens>| {
            if works.load(Ordering::SeqCst) == 0 {
                return Err("the keyring went away".into());
            }
            into.lock()
                .unwrap()
                .push(tokens.map(|tokens| tokens.refresh_token.clone()));
            Ok(())
        });
        let holder = TokenHolder::saved("refresh-0".into(), endpoints(fake.clone()), save);
        // The old refresh token no longer works, so the new one is used regardless.
        assert_eq!(holder.access_token().unwrap(), "access-1");
        assert!(!holder.is_kept());
        assert_eq!(
            holder.keep_again(),
            Err("the keyring went away".to_string())
        );
        assert!(!holder.is_kept());
        working.store(1, Ordering::SeqCst);
        assert_eq!(holder.keep_again(), Ok(()));
        assert!(holder.is_kept());
        assert_eq!(*saved.lock().unwrap(), [Some("refresh-1".to_string())]);
        // Nothing left to keep.
        assert_eq!(holder.keep_again(), Ok(()));
        assert_eq!(saved.lock().unwrap().len(), 1);

        // Not kept at sign-in, it's kept later the same way, unless a refresh that
        // could be kept has come since.
        let tokens: Tokens = serde_json::from_str(&issued(9)).unwrap();
        let (saved, save) = saving();
        let holder = TokenHolder::signed_in(&tokens, false, endpoints(fake.clone()), save);
        assert!(!holder.is_kept());
        holder.access_token_replacing("access-9").unwrap();
        assert!(holder.is_kept());
        assert_eq!(holder.keep_again(), Ok(()));
        assert_eq!(*saved.lock().unwrap(), [Some("refresh-2".to_string())]);
    }

    #[test]
    fn the_access_token_is_refreshed_a_minute_before_it_runs_out() {
        let fake = Arc::new(Fake::new(|_| answer(200, &issued(2))));
        let (_, save) = saving();
        let tokens: Tokens = serde_json::from_str(&issued(1)).unwrap();
        let mut holder = TokenHolder::signed_in(&tokens, true, endpoints(fake.clone()), save);
        let later = Arc::new(AtomicU64::new(0));
        let start = Instant::now();
        let at = later.clone();
        holder.now = Box::new(move || start + Duration::from_secs(at.load(Ordering::SeqCst)));
        assert_eq!(holder.access_token().unwrap(), "access-1");
        later.store(900 - 61, Ordering::SeqCst);
        assert_eq!(holder.access_token().unwrap(), "access-1");
        later.store(900 - 59, Ordering::SeqCst);
        assert_eq!(holder.access_token().unwrap(), "access-2");
    }

    #[test]
    fn only_one_refresh_runs_and_everyone_waits_for_it() {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        let fake = Arc::new(Fake::new(move |_| {
            std::thread::sleep(Duration::from_millis(50));
            answer(200, &issued(counted.fetch_add(1, Ordering::SeqCst) + 1))
        }));
        let (_, save) = saving();
        let holder = Arc::new(TokenHolder::saved(
            "refresh-0".into(),
            endpoints(fake),
            save,
        ));
        let start = Arc::new(Barrier::new(4));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let holder = holder.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    holder.access_token().unwrap()
                })
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().unwrap(), "access-1");
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refresh_whose_answer_is_lost_is_asked_for_again_straight_away() {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        let fake = Arc::new(Fake::new(move |_| {
            if counted.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(Error::Unreachable("timed out".into()))
            } else {
                answer(200, &issued(1))
            }
        }));
        let (_, save) = saving();
        let mut holder = TokenHolder::saved("refresh-0".into(), endpoints(fake.clone()), save);
        let slept = Arc::new(Mutex::new(Vec::new()));
        let into = slept.clone();
        holder.sleep = Box::new(move |delay| into.lock().unwrap().push(delay));
        assert_eq!(holder.access_token().unwrap(), "access-1");
        // The same refresh token each time, as the server still takes it.
        assert_eq!(sent_refresh_tokens(&fake), ["refresh-0"; 3]);
        assert_eq!(*slept.lock().unwrap(), RETRY_DELAYS[..2]);
    }

    #[test]
    fn a_refresh_is_only_tried_again_while_the_server_would_still_take_it() {
        let fake = Arc::new(Fake::new(|_| Err(Error::Unreachable("timed out".into()))));
        let (_, save) = saving();
        let mut holder = TokenHolder::saved("refresh-0".into(), endpoints(fake.clone()), save);
        // Each try takes 20 seconds to give up.
        let elapsed = Arc::new(AtomicU64::new(0));
        let start = Instant::now();
        let at = elapsed.clone();
        holder.now =
            Box::new(move || start + Duration::from_secs(at.fetch_add(20, Ordering::SeqCst)));
        holder.sleep = Box::new(|_| {});
        assert!(matches!(holder.access_token(), Err(Error::Unreachable(_))));
        assert_eq!(sent_refresh_tokens(&fake).len(), 2);
    }

    #[test]
    fn a_refresh_token_that_no_longer_works_signs_out() {
        let fake = Arc::new(Fake::new(|_| answer(400, r#"{"error": "invalid_grant"}"#)));
        let (saved, save) = saving();
        let holder = TokenHolder::saved("refresh-0".into(), endpoints(fake.clone()), save);
        assert_eq!(holder.access_token(), Err(Error::SignedOut));
        assert_eq!(*saved.lock().unwrap(), [None]);
        // Nothing left to try.
        assert_eq!(holder.access_token(), Err(Error::SignedOut));
        assert_eq!(fake.sent().len(), 1);
    }

    #[test]
    fn signing_out_revokes_the_newest_token_and_forgets_it() {
        let fake = Arc::new(Fake::new(|request| {
            if request.url.ends_with("/oauth/revoke") {
                answer(200, "")
            } else {
                answer(200, &issued(1))
            }
        }));
        let (saved, save) = saving();
        let holder = TokenHolder::saved("refresh-0".into(), endpoints(fake.clone()), save);
        holder.access_token().unwrap();
        holder.sign_out().unwrap();
        assert_eq!(
            *saved.lock().unwrap(),
            [Some("refresh-1".to_string()), None]
        );
        let revoked = fake.sent().last().unwrap().clone();
        assert!(revoked.url.ends_with("/oauth/revoke"));
        assert_eq!(
            revoked.body,
            Body::Form("client_id=maxnines-desktop-linux&token=refresh-1".into())
        );
        assert_eq!(holder.access_token(), Err(Error::SignedOut));
    }
}
