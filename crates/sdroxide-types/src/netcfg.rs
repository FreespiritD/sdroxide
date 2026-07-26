//! Configuration for the network "cockpit" features: spot feeds (DX cluster,
//! POTA, SOTA, PSK Reporter, FreeDV Reporter), callsign lookup, and QSO upload.
//! Pure data + serde, persisted by `sdroxide-config` as `net.json` and carried
//! to the engine by [`crate::Command::SetNetworkConfig`].
//!
//! Deliberately **not** here: the operator's callsign and grid. Those live in
//! [`crate::DigiConfig`] and are the same identity everywhere in the app; the
//! engine hands them to the network workers separately. A second copy would
//! only give the two a way to disagree.
//!
//! Credentials are stored in plaintext (matching the existing config
//! convention). Every field defaults so an older/absent file always loads.

use serde::{Deserialize, Serialize};

/// A DX-cluster telnet node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterConfig {
    pub enabled: bool,
    /// Host name of the telnet cluster node.
    pub host: String,
    /// TCP port (commonly 7300/7373/8000).
    pub port: u16,
    /// Login callsign sent at the node's `login:` prompt. Falls back to the
    /// operator callsign when empty.
    pub login: String,
    /// Extra commands sent after login (e.g. `SET/FT8`, band/spotter filters),
    /// one per line.
    pub commands: Vec<String>,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        ClusterConfig {
            enabled: false,
            host: String::new(),
            port: 7373,
            login: String::new(),
            commands: Vec::new(),
        }
    }
}

/// A polled HTTP spot feed (POTA / SOTA).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedConfig {
    pub enabled: bool,
    /// Poll interval in seconds (clamped to a sane minimum by the client).
    pub interval_secs: u32,
}

impl Default for FeedConfig {
    fn default() -> Self {
        FeedConfig { enabled: false, interval_secs: 60 }
    }
}

/// PSK Reporter reception-report retrieval, for a "who is active on this band"
/// overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PskConfig {
    pub enabled: bool,
    /// Poll interval (PSK Reporter asks for ≥ 300 s between queries).
    pub interval_secs: u32,
}

impl Default for PskConfig {
    fn default() -> Self {
        PskConfig { enabled: false, interval_secs: 300 }
    }
}

/// FreeDV Reporter (<https://qso.freedv.org/>): announce our station and
/// surface everyone else's as spots. Both halves ride one Socket.IO connection.
///
/// The reported identity is the operator's callsign and grid, set once on the
/// General tab, rather than a copy of its own. With either unset we connect
/// read-only and never appear on the site.
///
/// We are only *shown* to others while the radio is in RADE — in any other mode
/// the connection stays up but the station is hidden, matching FreeDV GUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FreeDvReporterConfig {
    pub enabled: bool,
    /// Reporter host name. Empty falls back to the public server.
    pub host: String,
    pub port: u16,
    /// Reserved: the public server also answers TLS on 443, but FreeDV GUI
    /// speaks plain `ws://` and so do we. Not yet implemented.
    pub tls: bool,
    /// Advertise this station as receive-only.
    pub rx_only: bool,
    /// Free-text status shown beside our callsign on the site.
    pub message: String,
    /// Send an `rx_report` for every callsign decoded from a RADE End-of-Over
    /// frame.
    pub report_rx: bool,
    /// Surface other reporter stations as spots on the panadapter and map.
    pub show_spots: bool,
}

impl Default for FreeDvReporterConfig {
    fn default() -> Self {
        FreeDvReporterConfig {
            enabled: false,
            host: "qso.freedv.org".to_string(),
            port: 80,
            tls: false,
            rx_only: false,
            message: String::new(),
            report_rx: true,
            show_spots: true,
        }
    }
}

/// Which callsign-lookup provider to use for auto-fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LookupProvider {
    #[default]
    None,
    Qrz,
    HamQth,
}

impl LookupProvider {
    pub fn label(self) -> &'static str {
        match self {
            LookupProvider::None => "Off",
            LookupProvider::Qrz => "QRZ.com",
            LookupProvider::HamQth => "HamQTH",
        }
    }
    pub const ALL: [LookupProvider; 3] =
        [LookupProvider::None, LookupProvider::Qrz, LookupProvider::HamQth];
}

/// Username/password for a lookup or upload service (plaintext).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    pub user: String,
    pub password: String,
}

/// The whole network-feature configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    // ── Spot feeds ──
    pub cluster: ClusterConfig,
    pub pota: FeedConfig,
    pub sota: FeedConfig,
    pub psk: PskConfig,
    pub freedv_reporter: FreeDvReporterConfig,
    /// Drop/expire spots older than this many seconds.
    pub spot_max_age_secs: u32,
    /// Show only spots that fall in the operator's current band.
    pub spot_current_band_only: bool,

    // ── Callsign lookup ──
    pub lookup_provider: LookupProvider,
    pub qrz: Credentials,
    pub hamqth: Credentials,
    /// Auto-look-up on spot click / QSO start / manual call entry.
    pub auto_lookup: bool,

    // ── Uploads / confirmations ──
    pub eqsl: Credentials,
    /// QRZ Logbook API key (from the QRZ logbook settings, not the XML login).
    pub qrz_logbook_key: String,
    /// Club Log account (email in `user`, password in `password`).
    pub clublog: Credentials,
    /// Club Log application API key.
    pub clublog_api_key: String,
    /// LoTW login, used only to *download* confirmations (no auto-upload).
    pub lotw: Credentials,
    /// Automatically upload each newly logged QSO to the enabled targets.
    pub auto_upload: bool,
    pub auto_upload_eqsl: bool,
    pub auto_upload_qrz: bool,
    pub auto_upload_clublog: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        NetworkConfig {
            cluster: ClusterConfig::default(),
            pota: FeedConfig::default(),
            sota: FeedConfig::default(),
            psk: PskConfig::default(),
            freedv_reporter: FreeDvReporterConfig::default(),
            spot_max_age_secs: 900,
            spot_current_band_only: false,
            lookup_provider: LookupProvider::None,
            qrz: Credentials::default(),
            hamqth: Credentials::default(),
            auto_lookup: false,
            eqsl: Credentials::default(),
            qrz_logbook_key: String::new(),
            clublog: Credentials::default(),
            clublog_api_key: String::new(),
            lotw: Credentials::default(),
            auto_upload: false,
            auto_upload_eqsl: false,
            auto_upload_qrz: false,
            auto_upload_clublog: false,
        }
    }
}
