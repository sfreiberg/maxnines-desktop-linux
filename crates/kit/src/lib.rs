//! What MaxNines Desktop for Linux knows without a desktop: the server's API, signing
//! in through the browser, keeping the sign-in fresh, and what's worth a
//! notification. The app crate puts it on screen.
//!
//! It mirrors the shared code of the Mac and Windows apps, so all three say and do the
//! same things.

pub mod client;
pub mod error;
pub mod http;
pub mod models;
pub mod notices;
pub mod overview;
pub mod signin;
pub mod times;
pub mod tokens;

pub use client::{Client, ServerAddress};
pub use error::Error;
pub use models::{Alerts, MonitoredHost, RemoteCheck, ServerAlert, Severity, Silence, Silences};
pub use notices::{Notice, NotifyWhen, notices};
pub use overview::Overview;
pub use signin::Tokens;
pub use tokens::TokenHolder;
