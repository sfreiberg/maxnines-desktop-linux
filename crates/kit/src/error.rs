/// What went wrong, worded to show to the person.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The address isn't one a server could be at.
    #[error("The server address isn't a web address")]
    BadAddress,
    /// The server turned the sign-in down even after fresh tokens.
    #[error("The server didn't accept the sign in")]
    NotSignedIn,
    /// The sign-in has run out or been revoked, so the person has to sign in again.
    #[error("Signed out: sign in again")]
    SignedOut,
    /// What the person signed in as isn't allowed to do this, with what the server
    /// said, if it said anything.
    #[error("{}", .0.as_deref().unwrap_or("This sign in isn't allowed to do that. Acknowledging and silencing need the responder role"))]
    NotAllowed(Option<String>),
    #[error("The server answered with status {0}")]
    Status(u16),
    /// Something answered, but not as a MaxNines server does.
    #[error("That address didn't answer like a MaxNines server")]
    NotMaxNines,
    /// The alert's rule has been deleted, so a silence can't name it.
    #[error("This alert's rule has been deleted, so it can't be silenced from here")]
    CantSilence,
    /// The server couldn't be reached at all, with why.
    #[error("Can't reach the server: {0}")]
    Unreachable(String),
    /// The sign-in endpoints turned something down: OAuth's word for it, and the
    /// server's explanation.
    #[error("{1}")]
    OAuth(String, String),
    /// The person chose Deny in the browser.
    #[error("You chose Deny, so the app isn't signed in")]
    Denied,
    /// The server couldn't ask the person to sign in, in OAuth's words.
    #[error("The server couldn't ask you to sign in ({0})")]
    SignInFailed(String),
    /// Nothing came back from the browser in time.
    #[error("Nothing came back from the browser, so the app isn't signed in. Try again")]
    TimedOut,
    /// Signing in was cancelled.
    #[error("Signing in was cancelled")]
    Cancelled,
    /// What was asking the server stopped partway, which is a bug in the app.
    #[error("Asking the server stopped partway, which is a bug in the app")]
    Stopped,
}
