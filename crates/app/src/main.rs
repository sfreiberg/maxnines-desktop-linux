//! MaxNines Desktop for Linux: a MaxNines server's alerts in the tray, with
//! notifications as they start and resolve, and a window with the rest. It's the Linux
//! version of the Mac menu bar app and the Windows notification area app, and does the
//! same things.

mod notify;
mod secrets;
mod settings;
mod store;
mod tray;
mod window;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

/// The app's ID, for the desktop: its desktop entry, its icon and its notifications.
pub const APP_ID: &str = "com.maxnines.Desktop";

/// What the tray and threads ask the app to do. They run outside GTK's thread, so
/// they send it here rather than touch the window.
#[derive(Debug, Clone)]
pub enum Command {
    /// Show the window.
    Show,
    /// Ask the server now.
    Refresh,
    /// Open a page of the web interface, such as `/alerts`.
    Open(String),
    /// Open a whole address, such as the sign-in page.
    OpenUrl(String),
    /// A refresh brought new tokens, or the sign-in ended: who's signed in now, for
    /// the connection it happened on.
    SignedInAs(u64, Option<(String, String)>),
    Quit,
}

fn main() -> glib::ExitCode {
    // A window of lists and buttons doesn't need the GPU, and GTK's GL and Vulkan
    // renderers hold on to a lot of memory for it, hundreds of megabytes on a virtual
    // machine's software GL. So it draws with cairo, unless the person has chosen.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: this is the start of main, before GTK starts or any thread is
        // spawned, so nothing else can be reading the environment as it changes.
        unsafe { std::env::set_var("GSK_RENDERER", "cairo") };
    }
    // Says what it is without starting, as the package tests use to check it runs.
    if std::env::args().any(|argument| argument == "--version") {
        println!("maxnines-desktop {}", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }
    // Started at sign-in, it waits in the tray rather than opening its window.
    let background = std::env::args().any(|argument| argument == "--background");
    glib::set_application_name("MaxNines Desktop");
    let app = adw::Application::builder().application_id(APP_ID).build();
    let shared: Rc<RefCell<Option<store::Shared>>> = Rc::default();
    {
        let shared = shared.clone();
        notify::add_actions(&app, move |app, pressed| {
            let store = shared.borrow().clone();
            match store {
                Some(store) => store::pressed(&store, pressed),
                // Pressed as it started the app: it starts as if opened, and what
                // was pressed, from a run before, is for a connection gone.
                None => app.activate(),
            }
        });
    }
    app.connect_activate(move |app| {
        // Started again while running: GTK hands it to this copy, which shows itself.
        if let Some(store) = shared.borrow().as_ref() {
            store::show(store);
            return;
        }
        let store = store::start(app);
        let quietly = background && store.borrow().was_signed_in() && store.borrow().has_tray();
        if !quietly {
            store::show(&store);
        }
        *shared.borrow_mut() = Some(store);
    });
    // Its own arguments are read above, so GTK isn't given ones it doesn't know.
    app.run_with_args(&["maxnines-desktop"])
}
