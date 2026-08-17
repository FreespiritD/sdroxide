//! Which front end a dial setting asks for, and what the downconverter has to
//! do about it.
//!
//! All arithmetic, no I/O: the awkward parts of the VHF path are the crossover,
//! the two-stage tuning and the spectrum inversion, and every one of them is a
//! decision that can be made — and tested — without a receiver attached.
//!
//! # Coarse LO, fine downconverter
//!
//! The obvious design pins the downconverter on the tuner's IF and lets the
//! dial ride entirely on the tuner. It is unaffordable. An R828D retune sleeps
//! up to 20 ms waiting for PLL lock, on the thread servicing the bulk endpoint,
//! which holds about 32 ms of samples — and dragging the panadapter emits
//! retunes as fast as that thread can spin.
//!
//! So the tuner parks and the downconverter slides inside the IF passband:
//! small dial movements cost nothing, exactly as on HF, and the tuner only
//! follows when the dial leaves the window.

/// The R828D's IF carrier with the 8 MHz filter selected.
///
/// Not chosen here — it is what the tuner driver's `set_bandwidth(8 MHz)`
/// reports, and upstream's `R828D_IF_CARRIER` states the same figure.
pub const IF_CENTER_HZ: f64 = 4_570_000.0;

/// The IF filter's width in that mode, and so the width of the VHF full-band
/// display.
pub const IF_BW_HZ: f64 = 8_000_000.0;

/// How far the downconverter may slide either side of the IF carrier before the
/// tuner has to follow.
///
/// ±1 MHz keeps the whole 2.025 MHz output inside the flat part of the filter
/// and well clear of the ADC's own DC region. It exists so that small dial
/// movements stay free — see the note on this module.
pub const FINE_SPAN_HZ: f64 = 1_000_000.0;

/// The tuner cannot reach below this, so the automatic crossover never hands it
/// a frequency it will not lock on. `librtlsdr` publishes the same floor.
pub const TUNER_MIN_HZ: f64 = 24_000_000.0;
/// Top of the R828D's range.
pub const TUNER_MAX_HZ: f64 = 1_766_000_000.0;

/// Hysteresis at the crossover, so a dial parked on it cannot oscillate between
/// the two front ends. Each switch costs a tuner bring-up and a gap in the
/// stream, which is the same reason the RTL-SDR backend has one.
pub const CROSSOVER_HYSTERESIS_HZ: f64 = 500_000.0;

/// The top of the IF passband, which is what has to fit under the ADC's
/// Nyquist limit for VHF to be possible at all.
pub const IF_TOP_HZ: f64 = IF_CENTER_HZ + IF_BW_HZ / 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Hf,
    Vhf,
}

/// Where the front end is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandState {
    pub band: Band,
    /// The RF frequency the tuner is parked on. Meaningless in HF.
    pub lo_dial_hz: f64,
}

impl Default for BandState {
    fn default() -> Self {
        BandState { band: Band::Hf, lo_dial_hz: 0.0 }
    }
}

/// What a dial setting asks of the hardware.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandPlan {
    pub band: Band,
    pub lo_dial_hz: f64,
    /// Whether the tuner's PLL has to be reprogrammed — the expensive bit.
    pub move_lo: bool,
    /// Where the wideband downconverter must be centred, in ADC baseband Hz.
    pub ddc_center_hz: f64,
    /// Whether the front end mirrors the spectrum.
    pub conjugate: bool,
}

/// Whether this ADC clock leaves room for the tuner's IF under Nyquist.
///
/// Derived rather than declared: the 8 MHz IF filter reaches up to
/// [`IF_TOP_HZ`], and sampling below twice that folds the top of the passband
/// back over the wanted signal. At 16.2 Msps it does not fit; from 32.4 Msps up
/// it does, with room to spare.
pub fn adc_rate_allows_vhf(adc_rate_hz: f64) -> bool {
    adc_rate_hz / 2.0 > IF_TOP_HZ
}

/// Whether the VHF front end can be used at all.
pub fn vhf_available(adc_rate_hz: f64, has_tuner: bool) -> bool {
    has_tuner && adc_rate_allows_vhf(adc_rate_hz)
}

/// Where the automatic switch happens.
///
/// Upstream's rule is the ADC's Nyquist limit, which is where direct sampling
/// runs out. Below a 48 Msps clock that lands under the tuner's own floor, so
/// the floor wins and the receiver has an honest gap between its two ranges
/// rather than a band it claims and cannot hear.
pub fn crossover_hz(adc_rate_hz: f64) -> f64 {
    (adc_rate_hz / 2.0).max(TUNER_MIN_HZ)
}

/// The ranges this receiver can actually tune, for `DeviceCaps`.
pub fn freq_ranges(adc_rate_hz: f64, vhf: bool) -> Vec<(f64, f64)> {
    let nyquist = adc_rate_hz / 2.0;
    let hf = (0.0, nyquist);
    if !vhf {
        return vec![hf];
    }
    let x = crossover_hz(adc_rate_hz);
    // When the crossover sits above Nyquist the two ranges do not meet, and
    // saying so is the point: that gap is real and unreachable.
    vec![hf, (x, TUNER_MAX_HZ)]
}

/// Work out what `dial_hz` requires, given where the front end is now.
pub fn plan(dial_hz: f64, adc_rate_hz: f64, vhf: bool, cur: BandState) -> BandPlan {
    let hf_plan = |move_lo: bool| BandPlan {
        band: Band::Hf,
        lo_dial_hz: 0.0,
        move_lo,
        ddc_center_hz: dial_hz,
        conjugate: false,
    };

    if !vhf {
        return hf_plan(cur.band == Band::Vhf);
    }

    // Cross only once the dial is clearly on the other side, so a dial parked
    // on the boundary cannot flip the front end back and forth.
    let x = crossover_hz(adc_rate_hz);
    let want_vhf = match cur.band {
        Band::Hf => dial_hz >= x + CROSSOVER_HYSTERESIS_HZ,
        Band::Vhf => dial_hz >= x - CROSSOVER_HYSTERESIS_HZ,
    };
    if !want_vhf {
        return hf_plan(cur.band == Band::Vhf);
    }

    let entering = cur.band == Band::Hf;
    let left_window = (dial_hz - cur.lo_dial_hz).abs() > FINE_SPAN_HZ;
    let move_lo = entering || left_window;
    let lo = if move_lo { dial_hz.clamp(TUNER_MIN_HZ, TUNER_MAX_HZ) } else { cur.lo_dial_hz };

    BandPlan {
        band: Band::Vhf,
        lo_dial_hz: lo,
        move_lo,
        // High-side injection: the tuner's LO sits *above* the wanted signal,
        // so the IF runs backwards and the downconverter has to move down as
        // the dial moves up. This sign and `conjugate` are the same physical
        // fact — get one right and the other wrong and the radio tunes
        // backwards while looking like it works.
        ddc_center_hz: IF_CENTER_HZ - (dial_hz - lo),
        conjugate: true,
    }
}

/// Which analyser bins to publish for the full-band display, and the RF axis
/// they sit on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WideMap {
    pub lo_bin: usize,
    pub hi_bin: usize,
    /// Whether the slice ascends in RF, or has to be reversed first.
    pub reverse: bool,
    pub center_hz: f64,
    pub span_hz: f64,
}

/// Map the analyser's frame onto the axis the front end is actually on.
///
/// In HF the analyser's own axis is the answer: DC to Nyquist. In VHF it is
/// not — the analyser is looking at the tuner's IF, so the display is the slice
/// of bins covering the 8 MHz filter, reversed because ascending IF is
/// descending RF, and labelled with the RF the tuner is parked on.
///
/// The centre and span are derived from the bins that survive the clamp rather
/// than from the nominal window, so a slice cut short at the band edge still
/// carries a truthful axis.
pub fn wide_map(band: Band, lo_dial_hz: f64, adc_rate_hz: f64, bins: usize) -> WideMap {
    if band == Band::Hf {
        return WideMap {
            lo_bin: 0,
            hi_bin: bins,
            reverse: false,
            center_hz: adc_rate_hz / 4.0,
            span_hz: adc_rate_hz / 2.0,
        };
    }

    // The analyser's `bins` cover DC..Nyquist.
    let bin_hz = adc_rate_hz / 2.0 / bins as f64;
    let lo_bin = (((IF_CENTER_HZ - IF_BW_HZ / 2.0) / bin_hz).floor().max(0.0) as usize).min(bins);
    let hi_bin = ((((IF_CENTER_HZ + IF_BW_HZ / 2.0) / bin_hz).ceil() as usize) + 1).min(bins);
    let hi_bin = hi_bin.max(lo_bin);

    // Centre of the bins actually taken, expressed as an IF, then reflected
    // through the tuner's park frequency into RF.
    let mid_if = bin_hz * (lo_bin + hi_bin) as f64 / 2.0;
    WideMap {
        lo_bin,
        hi_bin,
        reverse: true,
        center_hz: lo_dial_hz + (IF_CENTER_HZ - mid_if),
        span_hz: bin_hz * (hi_bin - lo_bin) as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADC: f64 = 64_800_000.0;

    fn vhf_state(lo: f64) -> BandState {
        BandState { band: Band::Vhf, lo_dial_hz: lo }
    }

    /// A dial parked on the crossover must not flip the front end back and
    /// forth. Each switch is a tuner bring-up and a gap in the stream.
    #[test]
    fn the_crossover_does_not_oscillate_on_the_boundary() {
        let x = crossover_hz(ADC);
        let mut st = BandState::default();
        let mut changes = 0;

        // Walk up through the crossover and back down again, feeding each
        // decision back in as the current state — which is what actually
        // happens when someone drags the dial.
        let steps: Vec<f64> =
            (-200..=200).chain((-200..=200).rev()).map(|k| x + f64::from(k) * 10_000.0).collect();
        for hz in steps {
            let p = plan(hz, ADC, true, st);
            if p.band != st.band {
                changes += 1;
            }
            st = BandState { band: p.band, lo_dial_hz: p.lo_dial_hz };
        }

        assert!(changes <= 2, "the front end switched {changes} times over one sweep and back");
    }

    /// The crossover must never land somewhere the tuner cannot reach, or the
    /// receiver would switch to VHF below the tuner's floor and hear nothing.
    #[test]
    fn the_crossover_never_lands_below_the_tuners_floor() {
        for rate in crate::device::ADC_RATES {
            assert!(
                crossover_hz(*rate) >= TUNER_MIN_HZ,
                "a {rate} Hz clock crosses over at {}",
                crossover_hz(*rate)
            );
        }
    }

    /// Small dial movements must stay free — the whole reason the
    /// downconverter slides instead of the tuner.
    #[test]
    fn the_tuner_only_moves_when_the_dial_leaves_the_window() {
        let st = vhf_state(145_000_000.0);

        for delta in [0.0, 500_000.0, -900_000.0, FINE_SPAN_HZ] {
            let p = plan(145_000_000.0 + delta, ADC, true, st);
            assert!(!p.move_lo, "a {delta} Hz nudge reprogrammed the PLL");
            assert_eq!(p.lo_dial_hz, 145_000_000.0);
            // The downconverter took up the slack instead.
            assert!((p.ddc_center_hz - (IF_CENTER_HZ - delta)).abs() < 1.0);
        }

        // Past the window the tuner has to follow, and the downconverter goes
        // back to the middle of the IF.
        let p = plan(146_100_000.0, ADC, true, st);
        assert!(p.move_lo);
        assert_eq!(p.lo_dial_hz, 146_100_000.0);
        assert!((p.ddc_center_hz - IF_CENTER_HZ).abs() < 1.0);
    }

    /// The sliding downconverter must not wander outside the IF filter, or the
    /// signal is attenuated by the very filter that selects it.
    #[test]
    fn the_downconverter_stays_inside_the_if_filter() {
        let st = vhf_state(145_000_000.0);
        // The widest output the converter produces, so both edges are checked.
        let half_out = 2_025_000.0 / 2.0;

        for k in -100..=100 {
            let dial = 145_000_000.0 + f64::from(k) * 10_000.0;
            let p = plan(dial, ADC, true, st);
            assert!((p.ddc_center_hz - IF_CENTER_HZ).abs() <= FINE_SPAN_HZ + 1.0);
            assert!(
                p.ddc_center_hz - half_out > IF_CENTER_HZ - IF_BW_HZ / 2.0,
                "the low edge fell out of the IF filter at {dial}"
            );
            assert!(
                p.ddc_center_hz + half_out < IF_CENTER_HZ + IF_BW_HZ / 2.0,
                "the high edge fell out of the IF filter at {dial}"
            );
        }
    }

    /// The inversion, stated as a test so it cannot be quietly "fixed" by
    /// someone who has not read the comment next to it.
    #[test]
    fn tuning_up_moves_the_downconverter_down() {
        let st = vhf_state(145_000_000.0);
        let lower = plan(144_800_000.0, ADC, true, st).ddc_center_hz;
        let higher = plan(145_200_000.0, ADC, true, st).ddc_center_hz;
        assert!(
            higher < lower,
            "high-side injection inverts the IF: {higher} should be below {lower}"
        );
        // And the same fact is reported to the converter.
        assert!(plan(145_200_000.0, ADC, true, st).conjugate);
        assert!(!plan(10_000_000.0, ADC, true, st).conjugate, "HF is not inverted");
    }

    #[test]
    fn a_receiver_without_a_tuner_never_leaves_hf() {
        let p = plan(145_000_000.0, ADC, false, BandState::default());
        assert_eq!(p.band, Band::Hf);
        assert!(!p.conjugate);
        assert_eq!(freq_ranges(ADC, false), vec![(0.0, 32_400_000.0)]);
    }

    /// The IF has to fit under Nyquist, and at the lowest offered clock it does
    /// not.
    #[test]
    fn vhf_needs_an_adc_clock_with_room_for_the_if() {
        assert!(!adc_rate_allows_vhf(16_200_000.0), "8.1 MHz Nyquist cannot hold an 8.57 MHz IF");
        assert!(adc_rate_allows_vhf(32_400_000.0));
        assert!(adc_rate_allows_vhf(64_800_000.0));
        assert!(!vhf_available(64_800_000.0, false), "no tuner, no VHF");
    }

    /// At the default clock the two ranges meet; at a slower one they do not,
    /// and the gap is reported rather than papered over.
    #[test]
    fn the_published_ranges_admit_the_gap_when_there_is_one() {
        assert_eq!(freq_ranges(ADC, true), vec![(0.0, 32_400_000.0), (32_400_000.0, TUNER_MAX_HZ)]);

        let slow = freq_ranges(32_400_000.0, true);
        assert_eq!(slow, vec![(0.0, 16_200_000.0), (24_000_000.0, TUNER_MAX_HZ)]);
        assert!(slow[0].1 < slow[1].0, "16.2–24 MHz is genuinely unreachable here");
    }

    #[test]
    fn the_hf_strip_is_the_whole_nyquist_band() {
        let m = wide_map(Band::Hf, 0.0, ADC, 4096);
        assert_eq!((m.lo_bin, m.hi_bin, m.reverse), (0, 4096, false));
        assert_eq!(m.center_hz, ADC / 4.0);
        assert_eq!(m.span_hz, ADC / 2.0);
    }

    /// In VHF the strip is a slice of the analyser centred on the tuner, and
    /// it runs backwards.
    #[test]
    fn the_vhf_strip_is_centred_on_the_tuner_and_reversed() {
        let bins = 4096;
        let m = wide_map(Band::Vhf, 145_000_000.0, ADC, bins);

        assert!(m.reverse, "ascending IF is descending RF");
        assert!(m.lo_bin < m.hi_bin && m.hi_bin <= bins);
        // Centred on the dial, to within a bin.
        let bin_hz = ADC / 2.0 / bins as f64;
        assert!(
            (m.center_hz - 145_000_000.0).abs() < bin_hz,
            "the strip is centred on {} rather than the tuner",
            m.center_hz
        );
        // And about as wide as the filter that shapes it.
        assert!((m.span_hz - IF_BW_HZ).abs() < 2.0 * bin_hz, "span was {}", m.span_hz);
    }
}
