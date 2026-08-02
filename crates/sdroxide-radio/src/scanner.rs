//! The arithmetic behind a frequency scan: where to put the front end, and what
//! in the resulting spectrum counts as a busy channel.
//!
//! Kept apart from the engine's state machine because this is the part with
//! answers that can be checked. The state machine decides *when* to look; these
//! decide *where*, and whether what came back was a signal.

/// Fraction of the device span either side of the centre that a sweep trusts.
/// The outer edges are in the anti-alias filter's roll-off, where a real signal
/// reads low and might be missed.
const USABLE_HALF: f64 = 0.4;
/// How far the front end moves between slices, as a fraction of the span. Less
/// than twice [`USABLE_HALF`], so consecutive slices overlap rather than merely
/// touching — a signal exactly on a seam is then seen twice instead of not at
/// all.
const SLICE_STRIDE: f64 = 0.7;
/// Bins either side of DC ignored when hunting for peaks. A zero-IF front end
/// piles its LO leakage there, and it is not a station.
const DC_GUARD_BINS: usize = 4;

/// Where to put the hardware centre, in order, to cover `lo..hi` with a front
/// end that sees `span_hz` at a time.
///
/// One slice when the whole range already fits, which is the common case on a
/// wideband receiver: scanning 2 m at 1.536 Msps is then a single tune and one
/// look at the FFT, not a hundred and sixty visits.
pub fn slice_centers(lo: f64, hi: f64, span_hz: f64) -> Vec<f64> {
    if !(lo.is_finite() && hi.is_finite()) || hi <= lo || !(span_hz > 0.0) {
        return Vec::new();
    }
    let usable = span_hz * USABLE_HALF;
    if hi - lo <= usable * 2.0 {
        return vec![(lo + hi) / 2.0];
    }
    let stride = span_hz * SLICE_STRIDE;
    let mut out = Vec::new();
    let mut c = lo + usable;
    // `<` rather than `<=` on the far edge, plus the unconditional last slice
    // below: the final step is placed to end exactly on `hi` instead of running
    // past it, so the top of the range is never searched at the roll-off.
    while c + usable < hi {
        out.push(c);
        c += stride;
    }
    out.push(hi - usable);
    out
}

/// Frequencies inside `lo..hi` where the spectrum says something is on the air.
///
/// `bins` are dBFS, frequency-ascending, covering `center_hz ± span_hz/2`. The
/// test is *channel* power — the total across a channel's worth of bins, not the
/// height of one — because that is the same quantity the receiver's own squelch
/// measures, so one threshold in one unit serves the search, the confirmation
/// and the audio gate. A single-bin test would instead make wide signals look
/// weak and narrow ones look strong, and the threshold would mean something
/// different for every mode.
///
/// Results are snapped to the `step_hz` channel grid and deduplicated, so a
/// signal spread across several bins yields one channel rather than a cluster.
pub fn busy_channels(
    bins: &[f32],
    center_hz: f64,
    span_hz: f64,
    lo: f64,
    hi: f64,
    bandwidth_hz: f64,
    step_hz: f64,
    threshold_db: f32,
) -> Vec<f64> {
    let n = bins.len();
    if n == 0 || !(span_hz > 0.0) || !(step_hz > 0.0) {
        return Vec::new();
    }
    let bin_hz = span_hz / n as f64;
    let width = ((bandwidth_hz / bin_hz).ceil() as usize).clamp(1, n);
    let base = center_hz - span_hz / 2.0;

    // Channel power at each window position, as dB again.
    let linear: Vec<f64> = bins.iter().map(|&d| 10f64.powf(d as f64 / 10.0)).collect();
    let mut running: f64 = linear[..width].iter().sum();
    let mut channel = Vec::with_capacity(n - width + 1);
    channel.push(running);
    for i in width..n {
        running += linear[i] - linear[i - width];
        channel.push(running);
    }

    let dc = n / 2;
    let usable = span_hz * USABLE_HALF;
    let mut out: Vec<f64> = Vec::new();
    let mut run_start: Option<usize> = None;
    for i in 0..=channel.len() {
        // The window's centre is what its power belongs to.
        let over = i < channel.len() && 10.0 * channel[i].log10() >= threshold_db as f64;
        match (over, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(start)) => {
                // A window narrower than the signal reads the same total at
                // every position that covers all of it, so the run has a
                // plateau rather than a peak; its middle is the channel. Taking
                // either end instead would bias the answer by half a window,
                // which at a 12.5 kHz grid is enough to snap to the wrong
                // channel.
                let best = (start..i).fold(0.0f64, |m, k| m.max(channel[k]));
                let plateau = (start..i).filter(|&k| channel[k] >= best * 0.9);
                let (first, last) = (
                    plateau.clone().next().expect("a non-empty run"),
                    plateau.last().expect("a non-empty run"),
                );
                let mid = (first + last) / 2 + width / 2;
                let f = base + mid as f64 * bin_hz;
                let near_dc = mid.abs_diff(dc) <= DC_GUARD_BINS + width / 2;
                if !near_dc && (f - center_hz).abs() <= usable && f >= lo && f <= hi {
                    let snapped = (f / step_hz).round() * step_hz;
                    if snapped >= lo
                        && snapped <= hi
                        && out.last().is_none_or(|&p| (snapped - p).abs() > step_hz / 2.0)
                    {
                        out.push(snapped);
                    }
                }
                run_start = None;
            }
            _ => {}
        }
    }
    out
}

/// Channel power (dBFS) around `at_hz`, from the same spectrum
/// [`busy_channels`] reads.
///
/// The fallback level reading for an engine with no audio chain at all — a
/// headless one with no sound device, which still has a panadapter and so still
/// has evidence. Returns `None` when the frequency is outside the span.
pub fn channel_power_db(
    bins: &[f32],
    center_hz: f64,
    span_hz: f64,
    at_hz: f64,
    bandwidth_hz: f64,
) -> Option<f32> {
    let n = bins.len();
    if n == 0 || !(span_hz > 0.0) {
        return None;
    }
    let bin_hz = span_hz / n as f64;
    let width = ((bandwidth_hz / bin_hz).ceil() as usize).clamp(1, n);
    let mid = ((at_hz - (center_hz - span_hz / 2.0)) / bin_hz).round();
    if !(0.0..n as f64).contains(&mid) {
        return None;
    }
    let start = (mid as usize).saturating_sub(width / 2).min(n - width);
    let total: f64 = bins[start..start + width].iter().map(|&d| 10f64.powf(d as f64 / 10.0)).sum();
    Some((10.0 * (total + 1e-30).log10()) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPAN: f64 = 1_536_000.0;

    #[test]
    fn a_range_inside_one_span_is_a_single_tune() {
        let c = slice_centers(144e6, 145e6, SPAN);
        assert_eq!(c, vec![144.5e6], "1 MHz fits inside 0.8 x 1.536 MHz");
    }

    /// Every frequency in the range has to fall inside some slice's usable
    /// window, or the scan quietly has holes in it.
    #[test]
    fn slices_cover_the_whole_range() {
        for (lo, hi) in [(144e6, 146e6), (430e6, 440e6), (88e6, 108e6), (144e6, 144.9e6)] {
            let centers = slice_centers(lo, hi, SPAN);
            assert!(!centers.is_empty(), "{lo}..{hi}");
            let usable = SPAN * USABLE_HALF;
            let mut f = lo;
            while f <= hi {
                assert!(
                    centers.iter().any(|&c| (f - c).abs() <= usable),
                    "{f} is in no slice of {lo}..{hi}"
                );
                f += 1_000.0;
            }
            // And nothing is searched outside the range's own span plus a slice.
            for &c in &centers {
                assert!(c > lo - SPAN && c < hi + SPAN, "slice {c} is nowhere near {lo}..{hi}");
            }
        }
    }

    #[test]
    fn a_backwards_or_empty_range_has_no_slices() {
        assert!(slice_centers(146e6, 144e6, SPAN).is_empty());
        assert!(slice_centers(144e6, 144e6, SPAN).is_empty());
        assert!(slice_centers(144e6, 146e6, 0.0).is_empty());
    }

    /// A spectrum with one signal in it: `n` bins of noise floor with a carrier
    /// of `width` bins at `at`.
    fn spectrum(n: usize, floor: f32, at: usize, width: usize, peak: f32) -> Vec<f32> {
        let mut v = vec![floor; n];
        for b in v.iter_mut().skip(at).take(width) {
            *b = peak;
        }
        v
    }

    #[test]
    fn a_carrier_is_found_on_the_channel_grid() {
        let n = 4096;
        // 12.5 kHz above the centre, at a 1.536 MHz span: 375 Hz per bin.
        let at = n / 2 + 33;
        let bins = spectrum(n, -110.0, at, 20, -60.0);
        let found =
            busy_channels(&bins, 145_000_000.0, SPAN, 144e6, 146e6, 12_500.0, 12_500.0, -70.0);
        assert_eq!(found.len(), 1, "one signal, one channel: {found:?}");
        assert!(
            (found[0] - 145_012_500.0).abs() < 1.0,
            "snapped to {} rather than 145.0125 MHz",
            found[0]
        );
    }

    #[test]
    fn a_quiet_band_yields_nothing() {
        let bins = vec![-110.0f32; 4096];
        assert!(
            busy_channels(&bins, 145e6, SPAN, 144e6, 146e6, 12_500.0, 12_500.0, -70.0).is_empty()
        );
    }

    /// The point of measuring channel power rather than peak bin height: a wide
    /// signal and a narrow one of the same total power have to read the same, or
    /// one threshold cannot serve every mode.
    #[test]
    fn wide_and_narrow_signals_of_equal_power_read_alike() {
        let n = 4096;
        let narrow = spectrum(n, -140.0, n / 2 + 200, 4, -60.0);
        // Ten times the bins at a tenth the power each: same channel total.
        let wide = spectrum(n, -140.0, n / 2 + 200, 40, -70.0);
        let args = (145e6, SPAN, 100e6, 200e6, 20_000.0, 5_000.0);
        for t in [-56.0f32, -54.0] {
            let a = busy_channels(&narrow, args.0, args.1, args.2, args.3, args.4, args.5, t);
            let b = busy_channels(&wide, args.0, args.1, args.2, args.3, args.4, args.5, t);
            assert_eq!(a.is_empty(), b.is_empty(), "threshold {t}: {a:?} vs {b:?}");
        }
    }

    /// Zero-IF front ends leave their own LO leakage at the centre of the span.
    /// It is loud, it is always there, and it is not a station.
    #[test]
    fn lo_leakage_at_dc_is_not_a_station() {
        let n = 4096;
        let bins = spectrum(n, -110.0, n / 2 - 2, 5, -30.0);
        let found = busy_channels(&bins, 145e6, SPAN, 144e6, 146e6, 12_500.0, 12_500.0, -70.0);
        assert!(found.is_empty(), "the DC spike was reported as {found:?}");
    }

    /// Anything past the usable window is in the roll-off, where levels cannot
    /// be trusted — the neighbouring slice covers it properly.
    #[test]
    fn signals_outside_the_usable_window_are_left_to_the_next_slice() {
        let n = 4096;
        // 45 % of the span out, past the 40 % the sweep trusts.
        let at = n / 2 + (n as f64 * 0.45) as usize;
        let bins = spectrum(n, -110.0, at, 20, -60.0);
        let found = busy_channels(&bins, 145e6, SPAN, 100e6, 200e6, 12_500.0, 12_500.0, -70.0);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn channel_power_reads_a_carrier_and_a_quiet_channel_apart() {
        let n = 4096;
        let bins = spectrum(n, -120.0, n / 2 + 100, 8, -50.0);
        let on = channel_power_db(&bins, 145e6, SPAN, 145e6 + 100.0 * (SPAN / n as f64), 20_000.0)
            .expect("in span");
        let off =
            channel_power_db(&bins, 145e6, SPAN, 145e6 - 300_000.0, 20_000.0).expect("in span");
        assert!(on > off + 40.0, "carrier {on:.1} dB vs floor {off:.1} dB");
        assert_eq!(channel_power_db(&bins, 145e6, SPAN, 150e6, 20_000.0), None, "outside the span");
    }

    /// Candidates outside the operator's range are none of the scan's business,
    /// even when the front end can hear them.
    #[test]
    fn candidates_outside_the_asked_for_range_are_dropped() {
        let n = 4096;
        let bins = spectrum(n, -110.0, n / 2 + 300, 20, -60.0);
        let inside = busy_channels(&bins, 145e6, SPAN, 144e6, 146e6, 12_500.0, 12_500.0, -70.0);
        assert_eq!(inside.len(), 1);
        let outside = busy_channels(&bins, 145e6, SPAN, 144e6, 145e6, 12_500.0, 12_500.0, -70.0);
        assert!(outside.is_empty(), "{outside:?} is above the range's top");
    }
}
