//! Desktop notifications, through GTK's own: the application hands them to GNOME
//! Shell, or to the freedesktop notification service other desktops run, and their
//! buttons come back as the application's actions. Nothing waits for them meanwhile,
//! so one moved to a message list and never closed costs nothing.
//!
//! Each button names the connection the notification was about, so pressing one
//! after signing in elsewhere, or after the app started again, does nothing to the
//! server signed in now.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use maxnines_desktop_kit::Notice;

/// What pressing a notification asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pressed {
    /// Acknowledge the alert, on the connection it fired on.
    Acknowledge { connection: u64, alert: i64 },
    /// Open a page of the web interface of the server it came from.
    Open { connection: u64, path: String },
}

/// The application's actions the buttons name, as `app.` and these.
const ACKNOWLEDGE: &str = "acknowledge";
const OPEN: &str = "open";

impl Pressed {
    /// What an action from a notification asks for, if its target is as they send
    /// it. Targets come over D-Bus from whatever sent them, so anything else is
    /// nothing.
    fn read(action: &str, target: Option<&glib::Variant>) -> Option<Self> {
        let target = target?;
        match action {
            ACKNOWLEDGE => target
                .get::<(u64, i64)>()
                .map(|(connection, alert)| Self::Acknowledge { connection, alert }),
            OPEN => target
                .get::<(u64, String)>()
                .map(|(connection, path)| Self::Open { connection, path }),
            _ => None,
        }
    }

    /// The action it's sent as, and its target.
    fn action(&self) -> (String, glib::Variant) {
        match self {
            Self::Acknowledge { connection, alert } => (
                format!("app.{ACKNOWLEDGE}"),
                (*connection, *alert).to_variant(),
            ),
            Self::Open { connection, path } => (
                format!("app.{OPEN}"),
                (*connection, path.clone()).to_variant(),
            ),
        }
    }
}

/// Adds the actions notifications' buttons name to `app`, which tell `pressed`. They
/// exist from the start, so a notification pressed as it starts the app finds them.
pub fn add_actions(app: &adw::Application, pressed: impl Fn(&adw::Application, Pressed) + 'static) {
    let pressed = Rc::new(pressed);
    for (name, target) in [
        (ACKNOWLEDGE, (0u64, 0i64).to_variant()),
        (OPEN, (0u64, String::new()).to_variant()),
    ] {
        let action = gio::SimpleAction::new(name, Some(target.type_()));
        let app_for_action = app.downgrade();
        let pressed = pressed.clone();
        action.connect_activate(move |_, target| {
            if let Some(app) = app_for_action.upgrade()
                && let Some(asked) = Pressed::read(name, target)
            {
                pressed(&app, asked);
            }
        });
        app.add_action(&action);
    }
}

/// Shows `notice`, about the server on `connection`. Clicking it opens its page, as
/// does its Open in MaxNines button.
pub fn post(app: &adw::Application, notice: &Notice, connection: u64) {
    let notification = gio::Notification::new(&notice.title);
    notification.set_body(Some(&body(&notice.body)));
    notification.set_icon(&gio::ThemedIcon::new(crate::APP_ID));
    let (open, open_target) = Pressed::Open {
        connection,
        path: notice.path.clone(),
    }
    .action();
    notification.set_default_action_and_target_value(&open, Some(&open_target));
    if let Some(alert) = notice.acknowledges {
        let (acknowledge, target) = Pressed::Acknowledge { connection, alert }.action();
        notification.add_button_with_target_value("Acknowledge", &acknowledge, Some(&target));
    }
    notification.add_button_with_target_value("Open in MaxNines", &open, Some(&open_target));
    // Each change has an ID of its own, and the same one replaces rather than repeats.
    app.send_notification(Some(&notice.id), &notification);
}

/// `text` as a notification's body, reading the same whether the notification service
/// takes it as plain text, as GNOME Shell does, or as markup, as the freedesktop
/// services that say so do. Which one gets it is GLib's choice, which it doesn't
/// share, so the two characters that start markup are sent as look-alikes that don't.
fn body(text: &str) -> String {
    text.replace('&', "\u{FF06}").replace('<', "\u{FF1C}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_reads_the_same_as_plain_text_or_markup() {
        let said = body("Load on <db> & \"web\" is > 4");
        assert_eq!(said, "Load on ＜db> ＆ \"web\" is > 4");
        // As markup, it's all text, saying the same.
        let (_, text, _) = gtk::pango::parse_markup(&said, '\0').unwrap();
        assert_eq!(text.as_str(), said);
    }

    #[test]
    fn what_a_button_sends_comes_back_as_it_was() {
        for pressed in [
            Pressed::Acknowledge {
                connection: 7,
                alert: 42,
            },
            Pressed::Open {
                connection: 7,
                path: "/hosts/abc".into(),
            },
        ] {
            let (action, target) = pressed.action();
            let name = action.strip_prefix("app.").unwrap();
            assert_eq!(Pressed::read(name, Some(&target)), Some(pressed));
        }
    }

    #[test]
    fn an_action_that_isnt_as_sent_is_nothing() {
        let acknowledge = (7u64, 42i64).to_variant();
        assert_eq!(Pressed::read(ACKNOWLEDGE, None), None);
        assert_eq!(Pressed::read(OPEN, Some(&acknowledge)), None);
        assert_eq!(Pressed::read("quit", Some(&acknowledge)), None);
        assert_eq!(Pressed::read(ACKNOWLEDGE, Some(&"42".to_variant())), None);
    }
}
