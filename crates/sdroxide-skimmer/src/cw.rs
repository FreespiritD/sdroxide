//! The CW skimmer core: a streaming STFT over a wide complex-baseband window,
//! per-bin on/off-keying envelope detection with an adaptive noise floor, and
//! light signal tracking. Produces one [`SkimmerSpot`] per tracked CW signal.
//!
//! Finding the signals and reading them are two separate jobs here, and only the
//! second one changed when DeepCW arrived. The STFT below still decides where
//! the stations are, how strong they are and what frequency to put the marker
//! on — all of which it does from bin power alone, and none of which a character
//! model has an opinion about. The text comes from [`crate::deep`], which lifts
//! each station's slice out of a second transform at the model's own resolution
//! and hands it to a pool of decoders.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner};
use sdroxide_deepcw::Pool;
use sdroxide_dsp::Complex32 as C32;
use sdroxide_types::{SkimmerKind, SkimmerSpot};

use crate::callsign::find_callsign;
use crate::deep::DeepFront;

// A 4096-pt window over the ~200 kHz skim rate is ~20 ms / ~49 Hz per bin —
// close to a CW signal's own bandwidth, so the carrier lands in one bin with
// good SNR, while the window is still shorter than a dit at moderate speeds.
const FFT_SIZE: usize = 4096;
/// Hop between analysis frames (75% overlap). Frame time = HOP / skim_rate
/// (~5 ms at 200 kHz) — good keying resolution for CW.
const HOP: usize = 1024;
/// Frames of noise-floor priming before detection starts.
const WARMUP: u32 = 40;

/// Detection threshold above the per-bin noise floor (power ratio; ~10 dB).
/// High enough that random noise rarely crosses it across thousands of bins.
///
/// This only decides where the *signals* are — which bins are worth tracking,
/// where the spot marker goes, and whether a track is still alive. Whether the
/// key was down at any given moment is [`CwDecoder`]'s business, and it decides
/// it by fitting a threshold of its own to each track's envelope.
const ON_RATIO: f32 = 10.0;
/// Bins per region for the median noise-floor estimate (~3 kHz at 47 Hz/bin).
/// The median of a region is a noise bin as long as CW signals stay sparse in
/// it, so a signal — however strong or persistent — can't inflate the floor.
const REGION_BINS: usize = 64;
/// |FFT|² of Gaussian noise is exponential, whose median is `ln 2`·mean; scale
/// the region median back up to estimate the mean noise the thresholds expect.
const MEDIAN_TO_MEAN: f32 = 1.4427;
/// A peak must be the strongest bin within ±this window to count — enforces a
/// minimum signal spacing and rejects a strong signal's own leakage sidelobes.
const PEAK_SPACING: usize = 8; // ~±390 Hz at 49 Hz/bin
/// Bins within this of a track's center are "the same signal" (spawn tolerance).
const TRACK_TOL: i64 = 3;
/// Guard band around DC (the window center) to ignore, in bins.
const DC_GUARD: i64 = 3;
/// Frames a track must be detected before it's reported (rejects noise blips;
/// one dit at 20 WPM is ~12 frames at this hop).
const MIN_HITS: u32 = 8;
/// Track pruning (ms). A blip that never became a signal goes quickly; anything
/// that did is kept long enough to be worth keeping.
///
/// The middle case exists because the model needs several seconds of a station
/// before it will say anything at all. A track that has been keying but has no
/// text yet is not a failure, it is one that has not been read yet, and pruning
/// it on the old empty-track timer would drop every station that paused inside
/// its first few seconds — before it had ever had a chance to decode.
const PRUNE_EMPTY_MS: f64 = 1200.0;
const PRUNE_PENDING_MS: f64 = 9000.0;
const PRUNE_DECODED_MS: f64 = 8000.0;
/// A track counts as "active" (currently keying) within this of its last mark.
const ACTIVE_MS: f64 = 1500.0;
/// Bound on simultaneous tracks.
const MAX_TRACKS: usize = 256;
/// Rolling decoded text kept per track.
const MAX_TEXT: usize = 64;

/// Spin up the model and its threads, or report why not and carry on without.
///
/// A skimmer that finds and marks signals but cannot read them is still worth
/// having, so a model that will not load is a downgrade rather than a failure.
fn build_deep(skim_rate: f64) -> Option<(DeepFront, Pool)> {
    // Inference is the only thing here worth spreading, and it is not worth the
    // whole machine: the skimmer shares it with a receiver.
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(1, 4);
    match Pool::new(threads) {
        Ok(pool) => Some((DeepFront::new(skim_rate, FFT_SIZE), pool)),
        Err(e) => {
            tracing::error!("DeepCW unavailable, the CW skimmer will mark signals only: {e}");
            None
        }
    }
}

struct Track {
    id: u64,
    bin: i64, // signed offset bin from DC (negative = below center)
    /// Whatever the model last made of this station, and the speed implied by
    /// how fast the text arrived. Both are replaced wholesale each time a decode
    /// comes back rather than accumulated, because the model re-reads its whole
    /// window every round and may revise what it said last time.
    text: String,
    wpm: u16,
    last_on_ms: f64,
    snr_db: i16,
    /// Frames this track has been keyed (for confirmation).
    hits: u32,
    /// Time-smoothed power at [bin-1, bin, bin+1], accumulated over keyed-on
    /// frames. Quadratic interpolation over these three resolves the carrier to
    /// a fraction of a bin, so the spot marker lands on the signal instead of
    /// snapping to the 49 Hz grid. Smoothing is essential: a single frame of a
    /// keying CW signal is spiky and would bias the estimate.
    pk: [f32; 3],
}

pub struct CwSkimmer {
    skim_rate: f64,
    skim_center_hz: f64,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    inbuf: Vec<C32>,
    /// Read cursor into `inbuf`; consumed samples are compacted out rarely so
    /// the STFT never memmoves the whole buffer every frame.
    read_pos: usize,
    scratch: Vec<C32>,
    power: Vec<f32>,
    /// Per-bin noise floor (the region median scaled to a mean), time-smoothed.
    noise: Vec<f32>,
    /// Reused scratch for the per-region median (one region's power values).
    med_scratch: Vec<f32>,
    /// Reused per-frame scratch (avoids re-allocating every frame).
    cands: Vec<(f32, i64)>,
    centers: Vec<i64>,
    frame_ms: f32,
    frames: u32,
    now_ms: f64,
    tracks: Vec<Track>,
    next_id: u64,
    /// The model's front end, and the threads that run it. `None` if the model
    /// would not load, in which case the skimmer still finds and marks signals
    /// but reports no text for them.
    deep: Option<(DeepFront, Pool)>,
    /// Centers seen last frame, so a track spawns only on a peak that persists
    /// (a single-frame noise blip never becomes a track).
    prev_centers: Vec<i64>,
}

impl CwSkimmer {
    pub fn new(skim_rate: f64, skim_center_hz: f64) -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        // Hann window (reduces spectral leakage between adjacent CW signals).
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                let x = std::f32::consts::PI * i as f32 / (FFT_SIZE as f32 - 1.0);
                x.sin().powi(2)
            })
            .collect();
        CwSkimmer {
            skim_rate,
            skim_center_hz,
            fft,
            window,
            inbuf: Vec::with_capacity(FFT_SIZE * 4),
            read_pos: 0,
            scratch: vec![C32::default(); FFT_SIZE],
            power: vec![0.0; FFT_SIZE],
            noise: vec![0.0; FFT_SIZE],
            med_scratch: Vec::with_capacity(REGION_BINS),
            cands: Vec::with_capacity(512),
            centers: Vec::with_capacity(256),
            frame_ms: (HOP as f64 / skim_rate * 1000.0) as f32,
            frames: 0,
            now_ms: 0.0,
            tracks: Vec::new(),
            next_id: 1,
            deep: build_deep(skim_rate),
            prev_centers: Vec::new(),
        }
    }

    pub fn set_center(&mut self, center_hz: f64) {
        if (center_hz - self.skim_center_hz).abs() > 1.0 {
            self.skim_center_hz = center_hz;
            self.reset();
        }
    }

    /// Forget every track and re-prime the noise floor (band moved, or the
    /// skimmer was switched off and its state is now stale).
    pub fn reset(&mut self) {
        self.tracks.clear();
        self.inbuf.clear();
        self.read_pos = 0;
        self.frames = 0;
        if let Some((front, _)) = self.deep.as_mut() {
            front.reset();
        }
    }

    /// Feed a block of complex baseband IQ (skim-rate, centered on skim_center).
    pub fn process(&mut self, iq: &[C32]) {
        self.inbuf.extend_from_slice(iq);
        while self.read_pos + FFT_SIZE <= self.inbuf.len() {
            let base = self.read_pos;
            for i in 0..FFT_SIZE {
                self.scratch[i] = self.inbuf[base + i] * self.window[i];
            }
            self.fft.process(&mut self.scratch);
            self.on_frame();
            self.read_pos += HOP;
            self.frames = self.frames.saturating_add(1);
            self.now_ms += self.frame_ms as f64;
        }
        // Compact only once the consumed prefix is large, so the memmove is
        // amortized O(1)/sample instead of shifting the buffer every frame.
        if self.read_pos >= FFT_SIZE {
            self.inbuf.drain(..self.read_pos);
            self.read_pos = 0;
        }
        self.run_deep(iq);
    }

    /// Keep the model fed and collect whatever it has finished.
    fn run_deep(&mut self, iq: &[C32]) {
        let Some((front, pool)) = self.deep.as_mut() else { return };

        // Strongest first, so that when there are more stations than slots the
        // ones that get read are the ones most likely to be readable.
        let mut live: Vec<(u64, i64, i16)> =
            self.tracks.iter().map(|t| (t.id, t.bin, t.snr_db)).collect();
        live.sort_unstable_by(|a, b| b.2.cmp(&a.2));
        let live: Vec<(u64, i64)> = live.into_iter().map(|(id, bin, _)| (id, bin)).collect();
        front.sync(&live);
        front.process(iq);

        for (id, window) in front.due() {
            // A refused job is left unmarked and comes back next round.
            if !pool.submit(id, window) {
                break;
            }
            front.mark_submitted(id);
        }

        for (id, decoded) in pool.poll() {
            front.complete(id);
            let decoded = match decoded {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("DeepCW: {e}");
                    continue;
                }
            };
            let Some(track) = self.tracks.iter_mut().find(|t| t.id == id) else { continue };
            let text = decoded.normalized();
            if text.is_empty() {
                continue;
            }
            if let Some(wpm) = decoded.wpm() {
                track.wpm = wpm.round().clamp(0.0, 99.0) as u16;
            }
            // Keep the tail: a spot is a rolling read-out, not a transcript.
            track.text =
                match text.char_indices().nth(text.chars().count().saturating_sub(MAX_TEXT)) {
                    Some((cut, _)) => text[cut..].to_string(),
                    None => text,
                };
        }
    }

    fn on_frame(&mut self) {
        let n = FFT_SIZE;
        for k in 0..n {
            self.power[k] = self.scratch[k].norm_sqr();
        }

        // Per-bin noise floor from a per-region median. The old approach was a
        // single floor-gated EMA updated whenever `power <= floor * ON_RATIO` — a
        // positive-feedback trap: once a strong, near-continuous signal's floor
        // crept above `signal / ON_RATIO` (primed high while it was keying, or
        // riding a high ambient noise floor), the signal stopped exceeding the
        // key-on threshold and was folded straight back into its own floor, so it
        // never decoded — the "strong signal in high noise won't decode while
        // weaker ones do" failure.
        //
        // Fix: estimate the floor from the MEDIAN of each ~64-bin region rather
        // than per-bin history. CW signals are narrow and sparse, so the median
        // of a region is always a noise bin — a signal, however strong or
        // persistent, cannot pull the median up and trap the floor. Scale the
        // median to a mean (thresholds expect the mean) and smooth over time.
        let smooth = if self.frames < WARMUP { 0.3 } else { 0.05 };
        let mut med = std::mem::take(&mut self.med_scratch);
        for base in (0..n).step_by(REGION_BINS) {
            med.clear();
            med.extend_from_slice(&self.power[base..(base + REGION_BINS).min(n)]);
            let mid = med.len() / 2;
            med.select_nth_unstable_by(mid, |a, b| {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            });
            let est = med[mid] * MEDIAN_TO_MEAN;
            for k in base..(base + REGION_BINS).min(n) {
                self.noise[k] += smooth * (est - self.noise[k]);
            }
        }
        self.med_scratch = med;
        if self.frames < WARMUP {
            return; // priming only — no detection yet
        }

        // Signal centers via non-max suppression: collect above-threshold bins,
        // then take them strongest-first, suppressing anything within
        // ±PEAK_SPACING of an already-taken peak. This yields exactly one center
        // per signal (a plateau of near-equal bins can't spawn duplicates) and
        // rejects a strong signal's own leakage sidelobes.
        let mut cands = std::mem::take(&mut self.cands);
        cands.clear();
        for k in 0..n {
            let off = self.offset_bin(k);
            if off.abs() < DC_GUARD {
                continue;
            }
            let p = self.power[k];
            if p > self.noise[k] * ON_RATIO {
                cands.push((p, off));
            }
        }
        cands.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let spacing = PEAK_SPACING as i64;
        let mut centers = std::mem::take(&mut self.centers);
        centers.clear();
        for &(_, off) in cands.iter() {
            if centers.iter().all(|&c| (c - off).abs() > spacing) {
                centers.push(off);
            }
        }

        // Spawn a fixed-bin track for each center that has no track nearby and
        // that persisted from the previous frame (a single-frame noise blip
        // never becomes one). Tracks never move — CW carriers are stable to well
        // under a bin, so a fixed bin gives clean per-signal envelopes and can't
        // drift onto a neighbour's noise.
        for &off in &centers {
            let near = self.tracks.iter().any(|t| (t.bin - off).abs() <= spacing);
            let persisted = self.prev_centers.iter().any(|&c| (c - off).abs() <= TRACK_TOL);
            if !near && persisted && self.tracks.len() < MAX_TRACKS {
                let id = self.next_id;
                self.next_id += 1;
                let k = self.bin_index(off);
                let km = self.bin_index(off - 1);
                let kp = self.bin_index(off + 1);
                self.tracks.push(Track {
                    id,
                    bin: off,
                    text: String::new(),
                    wpm: 0,
                    last_on_ms: self.now_ms,
                    snr_db: 0,
                    hits: 0,
                    pk: [self.power[km], self.power[k], self.power[kp]],
                });
            }
        }
        // This frame's centers become next frame's `prev_centers`; the old
        // buffers are kept for reuse (no per-frame allocation).
        std::mem::swap(&mut self.prev_centers, &mut centers);
        self.centers = centers;
        self.cands = cands;

        // Advance every track: hand its bin's magnitude to the decoder, and keep
        // the bookkeeping the *spot* needs — is anything there, how strong, and
        // exactly which frequency — from a plain threshold on the bin.
        let now = self.now_ms;
        for t in self.tracks.iter_mut() {
            let k = (t.bin.rem_euclid(n as i64)) as usize;
            let floor = self.noise[k].max(1e-12);
            let p = self.power[k];
            if p > floor * ON_RATIO {
                t.hits = t.hits.saturating_add(1);
                t.last_on_ms = now;
                t.snr_db = (10.0 * (p / floor).log10()).round().clamp(-30.0, 60.0) as i16;
                // Accumulate the smoothed 3-bin peak shape while keyed.
                let km = ((t.bin - 1).rem_euclid(n as i64)) as usize;
                let kp = ((t.bin + 1).rem_euclid(n as i64)) as usize;
                t.pk[0] = 0.9 * t.pk[0] + 0.1 * self.power[km];
                t.pk[1] = 0.9 * t.pk[1] + 0.1 * self.power[k];
                t.pk[2] = 0.9 * t.pk[2] + 0.1 * self.power[kp];
            }
        }

        // Prune stale tracks.
        self.tracks.retain(|t| {
            let age = now - t.last_on_ms;
            let keep = match (t.text.is_empty(), t.hits >= MIN_HITS) {
                (false, _) => PRUNE_DECODED_MS,
                (true, true) => PRUNE_PENDING_MS,
                (true, false) => PRUNE_EMPTY_MS,
            };
            age < keep
        });
    }

    /// Snapshot the current tracked signals worth reporting. Filters out noise
    /// tracks: a spot needs a confirmed track, a plausible speed, and text with
    /// real content (a callsign, or several non-trivial characters — random
    /// noise mostly decodes to strings of E/I/T).
    pub fn spots(&self) -> Vec<SkimmerSpot> {
        let bin_hz = self.skim_rate / FFT_SIZE as f64;
        self.tracks
            .iter()
            .filter_map(|t| {
                if t.hits < MIN_HITS {
                    return None;
                }
                let text = t.text.trim_end();
                // A wider range than a timing decoder would need: this speed is
                // measured from how fast text arrived, so a station that pauses
                // reads slower than its fist.
                if !(5..=60).contains(&t.wpm) || text.is_empty() {
                    return None;
                }
                // Quadratic peak interpolation over the smoothed 3-bin shape
                // gives a sub-bin carrier offset in [-0.5, 0.5].
                let [a, b, c] = t.pk;
                let denom = a - 2.0 * b + c;
                let delta =
                    if denom < 0.0 { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) } else { 0.0 };
                let callsign = find_callsign(text);
                let meaty = text
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() && !matches!(c, 'E' | 'I' | 'T'))
                    .count();
                if callsign.is_none() && meaty < 3 {
                    return None;
                }
                Some(SkimmerSpot {
                    id: t.id,
                    kind: SkimmerKind::Cw,
                    freq_hz: self.skim_center_hz + (t.bin as f64 + delta as f64) * bin_hz,
                    callsign,
                    text: text.to_string(),
                    snr_db: t.snr_db,
                    wpm: t.wpm,
                    active: (self.now_ms - t.last_on_ms) < ACTIVE_MS,
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub fn debug_dump(&self) {
        for t in &self.tracks {
            if t.hits >= 3 {
                eprintln!("bin{} hits{} wpm{} text={:?}", t.bin, t.hits, t.wpm, t.text);
            }
        }
    }

    /// Signed offset (in bins) of FFT bin `k` from DC.
    fn offset_bin(&self, k: usize) -> i64 {
        let n = FFT_SIZE as i64;
        let k = k as i64;
        if k <= n / 2 { k } else { k - n }
    }

    /// FFT bin index for a signed offset bin.
    fn bin_index(&self, off: i64) -> usize {
        let n = FFT_SIZE as i64;
        (off.rem_euclid(n)) as usize
    }
}

#[cfg(test)]
mod tests {
    use sdroxide_dsp::CwTx;

    use super::*;

    /// Build skim IQ: a keyed CW tone at `off_hz` plus noise of amplitude
    /// `noise` per component (0.02 = light; a large value = a high noise floor).
    ///
    /// The keying comes from the shared [`CwTx`], so the skimmer is exercised
    /// against exactly the envelope shape the rest of the tree agrees is CW —
    /// raised-cosine edges and all — rather than against square keying only
    /// this test knows how to make.
    fn synth(text: &str, off_hz: f64, wpm: f32, rate: f64, noise: f32) -> Vec<C32> {
        // Generate the sidetone at a rate where a dit is plenty of samples,
        // then take its envelope and re-key a complex carrier at the skim rate.
        const AUDIO: f64 = 8000.0;
        let mut tx = CwTx::new(AUDIO, 1000.0, wpm);
        tx.push_text(text);
        let mut env: Vec<f32> = Vec::new();
        while !tx.drained() {
            let mut blk = [0.0f32; 512];
            tx.next_block(&mut blk);
            env.extend_from_slice(&blk);
        }
        // The sidetone's own envelope: its peak over each cycle of the 1 kHz
        // tone, which is one keying envelope sample every 8 audio samples.
        let key: Vec<f32> =
            env.chunks(8).map(|c| c.iter().fold(0.0f32, |a, b| a.max(b.abs()))).collect();
        let key_rate = AUDIO / 8.0;

        let lead = (key_rate * 0.25) as usize;
        let tail = (key_rate * 1.2) as usize;

        let mut iq = Vec::new();
        let mut phase = 0.0f64;
        let dphi = 2.0 * std::f64::consts::PI * off_hz / rate;
        let mut seed = 0x1234_5678u32;
        let mut rng = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        let spk = rate / key_rate; // skim samples per key-envelope sample
        let mut frac = 0.0f64;
        for i in 0..lead + key.len() + tail {
            let amp = key.get(i.wrapping_sub(lead)).copied().unwrap_or(0.0);
            frac += spk;
            let take = frac as usize;
            frac -= take as f64;
            for _ in 0..take {
                phase += dphi;
                iq.push(C32::new(
                    amp * phase.cos() as f32 + noise * rng(),
                    amp * phase.sin() as f32 + noise * rng(),
                ));
            }
        }
        iq
    }

    /// Let the model catch up. Decoding happens on a pool of threads now, so a
    /// test that stops feeding IQ has to wait for what is still in flight.
    fn settle(sk: &mut CwSkimmer) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut idle = 0;
        while std::time::Instant::now() < deadline && idle < 60 {
            let before = sk.spots().len();
            sk.process(&[]);
            let quiet = sk.deep.as_ref().is_none_or(|(_, pool)| pool.in_flight() == 0);
            if quiet && sk.spots().len() == before {
                idle += 1;
            } else {
                idle = 0;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn decodes_a_single_cw_tone() {
        let rate = 192_000.0;
        let center = 14_020_000.0;
        let off = 5_000.0;
        let iq = synth("CQ DE W1AW", off, 20.0, rate, 0.02);

        let mut sk = CwSkimmer::new(rate, center);
        for chunk in iq.chunks(8192) {
            sk.process(chunk);
        }
        settle(&mut sk);
        sk.debug_dump();
        let spots = sk.spots();
        assert!(!spots.is_empty(), "no spots decoded");
        // The spot should sit within a couple bins of the true frequency.
        let s = spots
            .iter()
            .min_by(|a, b| {
                (a.freq_hz - (center + off))
                    .abs()
                    .partial_cmp(&(b.freq_hz - (center + off)).abs())
                    .unwrap()
            })
            .unwrap();
        assert!(
            (s.freq_hz - (center + off)).abs() < 100.0,
            "freq off: {} vs {}",
            s.freq_hz,
            center + off
        );
        assert!(s.text.contains("W1AW"), "text: {:?}", s.text);
        assert_eq!(s.callsign.as_deref(), Some("W1AW"));
    }

    #[test]
    fn decodes_a_strong_signal_in_a_high_noise_floor() {
        // Regression for the noise-floor trap: a prominent, near-continuously
        // keyed CW signal riding a HIGH noise floor used to stop decoding (the
        // old floor-gated EMA folded it into its own floor), while weaker signals
        // kept working. The region-median floor can't be inflated by the signal.
        let rate = 192_000.0;
        let center = 7_020_000.0;
        let off = -4_000.0;
        // A high broadband noise floor (15× the light-noise case) with a strong
        // but not overwhelming carrier.
        let iq = synth("CQ TEST DE W1AW W1AW", off, 22.0, rate, 0.3);

        let mut sk = CwSkimmer::new(rate, center);
        for chunk in iq.chunks(8192) {
            sk.process(chunk);
        }
        settle(&mut sk);
        let spots = sk.spots();
        let hit = spots.iter().find(|s| s.text.contains("W1AW"));
        assert!(hit.is_some(), "strong signal in high noise did not decode: {spots:?}");
    }

    /// A crowded window: several stations at once, at different speeds and 25 dB
    /// apart in level, on a common noise floor.
    ///
    /// This is what a skimmer is for and the case a single global key-on
    /// threshold cannot serve — the level that reads the loud station's keying
    /// is above the weak one's marks entirely. Each track fits its own.
    #[test]
    fn decodes_several_stations_at_once() {
        let rate = 192_000.0;
        let center = 14_030_000.0;
        let want = [
            ("CQ TEST DE W1AW W1AW K", 6_000.0, 18.0, 1.0),
            ("CQ CQ DE K5ZZ K5ZZ K", -2_500.0, 30.0, 0.06),
            ("CQ DE VK3XY VK3XY K", 12_000.0, 24.0, 0.3),
        ];
        let mut mixed: Vec<C32> = Vec::new();
        for (text, off, wpm, amp) in want {
            let sig = synth(text, off, wpm, rate, 0.0);
            if mixed.len() < sig.len() {
                mixed.resize(sig.len(), C32::default());
            }
            for (m, s) in mixed.iter_mut().zip(sig.iter()) {
                *m += *s * amp;
            }
        }
        // One noise floor under all of them.
        let mut seed = 0xC0FF_EE01u32;
        for m in mixed.iter_mut() {
            let mut r = || {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            };
            *m += C32::new(0.02 * r(), 0.02 * r());
        }

        let mut sk = CwSkimmer::new(rate, center);
        for chunk in mixed.chunks(8192) {
            sk.process(chunk);
        }
        settle(&mut sk);
        sk.debug_dump();
        let spots = sk.spots();
        for (_, off, wpm, _) in want {
            let call = ["W1AW", "K5ZZ", "VK3XY"]
                [want.iter().position(|w| w.1 == off).expect("offset is one of ours")];
            let hit = spots
                .iter()
                .find(|s| s.callsign.as_deref() == Some(call))
                .unwrap_or_else(|| panic!("{call} not spotted; got {spots:?}"));
            let err = hit.freq_hz - (center + off);
            assert!(err.abs() < 60.0, "{call} spotted {err:+.0} Hz off");
            assert!(
                (hit.wpm as f32 - wpm).abs() < 4.0,
                "{call} read as {} WPM, sent at {wpm}",
                hit.wpm
            );
        }
    }

    /// End-to-end frequency accuracy through the *real* engine path: device-rate
    /// IQ → skim DDC → CwSkimmer. Catches any bin/decimation mismatch that would
    /// mistune the spot marker against the waterfall.
    #[test]
    fn frequency_is_accurate_through_the_ddc() {
        let dev_rate = 2_000_000.0;
        let center = 14_025_000.0;
        for off in [2_000.0f64, -3_500.0, 7_000.0] {
            let iq_dev = synth("CQ DE W1AW", off, 24.0, dev_rate, 0.02);
            let mut ddc = sdroxide_dsp::Ddc::new(dev_rate, 192_000.0);
            let skim_rate = ddc.out_rate();
            let mut sk = CwSkimmer::new(skim_rate, center);
            let mut buf = Vec::new();
            for chunk in iq_dev.chunks(16_384) {
                buf.clear();
                ddc.process(chunk, &mut buf);
                sk.process(&buf);
            }
            settle(&mut sk);
            let spots = sk.spots();
            assert!(!spots.is_empty(), "off {off}: no spots");
            let want = center + off;
            let s = spots
                .iter()
                .min_by(|a, b| {
                    (a.freq_hz - want).abs().partial_cmp(&(b.freq_hz - want).abs()).unwrap()
                })
                .unwrap();
            let err = s.freq_hz - want;
            eprintln!("off {off:+}: got {} want {} err {err:+.1} Hz", s.freq_hz, want);
            assert!(err.abs() < 20.0, "off {off}: freq error {err:+.1} Hz");
        }
    }
}
