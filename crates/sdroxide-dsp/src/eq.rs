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

/// One second-order section (Direct Form I), `f32` state and coefficients.
/// Private to this module: [`ParametricEq`] is the public surface.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    /// The identity filter, used for a band whose gain is 0 dB, so a flat
    /// band costs no more than passing samples through unchanged.
    fn identity() -> Self {
        Biquad { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    fn from_coeffs(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Biquad {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
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
        Self::from_coeffs(b0, b1, b2, a0, a1, a2)
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
        Self::from_coeffs(b0, b1, b2, a0, a1, a2)
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
        Self::from_coeffs(b0, b1, b2, a0, a1, a2)
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
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
        ParametricEq { low: Biquad::identity(), mid: Biquad::identity(), high: Biquad::identity() }
    }

    /// Rebuild all three bands from `cfg` at `rate` Hz. Cheap, but still only
    /// call it when `cfg` has actually changed since the last call.
    pub fn configure(&mut self, cfg: &TxEqState, rate: f64) {
        let band = |b: &TxEqBand| (b.freq_hz as f64, b.gain_db as f64, b.q as f64);
        let (lf, lg, lq) = band(&cfg.low);
        let (mf, mg, mq) = band(&cfg.mid);
        let (hf, hg, hq) = band(&cfg.high);
        self.low = Biquad::low_shelf(lf, lg, lq, rate);
        self.mid = Biquad::peaking(mf, mg, mq, rate);
        self.high = Biquad::high_shelf(hf, hg, hq, rate);
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
        let (mut s0, mut s1, mut s2) = (0.0, 0.0, 0.0);
        for &v in x {
            s0 = v as f64 + cw * s1 - s2;
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
        assert!((gain_db - 12.0).abs() < 1.0, "expected ~+12 dB at 1500 Hz, measured {gain_db:.1} dB");
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
        assert!(ratio_db.abs() < 1.0, "300 Hz should be near untouched by a 1500 Hz peak: {ratio_db:.1} dB");
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
        assert!(low_db < -6.0, "80 Hz should be well cut by a -12 dB low shelf at 300 Hz: {low_db:.1} dB");
        assert!(high_db > low_db + 6.0, "3 kHz should be far less affected than 80 Hz: {high_db:.1} vs {low_db:.1} dB");
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
        assert!(high_db > 6.0, "6 kHz should be well boosted by a +12 dB high shelf at 2800 Hz: {high_db:.1} dB");
        assert!(high_db > low_db + 6.0, "6 kHz should be boosted far more than 300 Hz: {high_db:.1} vs {low_db:.1} dB");
    }
}
