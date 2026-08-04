//! `CwController` — the receive decoder and keyer behind the CW panel.
//!
//! CW is not a digital mode and this is not a digital-mode controller in the
//! sense the others are: there is no modem, no framing and nothing that has to
//! be decoded before the operator can use the signal. The rig is in CW, the
//! demodulated tone is what an operator listens to, and everything here runs
//! *alongside* that rather than in place of it — the panel reads a copy of what
//! the ear is already hearing, and keys what the operator types.
//!
//! It implements [`DigiEngine`] all the same, because the seam is exactly the
//! right shape: an audio tap in, actions and transmit audio out. What it does
//! not take from the other modes is ownership of the audio path. The
//! demodulated CW stays audible throughout.
//!
//! Two things are one thing here, and that is deliberate. The decoder listens
//! at `cw_pitch_hz` above the dial and the keyer transmits there, because in CW
//! those are the same frequency: you answer a station on the frequency you
//! heard it on, and the waterfall cursor that picks one picks the other.
//!
//! The text comes from DeepCW, which reads the spectrogram with a neural net
//! and copies several dB further down than a timing fit reaches. It answers only
//! that one question, though — it has no notion of speed, signal-to-noise, or
//! where the tone actually is. So the classic [`CwRx`] front end stays, running
//! alongside for exactly those readouts, and its AFC is also what tells the
//! tuner where to find the signal.

use std::collections::VecDeque;
use std::time::SystemTime;

use sdroxide_deepcw::{Tuner, Worker};
use sdroxide_dsp::{CwRx, CwTx, MonoResampler};
use sdroxide_types::{CwStatus, DigiConfig, DigiStatus, Mode, QsoStep, TranscriptLine};

use crate::DigiEngine;
use crate::controller::DigiAction;

/// Internal rate for the decoder and the keyer. High enough that the sidetone
/// is clean and the envelope detector has samples to work with, low enough that
/// the per-sample mixing costs nothing.
const CW_RATE: f64 = 8000.0;
const OUT_RATE: f64 = 48_000.0;
/// Cap on the rolling receive text.
const RX_TEXT_CAP: usize = 8000;
/// Sidetone samples generated per fill iteration.
const TX_CHUNK: usize = 400;
/// Drop out of transmit after this long with nothing left to send.
///
/// Holding the key down between characters is what makes typing feel like
/// sending, but "between characters" has to end somewhere: a transmitter left
/// keyed on an empty buffer holds the frequency, and the operator who wandered
/// off is exactly the one not watching for it.
const TX_IDLE_S: f32 = 5.0;

pub struct CwController {
    cfg: DigiConfig,

    // RX
    rx: CwRx,
    rx_rs: Option<MonoResampler>,
    rx_text: String,
    /// The tail DeepCW has decoded but not settled on. Shown after `rx_text`
    /// and replaced wholesale each time, so the last word or two visibly firms
    /// up instead of appearing late.
    rx_pending: String,
    /// `None` only if the model would not load, in which case the classic
    /// decoder's text is used instead of leaving the panel blank.
    deep: Option<Worker>,
    tuner: Tuner,
    deep_scratch: Vec<f32>,

    // TX
    tx: CwTx,
    tx_rs: Option<MonoResampler>,
    tx48: VecDeque<f32>,
    tx_buffer: String,
    tx_pushed: usize,
    tx_active: bool,
    keyed: bool,
    last_sent: usize,
    /// Sidetone samples produced with an empty keyer queue.
    idle_samples: usize,

    scratch: Vec<f32>,
    scratch48: Vec<f32>,
    status_dirty: bool,
    /// Last reported decoder state, so status is emitted when it changes rather
    /// than on every poll — the speed readout moves by a tenth of a WPM
    /// constantly and the panel does not need to hear about it.
    last_cw: CwStatus,
}

impl CwController {
    pub fn new(cfg: DigiConfig, tap_rate: f64) -> Self {
        let pitch = cfg.cw_pitch_hz;
        let mut rx = CwRx::new(CW_RATE, pitch);
        rx.set_speed_lock(cfg.cw_speed_lock.then_some(cfg.cw_wpm));
        let deep = match Worker::new() {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::error!("DeepCW unavailable, falling back to the timing decoder: {e}");
                None
            }
        };
        CwController {
            rx,
            rx_rs: MonoResampler::new(tap_rate, CW_RATE),
            rx_text: String::new(),
            rx_pending: String::new(),
            deep,
            tuner: Tuner::new(tap_rate, pitch as f64),
            deep_scratch: Vec::new(),
            tx: CwTx::new(CW_RATE, pitch as f64, cfg.cw_wpm),
            tx_rs: MonoResampler::new(CW_RATE, OUT_RATE),
            tx48: VecDeque::new(),
            tx_buffer: String::new(),
            tx_pushed: 0,
            tx_active: false,
            keyed: false,
            last_sent: 0,
            idle_samples: 0,
            scratch: Vec::new(),
            scratch48: Vec::new(),
            status_dirty: true,
            last_cw: CwStatus::default(),
            cfg,
        }
    }

    /// Append settled text to the receive window, keeping it word-separated and
    /// bounded.
    fn append_rx(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // DeepCW commits whole words with the edges trimmed, so the separator
        // between one commit and the next has to be put back.
        if !self.rx_text.is_empty()
            && !self.rx_text.ends_with(' ')
            && !text.starts_with(' ')
            && self.deep.is_some()
        {
            self.rx_text.push(' ');
        }
        self.rx_text.push_str(text);
        if self.rx_text.len() > RX_TEXT_CAP {
            let cut = self.rx_text.len() - RX_TEXT_CAP;
            let cut = (cut..self.rx_text.len())
                .find(|&i| self.rx_text.is_char_boundary(i))
                .unwrap_or(self.rx_text.len());
            self.rx_text.drain(..cut);
        }
        self.status_dirty = true;
    }

    /// Collect whatever the model finished since the last poll.
    fn drain_deep(&mut self) {
        let updates = self.deep.as_ref().map(Worker::poll).unwrap_or_default();
        for update in updates {
            match update {
                Ok(update) => {
                    self.append_rx(&update.committed);
                    if self.rx_pending != update.pending {
                        self.rx_pending = update.pending;
                        self.status_dirty = true;
                    }
                }
                // One failed inference is not fatal; the next window is a fresh
                // attempt at the same audio.
                Err(e) => tracing::warn!("DeepCW: {e}"),
            }
        }
    }

    /// The receive window as the panel should show it: settled text, then the
    /// tail that is still moving.
    fn rx_display(&self) -> String {
        if self.rx_pending.is_empty() {
            return self.rx_text.clone();
        }
        let mut out = String::with_capacity(self.rx_text.len() + self.rx_pending.len() + 1);
        out.push_str(&self.rx_text);
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(&self.rx_pending);
        out
    }

    fn cw_status(&self) -> CwStatus {
        CwStatus {
            locked: self.rx.locked(),
            wpm: self.rx.wpm(),
            snr_db: self.rx.snr_db(),
            tone_hz: self.rx.tone_hz(),
        }
    }

    /// True while we should keep generating sidetone: keyed, or still draining
    /// characters the operator typed before releasing transmit.
    fn producing(&self) -> bool {
        self.tx_active || !self.tx.drained()
    }

    fn build_status(&self) -> DigiStatus {
        DigiStatus {
            mode: Mode::Cw,
            step: QsoStep::Idle,
            dx_call: None,
            dx_grid: None,
            tx_next: self.tx_active,
            tx_pending_msg: (!self.tx_buffer.is_empty()).then(|| self.tx_buffer.clone()),
            audio_hz: self.cfg.cw_pitch_hz,
            tx_even: false,
            transmitting: self.keyed,
            tx_watchdog: false,
            transcript: Vec::<TranscriptLine>::new(),
            config: self.cfg.clone(),
            text_rx: self.rx_display(),
            tx_sent: self.tx.sent_chars(),
            fsq_heard: Vec::new(),
            fsq_messages: Vec::new(),
            rade: None,
            js8: None,
            fox_queue: Vec::new(),
            call_queue: Vec::new(),
            clock_offset_s: None,
            cw: Some(self.cw_status()),
            wspr: None,
        }
    }
}

impl DigiEngine for CwController {
    fn mode(&self) -> Mode {
        Mode::Cw
    }

    fn on_rx_audio(&mut self, tap: &[f32]) {
        // Our own sidetone is not a signal to copy. Full break-in would let the
        // decoder read the other station between our own elements, but the tap
        // carries what we are sending, not what they are, so reading it would
        // only echo us back onto the panel.
        if self.keyed {
            return;
        }
        self.scratch.clear();
        match &mut self.rx_rs {
            Some(r) => r.push(tap, &mut self.scratch),
            None => self.scratch.extend_from_slice(tap),
        }
        // The classic front end runs either way: it is where speed, SNR, lock
        // and the tone offset come from, and it is cheap next to the model.
        let classic = self.rx.process(&self.scratch);

        let Some(deep) = self.deep.as_ref() else {
            self.append_rx(&classic);
            return;
        };

        // Follow the tone the AFC settled on rather than the pitch the operator
        // dialled, so a drifting or mistuned signal still lands mid-band.
        self.tuner.set_tone(self.rx.tone_hz() as f64);
        self.deep_scratch.clear();
        self.tuner.push(tap, &mut self.deep_scratch);
        deep.push(&self.deep_scratch);
    }

    fn poll(&mut self, _now: SystemTime, _dial_hz: f64) -> Vec<DigiAction> {
        let mut actions = Vec::new();
        self.drain_deep();
        if self.tx_active && !self.keyed {
            self.keyed = true;
            self.status_dirty = true;
            // Nothing more will arrive for this over, so settle the tail now
            // rather than leaving the last word unconfirmed until they come
            // back to us.
            if let Some(deep) = self.deep.as_ref() {
                deep.flush();
            }
            actions.push(DigiAction::KeyTx);
        }
        // The decoder's readouts change continuously; only a change worth
        // showing repaints the panel.
        let cw = self.cw_status();
        let moved = cw.locked != self.last_cw.locked
            || (cw.wpm - self.last_cw.wpm).abs() > 0.5
            || (cw.snr_db - self.last_cw.snr_db).abs() > 1.0
            || (cw.tone_hz - self.last_cw.tone_hz).abs() > 2.0;
        if moved {
            self.last_cw = cw;
            self.status_dirty = true;
        }
        if self.status_dirty {
            self.status_dirty = false;
            actions.push(DigiAction::Status(self.build_status()));
        }
        actions
    }

    fn tx_burst_active(&self) -> bool {
        self.keyed
    }

    fn fill_tx_block(&mut self, out: &mut [f32]) -> bool {
        if self.tx.drained() {
            self.idle_samples += out.len();
            if self.idle_samples as f32 > TX_IDLE_S * OUT_RATE as f32 {
                self.tx_active = false;
                self.status_dirty = true;
            }
        } else {
            self.idle_samples = 0;
        }
        while self.tx48.len() < out.len() && self.producing() {
            self.scratch.clear();
            self.scratch.resize(TX_CHUNK, 0.0);
            self.tx.next_block(&mut self.scratch);
            self.scratch48.clear();
            match &mut self.tx_rs {
                Some(r) => r.push(&self.scratch, &mut self.scratch48),
                None => self.scratch48.extend_from_slice(&self.scratch),
            }
            self.tx48.extend(self.scratch48.iter().copied());
        }
        for s in out.iter_mut() {
            *s = self.tx48.pop_front().unwrap_or(0.0);
        }
        if self.tx.sent_chars() != self.last_sent {
            self.last_sent = self.tx.sent_chars();
            self.status_dirty = true;
        }
        !self.producing() && self.tx48.is_empty()
    }

    fn on_burst_done(&mut self) {
        self.keyed = false;
        self.idle_samples = 0;
        // The decoder heard nothing but our own sending for the length of that
        // over; its window is stale and its speed fit is ours, not theirs.
        self.rx.reset();
        if let Some(deep) = self.deep.as_ref() {
            deep.reset();
        }
        self.rx_pending.clear();
        self.status_dirty = true;
    }

    fn abort(&mut self) {
        self.abort_tx();
    }

    fn abort_tx(&mut self) {
        self.tx.clear();
        self.tx48.clear();
        self.tx_buffer.clear();
        self.tx_pushed = 0;
        self.tx_active = false;
        self.last_sent = 0;
        self.idle_samples = 0;
        self.status_dirty = true;
    }

    fn set_config(&mut self, cfg: DigiConfig) {
        if (cfg.cw_pitch_hz - self.cfg.cw_pitch_hz).abs() > 0.5 {
            self.rx.set_pitch(cfg.cw_pitch_hz);
            self.tuner.set_tone(cfg.cw_pitch_hz as f64);
        }
        self.rx.set_speed_lock(cfg.cw_speed_lock.then_some(cfg.cw_wpm));
        self.tx.set_params(cfg.cw_pitch_hz as f64, cfg.cw_wpm, cfg.cw_farnsworth_wpm);
        self.cfg = cfg;
        self.status_dirty = true;
    }

    /// The waterfall cursor. In CW the frequency being copied and the frequency
    /// being transmitted on are one and the same, so this moves both.
    fn set_audio_hz(&mut self, hz: f32) {
        let hz = hz.clamp(200.0, 3000.0);
        self.cfg.cw_pitch_hz = hz;
        self.rx.set_pitch(hz);
        self.tuner.set_tone(hz as f64);
        // The old frequency's audio is still buffered and is not this signal.
        if let Some(deep) = self.deep.as_ref() {
            deep.reset();
        }
        self.rx_pending.clear();
        self.tx.set_params(hz as f64, self.cfg.cw_wpm, self.cfg.cw_farnsworth_wpm);
        self.status_dirty = true;
    }

    fn audio_hz(&self) -> f32 {
        self.cfg.cw_pitch_hz
    }

    fn status(&self) -> DigiStatus {
        self.build_status()
    }

    fn call_cq(&mut self) {
        let call = if self.cfg.my_call.is_empty() { "NOCALL" } else { &self.cfg.my_call };
        let cq = format!("CQ CQ CQ DE {call} {call} {call} K ");
        self.set_tx_text(cq);
        self.set_tx_active(true);
    }

    /// Take the operator's outgoing buffer, keying whatever is new in it.
    ///
    /// Only the tail past what has already been queued goes to the keyer, so a
    /// character typed mid-transmission is sent on the end of what is already
    /// going out rather than restarting the message — which is the whole point
    /// of a keyboard on a CW panel.
    fn set_tx_text(&mut self, text: String) {
        let n = text.chars().count();
        if n > self.tx_pushed {
            let tail: String = text.chars().skip(self.tx_pushed).collect();
            self.tx.push_text(&tail);
            self.tx_pushed = n;
        }
        self.tx_buffer = text;
        self.status_dirty = true;
    }

    fn set_tx_active(&mut self, on: bool) {
        self.tx_active = on;
        self.idle_samples = 0;
        self.status_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DigiConfig {
        DigiConfig { my_call: "W1AW".into(), cw_wpm: 25.0, ..Default::default() }
    }

    /// Settle the receive window: the model decodes on its own thread, so the
    /// text a test wants has to be waited for rather than read straight back.
    fn settle(c: &mut CwController) -> String {
        if let Some(deep) = c.deep.as_ref() {
            deep.flush();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut idle = 0;
        while std::time::Instant::now() < deadline && idle < 100 {
            let before = c.rx_display();
            c.poll(SystemTime::now(), 14_030_000.0);
            if c.rx_display() == before {
                idle += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            } else {
                idle = 0;
            }
        }
        c.rx_display().trim().to_string()
    }

    /// Transmit what the operator types, receive it back through the decoder.
    /// The controller sits between two halves that have to agree about pitch
    /// and speed, and this is the cheapest way to keep it honest.
    #[test]
    fn keys_what_is_typed_and_reads_it_back() {
        let mut c = CwController::new(cfg(), 48_000.0);
        c.set_tx_text("CQ DE W1AW K ".into());
        c.set_tx_active(true);
        c.poll(SystemTime::now(), 14_030_000.0);

        // Pull the transmission as the engine would, in 10 ms blocks.
        let mut audio = Vec::new();
        for _ in 0..2000 {
            let mut blk = [0.0f32; 480];
            let done = c.fill_tx_block(&mut blk);
            audio.extend_from_slice(&blk);
            if done {
                break;
            }
        }
        assert_eq!(c.tx.sent_chars(), 13, "not every character went out");

        // …and back in through the receive side, at the tap rate.
        // A receiver has been listening before the other station starts; the
        // decoder needs a second or so of channel to measure its noise floor
        // against before the first character arrives.
        let mut rx = CwController::new(cfg(), 48_000.0);
        let mut lead = vec![0.0f32; 48_000 * 3 / 2];
        lead.extend_from_slice(&audio);
        lead.resize(lead.len() + 48_000 * 3 / 2, 0.0);
        // A receiver always has a noise floor; the decoder measures against one.
        let mut seed = 99u32;
        for a in lead.iter_mut() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *a += 0.0004 * ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5);
        }
        for blk in lead.chunks(1920) {
            rx.on_rx_audio(blk);
        }
        assert_eq!(settle(&mut rx), "CQ DE W1AW K");
        // The classic front end is still running, and is still where the speed
        // readout comes from.
        assert!((rx.rx.wpm() - 25.0).abs() < 2.0, "read {} WPM", rx.rx.wpm());
    }

    /// The cursor moves the decoder and the transmitter together — in CW they
    /// are the same frequency, and a panel where they could differ would be a
    /// panel that answers a station off its own frequency.
    #[test]
    fn the_cursor_moves_receive_and_transmit_together() {
        let mut c = CwController::new(cfg(), 48_000.0);
        c.set_audio_hz(600.0);
        assert_eq!(c.audio_hz(), 600.0);
        assert!((c.rx.tone_hz() - 600.0).abs() < 1.0);
        let st = c.status();
        assert_eq!(st.audio_hz, 600.0);
        assert_eq!(st.config.cw_pitch_hz, 600.0);

        // …and the keyer follows, which is only visible in what it generates.
        c.set_tx_text("E".into());
        c.set_tx_active(true);
        let mut audio = vec![0.0f32; 0];
        for _ in 0..200 {
            let mut blk = [0.0f32; 480];
            let done = c.fill_tx_block(&mut blk);
            audio.extend_from_slice(&blk);
            if done {
                break;
            }
        }
        // Correlate the keyed audio against each candidate pitch: the one the
        // cursor asked for has to win, and the default it moved off has to lose.
        let power_at = |hz: f64| -> f64 {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (i, &s) in audio.iter().enumerate() {
                let p = std::f64::consts::TAU * hz * i as f64 / OUT_RATE;
                re += s as f64 * p.cos();
                im += s as f64 * p.sin();
            }
            (re * re + im * im).sqrt() / audio.len() as f64
        };
        let want = power_at(600.0);
        for other in [500.0, 700.0, 800.0] {
            assert!(
                want > 4.0 * power_at(other),
                "sidetone is not at 600 Hz: {want:.4} there against {:.4} at {other}",
                power_at(other)
            );
        }
    }

    /// An operator who stops typing stops transmitting. Holding the key down
    /// between characters is the point; holding it down for ever is not.
    #[test]
    fn transmit_releases_itself_when_there_is_nothing_left_to_send() {
        let mut c = CwController::new(cfg(), 48_000.0);
        c.set_tx_text("E".into());
        c.set_tx_active(true);
        c.poll(SystemTime::now(), 14_030_000.0);

        let mut blk = [0.0f32; 480];
        let mut blocks = 0;
        loop {
            let done = c.fill_tx_block(&mut blk);
            blocks += 1;
            if done {
                break;
            }
            assert!(blocks < 2000, "transmit never released the key");
        }
        c.on_burst_done();
        assert!(!c.tx_burst_active());
        // Roughly the idle timeout, and certainly not immediately after the dit.
        let held_s = blocks as f32 * 480.0 / OUT_RATE as f32;
        assert!((TX_IDLE_S..TX_IDLE_S + 1.0).contains(&held_s), "held the key for {held_s:.1} s");
    }

    /// Transmit must not decode itself. The tap carries our own sidetone while
    /// we key, and copying it would fill the receive pane with our own callsign.
    #[test]
    fn does_not_copy_its_own_sending() {
        let mut c = CwController::new(cfg(), 48_000.0);
        c.set_tx_text("CQ DE W1AW K ".into());
        c.set_tx_active(true);
        c.poll(SystemTime::now(), 14_030_000.0);
        assert!(c.tx_burst_active());
        for _ in 0..600 {
            let mut blk = [0.0f32; 480];
            let done = c.fill_tx_block(&mut blk);
            // The engine loops the transmitted audio back through the tap.
            c.on_rx_audio(&blk);
            if done {
                break;
            }
        }
        assert!(c.rx_text.is_empty(), "copied itself: {:?}", c.rx_text);
    }
}
