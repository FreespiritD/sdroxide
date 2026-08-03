//! WSPR vocabulary shared by the engine, the wire protocol, WSPRnet and the panel.
//!
//! **Interface facts only.** The protocol itself — the sync vector, the
//! convolutional code, the 50-bit packing — lives in `mfsk-core` behind
//! `sdroxide-digi`, which already carries the GPL obligation. What is here is
//! what the UI and the wire need in order to talk about WSPR at all: what a
//! reception report consists of, and what the beacon is doing right now.
//!
//! The one thing worth saying twice: a WSPR decode is **not** a message
//! somebody sent us. There is no addressing in the mode and nobody to answer.
//! What comes out of it is a measurement of a path — which is why this is a
//! [`WsprSpot`] and not a [`crate::Decode`], and why the transmit power is a
//! first-class field rather than a footnote.

use serde::{Deserialize, Serialize};

/// The WSPR window: the 200 Hz stretch above the dial where every transmission
/// lives. Both ends of the decoder and the panel's tuning display work from
/// these, so they are named once rather than spelled out in each.
pub const WINDOW_LO_HZ: f32 = 1400.0;
pub const WINDOW_HI_HZ: f32 = 1600.0;

/// Where our own transmission is placed inside the window by default — the
/// centre, which is also where WSJT-X puts it before the operator moves it.
pub const DEFAULT_TX_HZ: f32 = 1500.0;

/// Slot length in seconds. WSPR-2, the only variant in common use; WSPR-15
/// exists and is not implemented.
pub const SLOT_S: f64 = 120.0;

/// Seconds from the slot boundary to the first symbol. One second, by
/// convention and by the decoder's `dt` reference.
pub const TX_OFFSET_S: f64 = 1.0;

/// On-air length of a transmission: 162 symbols at 8192/12000 s each.
pub const BURST_S: f64 = 162.0 * 8192.0 / 12_000.0;

/// The power levels a WSPR message can express, in dBm.
///
/// Not a free integer: the 50-bit payload spends part of one field on power and
/// can only name these nineteen. Anything else has to be rounded to one of them
/// before transmitting, which [`round_power_dbm`] does — a beacon that quietly
/// announced a power it was not running would corrupt everybody else's path
/// measurements, not just its own.
pub const POWERS_DBM: [i16; 19] =
    [0, 3, 7, 10, 13, 17, 20, 23, 27, 30, 33, 37, 40, 43, 47, 50, 53, 57, 60];

/// The nearest power WSPR can actually express, at or below `dbm`.
///
/// Rounds *down* rather than to nearest on purpose: over-declaring power makes
/// a path look worse than it is to everyone who receives the beacon, and the
/// error is invisible to the operator. Under-declaring only understates our own
/// reach. Below the lowest expressible level it clamps up to it, because there
/// is nothing else to say.
pub fn round_power_dbm(dbm: i16) -> i16 {
    match POWERS_DBM.iter().rev().find(|&&p| p <= dbm) {
        Some(&p) => p,
        None => POWERS_DBM[0],
    }
}

/// Milliwatts for a dBm level, for the setup panel's "5 W" label beside "37".
pub fn dbm_to_mw(dbm: i16) -> f64 {
    10f64.powf(dbm as f64 / 10.0)
}

/// A human label for a power level: milliwatts under a watt, watts above.
pub fn power_label(dbm: i16) -> String {
    let mw = dbm_to_mw(dbm);
    if mw < 1000.0 { format!("{mw:.0} mW") } else { format!("{:.1} W", mw / 1000.0) }
}

/// One WSPR reception: a beacon this station decoded, or — when it came back
/// from WSPRnet — somebody else decoding one.
///
/// Every field except `call` is part of the measurement rather than decoration.
/// `power_dbm` in particular: a −20 dB report of a 200 mW beacon describes a far
/// better path than the same report of a 5 W one, and a propagation display that
/// ignored it would rank the two the same.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WsprSpot {
    /// Unix seconds at the start of the two-minute slot this was sent in.
    pub slot_utc: i64,
    /// The transmitting station, uppercased.
    pub call: String,
    /// The transmitter's 4- or 6-character locator.
    ///
    /// `None` for a Type-2 message, which spends its grid field on a compound
    /// callsign prefix or suffix instead — `PJ4/K1ABC` says where it is in the
    /// callsign and has no room left to say it again.
    pub grid: Option<String>,
    /// The power the beacon declared, dBm. Always one of [`POWERS_DBM`].
    pub power_dbm: i16,
    /// Absolute RF frequency of the transmission (Hz): dial plus tone offset.
    pub freq_hz: f64,
    /// Signal-to-noise ratio in dB, referenced to a 2500 Hz bandwidth — the
    /// convention WSJT-X and WSPRnet both report in.
    pub snr_db: i16,
    /// Offset of the first symbol from the nominal slot anchor, in seconds.
    /// Mostly a clock-health signal: a whole band reading +1.4 s is our clock,
    /// not theirs.
    pub dt: f32,
    /// Carrier drift across the whole transmission, in Hz. A beacon on a
    /// free-running oscillator still warming up drifts; one on a GPS reference
    /// does not.
    ///
    /// *Across the transmission*, not per minute, however the column happens to
    /// be labelled elsewhere. This is the integer `wsprd` searches over
    /// (`±4` by default) and prints, and it is what every other client uploads
    /// to WSPRnet verbatim — so converting it to a rate here would make our
    /// spots disagree with everyone else's for the same signal.
    pub drift_hz: f32,
    /// Who decoded it. `None` means this station did — the common case, and the
    /// one that gets uploaded. `Some` is a report that came back from WSPRnet
    /// saying somebody else heard the transmission.
    pub reporter: Option<String>,
    /// The reporter's locator, where the feed gave one.
    pub reporter_grid: Option<String>,
}

impl WsprSpot {
    /// True when this is a report of *our* transmission being heard elsewhere,
    /// rather than something we decoded ourselves.
    pub fn is_heard_by_other(&self) -> bool {
        self.reporter.is_some()
    }

    /// The message as WSJT-X would print it: `K1ABC FN42 37`.
    pub fn message(&self) -> String {
        match &self.grid {
            Some(g) => format!("{} {} {}", self.call, g, self.power_dbm),
            None => format!("{} {}", self.call, self.power_dbm),
        }
    }

    /// A stable identity for de-duplication: one station, one slot, one
    /// frequency to the nearest 5 Hz.
    ///
    /// Frequency is bucketed rather than exact because the same transmission
    /// decoded twice — once locally and once as it comes back from WSPRnet —
    /// will not agree to the hertz, and two entries for one beacon is a bug the
    /// operator sees as the mode double-counting.
    pub fn dedup_key(&self) -> (i64, String, i64, Option<String>) {
        (
            self.slot_utc,
            self.call.clone(),
            (self.freq_hz / 5.0).round() as i64,
            self.reporter.clone(),
        )
    }
}

/// What the WSPR engine is doing, for the panel's status strip.
///
/// `None` on [`crate::DigiStatus`] in every other mode, so the panel that draws
/// it is its own "are we in WSPR?" test — the same arrangement
/// [`crate::Js8Status`] uses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WsprStatus {
    /// Unix seconds at the start of the slot now in progress.
    pub slot_utc: i64,
    /// True while this slot's audio is still being decoded. A WSPR scan takes
    /// tens of seconds, so "nothing yet" and "nothing at all" are genuinely
    /// different states and the panel says which.
    pub decoding: bool,
    /// Whether the beacon will transmit in the next slot, as the duty cycle has
    /// already decided it.
    pub tx_next: bool,
    /// Spots decoded in the last completed slot — a count, not the list, which
    /// travels as its own event.
    pub last_slot_spots: u32,
    /// Where band hopping will put the dial next, in Hz, when it is running.
    pub next_dial_hz: Option<f64>,
    /// Why the last hop did not happen, when one was refused. Shown rather than
    /// swallowed: a beacon that silently stopped hopping looks like a bug.
    pub hop_blocked: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_power_is_rounded_down_to_something_wspr_can_actually_say() {
        // 5 W is expressible exactly.
        assert_eq!(round_power_dbm(37), 37);
        // 10 W is not; 37 dBm (5 W) is the honest thing to announce.
        assert_eq!(round_power_dbm(40 - 1), 37);
        // Never rounds up: announcing more power than we run makes the path
        // look worse to everyone who hears us.
        for dbm in -10..=60 {
            assert!(round_power_dbm(dbm) <= dbm.max(POWERS_DBM[0]));
            assert!(POWERS_DBM.contains(&round_power_dbm(dbm)));
        }
    }

    #[test]
    fn a_power_below_the_lowest_level_clamps_up_rather_than_vanishing() {
        assert_eq!(round_power_dbm(-30), 0);
    }

    #[test]
    fn the_burst_fits_inside_the_slot_with_the_start_delay() {
        assert!(TX_OFFSET_S + BURST_S < SLOT_S, "{TX_OFFSET_S} + {BURST_S} does not fit {SLOT_S}");
        // 110.6 s is the figure every WSPR description quotes.
        assert!((BURST_S - 110.6).abs() < 0.05, "burst is {BURST_S} s");
    }

    #[test]
    fn a_type_2_spot_prints_without_a_grid_rather_than_an_empty_one() {
        let s = WsprSpot {
            slot_utc: 0,
            call: "PJ4/K1ABC".into(),
            grid: None,
            power_dbm: 37,
            freq_hz: 14_097_100.0,
            snr_db: -20,
            dt: 0.3,
            drift_hz: 0.0,
            reporter: None,
            reporter_grid: None,
        };
        assert_eq!(s.message(), "PJ4/K1ABC 37");
        assert!(!s.is_heard_by_other());
    }

    /// The same transmission decoded here and reported back by WSPRnet must not
    /// collapse into one entry: they are different statements, and the reporter
    /// is what tells them apart.
    #[test]
    fn a_report_of_us_and_a_local_decode_are_not_the_same_spot() {
        let mine = WsprSpot {
            slot_utc: 1_800_000_000,
            call: "K1ABC".into(),
            grid: Some("FN42".into()),
            power_dbm: 37,
            freq_hz: 14_097_100.0,
            snr_db: -20,
            dt: 0.3,
            drift_hz: 0.0,
            reporter: None,
            reporter_grid: None,
        };
        let theirs = WsprSpot { reporter: Some("G0XYZ".into()), ..mine.clone() };
        assert_ne!(mine.dedup_key(), theirs.dedup_key());
        // A hertz of frequency disagreement between the two feeds does not.
        let jittered = WsprSpot { freq_hz: 14_097_101.0, ..mine.clone() };
        assert_eq!(mine.dedup_key(), jittered.dedup_key());
    }

    #[test]
    fn power_reads_in_the_unit_an_operator_thinks_in() {
        assert_eq!(power_label(0), "1 mW");
        assert_eq!(power_label(23), "200 mW");
        assert_eq!(power_label(37), "5.0 W");
        assert_eq!(power_label(60), "1000.0 W");
    }
}
