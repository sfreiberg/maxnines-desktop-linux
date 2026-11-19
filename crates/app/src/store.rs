//! What the app knows, and everything it does: asking the server on a timer, signing
//! in and out, acknowledging and silencing. It all lives on GTK's thread; the slow
//! parts run on other threads and come back here with their answers.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use adw::prelude::*;
use chrono::Utc;
use gtk::{gio, glib};
use ksni::blocking::TrayMethods;
use maxnines_desktop_kit::client::AccessTokens;
use maxnines_desktop_kit::http::Ureq;
use maxnines_desktop_kit::models::{MonitoredHost, RemoteCheck};
use maxnines_desktop_kit::signin::{self, SignInEndpoints};
use maxnines_desktop_kit::{
    Alerts, Client, Error, Notice, Overview, ServerAddress, ServerAlert, Silences, TokenHolder,
    Tokens, notices,
};

use crate::notify::{self, Pressed};
use crate::settings::Settings;
use crate::tray::{IconState, MenuAlert, Tray};
use crate::window::Ui;
use crate::{Command, secrets};

pub type Shared = Rc<RefCell<Store>>;

pub struct Store {
    pub settings: Settings,
    pub client: Option<Client>,
    holder: Option<Arc<TokenHolder>>,
    /// What the server last listed. Kept while a later try fails, as old news that
    /// says it's old beats none.
    pub overview: Option<Overview>,
    pub updated: Option<glib::DateTime>,
    /// Why the server couldn't be asked, if it couldn't.
    pub problem: Option<String>,
    /// Why the last acknowledge or silence didn't work.
    pub action_problem: Option<String>,
    /// The alerts an acknowledge or silence is running for.
    pub busy: BTreeSet<i64>,
    /// Set while waiting for the person to sign in in the browser; setting what it
    /// holds cancels that.
    pub signing_in: Option<Arc<AtomicBool>>,
    /// Why the last sign-in or sign-out didn't go as it should.
    pub sign_in_problem: Option<String>,
    /// Why the keyring couldn't be read at the start, while it's tried again.
    pub keyring_problem: Option<String>,
    /// The alerts the server listed last time, which notifications are worked out
    /// against. Nothing until the first answer, so what was already firing isn't
    /// announced.
    last_alerts: Option<Alerts>,
    /// Counts each sign-in, sign-out and change of server, so an answer for the old
    /// one, or a notification about it, is dropped. It starts from when the app did,
    /// so a notification from a run before isn't taken for one about this.
    connection: u64,
    /// The same, for the threads that keep the refresh token, so one for a connection
    /// that's ended leaves the keyring be.
    current: Arc<AtomicU64>,
    /// Counts each time the server is asked, and which answer was used last, so one
    /// overtaken by a later one is dropped.
    asks: u64,
    answered: u64,
    commands: async_channel::Sender<Command>,
    tray: Option<ksni::blocking::Handle<Tray>>,
    /// The window, while it's open.
    pub ui: Option<Ui>,
    app: adw::Application,
    /// Keeps the app running while its window is closed, when there's a tray to
    /// bring it back from.
    _hold: Option<gio::ApplicationHoldGuard>,
}

impl Store {
    /// Whether the sign-in ran out or was revoked, rather than never having been.
    pub fn was_signed_out(&self) -> bool {
        self.client.is_none() && self.problem.as_deref() == Some(&Error::SignedOut.to_string())
    }

    pub fn headline(&self) -> String {
        if self.client.is_none() {
            return if self.keyring_problem.is_some() {
                "Can't read the keyring".into()
            } else if self.was_signed_out() {
                "Signed out".into()
            } else {
                "Not set up".into()
            };
        }
        match (&self.overview, &self.problem) {
            (Some(overview), None) => overview.headline(),
            (_, Some(_)) => "Can't reach the server".into(),
            (None, None) => "Checking".into(),
        }
    }

    fn icon_state(&self) -> IconState {
        match (&self.overview, &self.problem) {
            (Some(overview), None) => match overview.worst() {
                Some(maxnines_desktop_kit::Severity::Critical) => {
                    IconState::Critical(overview.firing.len())
                }
                Some(maxnines_desktop_kit::Severity::Warning) => {
                    IconState::Warnings(overview.firing.len())
                }
                None => IconState::Quiet,
            },
            _ => IconState::Unknown,
        }
    }

    pub fn has_tray(&self) -> bool {
        self.tray.is_some()
    }

    /// Whether it was signed in when it last ran, so has a sign-in to read from the
    /// keyring, which happens after the start.
    pub fn was_signed_in(&self) -> bool {
        self.settings.user.is_some() && ServerAddress::parse(&self.settings.address).is_ok()
    }
}

/// Starts the app: its tray, and asking the server if it's signed in. The window is
/// built when it's shown.
pub fn start(app: &adw::Application) -> Shared {
    let (commands, received) = async_channel::unbounded();
    // Counted from the clock, so a notification from an earlier run of the app names
    // a connection that's never current again.
    let connection = u64::try_from(Utc::now().timestamp_millis()).unwrap_or_default();
    let store = Rc::new(RefCell::new(Store {
        settings: Settings::load(),
        client: None,
        holder: None,
        overview: None,
        updated: None,
        problem: None,
        action_problem: None,
        busy: BTreeSet::new(),
        signing_in: None,
        sign_in_problem: None,
        keyring_problem: None,
        last_alerts: None,
        connection,
        current: Arc::new(AtomicU64::new(connection)),
        asks: 0,
        answered: 0,
        commands: commands.clone(),
        tray: None,
        ui: None,
        app: app.clone(),
        _hold: None,
    }));
    let tray = Tray {
        headline: "Not set up".into(),
        icons: crate::tray::icons(IconState::Unknown),
        firing: Vec::new(),
        connected: false,
        commands,
    };
    match tray.spawn() {
        Ok(handle) => {
            let mut state = store.borrow_mut();
            state.tray = Some(handle);
            state._hold = Some(app.hold());
        }
        // A GNOME without the AppIndicator extension has nowhere to show it: the app
        // still works, as a window that quits when it's closed.
        Err(error) => {
            eprintln!("no tray to show the icon in ({error}), so closing the window quits")
        }
    }
    notify::learn_how_bodies_are_read();

    let listening = store.clone();
    glib::spawn_future_local(async move {
        while let Ok(command) = received.recv().await {
            handle(&listening, command);
        }
    });

    render(&store);
    let saved = ServerAddress::parse(&store.borrow().settings.address).ok();
    if let Some(server) = saved {
        sign_in_as_saved(&store, server);
    }
    store
}

/// The first wait before reading the keyring again, and the longest, in seconds.
const KEYRING_RETRY_FIRST: u32 = 5;
const KEYRING_RETRY_MOST: u32 = 300;

/// What the window says while the keyring can't be read, to be tried again in
/// `wait` seconds.
fn keyring_problem(error: &str, wait: u32) -> String {
    let when = match wait {
        60 => "a minute".to_string(),
        wait if wait % 60 == 0 => format!("{} minutes", wait / 60),
        wait => format!("{wait} seconds"),
    };
    format!(
        "Can't read the keyring yet, so the app can't sign in. It tries again in {when}. \
         ({error})"
    )
}

/// Signs in to `server` as the refresh token saved in the keyring says, if there is
/// one. A keyring that can't be read yet, as at login before its service has started
/// or while it's locked, is tried again, waiting longer each time, rather than taken
/// for having nothing in it.
fn sign_in_as_saved(store: &Shared, server: ServerAddress) {
    let store = store.clone();
    glib::spawn_future_local(async move {
        let started = store.borrow().connection;
        let mut wait = KEYRING_RETRY_FIRST;
        loop {
            let read = gio::spawn_blocking(secrets::refresh_token)
                .await
                .unwrap_or_else(|_| Err("reading it stopped".into()));
            let saved = {
                let mut state = store.borrow_mut();
                match read {
                    // Signed in meanwhile, what was saved is old news.
                    _ if state.connection != started => {
                        state.keyring_problem = None;
                        Ok(None)
                    }
                    Ok(saved) => {
                        state.keyring_problem = None;
                        Ok(saved)
                    }
                    Err(error) => {
                        eprintln!("couldn't read the keyring, trying again in {wait} s: {error}");
                        state.keyring_problem = Some(keyring_problem(&error, wait));
                        Err(())
                    }
                }
            };
            match saved {
                Ok(saved) => {
                    if let Some(refresh_token) = saved {
                        connect(&store, server, |endpoints, save| {
                            TokenHolder::saved(refresh_token, endpoints, save)
                        });
                    }
                    render(&store);
                    return;
                }
                Err(()) => {
                    render(&store);
                    glib::timeout_future_seconds(wait).await;
                    wait = (wait * 2).min(KEYRING_RETRY_MOST);
                }
            }
        }
    });
}

fn handle(store: &Shared, command: Command) {
    match command {
        Command::Show => show(store),
        Command::Refresh => refresh(store),
        Command::Open(path) => {
            let url = store
                .borrow()
                .client
                .as_ref()
                .map(|client| client.server.page(&path));
            if let Some(url) = url {
                open_url(&url);
            }
        }
        Command::OpenUrl(url) => open_url(&url),
        Command::SignedInAs(connection, account) => {
            let mut state = store.borrow_mut();
            if state.connection != connection {
                return;
            }
            set_account(&mut state.settings, account);
            drop(state);
            render(store);
        }
        Command::Quit => {
            let app = store.borrow().app.clone();
            app.quit();
        }
    }
}

/// A notification's button, or the notification itself, was pressed. One about a
/// connection that's since ended shows the window instead, which says what's signed
/// in now.
pub fn pressed(store: &Shared, pressed: Pressed) {
    let current = store.borrow().connection;
    match pressed {
        Pressed::Acknowledge { connection, alert } if connection == current => {
            let app = store.borrow().app.clone();
            act(
                store,
                alert,
                move |client| client.acknowledge(alert),
                move |problem| {
                    // Acknowledged from a notification, so a notification says if it
                    // didn't work: the window, where it would otherwise show, may be shut.
                    let notice = Notice {
                        id: format!("acknowledge-failed-{alert}"),
                        title: "Couldn't acknowledge the alert".into(),
                        body: problem.to_string(),
                        acknowledges: None,
                        path: "/alerts".into(),
                    };
                    notify::post(&app, &notice, current);
                },
            );
        }
        Pressed::Open { connection, path } if connection == current => {
            handle(store, Command::Open(path));
        }
        _ => show(store),
    }
}

fn set_account(settings: &mut Settings, account: Option<(String, String)>) {
    let (user, role) = account.unzip();
    settings.user = user;
    settings.role = role;
    settings.save();
}

/// Shows the window, building it if it isn't open, as at the start or after it was
/// closed.
pub fn show(store: &Shared) {
    let open = store.borrow().ui.as_ref().map(|ui| ui.window.clone());
    let window = match open {
        Some(window) => window,
        None => {
            let app = store.borrow().app.clone();
            let ui = Ui::build(&app, store);
            let window = ui.window.clone();
            store.borrow_mut().ui = Some(ui);
            render(store);
            window
        }
    };
    window.present();
}

/// Opens an address in the person's browser.
pub fn open_url(url: &str) {
    if let Err(error) = gio::AppInfo::launch_default_for_uri(url, None::<&gio::AppLaunchContext>) {
        eprintln!("couldn't open {url}: {error}");
    }
}

/// Shows what the app knows now, in the window and the tray.
pub fn render(store: &Shared) {
    let state = store.borrow();
    if let Some(ui) = &state.ui {
        ui.render(&state, store);
    }
    if let Some(tray) = &state.tray {
        let headline = state.headline();
        let icons = crate::tray::icons(state.icon_state());
        let connected = state.client.is_some();
        let firing: Vec<MenuAlert> = state
            .overview
            .as_ref()
            .map(|overview| {
                overview
                    .firing
                    .iter()
                    .map(|alert| MenuAlert {
                        label: format!(
                            "{} {}",
                            if alert.severity == maxnines_desktop_kit::Severity::Critical {
                                "🔴"
                            } else {
                                "🟡"
                            },
                            alert.summary
                        ),
                        path: alert.page(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        tray.update(move |tray| {
            tray.headline = headline;
            tray.icons = icons;
            tray.firing = firing;
            tray.connected = connected;
        });
    }
}

/// Stops asking the server, and forgets what it said.
fn disconnect(state: &mut Store) {
    state.problem = None;
    state.overview = None;
    state.updated = None;
    // Another server's alerts aren't news about this one's.
    state.last_alerts = None;
    state.connection += 1;
    state.current.store(state.connection, Ordering::SeqCst);
    state.client = None;
    state.holder = None;
}

type Save = Box<dyn Fn(Option<&Tokens>) + Send + Sync>;

/// Starts asking `server`, with tokens from `holder`.
fn connect(
    store: &Shared,
    server: ServerAddress,
    holder: impl FnOnce(SignInEndpoints, Save) -> TokenHolder,
) {
    let mut state = store.borrow_mut();
    disconnect(&mut state);
    let connection = state.connection;
    let current = state.current.clone();
    let commands = state.commands.clone();
    // Each refresh token is kept as soon as it comes, and who's signed in kept up to
    // date, unless this sign-in has since ended. This runs on the thread that
    // refreshed, before the new token is used.
    let save: Save = Box::new(move |tokens: Option<&Tokens>| {
        let token = tokens.map(|tokens| tokens.refresh_token.as_str());
        if let Err(error) = secrets::set_refresh_token(token, connection, &current) {
            eprintln!("couldn't update the keyring: {error}");
        }
        let account = tokens.map(|tokens| (tokens.user.clone(), tokens.role.clone()));
        let _ = commands.send_blocking(Command::SignedInAs(connection, account));
    });
    let http = Arc::new(Ureq::default());
    let holder = Arc::new(holder(
        SignInEndpoints::new(server.clone(), http.clone()),
        save,
    ));
    state.client = Some(Client::new(
        server,
        holder.clone() as Arc<dyn AccessTokens>,
        http,
    ));
    state.holder = Some(holder);
    drop(state);

    let polling = store.clone();
    glib::spawn_future_local(async move {
        loop {
            if polling.borrow().connection != connection {
                break;
            }
            asked(&polling).await;
            let seconds = polling.borrow().settings.interval_secs;
            glib::timeout_future_seconds(seconds).await;
        }
    });
}

/// Asks the server now, as well as on the timer.
pub fn refresh(store: &Shared) {
    let store = store.clone();
    glib::spawn_future_local(async move { asked(&store).await });
}

/// Everything the window shows, from one round of asking.
struct Answers {
    alerts: Alerts,
    hosts: Vec<MonitoredHost>,
    checks: Option<Vec<RemoteCheck>>,
    silences: Option<Silences>,
}

fn ask(client: &Client) -> Result<Answers, Error> {
    let alerts = client.alerts()?;
    let hosts = client.hosts()?;
    // Checks and silences are extras: a server too old to list them, or a sign-in not
    // allowed to, still shows its alerts.
    Ok(Answers {
        alerts,
        hosts,
        checks: client.remote_checks().ok(),
        silences: client.silences().ok(),
    })
}

async fn asked(store: &Shared) {
    let (client, connection, ask_number) = {
        let mut state = store.borrow_mut();
        let Some(client) = state.client.clone() else {
            return;
        };
        state.asks += 1;
        (client, state.connection, state.asks)
    };
    let Ok(result) = gio::spawn_blocking(move || ask(&client)).await else {
        return;
    };
    let mut state = store.borrow_mut();
    // Overtaken by a later answer, or for a sign-in that's since ended, it says
    // nothing about the server now.
    if state.connection != connection || ask_number <= state.answered {
        return;
    }
    state.answered = ask_number;
    match result {
        Ok(answers) => {
            let now = Utc::now();
            state.overview = Some(Overview::new(
                &answers.alerts,
                &answers.hosts,
                answers.checks.as_deref(),
                &answers
                    .silences
                    .map(|silences| silences.active)
                    .unwrap_or_default(),
                now,
            ));
            state.updated = glib::DateTime::now_local().ok();
            state.problem = None;
            if let Some(last) = &state.last_alerts {
                for notice in notices(last, &answers.alerts, state.settings.notify_when(), now) {
                    notify::post(&state.app, &notice, connection);
                }
            }
            state.last_alerts = Some(answers.alerts);
        }
        Err(Error::SignedOut) => signed_out(&mut state),
        Err(error) => state.problem = Some(error.to_string()),
    }
    drop(state);
    render(store);
}

/// The sign-in ran out or was revoked: stop asking, and say to sign in again.
fn signed_out(state: &mut Store) {
    disconnect(state);
    set_account(&mut state.settings, None);
    state.problem = Some(Error::SignedOut.to_string());
}

/// Opens the server's sign-in page in the browser and waits for the person to approve
/// the app there.
pub fn sign_in(store: &Shared, typed: &str) {
    let server = match ServerAddress::parse(typed) {
        Ok(server) => server,
        Err(error) => {
            store.borrow_mut().sign_in_problem = Some(error.to_string());
            render(store);
            return;
        }
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let commands = {
        let mut state = store.borrow_mut();
        if let Some(earlier) = state.signing_in.replace(cancelled.clone()) {
            earlier.store(true, Ordering::Relaxed);
        }
        state.sign_in_problem = None;
        state.commands.clone()
    };
    render(store);
    let typed = typed.trim().to_string();
    let store = store.clone();
    glib::spawn_future_local(async move {
        let device = gethostname::gethostname().to_string_lossy().into_owned();
        let endpoints = SignInEndpoints::new(server.clone(), Arc::new(Ureq::default()));
        let waiting = cancelled.clone();
        let result = gio::spawn_blocking(move || {
            let open = |url: &str| {
                let _ = commands.send_blocking(Command::OpenUrl(url.to_string()));
            };
            signin::through_browser(&endpoints, &device, &open, &waiting, signin::BROWSER_WAIT)
        })
        .await
        .unwrap_or(Err(Error::Cancelled));
        // Cancelled, or started again since, it has nothing to say.
        let is_ours = |state: &Store| {
            state
                .signing_in
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
        };
        if !is_ours(&store.borrow()) {
            return;
        }
        let tokens = match result {
            Ok(tokens) => tokens,
            Err(error) => {
                let mut state = store.borrow_mut();
                state.signing_in = None;
                if error != Error::Cancelled {
                    state.sign_in_problem = Some(error.to_string());
                }
                drop(state);
                render(&store);
                return;
            }
        };
        // Whatever was signed in before ends first, so nothing it does can touch the
        // keyring once this sign-in's token is kept there, and that's kept before
        // anything uses it. It's still signing in meanwhile, which lasts while the
        // keyring asks to be unlocked.
        let (connection, current) = {
            let mut state = store.borrow_mut();
            disconnect(&mut state);
            (state.connection, state.current.clone())
        };
        let token = tokens.refresh_token.clone();
        let keeping = current.clone();
        let kept = gio::spawn_blocking(move || {
            secrets::set_refresh_token(Some(&token), connection, &keeping)
        })
        .await
        .unwrap_or_else(|_| Err("keeping it stopped".into()));
        if !is_ours(&store.borrow()) {
            // Cancelled while it was kept, so it's forgotten again, unless a sign-in
            // since has moved on and kept its own.
            let _ =
                gio::spawn_blocking(move || secrets::set_refresh_token(None, connection, &current))
                    .await;
            return;
        }
        let mut state = store.borrow_mut();
        state.signing_in = None;
        state.sign_in_problem = kept.err().map(|error| {
            format!(
                "Signed in, but the keyring couldn't keep the sign-in, so it lasts only until \
                 the app quits ({error})."
            )
        });
        state.settings.address = typed;
        set_account(
            &mut state.settings,
            Some((tokens.user.clone(), tokens.role.clone())),
        );
        drop(state);
        connect(&store, server, |endpoints, save| {
            TokenHolder::signed_in(&tokens, endpoints, save)
        });
        render(&store);
    });
}

pub fn cancel_sign_in(store: &Shared) {
    if let Some(cancelled) = store.borrow_mut().signing_in.take() {
        cancelled.store(true, Ordering::Relaxed);
    }
    render(store);
}

/// Signs out here and on the server. If the server can't be told, it's still signed
/// out here, and the person is told to revoke it on the server.
pub fn sign_out(store: &Shared) {
    let (holder, connection, current) = {
        let mut state = store.borrow_mut();
        let holder = state.holder.take();
        disconnect(&mut state);
        set_account(&mut state.settings, None);
        state.sign_in_problem = None;
        (holder, state.connection, state.current.clone())
    };
    render(store);
    let store = store.clone();
    glib::spawn_future_local(async move {
        // Forgotten here first, then the server is told. A sign-in since, which has
        // moved the connection on, keeps its own token.
        let told = gio::spawn_blocking(move || {
            if let Err(error) = secrets::set_refresh_token(None, connection, &current) {
                eprintln!("couldn't update the keyring: {error}");
            }
            holder.map_or(Ok(()), |holder| holder.sign_out())
        })
        .await;
        if !matches!(told, Ok(Ok(()))) {
            store.borrow_mut().sign_in_problem = Some(
                "Signed out here, but the server couldn't be told. Revoke it in MaxNines under \
                 Settings, then Signed-in apps."
                    .into(),
            );
            render(&store);
        }
    });
}

pub fn acknowledge(store: &Shared, alert: &ServerAlert) {
    let id = alert.id;
    act(store, id, move |client| client.acknowledge(id), |_| {});
}

pub fn silence_for_an_hour(store: &Shared, alert: &ServerAlert) {
    let alert = alert.clone();
    act(
        store,
        alert.id,
        move |client| client.silence(&alert, 3600),
        |_| {},
    );
}

/// Does something to `alert` on the server, then asks it again to show the result.
/// `failed` is told why it didn't work, as well as the window. Only one runs for an
/// alert at a time, and its buttons are off meanwhile.
fn act(
    store: &Shared,
    alert: i64,
    action: impl FnOnce(&Client) -> Result<(), Error> + Send + 'static,
    failed: impl FnOnce(&str) + 'static,
) {
    let (client, connection) = {
        let state = store.borrow();
        (state.client.clone(), state.connection)
    };
    let Some(client) = client else {
        failed("MaxNines isn't set up");
        return;
    };
    // Pressed again, or from its notification too, while the first still runs.
    if !store.borrow_mut().busy.insert(alert) {
        return;
    }
    render(store);
    let store = store.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || action(&client))
            .await
            .unwrap_or(Err(Error::Cancelled));
        {
            let mut state = store.borrow_mut();
            match &result {
                Ok(()) => state.action_problem = None,
                Err(error) => {
                    state.action_problem = Some(error.to_string());
                    failed(&error.to_string());
                    if *error == Error::SignedOut && state.connection == connection {
                        signed_out(&mut state);
                    }
                }
            }
        }
        render(&store);
        // Its buttons stay off until the server says how it is now.
        asked(&store).await;
        store.borrow_mut().busy.remove(&alert);
        render(&store);
    });
}

/// Changes how often the server is asked, from the next time.
pub fn set_interval(store: &Shared, seconds: u32) {
    let mut state = store.borrow_mut();
    state.settings.interval_secs = seconds;
    state.settings.save();
}

/// Changes which notifications are wanted.
pub fn set_notify(store: &Shared, change: impl FnOnce(&mut Settings)) {
    let mut state = store.borrow_mut();
    change(&mut state.settings);
    state.settings.save();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keyring_that_cant_be_read_says_when_its_tried_again() {
        let said = |wait| keyring_problem("locked", wait);
        assert_eq!(
            said(KEYRING_RETRY_FIRST),
            "Can't read the keyring yet, so the app can't sign in. It tries again in 5 \
             seconds. (locked)"
        );
        assert!(said(60).contains("in a minute."));
        assert!(said(KEYRING_RETRY_MOST).contains("in 5 minutes."));
        assert!(said(160).contains("in 160 seconds."));
    }
}
