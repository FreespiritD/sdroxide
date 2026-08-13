//! Front-end IQ correction for a zero-IF receiver: the DC spike in the middle
//! of the span, and the mirror image of every signal reflected about it.
//!
//! # What it corrects
//!
//! A quadrature front end mixes straight to baseband with two analogue paths
//! that are never quite identical, and the two defects that leaves both sit in
//! the spectrum where they are impossible to miss:
//!
//! * **DC** — the tuner's own LO leaks back into its mixer and demodulates
//!   itself, and the converter adds an offset of its own. On an RTL-SDR the
//!   8-bit samples are also centred on a value that is not exactly mid-scale.
//!   All of it lands on the centre of the span as a permanent carrier.
//! * **Amplitude and phase imbalance** — if the I and Q paths differ in gain,
//!   or their phases are not exactly 90° apart, the stream is no longer
//!   analytic and every signal appears again mirrored about the centre. On a
//!   typical dongle that image is 25–40 dB down, which is enough to look like a
//!   station that is not there.
//!
//! Neither can be fixed by tuning: the R820T family has no offset-tuning mode
//! to move the LO out of the passband, so a receiver that wants a clean centre
//! has to correct in DSP.
//!
//! # How
//!
//! DC is a leaky running mean, subtracted — see [`ComplexDcBlock`].
//!
//! The imbalance correction is the classic orthogonalise-and-normalise
//! estimator, run as a closed loop on its own output:
//!
//! ```text
//! i' = i
//! q' = gain · (q − alpha · i)
//! ```
//!
//! For any stream that is circularly symmetric — noise, and any collection of
//! signals that does not happen to be a mirror image of itself — I and Q are
//! uncorrelated and carry equal power. Both are properties of the *output*, so
//! every [`BLOCK`] samples the loop measures how far its own output misses them
//! and steps the two coefficients a fraction [`MU`] of the way towards closing
//! the gap: `alpha` until `E[i·q']` is zero, `gain` until `E[q'²]` matches
//! `E[i²]`. Convergence is deliberately unhurried — around a third of a second
//! at 2.4 Msps — so the estimate settles on the front end rather than chasing a
//! fading signal, and the residual jitter stays far below the image it is
//! there to remove.
//!
//! The estimator needs no knowledge of what is being received, but there is one
//! input it cannot read: a baseband that is genuinely symmetric about the
//! centre — two equally strong carriers either side of it, or a double-sideband
//! signal centred exactly on the dial — is indistinguishable from an imbalance,
//! because that is precisely what an imbalance manufactures. A block that
//! measures something no front end could be ([`IMPLAUSIBLE_ALPHA`],
//! [`IMPLAUSIBLE_RATIO`]) is therefore thrown away rather than learned from,
//! and what a milder one can do is bounded by the clamps ([`MAX_ALPHA`],
//! [`MAX_GAIN`]) and unwound as soon as the band stops looking like that.
//!
//! DC removal has no such escape: a carrier that really does sit on the centre
//! frequency *is* DC, so it goes with the offset — an AM station tuned dead on
//! comes out with its carrier stripped and its envelope detector distorting.
//! Tuning a kilohertz off, or switching the correction off, are both answers;
//! it is a large part of why this is a switch and not a permanent fixture.
//! (Front ends whose driver can park the LO clear of the passband —
//! `IqSource::lo_offset_hz` — never meet the question at all.)

use crate::Complex32;
use crate::demod::ComplexDcBlock;

/// Samples per estimator update. At 2.4 Msps this is ~7 ms, and 16 k samples
/// measure a correlation to about 1 % — an order of magnitude finer than the
/// imbalance being corrected, before [`MU`] smooths it further.
const BLOCK: usize = 16_384;

/// Fraction of each block's measured error folded into the estimate. A time
/// constant of roughly 50 blocks: ~0.35 s at 2.4 Msps, a few seconds at the
/// 250 kHz rate.
const MU: f64 = 0.02;

/// Largest phase skew the loop may apply, in radians of quadrature error
/// (0.2 ≈ 11°). Hardware needing more than this is broken, so a larger
/// estimate means the loop has been pulled by a signal that breaks its
/// assumptions rather than by the front end.
const MAX_ALPHA: f64 = 0.2;

/// Largest Q-branch gain correction, as a ratio. Same reasoning as
/// [`MAX_ALPHA`]: 1.25 (≈ 2 dB) is already outside what a working front end
/// produces.
const MAX_GAIN: f64 = 1.25;

/// A block that measures a quadrature error past ~30° is not measuring the
/// front end: the spectrum in it is close to a mirror image of itself — two
/// carriers either side of the centre, or a double-sideband signal on it —
/// which is indistinguishable from an imbalance and would drag the loop
/// somewhere it cannot see. Such a block is discarded rather than learned from,
/// which is also what makes the pull recoverable: nothing was taken from it.
const IMPLAUSIBLE_ALPHA: f64 = 0.5;

/// The same for the power ratio: no front end has one path 3:1 above the other,
/// so a block that says so is describing its signal, not the hardware.
const IMPLAUSIBLE_RATIO: f64 = 3.0;

/// DC removal plus adaptive amplitude/phase imbalance correction, applied to a
/// raw device stream before anything else sees it.
pub struct IqCorrect {
    dc: ComplexDcBlock,
    /// How much I to subtract from Q to square up the quadrature.
    alpha: f64,
    /// Q-branch gain that equalises the two paths' power.
    gain: f64,
    /// Samples accumulated into the sums below since the last update.
    n: usize,
    sum_ii: f64,
    sum_qq: f64,
    sum_iq: f64,
}

impl IqCorrect {
    /// `corner_hz` is the DC blocker's corner — see [`ComplexDcBlock`].
    pub fn new(corner_hz: f64, sample_rate: f64) -> Self {
        IqCorrect {
            dc: ComplexDcBlock::new(corner_hz, sample_rate),
            alpha: 0.0,
            gain: 1.0,
            n: 0,
            sum_ii: 0.0,
            sum_qq: 0.0,
            sum_iq: 0.0,
        }
    }

    /// Correct a block in place.
    pub fn process(&mut self, buf: &mut [Complex32]) {
        self.dc.process(buf);
        for s in buf {
            let (alpha, gain) = (self.alpha as f32, self.gain as f32);
            let i = s.re;
            let q = gain * (s.im - alpha * i);
            s.im = q;
            self.sum_ii += (i as f64) * (i as f64);
            self.sum_qq += (q as f64) * (q as f64);
            self.sum_iq += (i as f64) * (q as f64);
            self.n += 1;
            if self.n >= BLOCK {
                self.update();
            }
        }
    }

    /// Forget the estimate and start converging again. Worth doing when the
    /// correction is switched back on after a spell off the air, so it does not
    /// resume with a coefficient measured on a band the receiver has left.
    pub fn reset(&mut self) {
        self.alpha = 0.0;
        self.gain = 1.0;
        self.n = 0;
        self.sum_ii = 0.0;
        self.sum_qq = 0.0;
        self.sum_iq = 0.0;
    }

    /// The current quadrature-error correction in radians, and the Q-branch
    /// gain. Diagnostics: what the loop has converged on says how good the
    /// front end is.
    pub fn estimate(&self) -> (f64, f64) {
        (self.alpha, self.gain)
    }

    /// Fold one block of statistics into the estimate.
    fn update(&mut self) {
        let (ii, qq, iq) = (self.sum_ii, self.sum_qq, self.sum_iq);
        self.n = 0;
        self.sum_ii = 0.0;
        self.sum_qq = 0.0;
        self.sum_iq = 0.0;
        // A silent block — a stalled front end, or the gain turned all the way
        // down — carries no information about the imbalance. Hold the estimate.
        if ii <= f64::MIN_POSITIVE || qq <= f64::MIN_POSITIVE {
            return;
        }
        // Residual correlation, in units of Q per I. Subtracting `rho·i` from
        // the output would zero it outright; `alpha` reaches the output through
        // `gain`, and only a fraction MU of the step is taken.
        let rho = iq / ii;
        if rho.abs() <= IMPLAUSIBLE_ALPHA {
            self.alpha = (self.alpha + MU * rho / self.gain).clamp(-MAX_ALPHA, MAX_ALPHA);
        }
        // Same idea for the power ratio: the full correction is sqrt(ii/qq).
        let ratio = (ii / qq).sqrt();
        if (1.0 / IMPLAUSIBLE_RATIO..=IMPLAUSIBLE_RATIO).contains(&ratio) {
            self.gain = (self.gain * ratio.powf(MU)).clamp(1.0 / MAX_GAIN, MAX_GAIN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustfft::FftPlanner;

    /// Pseudo-random noise, so the tests do not depend on a random crate.
    fn noise(n: usize) -> Vec<Complex32> {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / 8_388_608.0 - 1.0
        };
        (0..n).map(|_| Complex32::new(0.1 * next(), 0.1 * next())).collect()
    }

    /// Apply a known front-end defect: gain and phase imbalance, then DC.
    fn spoil(buf: &mut [Complex32], gain: f32, phase_rad: f32, dc: Complex32) {
        for s in buf {
            let i = s.re;
            let q = gain * (s.im * phase_rad.cos() + s.re * phase_rad.sin());
            *s = Complex32::new(i + dc.re, q + dc.im);
        }
    }

    /// Power in the bin holding a tone at `cycles_per_sample`, and in its
    /// mirror, in dB relative to full scale.
    fn tone_and_image_db(buf: &[Complex32], cycles_per_sample: f64) -> (f64, f64) {
        let n = buf.len();
        let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
        let mut scratch: Vec<Complex32> = buf.to_vec();
        fft.process(&mut scratch);
        let bin = (cycles_per_sample * n as f64).round() as usize;
        let power = |k: usize| 20.0 * (scratch[k % n].norm() as f64 + 1e-30).log10();
        (power(bin), power(n - bin))
    }

    #[test]
    fn dc_offset_is_removed() {
        let mut buf = noise(200_000);
        spoil(&mut buf, 1.0, 0.0, Complex32::new(0.05, -0.03));
        let mut corr = IqCorrect::new(20.0, 2_400_000.0);
        corr.process(&mut buf);
        // The blocker needs a moment to charge; judge it on the settled tail.
        let tail = &buf[100_000..];
        let mean: Complex32 =
            tail.iter().sum::<Complex32>() / Complex32::new(tail.len() as f32, 0.0);
        assert!(mean.norm() < 1e-3, "residual DC {mean:?}");
    }

    #[test]
    fn the_mirror_image_is_suppressed() {
        // A tone a quarter of the way up the span, through a front end with 5 %
        // gain error and 3° of quadrature error — an ordinary dongle. Two
        // seconds of it at 2.4 Msps, because the loop is deliberately slow.
        let f = 0.25_f64;
        let n = 5_000_000;
        let mut buf: Vec<Complex32> = (0..n)
            .map(|k| {
                let ph = (std::f64::consts::TAU * f * k as f64) as f32;
                Complex32::new(0.2 * ph.cos(), 0.2 * ph.sin())
            })
            .collect();
        spoil(&mut buf, 1.05, 3.0_f32.to_radians(), Complex32::new(0.05, -0.03));

        let window = 1 << 14;
        let (tone_before, image_before) = tone_and_image_db(&buf[..window], f);
        let before = tone_before - image_before;

        let mut corr = IqCorrect::new(20.0, 2_400_000.0);
        corr.process(&mut buf);
        let (tone_after, image_after) = tone_and_image_db(&buf[n - window..], f);
        let after = tone_after - image_after;

        assert!(before < 40.0, "the spoiled signal should have a visible image: {before:.1} dB");
        assert!(
            after > before + 30.0,
            "image rejection {before:.1} dB -> {after:.1} dB, expected 30 dB better"
        );
        // Inverting `spoil` exactly means alpha = g·sin φ and gain = 1/(g·cos φ).
        let (alpha, gain) = corr.estimate();
        let (want_alpha, want_gain) = {
            let (g, p) = (1.05_f64, 3.0_f64.to_radians());
            (g * p.sin(), 1.0 / (g * p.cos()))
        };
        assert!(
            (alpha - want_alpha).abs() < 0.005 && (gain - want_gain).abs() < 0.005,
            "converged on ({alpha:.4}, {gain:.4}), wanted ({want_alpha:.4}, {want_gain:.4})"
        );
    }

    /// The input the estimator cannot read: a double-sideband signal centred on
    /// the dial is a mirror image of itself, so it *looks* like an all-I,
    /// no-Q front end. The loop must recognise that as impossible rather than
    /// swing the Q branch to the stop chasing it.
    #[test]
    fn a_mirror_symmetric_signal_does_not_pull_the_loop() {
        let n = 5_000_000;
        let mut buf = noise(n);
        for (k, s) in buf.iter_mut().enumerate() {
            // A carrier on the centre with 1 kHz of amplitude modulation, ten
            // times the noise: real-valued baseband, no Q content at all.
            let m = (std::f64::consts::TAU * 1e3 / 2.4e6 * k as f64) as f32;
            s.re += 1.0 + 0.5 * m.cos();
        }
        let mut corr = IqCorrect::new(20.0, 2_400_000.0);
        corr.process(&mut buf);
        let (alpha, gain) = corr.estimate();
        assert!(
            alpha.abs() < 0.01 && (gain - 1.0).abs() < 0.01,
            "pulled to ({alpha:.4}, {gain:.4}) by a signal it cannot measure"
        );
    }

    #[test]
    fn a_clean_stream_is_left_alone() {
        let mut buf = noise(400_000);
        let reference = buf.clone();
        let mut corr = IqCorrect::new(20.0, 2_400_000.0);
        corr.process(&mut buf);
        let worst = buf[200_000..]
            .iter()
            .zip(&reference[200_000..])
            .map(|(a, b)| (a - b).norm())
            .fold(0.0_f32, f32::max);
        assert!(worst < 5e-3, "clean input perturbed by {worst}");
        let (alpha, gain) = corr.estimate();
        assert!(alpha.abs() < 0.01 && (gain - 1.0).abs() < 0.01, "drifted: {alpha} {gain}");
    }
}
