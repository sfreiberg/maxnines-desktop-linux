//! The refresh token, kept in the desktop's keyring through the Secret Service, as
//! GNOME Keyring and KWallet both provide. The access token is only ever in memory.

const SERVICE: &str = "MaxNines Desktop";
const ACCOUNT: &str = "refresh-token";

fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT)
        .map_err(|error| eprintln!("the keyring can't be used: {error}"))
        .ok()
}

/// The saved refresh token, if there is one.
pub fn refresh_token() -> Option<String> {
    entry()?.get_password().ok()
}

/// Keeps `token`, or forgets the one kept when given nothing.
pub fn set_refresh_token(token: Option<&str>) {
    let Some(entry) = entry() else { return };
    let result = match token {
        Some(token) => entry.set_password(token),
        None => match entry.delete_credential() {
            Err(keyring::Error::NoEntry) => Ok(()),
            other => other,
        },
    };
    if let Err(error) = result {
        eprintln!("couldn't update the keyring: {error}");
    }
}
