//! The window: what's firing, what resolved lately and the silences running, on one
//! page, and the settings on another.

use adw::prelude::*;
use gtk::glib;
use maxnines_desktop_kit::times::{age, now_ms, time_left};
use maxnines_desktop_kit::{ServerAddress, ServerAlert, Severity};

use crate::settings::{INTERVALS, Settings, autostart};
use crate::store::{self, Shared, Store};

pub struct Ui {
    pub window: adw::ApplicationWindow,
    stack: adw::ViewStack,
    /// The alerts page's contents, rebuilt each time there's news.
    alerts: gtk::Box,
    address: gtk::Entry,
    account: adw::ActionRow,
    sign_in: gtk::Button,
    cancel: gtk::Button,
    sign_out: gtk::Button,
    sign_in_problem: gtk::Label,
}

impl Ui {
    pub fn build(app: &adw::Application, store: &Shared) -> Self {
        let stack = adw::ViewStack::new();

        let alerts = gtk::Box::new(gtk::Orientation::Vertical, 18);
        alerts.set_margin_top(18);
        alerts.set_margin_bottom(18);
        alerts.set_margin_start(12);
        alerts.set_margin_end(12);
        let clamp = adw::Clamp::builder()
            .maximum_size(640)
            .child(&alerts)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();
        stack
            .add_titled(&scrolled, Some("alerts"), "Alerts")
            .set_icon_name(Some("dialog-warning-symbolic"));

        let settings = adw::PreferencesPage::new();
        let state = store.borrow();

        // The server, and who's signed in to it.
        let server = adw::PreferencesGroup::builder()
            .title("Server")
            .description("The address you open MaxNines at.")
            .build();
        let address = gtk::Entry::builder()
            .text(&state.settings.address)
            .placeholder_text("https://monitor.example.com")
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let address_row = adw::ActionRow::builder().title("Address").build();
        address_row.add_suffix(&address);
        server.add(&address_row);
        let account = adw::ActionRow::builder().title("Not signed in").build();
        let sign_in = gtk::Button::builder()
            .label("Sign in with browser")
            .valign(gtk::Align::Center)
            .css_classes(["suggested-action"])
            .build();
        let cancel = gtk::Button::builder()
            .label("Cancel")
            .valign(gtk::Align::Center)
            .build();
        let sign_out = gtk::Button::builder()
            .label("Sign out")
            .valign(gtk::Align::Center)
            .build();
        account.add_suffix(&sign_in);
        account.add_suffix(&cancel);
        account.add_suffix(&sign_out);
        server.add(&account);
        // Worded as the Mac app words it. Shown for the address typed while signing
        // in, and the one signed in to after.
        let in_the_clear = gtk::Label::builder()
            .label(
                "Over plain http the sign in crosses the network unencrypted, so only use \
                 it on a network you trust.",
            )
            .wrap(true)
            .xalign(0.0)
            .css_classes(["warning"])
            .margin_top(6)
            .visible(is_in_the_clear(&state.settings.address))
            .build();
        server.add(&in_the_clear);
        let sign_in_problem = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .css_classes(["error"])
            .margin_top(6)
            .visible(false)
            .build();
        server.add(&sign_in_problem);
        settings.add(&server);

        // How often to ask.
        let checking = adw::PreferencesGroup::builder().title("Checking").build();
        let labels: Vec<String> = INTERVALS
            .iter()
            .map(|secs| match secs {
                60 => "Every minute".to_string(),
                secs => format!("Every {secs} seconds"),
            })
            .collect();
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let interval = adw::ComboRow::builder()
            .title("Ask the server")
            .model(&gtk::StringList::new(&label_refs))
            .selected(
                INTERVALS
                    .iter()
                    .position(|secs| *secs == state.settings.interval_secs)
                    .and_then(|at| u32::try_from(at).ok())
                    .unwrap_or(0),
            )
            .build();
        checking.add(&interval);
        settings.add(&checking);

        // Which notifications.
        let notifications = adw::PreferencesGroup::builder()
            .title("Notifications")
            .description(
                "Alerts held back by a silence or maintenance window never notify, as \
                 everywhere else in MaxNines.",
            )
            .build();
        let critical = switch_row(
            &notifications,
            "When a critical alert starts",
            state.settings.notify_critical,
        );
        let warnings = switch_row(
            &notifications,
            "When a warning starts",
            state.settings.notify_warnings,
        );
        let resolved = switch_row(
            &notifications,
            "When an alert resolves",
            state.settings.notify_resolved,
        );
        settings.add(&notifications);

        // Starting at sign-in.
        let startup = adw::PreferencesGroup::new();
        let at_login = switch_row(&startup, "Open at login", autostart::is_on());
        settings.add(&startup);

        let about = adw::PreferencesGroup::new();
        let version = gtk::Label::builder()
            .label(format!("MaxNines Desktop {}", env!("CARGO_PKG_VERSION")))
            .css_classes(["dim-label", "caption"])
            .build();
        about.add(&version);
        settings.add(&about);

        stack
            .add_titled(&settings, Some("settings"), "Settings")
            .set_icon_name(Some("emblem-system-symbolic"));
        drop(state);

        let header = adw::HeaderBar::builder()
            .title_widget(
                &adw::ViewSwitcher::builder()
                    .stack(&stack)
                    .policy(adw::ViewSwitcherPolicy::Wide)
                    .build(),
            )
            .build();
        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.set_tooltip_text(Some("Refresh"));
        header.pack_start(&refresh);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&stack);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("MaxNines Desktop")
            .default_width(460)
            .default_height(680)
            .content(&content)
            .build();

        {
            let store = store.clone();
            window.connect_close_request(move |window| {
                // With a tray to come back from, closing only hides it.
                if store.borrow().has_tray() {
                    window.set_visible(false);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        }
        {
            let store = store.clone();
            refresh.connect_clicked(move |_| store::refresh(&store));
        }
        {
            let store = store.clone();
            let address = address.clone();
            sign_in.connect_clicked(move |_| store::sign_in(&store, &address.text()));
        }
        {
            let store = store.clone();
            let address = address.clone();
            address.connect_activate(move |entry| {
                if store.borrow().client.is_none() && store.borrow().signing_in.is_none() {
                    store::sign_in(&store, &entry.text());
                }
            });
        }
        // Nothing but the warning listens, so this is safe while the store is
        // borrowed, as it is when a render sets the address.
        address.connect_changed(move |entry| {
            in_the_clear.set_visible(is_in_the_clear(&entry.text()));
        });
        {
            let store = store.clone();
            cancel.connect_clicked(move |_| store::cancel_sign_in(&store));
        }
        {
            let store = store.clone();
            sign_out.connect_clicked(move |_| store::sign_out(&store));
        }
        {
            let store = store.clone();
            interval.connect_selected_notify(move |row| {
                if let Some(&secs) = usize::try_from(row.selected())
                    .ok()
                    .and_then(|at| INTERVALS.get(at))
                {
                    store::set_interval(&store, secs);
                }
            });
        }
        connect_notify_switch(&critical, store, |settings, on| {
            settings.notify_critical = on
        });
        connect_notify_switch(&warnings, store, |settings, on| {
            settings.notify_warnings = on
        });
        connect_notify_switch(&resolved, store, |settings, on| {
            settings.notify_resolved = on
        });
        at_login.connect_state_set(move |switch, on| {
            if let Err(error) = autostart::set(on) {
                eprintln!("couldn't change opening at login: {error}");
                return glib::Propagation::Stop;
            }
            switch.set_state(on);
            glib::Propagation::Stop
        });

        Self {
            window,
            stack,
            alerts,
            address,
            account,
            sign_in,
            cancel,
            sign_out,
            sign_in_problem,
        }
    }

    /// Shows what the app knows now. Only sets what nothing listens to, as the store
    /// is borrowed while this runs.
    pub fn render(&self, state: &Store, store: &Shared) {
        // Settings: who's signed in, and what can be done about it.
        let signing_in = state.signing_in.is_some();
        let signed_in = state.client.is_some();
        match (&state.settings.user, &state.settings.role) {
            (Some(user), Some(role)) if signed_in => {
                self.account.set_title(&glib::markup_escape_text(user));
                self.account
                    .set_subtitle(&glib::markup_escape_text(&format!("Signed in as {role}")));
            }
            _ if signing_in => {
                self.account.set_title("Waiting for the browser");
                self.account
                    .set_subtitle("Allow MaxNines Desktop on the page that opened");
            }
            _ => {
                self.account.set_title(if state.was_signed_out() {
                    "Signed out"
                } else {
                    "Not signed in"
                });
                self.account.set_subtitle("");
            }
        }
        self.sign_in.set_visible(!signed_in && !signing_in);
        self.cancel.set_visible(signing_in);
        self.sign_out.set_visible(signed_in);
        self.address.set_sensitive(!signed_in && !signing_in);
        if signed_in && self.address.text() != state.settings.address {
            self.address.set_text(&state.settings.address);
        }
        self.sign_in_problem
            .set_label(state.sign_in_problem.as_deref().unwrap_or(""));
        self.sign_in_problem
            .set_visible(state.sign_in_problem.is_some());

        while let Some(child) = self.alerts.first_child() {
            self.alerts.remove(&child);
        }
        if !signed_in {
            let status = adw::StatusPage::builder()
                .icon_name("network-server-symbolic")
                .title(state.headline())
                .description(glib::markup_escape_text(
                    state
                        .keyring_problem
                        .as_deref()
                        .unwrap_or("Sign in to your MaxNines server to see its alerts here."),
                ))
                .vexpand(true)
                .build();
            let open = gtk::Button::builder()
                .label("Open Settings")
                .halign(gtk::Align::Center)
                .css_classes(["pill", "suggested-action"])
                .build();
            let stack = self.stack.clone();
            open.connect_clicked(move |_| stack.set_visible_child_name("settings"));
            status.set_child(Some(&open));
            self.alerts.append(&status);
            return;
        }
        self.render_alerts(state, store);
    }

    fn render_alerts(&self, state: &Store, store: &Shared) {
        let now = now_ms();
        let summary = adw::PreferencesGroup::new();
        let headline = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&state.headline()))
            .css_classes(["property"])
            .build();
        let detail = match (&state.problem, &state.updated) {
            (Some(problem), _) => {
                let mut said = problem.clone();
                if let Some(updated) = &state.updated
                    && let Ok(time) = updated.format("%H:%M:%S")
                {
                    said.push_str(&format!(". Showing what it said at {time}"));
                }
                said
            }
            (None, Some(updated)) => updated
                .format("Updated %H:%M:%S")
                .map(|text| text.to_string())
                .unwrap_or_default(),
            (None, None) => "Asking the server".into(),
        };
        headline.set_subtitle(&glib::markup_escape_text(&detail));
        summary.add(&headline);
        if let Some(overview) = &state.overview {
            summary.add(&info_row(
                "Hosts online",
                &format!("{} of {}", overview.hosts_online, overview.hosts),
            ));
            if let Some(checks) = overview.checks {
                let mut said = format!("{} of {} passing", checks.passing, checks.enabled);
                if checks.failing() > 0 {
                    said.push_str(&format!(", {} failing", checks.failing()));
                }
                summary.add(&info_row("Checks", &said));
            }
        }
        self.alerts.append(&summary);

        if let Some(problem) = &state.action_problem {
            let label = gtk::Label::builder()
                .label(problem)
                .wrap(true)
                .xalign(0.0)
                .css_classes(["error"])
                .build();
            self.alerts.append(&label);
        }

        let Some(overview) = &state.overview else {
            return;
        };
        if !overview.firing.is_empty() {
            let group = adw::PreferencesGroup::builder().title("Firing").build();
            for alert in &overview.firing {
                group.add(&firing_row(alert, now, store));
            }
            self.alerts.append(&group);
        }
        if !overview.resolved.is_empty() {
            let group = adw::PreferencesGroup::builder()
                .title("Resolved in the last hour")
                .build();
            for alert in &overview.resolved {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(alert.says_now()))
                    .subtitle(glib::markup_escape_text(&format!(
                        "{} · {} ago",
                        alert.rule_name,
                        age(alert.resolved_ms.unwrap_or(alert.started_ms), now)
                    )))
                    .activatable(true)
                    .build();
                row.add_prefix(&mark("✓", "success", "Resolved"));
                connect_open(&row, alert, store);
                group.add(&row);
            }
            self.alerts.append(&group);
        }
        if !overview.silences.is_empty() {
            let group = adw::PreferencesGroup::builder()
                .title("Silences running")
                .build();
            for silence in &overview.silences {
                let mut detail = format!("ends in {}", time_left(silence.ends_ms, now));
                if !silence.reason.is_empty() {
                    detail = format!("{} · {detail}", silence.reason);
                }
                group.add(
                    &adw::ActionRow::builder()
                        .title(glib::markup_escape_text(&silence.covers))
                        .subtitle(glib::markup_escape_text(&detail))
                        .build(),
                );
            }
            self.alerts.append(&group);
        }
    }
}

/// Whether `typed` is a server the sign-in would reach unencrypted, over plain http to
/// somewhere other than this computer.
fn is_in_the_clear(typed: &str) -> bool {
    ServerAddress::parse(typed).is_ok_and(|server| server.is_in_the_clear())
}

/// Saves which notifications are wanted as a switch is flipped.
fn connect_notify_switch(switch: &gtk::Switch, store: &Shared, change: fn(&mut Settings, bool)) {
    let store = store.clone();
    switch.connect_active_notify(move |switch| {
        let on = switch.is_active();
        store::set_notify(&store, |settings| change(settings, on));
    });
}

/// A row with a switch at its end, which clicking anywhere on the row flips.
fn switch_row(group: &adw::PreferencesGroup, title: &str, on: bool) -> gtk::Switch {
    let switch = gtk::Switch::builder()
        .active(on)
        .valign(gtk::Align::Center)
        .build();
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(&switch);
    row.set_activatable_widget(Some(&switch));
    group.add(&row);
    switch
}

fn info_row(title: &str, value: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(
        &gtk::Label::builder()
            .label(value)
            .css_classes(["dim-label"])
            .build(),
    );
    row
}

/// A coloured mark at the start of a row, with what it means.
fn mark(text: &str, class: &str, name: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .css_classes([class])
        .valign(gtk::Align::Center)
        .build();
    // Naming it for screen readers takes GTK 4.10; a tooltip works from 4.6, and
    // says what the colour means to anyone.
    label.set_tooltip_text(Some(name));
    label
}

/// Clicking the row opens the alert's host in the browser.
fn connect_open(row: &adw::ActionRow, alert: &ServerAlert, store: &Shared) {
    let store = store.clone();
    let path = alert.page();
    row.connect_activated(move |_| {
        let url = store
            .borrow()
            .client
            .as_ref()
            .map(|client| client.server.page(&path));
        if let Some(url) = url {
            store::open_url(&url);
        }
    });
}

fn firing_row(alert: &ServerAlert, now: i64, store: &Shared) -> adw::ActionRow {
    let mut detail = format!("{} · {} ago", alert.rule_name, age(alert.started_ms, now));
    if let Some(who) = &alert.acknowledged_by {
        detail.push_str(&format!(" · acknowledged by {who}"));
    }
    if alert.is_silenced() {
        detail.push_str(" · silenced");
    }
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&alert.summary))
        .subtitle(glib::markup_escape_text(&detail))
        .activatable(true)
        .build();
    row.add_prefix(&match alert.severity {
        Severity::Critical => mark("●", "error", "Critical"),
        Severity::Warning => mark("■", "warning", "Warning"),
    });
    connect_open(&row, alert, store);
    // Offered to every role, as in the other apps: the server says why a viewer
    // can't.
    if !alert.is_acknowledged() {
        let button = gtk::Button::builder()
            .label("Acknowledge")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let store = store.clone();
        let alert = alert.clone();
        button.connect_clicked(move |_| store::acknowledge(&store, &alert));
        row.add_suffix(&button);
    }
    if alert.can_be_silenced() && !alert.is_silenced() {
        let button = gtk::Button::builder()
            .label("Silence 1 hour")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let store = store.clone();
        let alert = alert.clone();
        button.connect_clicked(move |_| store::silence_for_an_hour(&store, &alert));
        row.add_suffix(&button);
    }
    row
}
