//! What the app knows, and everything it does: asking the server on a timer, signing
//! in and out, acknowledging and silencing. It all lives on GTK's thread; the slow
//! parts run on other threads and come back here with their answers.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gtk::{gio, glib};
use ksni::blocking::TrayMethods;
use maxnines_desktop_kit::client::AccessTokens;
use maxnines_desktop_kit::http::Ureq;
use maxnines_desktop_kit::models::{MonitoredHost, RemoteCheck};
use maxnines_desktop_kit::signin::{self, SignInEndpoints};
use maxnines_desktop_kit::times::now_ms;
use maxnines_desktop_kit::{
    Alerts, Client, Error, Notice, Overview, ServerAddress, ServerAlert, Silences, TokenHolder,
    Tokens, notices,
};

use crate::settings::Settings;
use crate::tray::{IconState, MenuAlert, Tray};
use crate::window::Ui;
use crate::{Command, notify, secrets};

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
    /// Set while waiting for the person to sign in in the browser; setting what it
    /// holds cancels that.
    pub signing_in: Option<Arc<AtomicBool>>,
    /// Why the last sign-in or sign-out didn't go as it should.
    pub sign_in_problem: Option<String>,
    /// The alerts the server listed last time, which notifications are worked out
    /// against. Nothing until the first answer, so what was already firing isn't
    /// announced.
    last_alerts: Option<Alerts>,
    /// Counts each sign-in, sign-out and change of server, so an answer for the old
    /// one is dropped.
    connection: u64,
    /// Counts each time the server is asked, and which answer was used last, so one
    /// overtaken by a later one is dropped.
    asks: u64,
    answered: u64,
    commands: async_channel::Sender<Command>,
    tray: Option<ksni::blocking::Handle<Tray>>,
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
            return if self.was_signed_out() {
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
}

/// Starts the app: its tray, its window, and asking the server if it's signed in.
pub fn start(app: &adw::Application) -> Shared {
    let (commands, received) = async_channel::unbounded();
    let store = Rc::new(RefCell::new(Store {
        settings: Settings::load(),
        client: None,
        holder: None,
        overview: None,
        updated: None,
        problem: None,
        action_problem: None,
        signing_in: None,
        sign_in_problem: None,
        last_alerts: None,
        connection: 0,
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
    let ui = Ui::build(app, &store);
    store.borrow_mut().ui = Some(ui);

    let listening = store.clone();
    glib::spawn_future_local(async move {
        while let Ok(command) = received.recv().await {
            handle(&listening, command);
        }
    });

    let saved = {
        let state = store.borrow();
        ServerAddress::parse(&state.settings.address)
            .ok()
            .zip(secrets::refresh_token())
    };
    if let Some((server, refresh_token)) = saved {
        connect(&store, server, |endpoints, save| {
            TokenHolder::saved(refresh_token, endpoints, save)
        });
    }
    render(&store);
    store
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
        Command::AcknowledgeFromNotice(alert) => {
            let commands = store.borrow().commands.clone();
            act(
                store,
                move |client| client.acknowledge(alert),
                move |problem| {
                    // Acknowledged from a notification, so a notification says if it
                    // didn't work: the window, where it would otherwise show, may be shut.
                    notify::post(
                        &Notice {
                            id: format!("acknowledge-failed-{alert}"),
                            title: "Couldn't acknowledge the alert".into(),
                            body: problem.to_string(),
                            acknowledges: None,
                            path: "/alerts".into(),
                        },
                        &commands,
                    );
                },
            );
        }
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

fn set_account(settings: &mut Settings, account: Option<(String, String)>) {
    let (user, role) = account.unzip();
    settings.user = user;
    settings.role = role;
    settings.save();
}

pub fn show(store: &Shared) {
    if let Some(ui) = &store.borrow().ui {
        ui.window.present();
    }
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
    let commands = state.commands.clone();
    // Each refresh token is kept as soon as it comes, and who's signed in kept up to
    // date, unless this sign-in has since ended.
    let save: Save = Box::new(move |tokens: Option<&Tokens>| {
        secrets::set_refresh_token(tokens.map(|tokens| tokens.refresh_token.as_str()));
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
            let now = now_ms();
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
                    notify::post(&notice, &state.commands);
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
        {
            let mut state = store.borrow_mut();
            // Cancelled, or started again since, it has nothing to say.
            if !state
                .signing_in
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
            {
                return;
            }
            state.signing_in = None;
        }
        match result {
            Ok(tokens) => {
                // Kept before anything uses it.
                secrets::set_refresh_token(Some(&tokens.refresh_token));
                {
                    let mut state = store.borrow_mut();
                    state.settings.address = typed;
                    set_account(
                        &mut state.settings,
                        Some((tokens.user.clone(), tokens.role.clone())),
                    );
                }
                connect(&store, server, |endpoints, save| {
                    TokenHolder::signed_in(&tokens, endpoints, save)
                });
            }
            Err(Error::Cancelled) => {}
            Err(error) => store.borrow_mut().sign_in_problem = Some(error.to_string()),
        }
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
    let holder = {
        let mut state = store.borrow_mut();
        let holder = state.holder.take();
        disconnect(&mut state);
        set_account(&mut state.settings, None);
        state.sign_in_problem = None;
        holder
    };
    secrets::set_refresh_token(None);
    render(store);
    let Some(holder) = holder else { return };
    let store = store.clone();
    glib::spawn_future_local(async move {
        let told = gio::spawn_blocking(move || holder.sign_out()).await;
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
    act(store, move |client| client.acknowledge(id), |_| {});
}

pub fn silence_for_an_hour(store: &Shared, alert: &ServerAlert) {
    let alert = alert.clone();
    act(store, move |client| client.silence(&alert, 3600), |_| {});
}

/// Does something on the server, then asks it again to show the result. `failed` is
/// told why it didn't work, as well as the window.
fn act(
    store: &Shared,
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
        asked(&store).await;
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
