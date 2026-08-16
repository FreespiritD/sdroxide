//! An [`IqSource`] for a CAT-controlled rig whose audio arrives over a USB
//! sound card: control (frequency/mode/PTT) goes over serial via
//! [`sdroxide_cat`], RX audio comes from the radio's capture device, and TX
//! audio goes to the radio's playback device. Two sound formats are supported:
//! stereo **IQ** (complex baseband → normal engine path) and mono **demod
//! audio** (real → the engine's audio-band bypass, `DeviceCaps.audio_mode`).

use sdroxide_dsp::MonoResampler;
use sdroxide_radio::rtrb;
use sdroxide_radio::{Complex32, ControlUpdate, IqSource, Result};
use sdroxide_types::{CatConfig, Mode, SoundFormat, TxTelemetry};

use crate::dial::Dial;

pub struct AudioCatSource {
    // RX audio from the rig (mono for demod, interleaved L/R for IQ). `None`
    // when the capture device could not be opened — the app still runs so the
    // user can fix the device in Settings; RX is just silent until then.
    _in_stream: Option<sdroxide_audio::AudioInput>,
    in_consumer: rtrb::Consumer<f32>,
    in_rate: f64,
    format: SoundFormat,
    audio_bw: f64,

    // TX audio to the rig (interleaved stereo playback ring).
    out: Option<(sdroxide_audio::AudioOutput, rtrb::Producer<f32>)>,
    tx_resampler: Option<MonoResampler>,
    tx_scratch: Vec<f32>,

    cat: sdroxide_cat::CatHandle,
    dial: Dial,
    label: String,
    /// Warning captured at open time (RX device unavailable / mono-for-IQ),
    /// surfaced to the UI. `None` when RX came up cleanly.
    status: Option<String>,
    /// Latest SWR the rig reported while keyed (via CI-V meter reads), held so
    /// the engine's 100 ms meter poll sees the most recent value between the
    /// rig's ~5 Hz updates. Cleared on unkey.
    last_telem: Option<TxTelemetry>,
    /// Latest S-meter reading (dBm) the rig reported while receiving, and when
    /// it arrived. Held for the same reason as `last_telem` — the engine's
    /// meter ticks far faster than the rig answers, and a gap between answers
    /// is not a signal that went away — but only for as long as it can still be
    /// called current: a rig that has stopped answering must not leave a needle
    /// standing at the last thing it said.
    last_signal: Option<(std::time::Instant, f32)>,
}

/// How long a reading from the rig's S-meter stands in for the next one. Comes
/// to a handful of the rig's own ~5 Hz answers, so an ordinary gap is covered
/// and a link that has gone quiet is not.
const SIGNAL_MAX_AGE: std::time::Duration = std::time::Duration::from_millis(1500);

impl AudioCatSource {
    /// Open the radio's sound-card streams and the CAT serial thread. `audio_in`
    /// / `audio_out` are cpal device names (`None` = system default).
    pub fn open(
        cfg: CatConfig,
        audio_in: Option<&str>,
        audio_out: Option<&str>,
    ) -> anyhow::Result<Self> {
        // Adopt the rig's current dial/mode before we start commanding it.
        let (init_freq, _init_mode) = sdroxide_cat::query_once(&cfg).unwrap_or((None, None));
        let center = init_freq.unwrap_or(14_074_000.0);

        // A rig with no sound card named falls back to the machine's default
        // input, which is almost never the radio — it is the operator's headset,
        // or, at a station with two rigs on two identical USB codecs, the *other*
        // radio's card. Worth saying out loud: the symptom is one radio with
        // audio and one without, and nothing else points at the cause.
        if audio_in.is_none() || audio_out.is_none() {
            tracing::warn!(
                "no sound card chosen for the {} rig on {} ({}) — falling back to the system \
                 default, which is not this radio unless it happens to be the default. Pick its \
                 card under Settings → General → Radio audio.",
                cfg.family.label(),
                sdroxide_cat::link_label(&cfg),
                match (audio_in.is_none(), audio_out.is_none()) {
                    (true, true) => "receive and transmit",
                    (true, false) => "receive",
                    _ => "transmit",
                },
            );
        }

        // RX capture is best-effort: a missing/unsupported device leaves RX
        // silent but keeps the app (and its Settings dialog) alive.
        let opened = match cfg.format {
            SoundFormat::Iq => sdroxide_audio::start_input_stereo(audio_in, 48_000),
            SoundFormat::DemodAudio => sdroxide_audio::start_input(audio_in, 48_000),
        };
        let dev_label = audio_in.unwrap_or("system default");
        // A dummy, always-empty ring keeps `read` returning silence when RX is
        // unavailable or guarded off.
        let silent = || {
            let (_p, c) = rtrb::RingBuffer::<f32>::new(1);
            c
        };
        let (in_stream, in_consumer, in_rate, status) = match opened {
            // Mono guard: I/Q needs two channels (I on left, Q on right); a
            // mono capture device physically can't carry it. Refuse rather than
            // silently duplicating one channel into a degenerate spectrum.
            Ok((s, _)) if matches!(cfg.format, SoundFormat::Iq) && s.channels < 2 => {
                let msg = format!(
                    "Radio IQ input “{dev_label}” is mono — IQ needs a stereo (2-channel) \
                     input. Pick a stereo line-input device, or switch the sound format to \
                     Demod audio."
                );
                tracing::warn!("{msg}");
                (None, silent(), s.sample_rate, Some(msg))
            }
            Ok((s, c)) => {
                let rate = s.sample_rate;
                (Some(s), c, rate, None)
            }
            Err(e) => {
                let msg = format!(
                    "Radio input “{dev_label}” is unavailable ({e}) — no receive audio. \
                     The device may be in use by another program, unplugged, or held by \
                     the system audio server."
                );
                tracing::warn!("{msg}");
                (None, silent(), 48_000.0, Some(msg))
            }
        };

        // TX playback is best-effort: a missing device just means no TX audio.
        let out = match sdroxide_audio::start_output(audio_out, 48_000) {
            Ok((o, p)) => Some((o, p)),
            Err(e) => {
                tracing::warn!("radio TX audio device unavailable ({e}); RX only");
                None
            }
        };
        // `MonoResampler::new` returns None when the rates match.
        let tx_resampler =
            out.as_ref().and_then(|(o, _)| MonoResampler::new(48_000.0, o.sample_rate));

        let label =
            format!("CAT rig ({}) on {}", cfg.family.label(), sdroxide_cat::link_label(&cfg));
        let audio_bw = cfg.audio_bw_hz;
        let format = cfg.format;
        let cat = sdroxide_cat::spawn(cfg);

        Ok(AudioCatSource {
            _in_stream: in_stream,
            in_consumer,
            in_rate,
            format,
            audio_bw,
            out,
            tx_resampler,
            tx_scratch: Vec::new(),
            cat,
            dial: Dial::at(center),
            label,
            status,
            last_telem: None,
            last_signal: None,
        })
    }
}

impl IqSource for AudioCatSource {
    fn sample_rate(&self) -> f64 {
        self.in_rate
    }
    fn center_hz(&self) -> f64 {
        self.dial.vfo
    }
    fn set_center_hz(&mut self, hz: f64) -> Result<()> {
        if let Some(f) = self.dial.set_vfo(hz) {
            self.cat.set_freq(f);
        }
        Ok(())
    }

    fn set_rit_hz(&mut self, hz: f64) {
        if let Some(f) = self.dial.set_rit(hz) {
            self.cat.set_freq(f);
        }
    }

    fn read(&mut self, buf: &mut [Complex32]) -> Result<usize> {
        match self.format {
            SoundFormat::DemodAudio => {
                let mut n = 0;
                while n < buf.len() {
                    match self.in_consumer.pop() {
                        Ok(s) => {
                            buf[n] = Complex32::new(s, 0.0);
                            n += 1;
                        }
                        Err(_) => break,
                    }
                }
                if n == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(n)
            }
            SoundFormat::Iq => {
                let mut n = 0;
                // Need pairs (I, Q); only consume when both are available.
                while n < buf.len() && self.in_consumer.slots() >= 2 {
                    let i = self.in_consumer.pop().unwrap_or(0.0);
                    let q = self.in_consumer.pop().unwrap_or(0.0);
                    buf[n] = Complex32::new(i, q);
                    n += 1;
                }
                if n == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(n)
            }
        }
    }

    fn describe(&self) -> String {
        self.label.clone()
    }

    fn open_status(&self) -> Option<String> {
        self.status.clone()
    }

    fn display_bandwidth(&self) -> Option<f64> {
        matches!(self.format, SoundFormat::DemodAudio).then_some(self.audio_bw)
    }

    fn poll_control(&mut self) -> Vec<ControlUpdate> {
        let mut out = Vec::new();
        for u in self.cat.poll() {
            match u {
                // The dial is not the VFO — it carries RIT, and for the length
                // of an over it carries XIT/split instead — so a report has to
                // be folded back before the engine sees it as a dial move.
                sdroxide_cat::CatUpdate::Freq(hz) => {
                    if let Some(vfo) = self.dial.report(hz) {
                        out.push(ControlUpdate::Freq(vfo));
                    }
                }
                sdroxide_cat::CatUpdate::Mode(m) => out.push(ControlUpdate::Mode(m)),
                // The power the rig came up on, read once when the port opened.
                // The engine adopts it into the Drive slider rather than
                // commanding the rig back — the radio's own setting is the
                // operator's, not a stale one to overwrite.
                sdroxide_cat::CatUpdate::Power(frac) => out.push(ControlUpdate::TxDrive(frac)),
                // Both meters arrive on their own telemetry channels, not here.
                sdroxide_cat::CatUpdate::Swr(_) | sdroxide_cat::CatUpdate::Signal(_) => {}
            }
        }
        out
    }

    fn set_control_mode(&mut self, mode: Mode) -> Result<()> {
        self.cat.set_mode(mode);
        Ok(())
    }

    /// The panel's width control, sent to the only filter in the path.
    ///
    /// There is no demodulator on this side of a CAT rig — the audio arrives
    /// already filtered, levelled by an AGC that has ridden the interference
    /// down, and narrowing it here would only cut what the radio had already
    /// let through. The rig's own filter is the one that does the work.
    fn set_control_filter(&mut self, mode: Mode, lo_hz: f64, hi_hz: f64) {
        self.cat.set_filter(mode, lo_hz as f32, hi_hz as f32);
    }

    // ── Output power ─────────────────────────────────────────────────────────
    // The rig's own power control, over CAT. It is the only transmit level that
    // means anything in every mode: the audio we put into the sound card is not
    // one in CW (the rig keys itself from its own keyer and never looks at the
    // sound card) and not one under TUNE either, so before this the Drive and
    // Tune sliders moved nothing at all on a CAT rig.
    //
    // The engine asserts the level that applies — drive, or the tune level
    // while tuning — before every key-down, so an over cannot go out at the
    // previous one.
    fn set_tx_drive(&mut self, frac: f64) {
        self.cat.set_power(frac as f32);
    }

    /// Nothing to set: a rig has one power register, and the engine commands the
    /// tune level through [`Self::set_tx_drive`] for as long as TUNE holds the
    /// transmitter.
    fn set_tune_drive(&mut self, _frac: f64) {}

    fn commands_tx_power(&self) -> bool {
        self.cat.commands_power()
    }

    // ── CW from the rig's own keyer ──────────────────────────────────────────
    // A rig in CW does not modulate the audio we send it — it keys its own
    // transmitter — so the panel's keyer hands it text instead of sidetone.
    fn cw_text_keying(&self) -> Option<usize> {
        self.cat.cw_chunk_len()
    }
    fn send_cw(&mut self, text: &str) {
        self.cat.send_cw(text.to_string());
    }
    fn abort_cw(&mut self) {
        self.cat.abort_cw();
    }
    fn set_cw_wpm(&mut self, wpm: f32) {
        self.cat.set_cw_wpm(wpm);
    }

    fn tx_begin(&mut self, center_hz: f64, _rate: f64) -> Result<f64> {
        // XIT and split have no DDC to ride on here — the rig's dial is the
        // whole of its frequency control — so an over that transmits away from
        // where we listen borrows the dial for its duration. The frequency is
        // queued before PTT and the CAT thread writes a pending frequency out
        // ahead of keying, so nothing goes on air at the receive frequency.
        if let Some(f) = self.dial.begin_tx(center_hz) {
            self.cat.set_freq(f);
        }
        self.cat.set_ptt(true);
        Ok(self.out.as_ref().map(|(o, _)| o.sample_rate).unwrap_or(self.in_rate))
    }

    fn tx_end(&mut self) -> Result<()> {
        self.cat.set_ptt(false);
        // Give the dial back, including any retune the operator asked for while
        // the over held it.
        if let Some(f) = self.dial.end_tx() {
            self.cat.set_freq(f);
        }
        self.last_telem = None; // drop the stale SWR reading on unkey
        Ok(())
    }

    fn discard_pending_rx(&mut self) {
        // The capture callback keeps filling this ring during TX too.
        while self.in_consumer.pop().is_ok() {}
    }

    fn tx_telemetry(&mut self) -> Option<TxTelemetry> {
        // The CI-V thread polls SWR at ~5 Hz; latch its latest reading so the
        // engine's 100 ms meter tick always has a value to show.
        if let Some(t) = self.cat.poll_telemetry() {
            self.last_telem = Some(t);
        }
        self.last_telem
    }

    fn rx_signal_dbm(&mut self) -> Option<f32> {
        // Latched for the same reason as `tx_telemetry`, and against the same
        // ~5 Hz stream of readings. A rig whose family (or dialect) has no
        // S-meter read never sends any — and one that has stopped answering
        // stops counting once its last reading goes stale. Either way the
        // engine falls back to the level of the audio itself, which is still
        // arriving whatever the control link is doing.
        if let Some(dbm) = self.cat.poll_signal() {
            self.last_signal = Some((std::time::Instant::now(), dbm));
        }
        self.last_signal.filter(|(at, _)| at.elapsed() < SIGNAL_MAX_AGE).map(|(_, dbm)| dbm)
    }

    fn tx_write_audio(&mut self, audio: &[f32]) -> Result<()> {
        let Some((_, producer)) = self.out.as_mut() else {
            return Ok(()); // no TX audio device — PTT still keyed the rig
        };
        // Resample 48 kHz → card rate, then interleave to stereo (both channels).
        self.tx_scratch.clear();
        match self.tx_resampler.as_mut() {
            Some(rs) => rs.push(audio, &mut self.tx_scratch),
            None => self.tx_scratch.extend_from_slice(audio),
        }
        // Block until the card drains room, applying backpressure so the engine's
        // TX loop is paced to real time. Without this a long continuous burst
        // (e.g. a 110 s SSTV image) is generated at CPU speed and mostly dropped
        // on a full ring, so the radio only transmits the first buffer-full.
        for &s in &self.tx_scratch {
            for _ in 0..2 {
                let mut v = s;
                let mut tries = 0u32;
                while let Err(rtrb::PushError::Full(x)) = producer.push(v) {
                    v = x;
                    tries += 1;
                    if tries > 200 {
                        break; // output device stalled — drop rather than hang TX
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
        Ok(())
    }

    fn tx_drain(&mut self) {
        // The output ring holds ~1 s; wait for it to play out before PTT is
        // released so the tail of a burst (critical for FT8 decode) isn't cut.
        if let Some((_, producer)) = self.out.as_ref() {
            let cap = producer.buffer().capacity();
            for _ in 0..1000 {
                let buffered = cap.saturating_sub(producer.slots());
                if buffered <= cap / 40 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}
