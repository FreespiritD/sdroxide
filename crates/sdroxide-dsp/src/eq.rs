//! Transmit-audio parametric EQ: three cascaded biquads (low shelf, mid peak,
//! high shelf) shaping the microphone signal ahead of the modulator.
//!
//! Coefficients follow the RBJ Audio EQ Cookbook, computed in `f64` and stored
//! as `f32`, the same precision split `DcBlock` uses. That's safe here because
//! these poles sit at ordinary audio corner frequencies (hundreds of Hz to a
//! few kHz over 48 kHz), nowhere near the unit circle the way a device-rate DC
//! blocker's is.
//!
//! Voice-only, hand-rolled: no biquad/filter-design crate exists anywhere in
//! this workspace, and this stays consistent with that (see `DcBlock` in
//! `demod.rs`).

use sdroxide_types::{TxEqBand, TxEqState};

/// The five normalized coefficients of one second-order section — the filter's
/// *shape*, with none of its memory. Kept apart from [`Biquad`] so that
/// retuning a band swaps only this and leaves the delay line alone; see
/// [`Biquad::set`].
#[derive(Clone, Copy)]
struct Coeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Coeffs {
    /// The identity filter, used for a band whose gain is 0 dB, so a flat
    /// band costs no more than passing samples through unchanged.
    fn identity() -> Self {
        Coeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 }
    }

    fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Coeffs {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        }
    }

    /// Peaking (bell) filter: boost/cut `gain_db` around `freq_hz`, `q` sets
    /// how narrow the bell is (higher = narrower).
    fn peaking(freq_hz: f64, gain_db: f64, q: f64, rate: f64) -> Self {
        if gain_db == 0.0 || freq_hz <= 0.0 || rate <= 0.0 {
            return Self::identity();
        }
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = std::f64::consts::TAU * freq_hz / rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q.max(0.05));

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    /// Low shelf: boost/cut `gain_db` below `freq_hz`. `slope` is the RBJ
    /// cookbook's `S` (0 < S <= 1; 1 is the steepest monotonic shelf). That's
    /// what [`TxEqBand::q`] means on a shelf band, distinct from a peaking
    /// band's Q.
    fn low_shelf(freq_hz: f64, gain_db: f64, slope: f64, rate: f64) -> Self {
        if gain_db == 0.0 || freq_hz <= 0.0 || rate <= 0.0 {
            return Self::identity();
        }
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = std::f64::consts::TAU * freq_hz / rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let s = slope.clamp(0.1, 1.0);
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).sqrt();
        let sqrt_a_2alpha = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + sqrt_a_2alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - sqrt_a_2alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + sqrt_a_2alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - sqrt_a_2alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    /// High shelf: boost/cut `gain_db` above `freq_hz`. See [`Self::low_shelf`]
    /// for what `slope` means.
    fn high_shelf(freq_hz: f64, gain_db: f64, slope: f64, rate: f64) -> Self {
        if gain_db == 0.0 || freq_hz <= 0.0 || rate <= 0.0 {
            return Self::identity();
        }
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = std::f64::consts::TAU * freq_hz / rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let s = slope.clamp(0.1, 1.0);
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).sqrt();
        let sqrt_a_2alpha = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + sqrt_a_2alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - sqrt_a_2alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + sqrt_a_2alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - sqrt_a_2alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }
}

/// One second-order section (Direct Form I): a set of [`Coeffs`] plus the two
/// input and two output samples they are applied to. Private to this module:
/// [`ParametricEq`] is the public surface.
struct Biquad {
    c: Coeffs,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn new() -> Self {
        Biquad { c: Coeffs::identity(), x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    /// Retune to `c` **without** disturbing the delay line.
    ///
    /// This is the whole reason [`Coeffs`] is a separate type. The operator
    /// tunes this EQ by dragging a control while transmitting and listening to
    /// the monitor, which reconfigures the filter on every frame of the drag —
    /// sixty times a second. Building a fresh `Biquad` each time would zero
    /// `x1`/`x2`/`y1`/`y2`, and a filter that forgets the last two samples
    /// resumes from nothing: a step discontinuity in the output, which is a
    /// click on the air, once per frame for as long as the drag lasts. Keeping
    /// the history means the new response simply takes over from the old one.
    fn set(&mut self, c: Coeffs) {
        self.c = c;
    }

    /// Forget the delay line, leaving the coefficients as they are. Used at
    /// key-down so an over never opens on the tail of the previous one.
    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.c.b0 * x + self.c.b1 * self.x1 + self.c.b2 * self.x2
            - self.c.a1 * self.y1
            - self.c.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Three-band parametric EQ on the mic-to-modulator path: low shelf, mid
/// peak, high shelf, cascaded in that order.
pub struct ParametricEq {
    low: Biquad,
    mid: Biquad,
    high: Biquad,
}

impl ParametricEq {
    pub fn new() -> Self {
        ParametricEq { low: Biquad::new(), mid: Biquad::new(), high: Biquad::new() }
    }

    /// Retune all three bands from `cfg` at `rate` Hz. Cheap, but still only
    /// call it when `cfg` has actually changed since the last call.
    ///
    /// Safe to call mid-transmission: only the coefficients are replaced, and
    /// each band goes on filtering from the samples it has already seen (see
    /// [`Biquad::set`]).
    pub fn configure(&mut self, cfg: &TxEqState, rate: f64) {
        let band = |b: &TxEqBand| (b.freq_hz as f64, b.gain_db as f64, b.q as f64);
        let (lf, lg, lq) = band(&cfg.low);
        let (mf, mg, mq) = band(&cfg.mid);
        let (hf, hg, hq) = band(&cfg.high);
        self.low.set(Coeffs::low_shelf(lf, lg, lq, rate));
        self.mid.set(Coeffs::peaking(mf, mg, mq, rate));
        self.high.set(Coeffs::high_shelf(hf, hg, hq, rate));
    }

    /// Drop the filter history, keeping the operator's settings. Called at
    /// key-down so the first block of an over is not coloured by the ring-out
    /// of the last one.
    pub fn reset(&mut self) {
        self.low.reset();
        self.mid.reset();
        self.high.reset();
    }

    /// Run `audio` through all three bands in place.
    pub fn process(&mut self, audio: &mut [f32]) {
        for s in audio.iter_mut() {
            *s = self.high.process(self.mid.process(self.low.process(*s)));
        }
    }
}

impl Default for ParametricEq {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|&s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// Single-frequency power via a Goertzel filter, the same tool
    /// `tests/chain.rs` uses to measure a filter's gain at one frequency
    /// without pulling in an FFT dependency for tests.
    fn goertzel(x: &[f32], freq: f64, rate: f64) -> f64 {
        let k = (0.5 + x.len() as f64 * freq / rate).floor();
        let w = std::f64::consts::TAU / x.len() as f64 * k;
        let cw = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s0 = v as f64 + cw * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        s1 * s1 + s2 * s2 - cw * s1 * s2
    }

    fn tone(rate: f64, freq: f64, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (std::f64::consts::TAU * freq * i as f64 / rate).sin() as f32)
            .collect()
    }

    fn flat_state() -> TxEqState {
        TxEqState::default()
    }

    #[test]
    fn disabled_config_is_the_identity_filter() {
        let rate = 48_000.0;
        let mut eq = ParametricEq::new();
        eq.configure(&flat_state(), rate);
        let mut buf = tone(rate, 1000.0, 0.3, 4800);
        let before = buf.clone();
        eq.process(&mut buf);
        for (a, b) in before.iter().zip(&buf) {
            assert!((a - b).abs() < 1e-4, "flat EQ changed a sample: {a} -> {b}");
        }
    }

    #[test]
    fn mid_peak_boosts_at_its_center_frequency() {
        let rate = 48_000.0;
        let mut cfg = flat_state();
        cfg.mid = TxEqBand { freq_hz: 1500.0, gain_db: 12.0, q: 1.0 };
        let mut eq = ParametricEq::new();
        eq.configure(&cfg, rate);

        let n = 8192;
        let mut on_freq = tone(rate, 1500.0, 0.1, n);
        let before = goertzel(&on_freq[n / 2..], 1500.0, rate);
        eq.process(&mut on_freq);
        let after = goertzel(&on_freq[n / 2..], 1500.0, rate);
        let gain_db = 10.0 * (after / before).log10();
        assert!(
            (gain_db - 12.0).abs() < 1.0,
            "expected ~+12 dB at 1500 Hz, measured {gain_db:.1} dB"
        );
    }

    #[test]
    fn mid_peak_leaves_a_far_off_tone_alone() {
        let rate = 48_000.0;
        let mut cfg = flat_state();
        cfg.mid = TxEqBand { freq_hz: 1500.0, gain_db: 12.0, q: 2.0 };
        let mut eq = ParametricEq::new();
        eq.configure(&cfg, rate);

        let n = 8192;
        let mut off_freq = tone(rate, 300.0, 0.1, n);
        let before = rms(&off_freq[n / 2..]);
        eq.process(&mut off_freq);
        let after = rms(&off_freq[n / 2..]);
        let ratio_db = 20.0 * (after / before).log10();
        assert!(
            ratio_db.abs() < 1.0,
            "300 Hz should be near untouched by a 1500 Hz peak: {ratio_db:.1} dB"
        );
    }

    #[test]
    fn low_shelf_cuts_low_frequencies_more_than_high() {
        let rate = 48_000.0;
        let mut cfg = flat_state();
        cfg.low = TxEqBand { freq_hz: 300.0, gain_db: -12.0, q: 0.9 };
        let mut eq = ParametricEq::new();
        eq.configure(&cfg, rate);

        let n = 8192;
        let mut low = tone(rate, 80.0, 0.2, n);
        let mut high = tone(rate, 3000.0, 0.2, n);
        let low_before = rms(&low[n / 2..]);
        let high_before = rms(&high[n / 2..]);
        eq.process(&mut low);
        eq.process(&mut high);
        let low_after = rms(&low[n / 2..]);
        let high_after = rms(&high[n / 2..]);
        let low_db = 20.0 * (low_after / low_before).log10();
        let high_db = 20.0 * (high_after / high_before).log10();
        assert!(
            low_db < -6.0,
            "80 Hz should be well cut by a -12 dB low shelf at 300 Hz: {low_db:.1} dB"
        );
        assert!(
            high_db > low_db + 6.0,
            "3 kHz should be far less affected than 80 Hz: {high_db:.1} vs {low_db:.1} dB"
        );
    }

    #[test]
    fn high_shelf_boosts_high_frequencies_more_than_low() {
        let rate = 48_000.0;
        let mut cfg = flat_state();
        cfg.high = TxEqBand { freq_hz: 2800.0, gain_db: 12.0, q: 0.9 };
        let mut eq = ParametricEq::new();
        eq.configure(&cfg, rate);

        let n = 8192;
        let mut low = tone(rate, 300.0, 0.2, n);
        let mut high = tone(rate, 6000.0, 0.2, n);
        let low_before = rms(&low[n / 2..]);
        let high_before = rms(&high[n / 2..]);
        eq.process(&mut low);
        eq.process(&mut high);
        let low_after = rms(&low[n / 2..]);
        let high_after = rms(&high[n / 2..]);
        let low_db = 20.0 * (low_after / low_before).log10();
        let high_db = 20.0 * (high_after / high_before).log10();
        assert!(
            high_db > 6.0,
            "6 kHz should be well boosted by a +12 dB high shelf at 2800 Hz: {high_db:.1} dB"
        );
        assert!(
            high_db > low_db + 6.0,
            "6 kHz should be boosted far more than 300 Hz: {high_db:.1} vs {low_db:.1} dB"
        );
    }

    /// The engine reconfigures whenever the operator's state differs from what
    /// the filters were built from, and a UI that re-sends the same settings
    /// must therefore be inaudible. Exact equality, because re-deriving the
    /// same coefficients from the same numbers is bit-for-bit reproducible —
    /// the only thing that could differ is the delay line.
    #[test]
    fn reapplying_the_same_settings_mid_stream_changes_nothing() {
        let rate = 48_000.0;
        let mut cfg = flat_state();
        cfg.mid = TxEqBand { freq_hz: 1500.0, gain_db: 9.0, q: 1.0 };
        let src = tone(rate, 700.0, 0.3, 2048);

        let mut straight = src.clone();
        let mut eq = ParametricEq::new();
        eq.configure(&cfg, rate);
        eq.process(&mut straight);

        let mut spliced = src.clone();
        let mut eq = ParametricEq::new();
        eq.configure(&cfg, rate);
        let (first, second) = spliced.split_at_mut(1024);
        eq.process(first);
        eq.configure(&cfg, rate);
        eq.process(second);

        assert_eq!(straight, spliced, "re-applying the same EQ perturbed the audio");
    }

    /// Dragging a gain control while transmitting retunes the filter on every
    /// frame of the drag. Each retune has to hand the new coefficients a
    /// filter that still remembers the last two samples; when it did not, the
    /// output restarted from nothing, which is a click on the air sixty times
    /// a second for as long as the drag lasted.
    ///
    /// Measured as how far the retuned filter strays from one that had been
    /// set that way all along and is fully settled — a retune should be a
    /// nudge onto the new response, not a restart of it. The single-sample
    /// step across the splice is *not* a usable measure: Direct Form I emits
    /// `b0 * x[n]` when its history is zeroed, and with `b0` near unity that
    /// lands close to where the waveform already was. The damage is in the
    /// settling that follows, which is what this looks at. Preserving the
    /// state holds the excursion to ~14% of the tone; zeroing it gives ~58%.
    #[test]
    fn moving_a_band_mid_stream_does_not_restart_the_filter() {
        let rate = 48_000.0;
        let mut six_db = flat_state();
        six_db.mid = TxEqBand { freq_hz: 1500.0, gain_db: 6.0, q: 1.0 };
        let mut nine_db = six_db;
        nine_db.mid.gain_db = 9.0;
        let src = tone(rate, 700.0, 0.3, 2048);

        // What the second half should sound like: +9 dB from the start, every
        // transient long since decayed.
        let mut settled = src.clone();
        let mut eq = ParametricEq::new();
        eq.configure(&nine_db, rate);
        eq.process(&mut settled);

        // What it does sound like when +9 dB arrives as one frame of a drag.
        let mut dragged = src.clone();
        let mut eq = ParametricEq::new();
        eq.configure(&six_db, rate);
        let (first, second) = dragged.split_at_mut(1024);
        eq.process(first);
        eq.configure(&nine_db, rate);
        eq.process(second);

        let amplitude = settled[1024..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let excursion = dragged[1024..]
            .iter()
            .zip(&settled[1024..])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            excursion < 0.25 * amplitude,
            "retuning restarted the filter: strayed {excursion:.4} from the settled \
             response, {:.0}% of the {amplitude:.4} tone",
            100.0 * excursion / amplitude
        );
    }
}
