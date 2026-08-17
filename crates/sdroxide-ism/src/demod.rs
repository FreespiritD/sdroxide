//! Turning a captured burst into numbers a slicer can work on: instantaneous
//! frequency, and the few statistics that describe what kind of signal it is.

use sdroxide_dsp::Complex32;

/// Instantaneous frequency in Hz, one value per input sample after the first.
///
/// `arg(z[n] · conj(z[n−1]))` is the phase advanced in one sample interval, so
/// scaling by `rate / 2π` gives Hz. This is the whole of an FM discriminator;
/// the reason not to reuse [`sdroxide_dsp::FskDemod`] here is that it carries a
/// passband and a DC block sized for one 4800-baud amateur protocol, and both
/// would have to be undone.
pub fn discriminate(iq: &[Complex32], rate_hz: f64, out: &mut Vec<f32>) {
    out.clear();
    if iq.len() < 2 {
        return;
    }
    out.reserve(iq.len() - 1);
    let scale = (rate_hz / std::f64::consts::TAU) as f32;
    for w in iq.windows(2) {
        let d = w[1] * w[0].conj();
        out.push(d.im.atan2(d.re) * scale);
    }
}

/// What a burst looks like, before anything has tried to decode it.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    /// Carrier offset from the channel centre, Hz. The midpoint of the two FSK
    /// tones, which for any roughly balanced data is where the carrier is.
    pub center_hz: f32,
    /// Half the tone separation, Hz — the FSK deviation.
    pub dev_hz: f32,
    /// Fractional envelope swing, `(p90 − p10) / p90`, so it lands in 0..1.
    ///
    /// Small for a constant-envelope signal such as FSK, large for on-off
    /// keying, which puts its data there instead. Reported as a *measurement*
    /// and nothing more: it is not used to choose a demodulator, because at the
    /// SNRs a burst gate lets through, noise alone lifts an FSK burst's swing
    /// into the range an OOK burst occupies. Splitting the two apart needs a
    /// comparison against the channel's noise floor as well, and that is worth
    /// writing when there is an OOK protocol to test it against — not before.
    pub env_depth: f32,
    /// The part of the burst that is actually signal, as a range into the
    /// discriminator trace.
    ///
    /// A gated burst is signal with padding on both ends: the pre-trigger ring in
    /// front and whatever of the hangover survived the trim behind. Over
    /// noise-only samples the discriminator output is *uniform across the whole
    /// channel*, so including them drags the percentiles the carrier estimate is
    /// built from into the noise — on a short burst far enough to put the slicing
    /// level in the wrong tone. Everything downstream works on this range.
    pub signal: std::ops::Range<usize>,
}

/// Percentile of a slice, by value. `p` in 0.0..=1.0.
///
/// Selection rather than a sort: this runs a few times per burst on a few
/// thousand samples, and the caller never needs the rest of the order.
fn percentile(scratch: &mut Vec<f32>, src: &[f32], p: f32) -> f32 {
    if src.is_empty() {
        return 0.0;
    }
    scratch.clear();
    scratch.extend_from_slice(src);
    let k = ((scratch.len() - 1) as f32 * p).round() as usize;
    scratch.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    scratch[k]
}

/// Fraction of the burst's peak envelope a sample must reach to count as signal.
///
/// Half of the 90th-percentile magnitude. Well above any plausible noise floor
/// inside a burst the gate thought worth opening on — the gate wants at least
/// 6 dB — and well below the envelope of real 2-FSK, which is constant.
const SIGNAL_FRAC: f32 = 0.5;

/// Describe a burst from its samples and its discriminator output.
pub fn stats(iq: &[Complex32], disc: &[f32], scratch: &mut Vec<f32>) -> Stats {
    let mag: Vec<f32> = iq.iter().map(|z| z.norm()).collect();
    let m10 = percentile(scratch, &mag, 0.1);
    let m90 = percentile(scratch, &mag, 0.9);

    // Where the signal starts and stops inside the padded burst. `disc[i]` is the
    // phase step from `iq[i]` to `iq[i + 1]`, so it is signal only when both ends
    // are.
    let floor = m90 * SIGNAL_FRAC;
    let strong = |i: usize| mag[i] > floor && mag.get(i + 1).is_some_and(|m| *m > floor);
    let start = (0..disc.len()).find(|&i| strong(i)).unwrap_or(0);
    let end = (start..disc.len()).rev().find(|&i| strong(i)).map_or(disc.len(), |i| i + 1);
    let signal = start..end.max(start);

    // The 10th and 90th percentiles land inside the low and high tone
    // respectively for any data that is not wildly unbalanced, so their
    // midpoint is the carrier and their difference is the tone separation.
    // Using the mean instead would let one long run of identical bits pull the
    // slicing level into the wrong tone.
    let body = &disc[signal.clone()];
    let lo = percentile(scratch, body, 0.1);
    let hi = percentile(scratch, body, 0.9);

    Stats {
        center_hz: (lo + hi) / 2.0,
        dev_hz: (hi - lo) / 2.0,
        // Referred to p90 rather than to the median, so the result is bounded
        // by 1 and a burst whose envelope reaches zero cannot divide by it.
        env_depth: (m90 - m10) / m90.max(1e-12),
        signal,
    }
}

/// Measure the symbol period from the run-up of a burst, in samples.
///
/// Every protocol here opens with a run of alternating bits, so the
/// discriminator crosses the carrier once per symbol and the median interval
/// between crossings *is* the symbol period. Measuring it beats trusting the
/// nominal rate: these transmitters are crystal-referenced but the crystals are
/// cheap, and a tenth of a percent of rate error accumulates to a lost symbol
/// over a long frame.
///
/// `nominal` bounds the search — intervals more than 20 % away are noise
/// crossings or data, not preamble, and including them would drag the estimate.
/// Returns `None` when too few plausible crossings were found to trust.
///
/// # Why the mean and not the median
///
/// A crossing lands on a whole sample, so every gap is an integer and their
/// median is an integer too. At 14.5 samples per symbol that is a 3 % error
/// before the signal has done anything wrong — worse than the rate error this
/// function exists to measure. The median is still needed, but only to decide
/// which gaps are preamble; the *mean* of those averages the quantisation away.
pub fn estimate_sps(disc: &[f32], level: f32, nominal: f64) -> Option<f64> {
    // The preamble is at the front. Looking at the whole burst would average in
    // the payload, whose crossings are at multiples of the symbol period.
    let look = (disc.len() / 4).max(64).min(disc.len());
    let seg = &disc[..look];

    let (lo, hi) = (nominal * 0.8, nominal * 1.2);
    let mut gaps: Vec<f64> = Vec::new();
    let mut last: Option<usize> = None;
    for i in 1..seg.len() {
        let crossed = (seg[i - 1] < level) != (seg[i] < level);
        if !crossed {
            continue;
        }
        if let Some(p) = last {
            let g = (i - p) as f64;
            if g >= lo && g <= hi {
                gaps.push(g);
            }
        }
        last = Some(i);
    }
    if gaps.len() < 8 {
        return None;
    }
    let mid = gaps.len() / 2;
    gaps.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    let med = gaps[mid];

    // Average the gaps that agree with the median, which excludes a doubled gap
    // from a crossing lost in noise while keeping the quantisation spread that
    // has to be averaged out.
    let (sum, n) = gaps
        .iter()
        .filter(|g| (*g - med).abs() <= med * 0.15)
        .fold((0.0f64, 0usize), |(s, n), g| (s + g, n + 1));
    if n < 8 {
        return None;
    }
    Some(sum / n as f64)
}

/// Measure a burst's symbol rate with **no protocol to guess from**.
///
/// [`estimate_sps`] refines a rate a protocol has already declared, and is bounded
/// to ±20 % of it. That is the right tool for decoding and useless for
/// identifying: an unknown burst has no nominal rate, and the whole question is
/// what its rate *is*.
///
/// Every interval between level crossings is an integer number of symbols — one
/// for a lone symbol, more for a run — so the symbol period is the largest value
/// that nearly divides all of them. Rather than search for it, take a low
/// percentile of the intervals as a candidate (most crossings in real data are one
/// symbol apart) and then *verify* it: report a rate only if most intervals really
/// are close to an integer multiple. Without that check, noise crossings would
/// return a confident and meaningless number for any input at all.
///
/// Returns the symbol rate in Hz and the fraction of intervals that fitted, so a
/// caller can show how much to trust it.
///
/// The two constants below are what make it refuse rather than invent; both are
/// pinned by tests, one on real signals and one on noise.
/// Shortest symbol period, in samples, that can be measured rather than guessed.
const MIN_MEASURABLE_PERIOD: f64 = 4.0;

/// Fraction of crossing intervals that must land on an integer multiple of the
/// candidate period. Clean signals reach 0.9; white noise reaches about 0.66,
/// because with a two-sample period every *even* gap fits by accident — which is
/// why this is well above a half.
const MIN_FIT: f32 = 0.75;

pub fn estimate_baud_free(disc: &[f32], level: f32, rate_hz: f64) -> Option<(f64, f32)> {
    let mut gaps: Vec<f64> = Vec::new();
    let mut last: Option<usize> = None;
    for i in 1..disc.len() {
        if (disc[i - 1] < level) == (disc[i] < level) {
            continue;
        }
        if let Some(p) = last {
            // One sample is not a symbol at any rate a receiver can carry; it is
            // the discriminator rattling on noise.
            if i - p >= 2 {
                gaps.push((i - p) as f64);
            }
        }
        last = Some(i);
    }
    if gaps.len() < 12 {
        return None;
    }

    let mut sorted = gaps.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    // Low but not lowest: the very shortest gaps are where noise lives.
    let candidate = sorted[sorted.len() / 5];
    // A period of two or three samples cannot be *measured*, whatever it is: the
    // quantisation is a third of the answer, and white noise crosses the level
    // about that often, so accepting it means reporting a symbol rate for silence.
    // The cost is that a rate above a quarter of the channel rate — wireless M-Bus
    // mode T's 100 kcps in a 250 kHz survey tile — comes back as "unknown" rather
    // than wrong.
    if candidate < MIN_MEASURABLE_PERIOD {
        return None;
    }

    // Refine on the gaps that agree with the candidate as single symbols, so the
    // sample quantisation averages out.
    let (sum, n) = gaps
        .iter()
        .filter(|g| (*g - candidate).abs() <= candidate * 0.3)
        .fold((0.0f64, 0usize), |(s, c), g| (s + g, c + 1));
    if n < 6 {
        return None;
    }
    let period = sum / n as f64;

    // The verification: how many intervals are within a quarter symbol of an
    // integer multiple of this period.
    let fitted = gaps
        .iter()
        .filter(|g| {
            let k = (*g / period).round().max(1.0);
            (*g - k * period).abs() <= period * 0.25
        })
        .count();
    let frac = fitted as f32 / gaps.len() as f32;
    if frac < MIN_FIT {
        return None;
    }
    Some((rate_hz / period, frac))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 250_000.0;

    /// Synthesise 2-FSK from the definition — a phase accumulator advanced at
    /// one of two frequencies — rather than from anything the demodulator does.
    fn fsk(bits: &[u8], baud: f64, dev_hz: f64, offset_hz: f64, amp: f32) -> Vec<Complex32> {
        let sps = RATE / baud;
        let mut out = Vec::new();
        let mut phase = 0.0f64;
        for (i, &b) in bits.iter().enumerate() {
            let f = offset_hz + if b != 0 { dev_hz } else { -dev_hz };
            let n = ((i + 1) as f64 * sps).round() as usize - (i as f64 * sps).round() as usize;
            for _ in 0..n {
                phase += std::f64::consts::TAU * f / RATE;
                out.push(Complex32::new(amp * phase.cos() as f32, amp * phase.sin() as f32));
            }
        }
        out
    }

    #[test]
    fn the_discriminator_reads_back_the_deviation_and_the_offset() {
        let bits: Vec<u8> = (0..200).map(|i| (i % 2) as u8).collect();
        let iq = fsk(&bits, 17_241.0, 50_000.0, 12_000.0, 0.5);

        let mut disc = Vec::new();
        discriminate(&iq, RATE, &mut disc);
        let mut scratch = Vec::new();
        let s = stats(&iq, &disc, &mut scratch);

        assert!((s.center_hz - 12_000.0).abs() < 1_500.0, "offset read {} Hz", s.center_hz);
        assert!((s.dev_hz - 50_000.0).abs() < 3_000.0, "deviation read {} Hz", s.dev_hz);
    }

    /// The envelope measurement has to separate a constant-envelope signal from
    /// one that keys its carrier on and off, at least when both are clean. It is
    /// reported and not acted on — see the note on [`Stats::env_depth`].
    #[test]
    fn the_envelope_measurement_tells_a_keyed_carrier_from_a_steady_one() {
        let bits: Vec<u8> = (0..200).map(|i| (i % 2) as u8).collect();
        let steady_iq = fsk(&bits, 17_241.0, 50_000.0, 0.0, 0.5);

        // The same carrier, keyed on and off every symbol instead of shifted.
        let sps = (RATE / 17_241.0) as usize;
        let mut keyed_iq = Vec::new();
        let mut phase = 0.0f64;
        for i in 0..200 {
            let amp = if i % 2 == 0 { 0.5f32 } else { 0.0 };
            for _ in 0..sps {
                phase += std::f64::consts::TAU * 5_000.0 / RATE;
                keyed_iq.push(Complex32::new(amp * phase.cos() as f32, amp * phase.sin() as f32));
            }
        }

        let mut disc = Vec::new();
        let mut scratch = Vec::new();
        discriminate(&steady_iq, RATE, &mut disc);
        let steady = stats(&steady_iq, &disc, &mut scratch).env_depth;
        discriminate(&keyed_iq, RATE, &mut disc);
        let keyed = stats(&keyed_iq, &disc, &mut scratch).env_depth;

        assert!(steady < 0.1, "a constant envelope measured {steady:.3}");
        assert!(keyed > 0.9, "a keyed carrier measured {keyed:.3}");
    }

    /// The regression this module's `signal` range exists for.
    ///
    /// A gated burst arrives padded with noise on both ends, and over noise the
    /// discriminator output is uniform across the whole channel — so percentiles
    /// taken over the padded buffer are pulled towards the middle of the channel
    /// rather than towards the carrier. On a burst that is only half signal that
    /// put the estimate tens of kilohertz out, which is the slicing level in the
    /// wrong tone and a frame that cannot decode.
    #[test]
    fn a_burst_padded_with_noise_still_measures_its_carrier() {
        let bits: Vec<u8> = (0..120).map(|i| (i % 2) as u8).collect();
        let signal = fsk(&bits, 17_241.0, 50_000.0, 30_000.0, 0.5);

        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let u = ((seed >> 11) as f64 / (1u64 << 53) as f64) as f32;
            Complex32::new((u - 0.5) * 0.004, (u * 7.0 % 1.0 - 0.5) * 0.004)
        };

        // Half as much padding again as there is signal, split either side —
        // more than a real gate leaves, so the fix is tested past what it has to
        // survive.
        let pad = signal.len() / 2;
        let mut iq: Vec<Complex32> = (0..pad).map(|_| noise()).collect();
        iq.extend_from_slice(&signal);
        iq.extend((0..pad).map(|_| noise()));

        let mut disc = Vec::new();
        discriminate(&iq, RATE, &mut disc);
        let mut scratch = Vec::new();
        let st = stats(&iq, &disc, &mut scratch);

        assert!(
            (st.center_hz - 30_000.0).abs() < 4_000.0,
            "carrier read {} Hz through the padding, not 30 kHz",
            st.center_hz
        );
        assert!((st.dev_hz - 50_000.0).abs() < 6_000.0, "deviation read {} Hz", st.dev_hz);

        // And the range really does exclude the padding.
        assert!(st.signal.start >= pad - 8, "signal starts at {}, pad is {pad}", st.signal.start);
        assert!(st.signal.end <= pad + signal.len() + 8, "signal ends at {}", st.signal.end);
    }

    /// The symbol period has to be measured, not assumed — and measured close
    /// enough that a long frame does not drift a whole symbol.
    #[test]
    fn the_symbol_period_is_measured_from_the_preamble() {
        let nominal = 17_241.0;
        for err in [-0.004f64, 0.0, 0.004] {
            let actual = nominal * (1.0 + err);
            // Preamble, then payload that is deliberately not alternating.
            let mut bits: Vec<u8> = (0..64).map(|i| (i % 2) as u8).collect();
            bits.extend([1, 1, 1, 0, 0, 1, 0, 1, 1, 0, 0, 0, 1, 1, 1, 1].repeat(8));
            let iq = fsk(&bits, actual, 50_000.0, 0.0, 0.5);

            let mut disc = Vec::new();
            discriminate(&iq, RATE, &mut disc);
            let sps = estimate_sps(&disc, 0.0, RATE / nominal).expect("no estimate");
            let want = RATE / actual;
            assert!(
                (sps - want).abs() / want < 0.01,
                "rate error {err}: measured {sps:.3} samples/symbol, truth {want:.3}"
            );
        }
    }

    /// The free estimator has to find a rate it was never told, across the whole
    /// range these protocols use — that is the point of it.
    #[test]
    fn the_free_estimator_finds_a_rate_it_was_not_given() {
        for baud in [4_800.0f64, 9_600.0, 17_241.0, 32_768.0] {
            // Preamble then payload, so the intervals are a mix of one symbol and
            // several — which is what the integer-multiple check has to cope with.
            let mut bits: Vec<u8> = (0..64).map(|i| (i % 2) as u8).collect();
            bits.extend([1, 1, 0, 1, 0, 0, 0, 1, 1, 1, 0, 0, 1, 0, 1, 1].repeat(6));
            let iq = fsk(&bits, baud, 30_000.0, 0.0, 0.5);

            let mut disc = Vec::new();
            discriminate(&iq, RATE, &mut disc);
            let (got, frac) = estimate_baud_free(&disc, 0.0, RATE)
                .unwrap_or_else(|| panic!("no estimate at {baud}"));
            assert!(
                (got - baud).abs() / baud < 0.05,
                "measured {got:.0} baud for a {baud:.0} baud signal (fit {frac:.2})"
            );
            assert!(frac > 0.8, "fit was only {frac:.2} on a clean signal");
        }
    }

    /// And it must refuse rather than invent one. A confident wrong rate is worse
    /// than no rate: it is what a protocol module would be written against.
    #[test]
    fn the_free_estimator_refuses_on_noise() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let disc: Vec<f32> = (0..8192)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                ((seed >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
            })
            .collect();
        assert!(estimate_baud_free(&disc, 0.0, RATE).is_none(), "noise produced a symbol rate");
    }

    /// Noise must not produce a confident symbol period.
    #[test]
    fn noise_yields_no_symbol_period() {
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        let disc: Vec<f32> = (0..4096)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                ((seed >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
            })
            .collect();
        // White noise crosses zero about every other sample, nowhere near a
        // 14.5-sample symbol, so nothing should survive the plausibility bound.
        assert!(estimate_sps(&disc, 0.0, RATE / 17_241.0).is_none());
    }
}
