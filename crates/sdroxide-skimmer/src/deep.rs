//! The DeepCW front end for the skimmer.
//!
//! Every tracked station needs its own window through the model, and the model
//! wants that window as a 65-bin spectrogram on a fixed grid: 12.5 Hz bins from
//! an 80 ms window, with the carrier in the middle. The obvious way to get there
//! is a filter chain per station — mix, decimate, resample — and it costs a
//! filter chain per station.
//!
//! One transform gets it for all of them at once. A CW skimmer is already
//! looking at a wide complex baseband window; taken at the model's own
//! resolution, every station's 65 bins are already sitting in it, and lifting
//! one out is a strided copy. Whether the band holds two signals or forty, the
//! transform is the same transform.
//!
//! What that costs is exactness in frequency: a station is centred to the
//! nearest whole bin of the detector's coarser grid rather than to the hertz.
//! The model's band is 800 Hz wide and it was trained across all of it, so being
//! a few tens of hertz off centre is not something it notices. Keeping the
//! centre *fixed* does matter, and an integer bin is fixed — an interpolated one
//! would wander as the estimate refined and smear the spectrogram it is meant to
//! be building.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;

use rustfft::{Fft, FftPlanner};
use sdroxide_deepcw::{BINS, CENTER_BIN, Window};
use sdroxide_dsp::Complex32 as C32;

/// Rolling spectrogram kept per station. Long enough to give the model real
/// context — it reads a whole window at once and copies better with more of it —
/// and short enough that thirty of them are a few megabytes.
const WINDOW_S: f64 = 12.0;
/// Don't decode a station until it has been heard this long. The model's own
/// documentation puts its floor at five seconds.
const MIN_S: f64 = 5.0;
/// How often each station is re-decoded.
const INTERVAL_S: f64 = 2.0;
/// Stations buffered and decoded at once.
///
/// The detector will happily track far more than this on a busy band, and the
/// ones past the cap keep their spot marker and their frequency — they simply
/// carry no text. That is a real limit and it is logged when it bites.
pub const MAX_TRACKS: usize = 32;

struct Buffered {
    /// Centre bin in the wide transform. Fixed for the life of the track.
    bin: i64,
    /// `[frames][BINS]` row-major, oldest first.
    rows: VecDeque<f32>,
    last_submit_ms: f64,
    /// Set once the track has been submitted, so the first decode waits for
    /// `MIN_S` but later ones only wait for `INTERVAL_S`.
    submitted: bool,
    /// A window for this station is with the pool right now.
    ///
    /// At most one may be, and that is the point. The pool runs several threads
    /// and finishes jobs in whatever order they happen to finish, so two windows
    /// of the same station in flight together can land backwards — and since each
    /// result replaces the station's text wholesale, an older, shorter window
    /// would overwrite a newer one. How often that happens depends on how loaded
    /// the machine is, which is the worst way for a decoder to be wrong.
    pending: bool,
}

pub struct DeepFront {
    fft: Arc<dyn Fft<f32>>,
    size: usize,
    hop: usize,
    /// Wide-transform bins per bin of the detector's coarser grid.
    bins_per_coarse: f64,
    /// Maps the wide transform's magnitudes onto the scale the model was trained
    /// on. Both a carrier and the noise around it scale with the number of
    /// samples in the window, and the window is the same 80 ms either way, so
    /// one factor puts both right — see the note on `Window::Spectrogram`.
    scale: f32,
    window: Vec<f32>,
    inbuf: Vec<C32>,
    read_pos: usize,
    scratch: Vec<C32>,
    mag: Vec<f32>,
    tracks: HashMap<u64, Buffered>,
    max_rows: usize,
    now_ms: f64,
    frame_ms: f64,
    /// Logged once per skimmer rather than once per frame.
    warned_full: bool,
}

impl DeepFront {
    /// `skim_rate` is the wide window's sample rate; `coarse_bins` is the size
    /// of the detector's own transform, whose bin numbering the caller uses.
    pub fn new(skim_rate: f64, coarse_bins: usize) -> Self {
        let size = (skim_rate / sdroxide_deepcw::BIN_HZ).round().max(64.0) as usize;
        let hop = (skim_rate * sdroxide_deepcw::FRAME_S).round().max(1.0) as usize;
        let fft = FftPlanner::<f32>::new().plan_fft_forward(size);
        // Periodic Hann, matching the model's own front end.
        let window: Vec<f32> = (0..size)
            .map(|i| 0.5 * (1.0 - (std::f64::consts::TAU * i as f64 / size as f64).cos()) as f32)
            .collect();
        let frame_ms = hop as f64 / skim_rate * 1000.0;
        DeepFront {
            fft,
            size,
            hop,
            bins_per_coarse: size as f64 / coarse_bins as f64,
            scale: (sdroxide_deepcw::SAMPLE_RATE / skim_rate) as f32,
            window,
            inbuf: Vec::new(),
            read_pos: 0,
            scratch: vec![C32::default(); size],
            mag: vec![0.0; size],
            tracks: HashMap::new(),
            max_rows: (WINDOW_S / sdroxide_deepcw::FRAME_S) as usize,
            now_ms: 0.0,
            frame_ms,
            warned_full: false,
        }
    }

    pub fn reset(&mut self) {
        self.tracks.clear();
        self.inbuf.clear();
        self.read_pos = 0;
    }

    /// Tell the front end which stations exist, strongest first.
    ///
    /// Stations that have gone away lose their buffer; new ones get one while
    /// there is room. Existing buffers are never evicted for a stronger arrival —
    /// a station part-way through a window would lose everything it had, and
    /// churning the strongest few would mean nobody ever accumulates enough to
    /// decode.
    pub fn sync(&mut self, live: &[(u64, i64)]) {
        self.tracks.retain(|id, _| live.iter().any(|(live_id, _)| live_id == id));
        for &(id, coarse_bin) in live {
            if self.tracks.contains_key(&id) {
                continue;
            }
            if self.tracks.len() >= MAX_TRACKS {
                if !self.warned_full {
                    self.warned_full = true;
                    tracing::info!(
                        max = MAX_TRACKS,
                        "more CW signals than DeepCW slots; the weakest carry no text"
                    );
                }
                break;
            }
            self.tracks.insert(
                id,
                Buffered {
                    bin: (coarse_bin as f64 * self.bins_per_coarse).round() as i64,
                    rows: VecDeque::with_capacity(self.max_rows * BINS),
                    last_submit_ms: self.now_ms,
                    submitted: false,
                    pending: false,
                },
            );
        }
    }

    /// Feed the same IQ the detector sees.
    pub fn process(&mut self, iq: &[C32]) {
        if self.tracks.is_empty() {
            // Nothing to buffer for; keep the cursor from growing without bound.
            self.inbuf.clear();
            self.read_pos = 0;
            return;
        }
        self.inbuf.extend_from_slice(iq);
        while self.read_pos + self.size <= self.inbuf.len() {
            for i in 0..self.size {
                self.scratch[i] = self.inbuf[self.read_pos + i] * self.window[i];
            }
            self.fft.process(&mut self.scratch);
            for (k, z) in self.scratch.iter().enumerate() {
                // log1p of the scaled magnitude — the model's normalisation.
                self.mag[k] = (z.norm() * self.scale).ln_1p();
            }
            self.append_rows();
            self.read_pos += self.hop;
            self.now_ms += self.frame_ms;
        }
        if self.read_pos >= self.size {
            self.inbuf.drain(..self.read_pos);
            self.read_pos = 0;
        }
    }

    /// Lift each station's 65 bins out of this frame.
    fn append_rows(&mut self) {
        let n = self.size as i64;
        for track in self.tracks.values_mut() {
            for d in 0..BINS {
                let src = track.bin + d as i64 - CENTER_BIN as i64;
                // Outside the wide window there is nothing to copy; a station
                // that close to the edge sees silence beside it.
                let v = if src > -n / 2 && src < n / 2 {
                    self.mag[src.rem_euclid(n) as usize]
                } else {
                    0.0
                };
                track.rows.push_back(v);
            }
            while track.rows.len() > self.max_rows * BINS {
                track.rows.drain(..BINS);
            }
        }
    }

    /// Stations with enough audio buffered that they are worth decoding again.
    pub fn due(&self) -> Vec<(u64, Window)> {
        self.tracks
            .iter()
            .filter(|(_, t)| {
                let seconds = t.rows.len() as f64 / BINS as f64 * sdroxide_deepcw::FRAME_S;
                let wait = if t.submitted { INTERVAL_S } else { MIN_S };
                !t.pending && seconds >= MIN_S && self.now_ms - t.last_submit_ms >= wait * 1000.0
            })
            .map(|(id, t)| (*id, Window::Spectrogram(t.rows.iter().copied().collect())))
            .collect()
    }

    /// Note that `id` was taken by the pool. A refused job is simply not marked,
    /// so it comes up again next round.
    pub fn mark_submitted(&mut self, id: u64) {
        if let Some(t) = self.tracks.get_mut(&id) {
            t.last_submit_ms = self.now_ms;
            t.submitted = true;
            t.pending = true;
        }
    }

    /// A station's window came back, so it may be given another.
    pub fn complete(&mut self, id: u64) {
        if let Some(t) = self.tracks.get_mut(&id) {
            t.pending = false;
        }
    }

    /// Seconds of spectrogram buffered for a station, for tests and logging.
    #[cfg(test)]
    pub fn buffered_s(&self, id: u64) -> f64 {
        self.tracks
            .get(&id)
            .map_or(0.0, |t| t.rows.len() as f64 / BINS as f64 * sdroxide_deepcw::FRAME_S)
    }

    #[cfg(test)]
    pub fn advance_for_test(&mut self, ms: f64) {
        self.now_ms += ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn front() -> DeepFront {
        DeepFront::new(192_000.0, 4096)
    }

    #[test]
    fn the_transform_lands_on_the_models_grid() {
        let f = front();
        // 12.5 Hz bins from an 80 ms window is the whole requirement.
        assert_eq!(f.size, 15_360);
        assert_eq!(192_000.0 / f.size as f64, sdroxide_deepcw::BIN_HZ);
        assert_eq!(f.size as f64 / 192_000.0, sdroxide_deepcw::WINDOW_S);
        assert_eq!(f.hop, 2_880);
        assert_eq!(f.hop as f64 / 192_000.0, sdroxide_deepcw::FRAME_S);
    }

    #[test]
    fn tracks_are_added_and_dropped_with_the_detector() {
        let mut f = front();
        f.sync(&[(1, 100), (2, -200)]);
        assert_eq!(f.tracks.len(), 2);
        f.sync(&[(2, -200)]);
        assert_eq!(f.tracks.len(), 1);
        assert!(f.tracks.contains_key(&2));
    }

    #[test]
    fn buffering_is_capped_and_stations_past_it_are_refused() {
        let mut f = front();
        let live: Vec<(u64, i64)> =
            (0..MAX_TRACKS as u64 + 8).map(|i| (i, i as i64 * 10)).collect();
        f.sync(&live);
        assert_eq!(f.tracks.len(), MAX_TRACKS);
    }

    #[test]
    fn a_buffer_does_not_grow_past_its_window() {
        let mut f = front();
        f.sync(&[(1, 0)]);
        let block = vec![C32::new(0.01, 0.0); f.hop * 40];
        for _ in 0..40 {
            f.process(&block);
        }
        assert!(f.buffered_s(1) <= WINDOW_S + 0.1, "buffered {:.1}s", f.buffered_s(1));
    }

    #[test]
    fn nothing_is_due_before_the_minimum_window() {
        let mut f = front();
        f.sync(&[(1, 0)]);
        f.process(&vec![C32::new(0.01, 0.0); f.hop * 100]); // ~1.5 s
        assert!(f.due().is_empty());
        f.process(&vec![C32::new(0.01, 0.0); f.hop * 300]); // past 5 s
        assert_eq!(f.due().len(), 1);
    }

    #[test]
    fn a_submitted_station_waits_its_interval() {
        let mut f = front();
        f.sync(&[(1, 0)]);
        f.process(&vec![C32::new(0.01, 0.0); f.hop * 400]);
        assert_eq!(f.due().len(), 1);
        f.mark_submitted(1);
        assert!(f.due().is_empty(), "resubmitted immediately");
        f.advance_for_test(INTERVAL_S * 1000.0 + 1.0);
        assert!(f.due().is_empty(), "resubmitted while its window was still out");
        f.complete(1);
        assert_eq!(f.due().len(), 1);
    }

    #[test]
    fn only_one_window_per_station_is_ever_out() {
        // Two in flight together can finish backwards, and the older one would
        // overwrite the newer station text.
        let mut f = front();
        f.sync(&[(1, 0)]);
        f.process(&vec![C32::new(0.01, 0.0); f.hop * 400]);
        assert_eq!(f.due().len(), 1);
        f.mark_submitted(1);
        for _ in 0..5 {
            f.advance_for_test(INTERVAL_S * 1000.0 + 1.0);
            assert!(f.due().is_empty());
        }
        f.complete(1);
        assert_eq!(f.due().len(), 1);
    }

    #[test]
    fn a_carrier_lands_in_the_middle_of_the_row() {
        // A tone at a known offset must come out at the band centre, which is
        // the whole point of the strided copy.
        let mut f = front();
        let coarse_bin = 100i64;
        f.sync(&[(7, coarse_bin)]);
        let hz = coarse_bin as f64 * 192_000.0 / 4096.0;
        let n = f.hop * 400;
        let iq: Vec<C32> = (0..n)
            .map(|i| {
                let ph = std::f64::consts::TAU * hz * i as f64 / 192_000.0;
                C32::new(ph.cos() as f32, ph.sin() as f32)
            })
            .collect();
        f.process(&iq);

        let Some((_, Window::Spectrogram(rows))) = f.due().into_iter().next() else {
            panic!("expected a spectrogram");
        };
        let frames = rows.len() / BINS;
        let row = &rows[(frames / 2) * BINS..][..BINS];
        let peak = row.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert_eq!(peak, CENTER_BIN, "carrier landed at bin {peak}");
    }
}
