//! The antenna-rotator client's configuration.
//!
//! sdroxide points a motorized antenna by talking *to* a Hamlib `rotctld`
//! daemon over TCP — the opposite direction from the built-in rigctld
//! *server*. One text protocol reaches every rotator Hamlib can drive, which
//! is essentially all of them, without this codebase growing a serial driver
//! per controller. (EasyComm II and GS-232 over a serial port are the obvious
//! future backends for stations that would rather not run a daemon; the
//! engine-side interface — "here is az/el, go" — would not change.)

use serde::{Deserialize, Serialize};

/// Where the rotctld daemon lives and how eagerly to chase the satellite.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RotatorConfig {
    /// Master switch. Off, the client thread does not exist and nothing
    /// connects anywhere.
    pub enabled: bool,
    pub host: String,
    /// Hamlib's registered rotctld port.
    pub port: u16,
    /// Below this elevation the rotator parks instead of tracking: no point
    /// grinding the motors at a satellite behind the neighbour's roofline.
    pub min_el_deg: f64,
    /// Added to every commanded azimuth — for a rotator whose north calibration
    /// is off by a known amount.
    pub az_offset_deg: f64,
    /// Movements smaller than this are not sent. Chasing tenths of a degree
    /// wears hardware for nothing; a degree is well inside any amateur
    /// antenna's beamwidth.
    pub min_move_deg: f64,
    /// Where to point when idle — `None` leaves the rotator wherever it was.
    pub park: Option<(f64, f64)>,
}

impl Default for RotatorConfig {
    fn default() -> Self {
        RotatorConfig {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 4533,
            min_el_deg: 0.0,
            az_offset_deg: 0.0,
            min_move_deg: 1.0,
            park: None,
        }
    }
}

impl RotatorConfig {
    /// What to dial, as one string.
    pub fn address(&self) -> String {
        format!("{}:{}", self.host.trim(), self.port)
    }
}
