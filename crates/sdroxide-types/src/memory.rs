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
