//! The JS8 engine seam: slot timing, decode dispatch, and the transmit queue.
//!
//! Structurally this is [`crate::DigiController`] with two things taken away
//! and one added.
//!
//! **No even/odd discipline.** FT8 stations alternate, and the FT8 controller
//! spends real effort staying opposite the station it is working. JS8 stations
//! transmit when they have something to say, so there is no period to be on the
//! wrong side of.
//!
//! **No QSO machine.** FT8's `QsoMachine` sequences a contest exchange through
//! Tx1–Tx6. JS8 carries a conversation: what to send next is whatever the
//! operator typed, and the only automatic traffic is a reply to a direct
//! question.
//!
//! **A frame queue instead.** A message longer than one frame occupies
//! consecutive slots, so transmit is a `VecDeque<Js8Payload>` drained one frame
//! per slot. Auto-replies join the back of that queue rather than jumping it,
//! which is what stops the station keying twice in a slot or interrupting the
//! operator mid-message.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender};
use std::time::SystemTime;

use sdroxide_dsp::MonoResampler;
use sdroxide_types::{
    DigiConfig, DigiStatus, Js8Heard, Js8Msg, Js8Speed, Js8Status, Mode, QsoStep,
};

use crate::DigiEngine;
use crate::clock::ClockMonitor;
use crate::controller::{BurstPlayer, DigiAction};
use crate::js8::assembler::Js8Assembler;
use crate::js8::decode::{Js8Decode, Js8Depth, decode_slot_for};
use crate::js8::directed::{ReplyPolicy, StationInfo, auto_reply};
use crate::js8::frame::{self, Compound, Js8Flags};
use crate::js8::message::Js8Payload;
use crate::js8::modem::{self, DECODE_RATE};
use crate::params::DigiParams;
use crate::scheduler::SlotScheduler;

const OUT_RATE: f64 = 48_000.0;

/// Decodes kept for the activity list.
const ACTIVITY_CAP: usize = 200;
/// Reassembled messages kept for the conversation view.
const MSG_CAP: usize = 200;
/// Stations kept in the heard list.
const HEARD_CAP: usize = 50;

/// Work handed to the decode thread.
struct DecodeJob {
    audio: Vec<i16>,
    slot_utc: i64,
    speed: Js8Speed,
}

pub struct Js8Controller {
    cfg: DigiConfig,
    speed: Js8Speed,
    params: DigiParams,
    scheduler: SlotScheduler,

    resampler: Option<MonoResampler>,
    slot_buf: Vec<i16>,
    tap_scratch: Vec<f32>,
    last_slot_idx: i64,

    dial_hz: f64,
    audio_hz: f32,

    job_tx: Sender<DecodeJob>,
    res_rx: Receiver<(i64, Vec<Js8Decode>)>,

    assembler: Js8Assembler,
    heard: Vec<Js8Heard>,
    messages: Vec<Js8Msg>,

    /// Frames still to go out, one per slot.
    tx_frames: VecDeque<Js8Payload>,
    tx_total: u8,
    tx_fired_slot: i64,
    burst: Option<BurstPlayer>,
    keyed: bool,

    last_hb_unix: i64,
    clock: ClockMonitor,
    status_dirty: bool,
}

impl Js8Controller {
    pub fn new(cfg: DigiConfig, tap_rate: f64) -> Self {
        let speed = cfg.js8_speed;
        let params = DigiParams::for_js8(speed);
        let (job_tx, job_rx) = std::sync::mpsc::channel::<DecodeJob>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<(i64, Vec<Js8Decode>)>();

        // Belief propagation must not run on the audio thread; a Slow slot is
        // 28 s of spectrogram and even Turbo is far more than one 10 ms block.
        std::thread::Builder::new()
            .name("sdroxide-js8-decode".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    // Ordered statistics runs only where belief propagation
                    // gave up, and measurably costs nothing: a busy Normal
                    // slot decodes in ~20 ms either way, because the sync gate
                    // turns most candidates away long before the FEC. There is
                    // no reason to make an operator opt into it.
                    let decodes = decode_slot_for(job.speed, &job.audio, Js8Depth::BpOsd);
                    if res_tx.send((job.slot_utc, decodes)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn js8 decode worker");

        let mut assembler = Js8Assembler::new(&Self::call_of(&cfg));
        assembler.set_timeout(i64::from(cfg.js8_assembly_timeout_s));
        assembler.set_my_groups(cfg.js8_groups.clone());

        Js8Controller {
            speed,
            params,
            scheduler: SlotScheduler::new(params.slot_s, params.tx_offset_s),
            resampler: MonoResampler::new(tap_rate, DECODE_RATE),
            slot_buf: Vec::new(),
            tap_scratch: Vec::new(),
            last_slot_idx: i64::MIN,
            dial_hz: 0.0,
            audio_hz: 1500.0,
            job_tx,
            res_rx,
            assembler,
            heard: Vec::new(),
            messages: Vec::new(),
            tx_frames: VecDeque::new(),
            tx_total: 0,
            tx_fired_slot: i64::MIN,
            burst: None,
            keyed: false,
            last_hb_unix: 0,
            clock: ClockMonitor::default(),
            status_dirty: true,
            cfg,
        }
    }

    fn call_of(cfg: &DigiConfig) -> String {
        if cfg.js8_call.is_empty() {
            cfg.my_call.to_uppercase()
        } else {
            cfg.js8_call.to_uppercase()
        }
    }

    fn my_call(&self) -> String {
        Self::call_of(&self.cfg)
    }

    fn station(&self) -> StationInfo {
        StationInfo {
            call: self.my_call(),
            grid: self.cfg.my_grid.to_uppercase(),
            status: self.cfg.js8_status.clone(),
            hearing: self.heard.iter().take(4).map(|h| h.call.clone()).collect(),
        }
    }

    fn slot_samples(&self) -> usize {
        (self.params.slot_s * DECODE_RATE) as usize
    }

    /// Queue the frames for a message, replacing anything already waiting.
    fn queue_frames(&mut self, frames: Vec<Js8Payload>) {
        self.tx_frames = frames.into();
        self.tx_total = self.tx_frames.len().min(255) as u8;
        self.status_dirty = true;
    }

    /// Split text into as many frames as it needs.
    ///
    /// The first frame is flagged FIRST and the last LAST; a message short
    /// enough for one frame gets both, which is how a receiver tells a complete
    /// message from the opening of a longer one.
    fn frames_for_text(&self, text: &str) -> Vec<Js8Payload> {
        let mut out = Vec::new();
        let mut rest = text.trim().to_ascii_uppercase();
        while !rest.is_empty() && out.len() < 32 {
            let Some((payload, used)) = frame::pack_data(&rest) else { break };
            if used == 0 {
                break;
            }
            out.push(payload);
            rest = rest[used.min(rest.len())..].to_string();
        }
        Self::flag_sequence(out)
    }

    /// Stamp FIRST on the opening frame and LAST on the closing one.
    fn flag_sequence(mut frames: Vec<Js8Payload>) -> Vec<Js8Payload> {
        let last = frames.len().saturating_sub(1);
        for (i, f) in frames.iter_mut().enumerate() {
            let mut flags = 0u8;
            if i == 0 {
                flags |= Js8Flags::FIRST;
            }
            if i == last {
                flags |= Js8Flags::LAST;
            }
            f.frame_type = flags;
        }
        frames
    }

    /// A single self-contained frame — a directed command or a heartbeat.
    fn single(mut payload: Js8Payload) -> Js8Payload {
        payload.frame_type = Js8Flags::FIRST | Js8Flags::LAST;
        payload
    }

    fn note_heard(&mut self, call: &str, grid: Option<String>, snr: i16, hz: f32, utc: i64) {
        if call.is_empty() {
            return;
        }
        if let Some(h) = self.heard.iter_mut().find(|h| h.call == call) {
            h.snr_db = snr;
            h.audio_hz = hz;
            h.last_utc = utc;
            if grid.is_some() {
                h.grid = grid;
            }
        } else {
            self.heard.push(Js8Heard {
                call: call.to_string(),
                grid,
                snr_db: snr,
                audio_hz: hz,
                last_utc: utc,
                speed: self.speed,
            });
        }
        // Most recently heard first, which is the order the panel wants and
        // the order `HEARING?` should answer in.
        self.heard.sort_by(|a, b| b.last_utc.cmp(&a.last_utc));
        self.heard.truncate(HEARD_CAP);
    }

    fn synth_burst_48k(&self, payload: Js8Payload) -> Option<BurstPlayer> {
        let audio = modem::encode_frame_12k(self.speed, payload, self.audio_hz, 0.5);
        let mut rs = MonoResampler::new(DECODE_RATE, OUT_RATE)?;
        let mut out = Vec::new();
        rs.push(&audio, &mut out);
        Some(BurstPlayer { samples: out, pos: 0 })
    }
}

impl DigiEngine for Js8Controller {
    fn mode(&self) -> Mode {
        Mode::Js8
    }

    fn on_rx_audio(&mut self, tap: &[f32]) {
        let Some(rs) = self.resampler.as_mut() else { return };
        self.tap_scratch.clear();
        rs.push(tap, &mut self.tap_scratch);
        self.slot_buf
            .extend(self.tap_scratch.iter().map(|&s| (s.clamp(-1.0, 1.0) * 28_000.0) as i16));
        // A slot and a quarter is plenty of slack for a late boundary; more
        // just costs memory.
        let cap = self.slot_samples() * 5 / 4;
        if self.slot_buf.len() > cap {
            let drop = self.slot_buf.len() - cap;
            self.slot_buf.drain(..drop);
        }
    }

    fn poll(&mut self, now: SystemTime, dial_hz: f64) -> Vec<DigiAction> {
        self.dial_hz = dial_hz;
        let mut actions = Vec::new();
        let unix_now = SlotScheduler::unix_now(now) as i64;

        // 1. Drain the decode worker.
        while let Ok((slot_utc, decodes)) = self.res_rx.try_recv() {
            if decodes.is_empty() {
                continue;
            }
            let mut shared = Vec::new();
            for d in &decodes {
                if let Some(msg) = self.assembler.push(d, slot_utc) {
                    self.note_heard(&msg.from, None, msg.snr_db, msg.audio_hz, msg.last_slot_utc);
                    // Answer before storing, so a reply is queued even if the
                    // conversation list is already full.
                    if let Some(reply) = auto_reply(
                        &msg,
                        &self.station(),
                        ReplyPolicy { auto_reply: self.cfg.js8_auto_reply },
                    ) {
                        if let Some(p) = reply.pack() {
                            self.tx_frames.push_back(Self::single(p));
                            self.tx_total = self.tx_total.saturating_add(1);
                        }
                    }
                    self.messages.push(msg);
                    if self.messages.len() > MSG_CAP {
                        self.messages.remove(0);
                    }
                }
                // The activity list shows the raw frame; the conversation
                // view shows the reassembled message. Callsign parsing happens
                // in the assembler, which is the only layer that sees enough
                // frames to know one.
                shared.push(sdroxide_types::Decode {
                    slot_utc,
                    snr_db: d.snr_db.round() as i16,
                    dt: d.dt_sec,
                    audio_hz: d.audio_hz,
                    message: d.payload.to_chars(),
                    to: None,
                    from: None,
                    grid: None,
                    is_cq: false,
                    cq_to: None,
                    rr73_to: None,
                    free_text: true,
                });
            }
            shared.truncate(ACTIVITY_CAP);
            // The clock estimate wants the whole slot's decodes at once — it
            // takes their median dt, which one decode cannot supply.
            self.clock.observe(&shared);
            actions.push(DigiAction::Decodes(shared));
            self.status_dirty = true;
        }

        for expired in self.assembler.expire(unix_now) {
            self.messages.push(expired);
            self.status_dirty = true;
        }

        // 2. Slot boundary — hand the finished slot to the worker.
        let idx = self.scheduler.slot_index(now);
        if idx != self.last_slot_idx {
            if self.last_slot_idx != i64::MIN && self.slot_buf.len() >= self.slot_samples() / 2 {
                let audio = std::mem::take(&mut self.slot_buf);
                let slot_utc = self.scheduler.slot_start_unix(self.last_slot_idx) as i64;
                // A dropped job costs one slot of decodes; a queue that grows
                // without bound costs the whole session.
                let _ = self.job_tx.send(DecodeJob { audio, slot_utc, speed: self.speed });
            } else {
                self.slot_buf.clear();
            }
            self.last_slot_idx = idx;
        }

        // 3. Periodic heartbeat, if the operator asked for one.
        if self.cfg.js8_heartbeat_min > 0 && !self.my_call().is_empty() {
            let every = i64::from(self.cfg.js8_heartbeat_min) * 60;
            if unix_now - self.last_hb_unix >= every {
                self.last_hb_unix = unix_now;
                let grid = self.cfg.my_grid.to_uppercase();
                let hb = Compound::heartbeat(
                    &self.my_call(),
                    (!grid.is_empty()).then_some(grid.as_str()),
                );
                if let Some(p) = hb.pack() {
                    self.tx_frames.push_back(Self::single(p));
                    self.tx_total = self.tx_total.saturating_add(1);
                    self.status_dirty = true;
                }
            }
        }

        // 4. Transmit one frame per slot, inside the window where it still fits.
        if self.burst.is_none() && idx != self.tx_fired_slot && !self.tx_frames.is_empty() {
            let into = self.scheduler.secs_into_slot(now);
            let latest = self.params.slot_s - self.params.burst_s + self.params.tx_offset_s;
            if into >= self.params.tx_offset_s && into <= latest {
                if let Some(payload) = self.tx_frames.pop_front() {
                    if let Some(b) = self.synth_burst_48k(payload) {
                        self.burst = Some(b);
                        self.keyed = true;
                        self.tx_fired_slot = idx;
                        actions.push(DigiAction::KeyTx);
                        self.status_dirty = true;
                    }
                }
            }
        }

        if self.status_dirty {
            actions.push(DigiAction::Status(self.status()));
            self.status_dirty = false;
        }
        actions
    }

    fn tx_burst_active(&self) -> bool {
        self.burst.is_some()
    }

    fn fill_tx_block(&mut self, out: &mut [f32]) -> bool {
        let Some(b) = self.burst.as_mut() else {
            out.fill(0.0);
            return true;
        };
        let mut done = false;
        for slot in out.iter_mut() {
            if b.pos < b.samples.len() {
                *slot = b.samples[b.pos];
                b.pos += 1;
            } else {
                *slot = 0.0;
                done = true;
            }
        }
        if done {
            self.burst = None;
        }
        done
    }

    fn on_burst_done(&mut self) {
        self.burst = None;
        self.keyed = false;
        if self.tx_frames.is_empty() {
            self.tx_total = 0;
        }
        self.status_dirty = true;
    }

    fn abort(&mut self) {
        self.abort_tx();
        self.slot_buf.clear();
    }

    fn abort_tx(&mut self) {
        self.burst = None;
        self.keyed = false;
        self.tx_frames.clear();
        self.tx_total = 0;
        self.status_dirty = true;
    }

    fn set_config(&mut self, cfg: DigiConfig) {
        // Every speed is a different waveform, so a change means the decoder
        // has to be rebuilt; the engine does that by recreating the controller.
        self.speed = cfg.js8_speed;
        self.params = DigiParams::for_js8(self.speed);
        self.scheduler = SlotScheduler::new(self.params.slot_s, self.params.tx_offset_s);
        self.assembler.set_my_call(&Self::call_of(&cfg));
        self.assembler.set_timeout(i64::from(cfg.js8_assembly_timeout_s));
        self.assembler.set_my_groups(cfg.js8_groups.clone());
        self.cfg = cfg;
        self.status_dirty = true;
    }

    fn set_audio_hz(&mut self, hz: f32) {
        self.audio_hz = hz;
        self.status_dirty = true;
    }

    fn audio_hz(&self) -> f32 {
        self.audio_hz
    }

    fn call_cq(&mut self) {
        let grid = self.cfg.my_grid.to_uppercase();
        let cq = Compound::cq(&self.my_call(), (!grid.is_empty()).then_some(grid.as_str()), 0);
        if let Some(p) = cq.pack() {
            self.queue_frames(vec![Self::single(p)]);
        }
    }

    /// Queue a message. `"CALL text"` addresses it; bare text is undirected.
    fn send_text(&mut self, text: String) {
        if text.trim().is_empty() {
            self.abort_tx();
            return;
        }
        self.queue_frames(self.frames_for_text(&text));
    }

    fn stop_qso(&mut self) {
        self.abort_tx();
    }

    fn status(&self) -> DigiStatus {
        DigiStatus {
            mode: Mode::Js8,
            step: QsoStep::Idle,
            dx_call: None,
            dx_grid: None,
            tx_next: self.keyed,
            tx_pending_msg: None,
            audio_hz: self.audio_hz,
            tx_even: false,
            transmitting: self.keyed,
            tx_watchdog: false,
            transcript: Vec::new(),
            config: self.cfg.clone(),
            text_rx: String::new(),
            tx_sent: 0,
            fsq_heard: Vec::new(),
            fsq_messages: Vec::new(),
            rade: None,
            js8: Some(Js8Status {
                speed: self.speed,
                heard: self.heard.clone(),
                messages: self.messages.clone(),
                tx_frames_pending: self.tx_frames.len().min(255) as u8,
                tx_frames_total: self.tx_total,
                next_hb_in_s: None,
            }),
            fox_queue: Vec::new(),
            call_queue: Vec::new(),
            clock_offset_s: self.clock.offset_s(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DigiConfig {
        DigiConfig {
            my_call: "N0JDS".into(),
            my_grid: "FN42".into(),
            js8_speed: Js8Speed::Normal,
            ..Default::default()
        }
    }

    #[test]
    fn a_short_message_becomes_one_frame_flagged_first_and_last() {
        let c = Js8Controller::new(cfg(), 48_000.0);
        let frames = c.frames_for_text("HELLO");
        assert_eq!(frames.len(), 1);
        let flags = Js8Flags(frames[0].frame_type);
        assert!(flags.is_first() && flags.is_last(), "a single frame is both ends");
    }

    #[test]
    fn a_long_message_spans_frames_flagged_only_at_the_ends() {
        let c = Js8Controller::new(cfg(), 48_000.0);
        let frames = c.frames_for_text(
            "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG AND THEN KEEPS ON RUNNING FOR A WHILE",
        );
        assert!(frames.len() > 2, "expected several frames, got {}", frames.len());
        let first = Js8Flags(frames[0].frame_type);
        let last = Js8Flags(frames[frames.len() - 1].frame_type);
        assert!(first.is_first() && !first.is_last());
        assert!(last.is_last() && !last.is_first());
        for mid in &frames[1..frames.len() - 1] {
            let f = Js8Flags(mid.frame_type);
            assert!(!f.is_first() && !f.is_last(), "a middle frame claims an end");
        }
    }

    #[test]
    fn a_message_round_trips_through_the_assembler() {
        // The two halves of the mode have to agree: what we transmit is what a
        // receiver reassembles.
        let c = Js8Controller::new(cfg(), 48_000.0);
        let text = "HELLO WORLD THIS IS A LONGER MESSAGE SPANNING SEVERAL FRAMES";
        let frames = c.frames_for_text(text);
        let mut a = Js8Assembler::new("KN4CRD");

        let mut got = None;
        for (i, f) in frames.iter().enumerate() {
            let d = Js8Decode {
                payload: *f,
                audio_hz: 1500.0,
                dt_sec: 0.0,
                snr_db: 0.0,
                sync_score: 3.0,
                hard_errors: 0,
                speed: Js8Speed::Normal,
            };
            got = a.push(&d, 1000 + i as i64 * 15);
        }
        let msg = got.expect("the last frame completes the message");
        assert!(text.starts_with(msg.text.trim_end()), "got {:?}", msg.text);
        assert_eq!(msg.frames as usize, frames.len());
    }

    #[test]
    fn aborting_clears_the_queue_rather_than_pausing_it() {
        let mut c = Js8Controller::new(cfg(), 48_000.0);
        c.send_text("A LONG MESSAGE THAT NEEDS SEVERAL FRAMES TO GET ALL THE WAY OUT".into());
        assert!(!c.tx_frames.is_empty());
        c.abort_tx();
        assert!(c.tx_frames.is_empty(), "abort must not leave frames to resume");
        assert_eq!(c.tx_total, 0);
    }

    #[test]
    fn empty_text_cancels_instead_of_transmitting_nothing() {
        let mut c = Js8Controller::new(cfg(), 48_000.0);
        c.send_text("HELLO".into());
        c.send_text("   ".into());
        assert!(c.tx_frames.is_empty());
    }

    #[test]
    fn calling_cq_queues_exactly_one_self_contained_frame() {
        let mut c = Js8Controller::new(cfg(), 48_000.0);
        c.call_cq();
        assert_eq!(c.tx_frames.len(), 1);
        let flags = Js8Flags(c.tx_frames[0].frame_type);
        assert!(flags.is_first() && flags.is_last());
        let cq = Compound::unpack(&c.tx_frames[0]).expect("a compound frame");
        assert!(cq.is_cq());
        assert_eq!(cq.call, "N0JDS");
    }

    #[test]
    fn the_mode_reports_itself_as_js8() {
        let c = Js8Controller::new(cfg(), 48_000.0);
        assert_eq!(c.mode(), Mode::Js8);
        assert_eq!(c.status().mode, Mode::Js8);
        assert!(c.status().js8.is_some(), "the panel keys off this being Some");
    }

    #[test]
    fn changing_speed_retimes_the_scheduler() {
        let mut c = Js8Controller::new(cfg(), 48_000.0);
        assert_eq!(c.params.slot_s, 15.0);
        c.set_config(DigiConfig { js8_speed: Js8Speed::Turbo, ..cfg() });
        assert_eq!(c.params.slot_s, 6.0);
        assert_eq!(c.status().js8.expect("js8 status").speed, Js8Speed::Turbo);
    }

    #[test]
    fn an_idle_controller_transmits_silence_and_says_it_is_done() {
        let mut c = Js8Controller::new(cfg(), 48_000.0);
        let mut out = [1.0f32; 64];
        assert!(c.fill_tx_block(&mut out), "nothing queued means done");
        assert!(out.iter().all(|&s| s == 0.0), "and silence, not stale audio");
    }
}
