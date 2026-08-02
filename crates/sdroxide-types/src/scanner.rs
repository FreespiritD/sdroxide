//! Scanning: working through a list of channels, or a slice of a band, and
//! stopping where something is on the air.
//!
//! The unusual part of doing this on a software-defined radio is that it need
//! not be slow. A handheld scanner has one receiver and has to visit each
//! channel in turn, which is why covering 2 m takes minutes; here the FFT that
//! already draws the panadapter sees a whole span at once, so a range scan moves
//! the hardware one span at a time and reads every channel in it together. Only
//! a front end with no wideband IQ of its own — a CAT rig on a sound card — has
//! to fall back to visiting channels one at a time.

use serde::{Deserialize, Serialize};

use crate::Mode;

/// What a scan runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ScanKind {
    /// The stored memory channels, each with its own mode and filter.
    #[default]
    Memories,
    /// A frequency range on a fixed channel grid, in one mode.
    Range,
}

impl ScanKind {
    pub const ALL: [ScanKind; 2] = [ScanKind::Memories, ScanKind::Range];

    pub fn label(self) -> &'static str {
        match self {
            ScanKind::Memories => "MEM",
            ScanKind::Range => "RANGE",
        }
    }
}

/// What ends a stop on a busy channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ScanResume {
    /// Carry on once the signal drops, after a short grace period — the usual
    /// scanner behaviour, and the one that follows a conversation.
    #[default]
    Carrier,
    /// Carry on after a fixed time whether or not the signal is still there.
    Timed,
    /// Stay until the operator says otherwise.
    Manual,
}

impl ScanResume {
    pub const ALL: [ScanResume; 3] = [ScanResume::Carrier, ScanResume::Timed, ScanResume::Manual];

    pub fn label(self) -> &'static str {
        match self {
            ScanResume::Carrier => "CARRIER",
            ScanResume::Timed => "TIMED",
            ScanResume::Manual => "MANUAL",
        }
    }
}

/// Channel spacings a range scan can step on, in Hz. 12.5 kHz is the European
/// VHF/UHF norm, 25 kHz the older wide spacing, 8.33 kHz airband, 5 kHz
/// broadcast and PMR, 6.25 kHz the narrowest in common use.
pub const SCAN_STEPS_HZ: [f64; 6] = [5_000.0, 6_250.0, 8_333.0, 10_000.0, 12_500.0, 25_000.0];

/// Persisted scanner settings (`scanner.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScannerConfig {
    pub kind: ScanKind,
    pub range_lo_hz: f64,
    pub range_hi_hz: f64,
    /// Channel grid a range scan snaps candidates to.
    pub step_hz: f64,
    /// Mode a range scan uses. A memory scan takes the mode from each memory.
    pub mode: Mode,
    /// Level (dBFS) a channel has to reach to count as busy. The same scale as
    /// [`crate::RxState::squelch_db`], and measured the same way, so the two
    /// agree about what "busy" means.
    pub threshold_db: f32,
    /// Take the threshold from the receiver's own squelch instead, so one
    /// control sets both what stops the scan and what opens the audio.
    pub follow_squelch: bool,
    /// How long to listen on a candidate before deciding it is really busy.
    pub dwell_ms: u32,
    pub resume: ScanResume,
    /// [`ScanResume::Timed`]: how long to stay. [`ScanResume::Carrier`]: how
    /// long to linger after the signal drops, so a gap between overs does not
    /// lose the conversation.
    pub resume_ms: u32,
    /// Memory ids to pass over.
    pub skip: Vec<u32>,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        ScannerConfig {
            kind: ScanKind::Memories,
            // The 2 m band: the most-scanned range there is, and a sane thing to
            // find already filled in the first time the window is opened.
            range_lo_hz: 144_000_000.0,
            range_hi_hz: 146_000_000.0,
            step_hz: 12_500.0,
            mode: Mode::Nfm,
            threshold_db: -80.0,
            follow_squelch: false,
            dwell_ms: 150,
            resume: ScanResume::Carrier,
            resume_ms: 2_000,
            skip: Vec::new(),
        }
    }
}

impl ScannerConfig {
    /// The range with its edges the right way round, which is how the engine
    /// wants it however the operator typed it in.
    pub fn range(&self) -> (f64, f64) {
        if self.range_lo_hz <= self.range_hi_hz {
            (self.range_lo_hz, self.range_hi_hz)
        } else {
            (self.range_hi_hz, self.range_lo_hz)
        }
    }

    /// Whether the settings describe a scan that could ever stop anywhere.
    pub fn range_is_usable(&self) -> bool {
        let (lo, hi) = self.range();
        lo.is_finite() && hi.is_finite() && lo > 0.0 && hi - lo >= self.step_hz.max(1.0)
    }
}

/// What the scanner is doing, small enough to ride in every [`crate::RadioState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScanState {
    pub running: bool,
    /// Stopped on a busy channel rather than moving.
    pub holding: bool,
}
