//! Who may drive this station from somewhere else.
//!
//! The server hands out one radio, and a radio is a transmitter. Left open, a
//! port forwarded so the operator can listen from work is also a port anybody
//! else can key, on the operator's licence and callsign — so the server asks
//! for a username and a password before it hands anything over.
//!
//! This is the *configuration* half, kept here rather than in `sdroxide-config`
//! because three crates need to name it: the config file that stores it, the
//! server that enforces it, and the settings dialog that edits it.

use serde::{Deserialize, Serialize};

/// The credentials a remote client has to present before the server will let it
/// near the radio.
///
/// Both fields empty means the server is open to anyone who can reach the port,
/// which is what every version before this one was — so an existing
/// `config.toml` upgrades into exactly the behaviour it already had, and the
/// operator turns this on when they mean to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteAccess {
    pub username: String,
    pub password: String,
}

impl RemoteAccess {
    /// Whether connecting clients are challenged at all.
    ///
    /// A password on its own is enough. A single-operator station has no use
    /// for a username, and insisting on one would only produce a shack full of
    /// accounts called `admin` — the password is the secret either way.
    pub fn is_enforced(&self) -> bool {
        !self.username.is_empty() || !self.password.is_empty()
    }

    /// Whether these are the configured credentials.
    ///
    /// Both halves are always compared and neither comparison stops early, so
    /// the time this takes says nothing about how much of a guess was right.
    /// The server's turnstile already makes a timing attack impractical; this
    /// costs a few hundred nanoseconds and means it never has to be the only
    /// thing standing in the way.
    pub fn accepts(&self, username: &str, password: &str) -> bool {
        let user_ok = ct_eq(self.username.as_bytes(), username.as_bytes());
        let pass_ok = ct_eq(self.password.as_bytes(), password.as_bytes());
        // Non-short-circuiting `&`, deliberately: `&&` would skip the second
        // test whenever the first failed, which is a timing signal telling an
        // attacker they have the username right.
        user_ok & pass_ok
    }
}

/// Byte comparison that takes as long when the first byte differs as when the
/// last one does.
///
/// The lengths are folded in rather than checked first — a length check that
/// returned early would leak the length of the secret, which for a password is
/// most of the search space. The loop runs over the longer of the two, so its
/// duration depends only on what the caller supplied.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u64;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= u64::from(x ^ y);
    }
    diff == 0
}

/// Where a connection stands with the server's sign-in challenge.
///
/// Reported by [`RadioController::auth_phase`](crate::RadioController::auth_phase)
/// so the UI can put a dialog up. An in-process engine is always [`Open`]:
/// there is no network and nothing to prove.
///
/// [`Open`]: AuthPhase::Open
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AuthPhase {
    /// Nothing to answer — a local engine, or a server with no credentials set.
    #[default]
    Open,
    /// The server is waiting for a username and password. `Some` carries why
    /// the previous attempt was turned down, for the dialog to show.
    Prompt(Option<String>),
    /// Credentials are with the server; waiting for its verdict.
    Checking,
}

impl AuthPhase {
    /// Whether the operator is being asked to sign in, either way.
    pub fn is_pending(&self) -> bool {
        !matches!(self, AuthPhase::Open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(user: &str, pass: &str) -> RemoteAccess {
        RemoteAccess { username: user.into(), password: pass.into() }
    }

    /// The upgrade path: a config that has never heard of this leaves the
    /// server exactly as open as it was.
    #[test]
    fn empty_credentials_do_not_challenge_anyone() {
        assert!(!RemoteAccess::default().is_enforced());
        assert!(creds("", "").accepts("", ""), "an open server accepts anything");
    }

    /// A password with no username is a complete configuration.
    #[test]
    fn a_password_alone_is_enough_to_close_the_door() {
        let want = creds("", "hunter2");
        assert!(want.is_enforced());
        assert!(want.accepts("", "hunter2"));
        assert!(!want.accepts("", "hunter3"));
        // ...and the empty username still has to match, so a client that sends
        // one cannot be treated as having sent none.
        assert!(!want.accepts("op", "hunter2"));
    }

    #[test]
    fn both_halves_have_to_match() {
        let want = creds("oe1test", "hunter2");
        assert!(want.accepts("oe1test", "hunter2"));
        assert!(!want.accepts("oe1test", "hunter"), "wrong password");
        assert!(!want.accepts("someone", "hunter2"), "wrong username");
        assert!(!want.accepts("", ""), "no credentials at all");
        // A prefix is not a match, in either direction.
        assert!(!want.accepts("oe1test", "hunter22"));
        assert!(!want.accepts("oe1tes", "hunter2"));
    }

    /// The comparison must not be fooled by anything the length arithmetic
    /// could paper over — an empty secret matching a long guess, or a guess
    /// whose length happens to XOR to zero against it.
    #[test]
    fn lengths_are_part_of_the_comparison() {
        assert!(ct_eq(b"", b""));
        assert!(!ct_eq(b"", b"x"));
        assert!(!ct_eq(b"x", b""));
        // "abc" vs "abc\0": equal over the overlap, and the trailing NUL is
        // exactly what `unwrap_or(0)` pads the shorter side with — only the
        // length term catches this one.
        assert!(!ct_eq(b"abc", b"abc\0"));
        assert!(ct_eq(b"abc", b"abc"));
    }

    #[test]
    fn a_local_engine_never_prompts() {
        assert!(!AuthPhase::default().is_pending());
        assert!(AuthPhase::Prompt(None).is_pending());
        assert!(AuthPhase::Checking.is_pending());
    }
}
