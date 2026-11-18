//! Desktop notifications, through the freedesktop notification service every desktop
//! runs, with Acknowledge and Open in MaxNines as their buttons.

use maxnines_desktop_kit::Notice;
use notify_rust::{Hint, Notification};

use crate::Command;

/// Shows `notice`. Waiting for someone to press one of its buttons blocks, so that
/// happens on a thread of its own, which tells the app through `commands`.
pub fn post(notice: &Notice, commands: &async_channel::Sender<Command>) {
    let mut notification = Notification::new();
    notification
        .appname("MaxNines")
        .summary(&notice.title)
        // Most notification servers read the body as simple markup.
        .body(&escape(&notice.body))
        .icon(crate::APP_ID)
        .hint(Hint::DesktopEntry(crate::APP_ID.into()))
        // Clicking the notification itself.
        .action("default", "Open in MaxNines");
    if notice.acknowledges.is_some() {
        notification.action("acknowledge", "Acknowledge");
    }
    notification.action("open", "Open in MaxNines");
    let commands = commands.clone();
    let alert = notice.acknowledges;
    let path = notice.path.clone();
    std::thread::spawn(move || match notification.show() {
        Ok(handle) => handle.wait_for_action(|action| {
            let command = match (action, alert) {
                ("acknowledge", Some(alert)) => Command::AcknowledgeFromNotice(alert),
                ("default" | "open", _) => Command::Open(path),
                _ => return,
            };
            let _ = commands.send_blocking(command);
        }),
        Err(error) => eprintln!("couldn't show a notification: {error}"),
    });
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
