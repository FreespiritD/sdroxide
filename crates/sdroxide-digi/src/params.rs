//! Per-mode FT8/FT4 protocol timing.

use sdroxide_types::{Js8Speed, Mode};

/// mfsk-core works entirely at this sample rate.
pub const DECODE_RATE: f64 = 12_000.0;

/// Frequency search window for decode/display (Hz within the passband).
pub const AUDIO_MIN_HZ: f32 = 100.0;
pub const AUDIO_MAX_HZ: f32 = 3300.0;

#[derive(Debug, Clone, Copy)]
pub struct DigiParams {
    pub mode: Mode,
    /// Slot length in seconds (FT8 15, FT4 7.5).
    pub slot_s: f64,
    /// Transmit start offset into the slot (FT8 0, FT4 0.5).
    pub tx_offset_s: f64,
    /// Nominal on-air burst length in seconds.
    pub burst_s: f64,
}

impl DigiParams {
    /// Timing for the modes whose slot length is fixed by the mode itself.
    ///
    /// The fallback arm used to rewrite `mode` to [`Mode::Ft8`], which combined
    /// with `make_digi`'s fall-through to `DigiController` meant any mode that
    /// forgot a branch quietly became FT8 — no error, no log line, just a
    /// receiver listening for the wrong protocol. It now keeps the mode it was
    /// given and complains in debug builds.
    pub fn for_mode(mode: Mode) -> Self {
        match mode {
            Mode::Ft4 => DigiParams { mode, slot_s: 7.5, tx_offset_s: 0.5, burst_s: 4.48 },
            // FT2 is FT4 at double the symbol rate: 105 × 288 / 12000 = 2.52 s
            // of signal in a 3.75 s slot, keyed 0.1 s in (Decodium's
            // `Modulator.cpp` sets `delay_ms=100` for FT2 against FT4's 300).
            // That leaves 1.13 s for decode and turnaround, which is the whole
            // point of the mode.
            Mode::Ft2 => DigiParams { mode, slot_s: 3.75, tx_offset_s: 0.1, burst_s: 2.52 },
            // Symbol 0 is nominally 0.5 s into the slot (matches WSJT-X /
            // mfsk-core dt reference).
            Mode::Ft8 => DigiParams { mode, slot_s: 15.0, tx_offset_s: 0.5, burst_s: 12.64 },
            // WSPR: 162 symbols of 8192/12000 s, starting one second into a
            // two-minute slot. The burst fills all but nine seconds of it,
            // which is why there is almost no latitude in when to key.
            Mode::Wspr => DigiParams {
                mode,
                slot_s: sdroxide_types::WSPR_SLOT_S,
                tx_offset_s: sdroxide_types::WSPR_TX_OFFSET_S,
                burst_s: sdroxide_types::WSPR_BURST_S,
            },
            other => {
                debug_assert!(
                    false,
                    "DigiParams::for_mode({other:?}) has no arm — add one rather than \
                     inheriting FT8's timing"
                );
                DigiParams { mode, slot_s: 15.0, tx_offset_s: 0.5, burst_s: 12.64 }
            }
        }
    }

    /// Timing for JS8, whose slot length is a runtime setting rather than a
    /// distinct [`Mode`] — so [`DigiParams::for_mode`] cannot express it.
    pub fn for_js8(speed: Js8Speed) -> Self {
        DigiParams {
            mode: Mode::Js8,
            slot_s: speed.slot_s(),
            tx_offset_s: speed.start_delay_s(),
            burst_s: speed.burst_s(),
        }
    }

    /// Samples of 12 kHz audio in one slot.
    pub fn slot_samples(&self) -> usize {
        (self.slot_s * DECODE_RATE) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digi_params_never_lies_about_the_mode() {
        // The regression this guards: the old fallback rewrote `mode` to
        // `Mode::Ft8`, so a caller could ask for one mode and silently get
        // another's timing *and* another's decoder.
        assert_eq!(DigiParams::for_mode(Mode::Ft8).mode, Mode::Ft8);
        assert_eq!(DigiParams::for_mode(Mode::Ft4).mode, Mode::Ft4);
        assert_eq!(DigiParams::for_mode(Mode::Ft2).mode, Mode::Ft2);
        assert_eq!(DigiParams::for_js8(Js8Speed::Normal).mode, Mode::Js8);
    }

    /// Every slotted mode's burst has to start and finish inside its slot, and
    /// its slot has to divide the minute or the two ends drift apart.
    #[test]
    fn slotted_timing_closes_on_the_minute() {
        for mode in [Mode::Ft8, Mode::Ft4, Mode::Ft2] {
            let p = DigiParams::for_mode(mode);
            assert!(p.tx_offset_s + p.burst_s < p.slot_s, "{mode:?} burst overruns its slot");
            assert!((60.0 / p.slot_s).fract() < 1e-9, "{mode:?} slot does not divide a minute");
        }
    }

    #[test]
    fn js8_timing_follows_the_speed() {
        for speed in Js8Speed::ALL {
            let p = DigiParams::for_js8(speed);
            assert_eq!(p.slot_s, speed.slot_s(), "{}", speed.label());
            assert_eq!(p.tx_offset_s, speed.start_delay_s(), "{}", speed.label());
            // The burst plus its start delay has to fit inside the slot.
            assert!(p.tx_offset_s + p.burst_s < p.slot_s, "{}", speed.label());
        }
    }
}
