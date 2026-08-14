//! Winlink radio-email vocabulary: the mailbox as the UI sees it, and the
//! account settings a session needs.
//!
//! The protocol itself lives in `sdroxide-winlink`, which is native-only. What
//! is here is only the shape of a mailbox and a session's status, because both
//! have to cross the wire to a browser client that links none of that code.
//!
//! The mailbox lives on the machine with the radio and is read **lazily**: a
//! listing carries metadata, and the body and attachments are fetched one
//! message at a time. That is the same arrangement the picture store uses, and
//! for the same reason — a mailbox with attachments in it is far too big to
//! mirror into every client on connect.

use serde::{Deserialize, Serialize};

/// Where a message sits in its life.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub enum MailFolder {
    /// Arrived from a forwarding partner.
    #[default]
    Inbox,
    /// Composed here, waiting for a session to carry it.
    Outbox,
    /// Handed to a forwarding partner and accepted.
    Sent,
    /// Kept, but out of the way.
    Archive,
}

impl MailFolder {
    pub const ALL: [MailFolder; 4] =
        [MailFolder::Inbox, MailFolder::Outbox, MailFolder::Sent, MailFolder::Archive];

    /// Directory name on disk. Also the stable key in the wire protocol, so it
    /// must not change once written.
    pub fn dir_name(self) -> &'static str {
        match self {
            MailFolder::Inbox => "inbox",
            MailFolder::Outbox => "outbox",
            MailFolder::Sent => "sent",
            MailFolder::Archive => "archive",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MailFolder::Inbox => "INBOX",
            MailFolder::Outbox => "OUTBOX",
            MailFolder::Sent => "SENT",
            MailFolder::Archive => "ARCHIVE",
        }
    }
}

/// Most entries one listing request may ask for, so a client cannot make the
/// server build an unbounded response.
pub const MAIL_PAGE_MAX: u32 = 200;

/// One message's metadata — enough to draw a list row without fetching it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailEntry {
    pub mid: String,
    /// `YYYY/MM/DD HH:MM`, UTC, as the message carries it.
    pub date: String,
    pub from: String,
    /// Recipients, already joined for display.
    pub to: String,
    pub subject: String,
    pub folder: MailFolder,
    /// Size of the stored message, so a list can say what a fetch will cost
    /// before pulling it down a phone link.
    pub bytes: u64,
    pub attachments: u32,
    /// Not yet opened. Only meaningful in the inbox.
    pub unread: bool,
}

/// One page of a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailListing {
    pub folder: MailFolder,
    /// Where `entries` starts within the folder.
    pub offset: u32,
    /// How many the folder holds altogether, so the UI can page exactly rather
    /// than guessing from what it was given.
    pub total: u32,
    pub entries: Vec<MailEntry>,
}

/// An attachment, with its bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailAttachment {
    pub name: String,
    pub data: Vec<u8>,
}

/// One whole message, answering a fetch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailMessage {
    pub mid: String,
    pub date: String,
    /// `Private` for ordinary mail; also `Service`, `Inquiry`, `Position`.
    pub msg_type: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<MailAttachment>,
    pub folder: MailFolder,
}

/// A message the operator has composed, on its way to the outbox.
///
/// Separate from [`MailMessage`] because the fields the operator fills in are
/// not the fields a stored message has: the MID and date are minted server-side
/// at the moment it is filed, and a client must not choose either.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailDraft {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<MailAttachment>,
}

/// What a forwarding session is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WinlinkStatus {
    /// A session is running right now.
    pub busy: bool,
    /// One line on what it is doing, for the window header.
    pub activity: String,
    /// Why the last session ended badly, if it did.
    pub last_error: Option<String>,
    /// Unix seconds of the last session that completed cleanly.
    pub last_session: Option<i64>,
    /// Messages received and sent by the last session.
    pub last_received: u32,
    pub last_sent: u32,
    /// Per-folder counts, in [`MailFolder::ALL`] order, so the folder tabs can
    /// show totals without listing anything.
    pub counts: [u32; 4],
    /// The protocol transcript of the most recent session. Kept because a
    /// forwarding session that fails is unreadable without it.
    pub log: Vec<String>,
}

/// The gateway the CMS telnet transport dials by default.
pub const DEFAULT_CMS_ADDRESS: &str = "server.winlink.org:8772";

/// Account settings for Winlink.
///
/// The password is the Winlink *account* password, which answers the secure
/// login challenge. It is stored in plaintext alongside the other service
/// credentials in [`crate::NetworkConfig`] — the same posture as QRZ, LoTW and
/// Club Log, and worth knowing rather than assuming otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WinlinkConfig {
    /// The account callsign. Upper-cased on use.
    pub callsign: String,
    /// Account password. **Case-sensitive** — the challenge response differs
    /// for `FOOBAR` and `FooBar`, so it must be entered exactly as issued.
    pub password: String,
    /// Maidenhead locator reported in the session greeting. Cosmetic.
    pub locator: String,
    /// Which CMS to dial. Overridable mainly for testing.
    pub cms_address: String,
    /// The client name announced in our SID.
    ///
    /// Winlink's production CMS refuses names it does not know, answering
    /// `*** Unknown client types are not allowed on production servers`. Until
    /// "sdroxide" is registered with the Winlink Development Team this is the
    /// knob that lets an operator get in, and it is deliberately visible in
    /// settings rather than silently defaulted to somebody else's client name.
    pub app_name: String,
    /// Connect automatically on a timer.
    pub auto_connect: bool,
    /// How often to connect when `auto_connect` is on.
    pub auto_connect_minutes: u32,
}

impl Default for WinlinkConfig {
    fn default() -> Self {
        WinlinkConfig {
            callsign: String::new(),
            password: String::new(),
            locator: String::new(),
            cms_address: DEFAULT_CMS_ADDRESS.to_string(),
            app_name: "sdroxide".to_string(),
            auto_connect: false,
            auto_connect_minutes: 30,
        }
    }
}

impl WinlinkConfig {
    /// Whether a session could even be attempted.
    pub fn is_usable(&self) -> bool {
        !self.callsign.trim().is_empty() && !self.password.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_dir_names_are_stable() {
        // These name directories on disk and ride the wire; changing one
        // orphans an operator's stored mail.
        let names: Vec<&str> = MailFolder::ALL.iter().map(|f| f.dir_name()).collect();
        assert_eq!(names, ["inbox", "outbox", "sent", "archive"]);
    }

    #[test]
    fn counts_line_up_with_folders() {
        let status = WinlinkStatus::default();
        assert_eq!(status.counts.len(), MailFolder::ALL.len());
    }

    #[test]
    fn default_config_is_not_usable() {
        // Nothing should try to connect until the operator has filled it in.
        assert!(!WinlinkConfig::default().is_usable());
        let cfg =
            WinlinkConfig { callsign: "OE3JJS".into(), password: "x".into(), ..Default::default() };
        assert!(cfg.is_usable());
        // Whitespace is not a callsign.
        let blank =
            WinlinkConfig { callsign: "   ".into(), password: "x".into(), ..Default::default() };
        assert!(!blank.is_usable());
    }
}
