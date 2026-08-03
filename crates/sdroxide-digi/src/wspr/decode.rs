//! The WSPR scan loop, rebuilt to keep the SNR and the drift.
//!
//! `mfsk_core::wspr::decode::decode_scan` does the right search and then throws
//! away two of its results. Its coarse stage produces a `BasebandCandidate`
//! carrying `snr_db` (wsprd-calibrated, referenced to the 2500 Hz bandwidth
//! WSPR reports in) and `drift_hz`; its `WsprResult` has neither field, so by
//! the time a decode comes back the numbers are gone. A WSPR spot without an
//! SNR is not a spot — it is the measurement the entire network exists to
//! pool — so the loop is reproduced here over the same crate's public
//! primitives, threading the originating candidate through to the result.
//!
//! Everything else is deliberately a faithful copy of upstream's structure,
//! including the two-pass successive-interference-cancellation shape and the
//! three-second front pad, so that what we decode is what its tests against the
//! WSJT-X reference recording actually validate. Where it differs, it is said
//! so in a comment.

use mfsk_core::msg::WsprMessage;
use mfsk_core::wspr;

/// One decoded WSPR transmission, with the numbers a spot is made of.
#[derive(Clone, Debug, PartialEq)]
pub struct WsprDecode {
    pub message: WsprMessage,
    /// Frequency of tone 0 in the audio passband, Hz. The signal an operator
    /// would call "the carrier" sits 1.5 tone spacings above this; [`carrier_hz`]
    /// does that conversion, because it is the one the spot uploads.
    pub freq_hz: f32,
    /// Offset of the first symbol from the nominal slot anchor (slot start plus
    /// one second), in seconds. Negative means the signal arrived early.
    pub dt_sec: f32,
    /// Signal-to-noise in dB, referenced to 2500 Hz.
    pub snr_db: f32,
    /// Total frequency drift across the transmission, in Hz — see
    /// [`sdroxide_types::WsprSpot::drift_hz`] for why this is not a rate.
    pub drift_hz: f32,
}

impl WsprDecode {
    /// The signal's centre frequency in the audio passband — what gets reported.
    ///
    /// The four tones straddle the centre, so tone 0 sits one and a half
    /// spacings below it. Upstream's coarse search and demodulator both work in
    /// the tone-0 convention and convert internally; a spot has to carry the
    /// centre, which is what every other client reports for the same signal.
    pub fn carrier_hz(&self) -> f32 {
        self.freq_hz + 1.5 * wspr::demod::TONE_SPACING_HZ
    }
}

/// Zero-padding prepended before the search, in seconds.
///
/// A transmission can start *before* the nominal anchor — a station whose clock
/// runs fast, reported by wsprd as `dt < -1.0`. The samples are not in the
/// recording and never will be, but with front padding the demodulator can
/// still align the rest of the frame and Fano recovers from a missing leading
/// symbol or two. Matches upstream's `NEGATIVE_DT_PAD_SEC`.
const NEGATIVE_DT_PAD_SEC: f32 = 3.0;

/// Frequency window either candidate list is searched over, relative to the
/// dial. The whole WSPR window and nothing else.
const FREQ_DEDUP_HZ: f32 = 5.0;
/// One symbol at 12 kHz.
const TIME_DEDUP_SAMPLES: i64 = 8192;

/// How many coarse candidates to attempt per pass.
///
/// Upstream defaults to 16. WSPR's Fano decoder has no CRC, so every candidate
/// attempted is a chance to accept a noise pattern as a message; the cost of
/// raising this is paid in wall clock *and* in false decodes, and 16 is what
/// upstream's recall figures were measured with.
const MAX_CANDIDATES: usize = 16;

/// Drift search half-width, in Hz across the transmission.
const MAX_DRIFT_HZ: i32 = 4;

/// Decode one slot of 12 kHz audio.
///
/// `audio` is the whole slot; `sample_rate` is 12 000 in every caller, and is a
/// parameter only because the demodulator's is.
pub fn decode_slot(audio: &[f32], sample_rate: u32) -> Vec<WsprDecode> {
    let pad = (NEGATIVE_DT_PAD_SEC * sample_rate as f32) as usize;
    let mut padded = vec![0f32; pad + audio.len()];
    padded[pad..].copy_from_slice(audio);

    // Decimate once: the coarse search and the demodulator both consume this
    // same 375 Hz baseband, so doing it per stage would cost a 1.4 M-point FFT
    // for nothing.
    let (idat, qdat) = wspr::baseband::decimate_to_baseband(&padded);

    let mut out: Vec<WsprDecode> = Vec::new();
    // Pass-1 decodes keep their padded-buffer alignment so pass 2 can subtract
    // them from the baseband at the right place.
    let mut pass1: Vec<(WsprDecode, usize)> = Vec::new();

    let cands =
        wspr::coarse_baseband::coarse_baseband(&idat, &qdat, pad, MAX_CANDIDATES, MAX_DRIFT_HZ);
    for c in &cands {
        let Some(r) = wspr::decode::decode_at_baseband(
            &idat,
            &qdat,
            sample_rate,
            c.start_sample,
            c.freq_hz,
            0.0,
        ) else {
            continue;
        };
        let refined = r.start_sample;
        let d = from_result(&r, c, pad, sample_rate);
        if push_unique(&mut out, d.clone(), refined.saturating_sub(pad)) {
            pass1.push((d, refined));
        }
    }

    // Pass 2 — upstream's third wsprd pass. Subtract every pass-1 decode from
    // the baseband and search the residual: a beacon sitting under a strong
    // neighbour's skirts is invisible until the neighbour is gone, and this is
    // the only path that reaches the weakest signals on the reference sample.
    if !pass1.is_empty() {
        let mut idat2 = idat.clone();
        let mut qdat2 = qdat.clone();
        for (d, refined) in &pass1 {
            let Some(info) = info_bits(&d.message) else { continue };
            let symbols = wspr::encode_channel_symbols(&info);
            wspr::subtract::subtract_signal_baseband(
                &mut idat2,
                &mut qdat2,
                d.carrier_hz(),
                (*refined as i32) / 32,
                0.0,
                &symbols,
            );
        }
        let cands2 = wspr::coarse_baseband::coarse_baseband(
            &idat2,
            &qdat2,
            pad,
            MAX_CANDIDATES,
            MAX_DRIFT_HZ,
        );
        for c in &cands2 {
            // Coherent block detection over 1, 2 and 3 symbols. Several dB of
            // extra margin at several times the cost — worth paying only here,
            // where the strong signals have already been removed and what is
            // left is by definition marginal.
            let Some(r) = wspr::decode::decode_at_baseband_nblocks(
                &idat2,
                &qdat2,
                sample_rate,
                c.start_sample,
                c.freq_hz,
                c.drift_hz,
                &[1, 2, 3],
            ) else {
                continue;
            };
            let refined = r.start_sample;
            let d = from_result(&r, c, pad, sample_rate);
            push_unique(&mut out, d, refined.saturating_sub(pad));
        }
    }

    out
}

/// Build our result from upstream's, taking the SNR and drift from the coarse
/// candidate that found it — the whole point of this module.
fn from_result(
    r: &wspr::WsprResult,
    c: &wspr::coarse_baseband::BasebandCandidate,
    pad: usize,
    sample_rate: u32,
) -> WsprDecode {
    WsprDecode {
        message: r.message.clone(),
        freq_hz: r.freq_hz,
        // Upstream computes this the same way; recomputed here rather than read
        // off `r.dt_sec` because `decode_at_baseband` is unaware of our padding
        // and reports the offset within the padded buffer.
        dt_sec: (r.start_sample as i64 - pad as i64) as f32 / sample_rate as f32 - 1.0,
        snr_db: c.snr_db,
        drift_hz: c.drift_hz,
    }
}

/// Append unless the same message is already present at nearly the same place.
///
/// Returns whether it was appended. `start_sample` is in the caller's
/// (unpadded) time base.
fn push_unique(out: &mut Vec<WsprDecode>, d: WsprDecode, start_sample: usize) -> bool {
    let dup = out.iter().any(|prev| {
        prev.message == d.message
            && (prev.freq_hz - d.freq_hz).abs() <= FREQ_DEDUP_HZ
            && (start_of(prev) as i64 - start_sample as i64).abs() <= TIME_DEDUP_SAMPLES
    });
    if !dup {
        out.push(d);
    }
    !dup
}

/// Reconstruct a decode's start sample from its `dt`, so de-duplication does not
/// need a parallel list of them.
fn start_of(d: &WsprDecode) -> usize {
    (((d.dt_sec + 1.0) * 12_000.0).max(0.0)) as usize
}

/// The 50 information bits for a decoded message, so pass 2 can rebuild the
/// on-air symbols and subtract them.
///
/// Only Type-1 can be re-packed: `mfsk-core` exposes no packer for the compound
/// callsign and hashed-callsign layouts. A Type-2 or Type-3 decode therefore
/// stays in the residual and may be found again in pass 2 — which the
/// de-duplication above absorbs. Losing the subtraction costs sensitivity for
/// whatever was hiding behind it; producing wrong symbols would corrupt the
/// residual for everything, which is worse.
fn info_bits(m: &WsprMessage) -> Option<[u8; 50]> {
    match m {
        WsprMessage::Type1 { callsign, grid, power_dbm } => {
            mfsk_core::msg::wspr::pack_type1(callsign, grid, *power_dbm)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The synthesised round trip. Weak evidence on its own — a decoder and a
    /// transmitter that share an author agree with each other by construction —
    /// which is why the real proof is `tests/wspr_corpus.rs` against the WSJT-X
    /// reference recording. This one is here to catch the plumbing: wiring the
    /// wrong buffer in, or losing the pad offset.
    #[test]
    fn a_synthesised_beacon_decodes_back_with_its_message_and_frequency() {
        let audio = crate::wspr::tx::synthesize("K1ABC", "FN42", 37, 12_000, 1500.0, 0.5)
            .expect("K1ABC FN42 37 is a valid type 1 message");
        // Place it where a real one sits: one second into a 120 s slot.
        let mut slot = vec![0f32; 120 * 12_000];
        let start = 12_000;
        slot[start..start + audio.len()].copy_from_slice(&audio);

        let got = decode_slot(&slot, 12_000);
        let hit = got
            .iter()
            .find(|d| matches!(&d.message, WsprMessage::Type1 { callsign, .. } if callsign == "K1ABC"))
            .unwrap_or_else(|| panic!("K1ABC not among {got:?}"));
        assert_eq!(hit.message.to_string(), "K1ABC FN42 37");
        // 1500 Hz is where `synthesize` was asked to put tone 0, and the
        // carrier sits 1.5 spacings above it.
        assert!((hit.freq_hz - 1500.0).abs() < 2.0, "tone 0 at {}", hit.freq_hz);
        assert!(hit.dt_sec.abs() < 0.5, "dt {} is not near zero", hit.dt_sec);
    }

    #[test]
    fn a_slot_of_silence_decodes_to_nothing() {
        assert!(decode_slot(&vec![0f32; 120 * 12_000], 12_000).is_empty());
    }

    /// A deterministic Gaussian source. No `rand` in this crate's tree, and a
    /// test that decoded a different signal on every run would be worse than no
    /// test at all.
    struct Noise(u64);

    impl Noise {
        fn next_u32(&mut self) -> u32 {
            // xorshift64*
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
        }
        fn unit(&mut self) -> f64 {
            (self.next_u32() as f64 + 0.5) / 4_294_967_296.0
        }
        /// Box–Muller, one value per call (the second is discarded; this is a
        /// test, not a hot loop).
        fn gaussian(&mut self) -> f64 {
            let (u1, u2) = (self.unit(), self.unit());
            (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
        }
    }

    /// Build a slot holding one beacon at a known SNR, referenced the way WSPR
    /// reports it — in a 2500 Hz noise bandwidth.
    ///
    /// The audio is real and spans 0–6000 Hz at 12 kHz, so noise of variance σ²
    /// has power `σ²·2500/6000` inside the reference bandwidth. That factor is
    /// the whole reason this helper exists rather than a bare `signal + noise`.
    fn slot_at_snr(target_db: f64, seed: u64) -> Vec<f32> {
        let amp = 0.05f32;
        let sig_power = (amp as f64 * amp as f64) / 2.0;
        let sigma2 = sig_power / 10f64.powf(target_db / 10.0) * (6000.0 / 2500.0);
        let sigma = sigma2.sqrt();

        let burst = crate::wspr::tx::synthesize("K1ABC", "FN42", 37, 12_000, 1500.0, amp)
            .expect("valid message");
        let mut slot = vec![0f32; 120 * 12_000];
        let start = 12_000;
        slot[start..start + burst.len()].copy_from_slice(&burst);
        let mut n = Noise(seed);
        for s in slot.iter_mut() {
            *s += (n.gaussian() * sigma) as f32;
        }
        slot
    }

    /// The calibration test, and the reason this module exists at all: the SNR
    /// we hand to WSPRnet has to be the SNR that was actually there.
    ///
    /// A spot is a measurement pooled with everybody else's. Reporting a number
    /// that is systematically wrong does not merely mislead this operator — it
    /// corrupts the path statistics for every station on the other end.
    ///
    /// The tolerance is wide (±5 dB) on purpose: the estimator is a coarse
    /// spectral one over a 0.73 Hz bin against a 30th-percentile noise floor,
    /// and `wsprd` itself is not better than a few dB. What this catches is the
    /// failure that matters — a constant offset, or the number not tracking the
    /// signal at all.
    #[test]
    fn the_reported_snr_tracks_the_signal_that_was_actually_there() {
        let mut seen = Vec::new();
        for (i, &target) in [-14.0f64, -20.0, -26.0].iter().enumerate() {
            let slot = slot_at_snr(target, 0x5EED_0000 + i as u64);
            let got = decode_slot(&slot, 12_000);
            let hit = got
                .iter()
                .find(|d| matches!(&d.message, WsprMessage::Type1 { callsign, .. } if callsign == "K1ABC"))
                .unwrap_or_else(|| panic!("no decode at {target} dB: {got:?}"));
            assert!(
                (hit.snr_db as f64 - target).abs() < 5.0,
                "at {target} dB the decoder reported {:.1}",
                hit.snr_db
            );
            seen.push(hit.snr_db as f64);
        }
        // And it moves the right way, monotonically, over a 12 dB span — the
        // check a constant offset would pass but a broken estimator would not.
        assert!(
            seen[0] > seen[1] && seen[1] > seen[2],
            "SNR did not fall with the signal: {seen:?}"
        );
    }

    #[test]
    fn the_carrier_sits_above_tone_zero_by_a_symmetric_half_of_the_tone_set() {
        let d = WsprDecode {
            message: WsprMessage::Type1 {
                callsign: "K1ABC".into(),
                grid: "FN42".into(),
                power_dbm: 37,
            },
            freq_hz: 1500.0,
            dt_sec: 0.0,
            snr_db: -20.0,
            drift_hz: 0.0,
        };
        // Four tones 1.4648 Hz apart: the centre is 1.5 spacings up, ≈2.2 Hz.
        assert!((d.carrier_hz() - 1502.197).abs() < 0.01, "{}", d.carrier_hz());
    }
}
