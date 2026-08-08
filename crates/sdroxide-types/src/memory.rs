use serde::{Deserialize, Serialize};

use crate::Mode;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryChannel {
    pub id: u32,
    pub name: String,
    pub freq_hz: f64,
    pub mode: Mode,
    pub filter_lo: f32,
    pub filter_hi: f32,
    /// The [`MemoryFolder`] this memory is filed under, `None` for the top
    /// level. Defaulted so a `memories.json` written before folders existed
    /// still loads. A dangling id (its folder gone from under it) reads as
    /// unfiled rather than as invisible.
    #[serde(default)]
    pub folder: Option<u32>,
    /// The RTTY modem setup this memory was stored with; `None` for a memory
    /// stored in any other mode (and for a `memories.json` written before
    /// this existed, which recalls on whatever the modem is already set to).
    #[serde(default)]
    pub rtty: Option<RttyMemory>,
}

/// The RTTY modem setup captured alongside a memory stored in RTTY mode.
///
/// The dial position is only half of an RTTY memory: a commercial broadcast
/// (DWD weather, 50 baud / 450 Hz shift, reverse) decodes as nonsense on the
/// amateur defaults, so recalling the frequency without the setup would hand
/// the operator a station they still have to configure from notes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RttyMemory {
    pub baud: f32,
    pub shift_hz: f32,
    pub reverse: bool,
    pub afc: bool,
}

/// A named folder in the memory list. One level deep — a folder holds
/// memories, not other folders — and deleting one moves its contents back to
/// the top level rather than deleting them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryFolder {
    pub id: u32,
    pub name: String,
}

/// One entry of a band-stack register (PowerSDR-style: up to 3 per band,
/// pressing the band button again cycles them).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BandStackEntry {
    pub freq_hz: f64,
    pub mode: Mode,
    pub filter_lo: f32,
    pub filter_hi: f32,
}
