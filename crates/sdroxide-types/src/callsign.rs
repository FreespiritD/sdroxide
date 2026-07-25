//! Callsign-lookup result, shared by the native lookup clients (QRZ/HamQTH in
//! `sdroxide-net`), the wire protocol, and the UI. Pure data + serde.

use serde::{Deserialize, Serialize};

/// Biographical / location data resolved for a callsign. Every field is
/// optional: providers return different subsets and some require a paid
/// subscription for the full record. The UI merges the populated fields into
/// the active log entry without overwriting what the operator already typed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallsignInfo {
    /// The callsign this record is for (uppercased), echoed back so a late
    /// result can be matched to the right log entry.
    pub call: String,
    /// Operator name (first + last, as the provider formats it).
    pub name: Option<String>,
    /// City / town (QTH).
    pub qth: Option<String>,
    /// Maidenhead grid locator.
    pub grid: Option<String>,
    /// US state (2-letter) or other primary administrative subdivision.
    pub state: Option<String>,
    /// US county, if provided.
    pub county: Option<String>,
    /// Country / DXCC entity name.
    pub country: Option<String>,
    /// DXCC entity number, if the provider returns it.
    pub dxcc: Option<u16>,
    /// CQ zone (1..40).
    pub cq_zone: Option<u8>,
    /// ITU zone (1..90).
    pub itu_zone: Option<u8>,
    /// QSL routing hint / manager, if any.
    pub qsl_via: Option<String>,
}

impl CallsignInfo {
    pub fn for_call(call: &str) -> Self {
        CallsignInfo { call: call.trim().to_ascii_uppercase(), ..Default::default() }
    }

    /// True when the lookup produced nothing usable.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.qth.is_none()
            && self.grid.is_none()
            && self.state.is_none()
            && self.county.is_none()
            && self.country.is_none()
            && self.dxcc.is_none()
            && self.cq_zone.is_none()
            && self.itu_zone.is_none()
            && self.qsl_via.is_none()
    }
}

/// Where to send a one-click QSO upload. Matches the credentials in
/// [`crate` network config] and the upload clients in `sdroxide-net`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UploadTarget {
    Eqsl,
    QrzLogbook,
    ClubLog,
}

impl UploadTarget {
    pub fn label(self) -> &'static str {
        match self {
            UploadTarget::Eqsl => "eQSL",
            UploadTarget::QrzLogbook => "QRZ",
            UploadTarget::ClubLog => "Club Log",
        }
    }

    pub const ALL: [UploadTarget; 3] =
        [UploadTarget::Eqsl, UploadTarget::QrzLogbook, UploadTarget::ClubLog];
}

/// The outcome of an upload attempt for one QSO to one target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadResult {
    /// The logbook id of the QSO this result is for.
    pub qso_id: u64,
    pub target: UploadTarget,
    pub ok: bool,
    /// Server message or error detail.
    pub message: String,
}
