//! The refresh token, kept in the desktop's keyring through the Secret Service, as
//! GNOME Keyring and KWallet both provide, with the server it's for. The access token
//! is only ever in memory.
//!
//! Every call can wait on the Secret Service, or on the person answering its unlock
//! prompt, so none is made on GTK's thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use keyring_core::Entry;
use keyring_core::api::CredentialStoreApi;
use maxnines_desktop_kit::ServerAddress;
use maxnines_desktop_kit::tokens::{kept, kept_for};

const SERVICE: &str = "MaxNines Desktop";
const ACCOUNT: &str = "refresh-token";

/// Held while the keyring is changed, so whether a change is still wanted is decided
/// with no other change running.
static CHANGING: Mutex<()> = Mutex::new(());

/// The entry, through a connection to the Secret Service made for this one use. The
/// app uses the keyring a few times an hour at most, and a connection kept from
/// before goes stale when the service restarts, as after a crash or an update, after
/// which everything through it fails. A new one also finds a service that started
/// after the app.
fn entry() -> Result<Entry, keyring_core::Error> {
    zbus_secret_service_keyring_store::Store::new()?.build(SERVICE, ACCOUNT, None)
}

/// The saved refresh token for `server`, nothing if there isn't one for it, or why
/// the keyring couldn't be read, such as it being locked or its service not having
/// started yet. One for another server, kept as signing in to that one was cut short,
/// is never sent to this one.
pub fn refresh_token(server: &ServerAddress) -> Result<Option<String>, String> {
    match entry().and_then(|entry| entry.get_password()) {
        Ok(kept) => Ok(kept_for(&kept, server)),
        Err(keyring_core::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Keeps `token`, a refresh token for the server with it, or forgets the one kept
/// when given nothing, for the sign-in on `connection`, unless `current` has since
/// moved on from it: a sign-out finishing late, or a refresh for a sign-in that's been
/// replaced, leaves the newer one's token be. Says why if the keyring couldn't be
/// changed.
pub fn set_refresh_token(
    token: Option<(&ServerAddress, &str)>,
    connection: u64,
    current: &AtomicU64,
) -> Result<(), String> {
    let changed = if_current(connection, current, || {
        let entry = entry()?;
        match token {
            Some((server, token)) => entry.set_password(&kept(server, token)),
            None => match entry.delete_credential() {
                Err(keyring_core::Error::NoEntry) => Ok(()),
                other => other,
            },
        }
    });
    match changed {
        Some(Err(error)) => Err(error.to_string()),
        Some(Ok(())) | None => Ok(()),
    }
}

/// Runs `change` if `connection` is still `current`, with nothing else changing the
/// keyring meanwhile. Whatever ends a connection moves `current` on before it changes
/// the keyring itself, so a change that finds its connection current is done before
/// that one.
fn if_current<T>(connection: u64, current: &AtomicU64, change: impl FnOnce() -> T) -> Option<T> {
    let _only_one = CHANGING.lock().unwrap_or_else(PoisonError::into_inner);
    (current.load(Ordering::SeqCst) == connection).then(change)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_current_connection_changes_the_keyring() {
        let current = AtomicU64::new(3);
        assert_eq!(if_current(3, &current, || "kept"), Some("kept"));
        // Signed out, or signed in again, since.
        current.store(4, Ordering::SeqCst);
        let mut ran = false;
        assert_eq!(if_current(3, &current, || ran = true), None);
        assert!(!ran);
    }
}
