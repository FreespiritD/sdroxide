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

/// Where the rig's dial has to be, and who currently owns it.
///
/// A CAT rig has exactly one frequency control and no DDC behind it, so the VFO,
/// RIT, XIT and split all have to take turns on the dial: it sits on the receive
/// frequency (VFO + RIT) while receiving, and an over that transmits somewhere
/// else — XIT, or split onto the other VFO — borrows it until unkey. That makes
/// the dial an interlock rather than a number, so it lives here where it can be
/// tested without a serial port. Each method returns the frequency to command,
/// or `None` when the dial should stay where it is.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Dial {
    /// The operator's VFO — the dial with RIT taken back out.
    vfo: f64,
    /// How much RIT the dial carries while receiving (0 when RIT is off).
    rit: f64,
    /// Where an over has parked the dial, or `None` while receiving.
    tx: Option<f64>,
}

impl Dial {
    /// Where the dial belongs while receiving.
    fn rx_hz(&self) -> f64 {
        self.vfo + self.rit
    }

    /// Move the VFO. Nothing is commanded while an over owns the dial —
    /// retuning then would drag the transmitter off the frequency it was
    /// cleared to use; [`Self::end_tx`] picks the new VFO up on unkey.
    fn set_vfo(&mut self, hz: f64) -> Option<f64> {
        self.vfo = hz;
        self.tx.is_none().then(|| self.rx_hz())
    }

    /// Change the RIT offset (0 = RIT off). Same deferral as [`Self::set_vfo`].
    fn set_rit(&mut self, hz: f64) -> Option<f64> {
        if self.rit == hz {
            return None;
        }
        self.rit = hz;
        self.tx.is_none().then(|| self.rx_hz())
    }

    /// Take the dial for an over on `tx_hz`. `None` when transmit already lands
    /// where we listen (no split, no XIT), so the common case costs no retune.
    fn begin_tx(&mut self, tx_hz: f64) -> Option<f64> {
        if (tx_hz - self.rx_hz()).abs() < 1.0 {
            return None;
        }
        self.tx = Some(tx_hz);
        Some(tx_hz)
    }

    /// Give the dial back, including any retune deferred during the over.
    /// `None` when the over never moved it.
    fn end_tx(&mut self) -> Option<f64> {
        self.tx.take().map(|_| self.rx_hz())
    }

    /// Fold a dial frequency the *rig* reported back into the VFO, returning the
    /// operator's new VFO. `None` while an over owns the dial: what the rig
    /// reports then is our own transmit frequency, which says nothing about
    /// where the operator wants to listen.
    fn report(&mut self, dial_hz: f64) -> Option<f64> {
        if self.tx.is_some() {
            return None;
        }
        self.vfo = dial_hz - self.rit;
        Some(self.vfo)
    }
}

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
}

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

        let label = format!("CAT rig ({}) on {}", cfg.family.label(), cfg.serial.path);
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
            dial: Dial { vfo: center, ..Dial::default() },
            label,
            status,
            last_telem: None,
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
                // SWR arrives on the separate telemetry channel, not here.
                sdroxide_cat::CatUpdate::Swr(_) => {}
            }
        }
        out
    }

    fn set_control_mode(&mut self, mode: Mode) -> Result<()> {
        self.cat.set_mode(mode);
        Ok(())
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

    fn tx_telemetry(&mut self) -> Option<TxTelemetry> {
        // The CI-V thread polls SWR at ~5 Hz; latch its latest reading so the
        // engine's 100 ms meter tick always has a value to show.
        if let Some(t) = self.cat.poll_telemetry() {
            self.last_telem = Some(t);
        }
        self.last_telem
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

#[cfg(test)]
mod tests {
    use super::Dial;

    fn at(vfo: f64) -> Dial {
        Dial { vfo, ..Dial::default() }
    }

    #[test]
    fn rit_rides_on_the_dial_and_comes_back_out_of_what_the_rig_reports() {
        let mut d = at(14_074_000.0);
        // RIT on: the dial moves, the VFO does not.
        assert_eq!(d.set_rit(700.0), Some(14_074_700.0));
        assert_eq!(d.vfo, 14_074_000.0);
        // The rig echoes that same dial back on its next poll. Folding the
        // offset out has to land on the VFO we started from — otherwise every
        // poll would walk the VFO up by the RIT offset.
        assert_eq!(d.report(14_074_700.0), Some(14_074_000.0));
        // The operator turns the rig's own dial 1 kHz up: their VFO moved with
        // it, and RIT still sits on top.
        assert_eq!(d.report(14_075_700.0), Some(14_075_000.0));
        assert_eq!(d.rx_hz(), 14_075_700.0);
        // Clearing RIT puts the dial back on the VFO.
        assert_eq!(d.set_rit(0.0), Some(14_075_000.0));
        // Re-asserting the same offset is not a retune.
        assert_eq!(d.set_rit(0.0), None);
    }

    #[test]
    fn transmitting_where_we_listen_never_touches_the_dial() {
        let mut d = at(14_074_000.0);
        assert_eq!(d.begin_tx(14_074_000.0), None, "no split, no XIT: nothing to do");
        assert_eq!(d.end_tx(), None);
        // Sub-hertz differences are rounding, not an offset worth a CAT write.
        assert_eq!(d.begin_tx(14_074_000.4), None);
    }

    #[test]
    fn split_borrows_the_dial_for_the_over_and_gives_it_back() {
        let mut d = at(14_074_000.0);
        d.set_rit(700.0);
        // Split onto the other VFO: transmit takes the dial…
        assert_eq!(d.begin_tx(14_200_000.0), Some(14_200_000.0));
        // …and holds it. A report while it does is our own transmit frequency,
        // so it must not be mistaken for the operator moving the dial.
        assert_eq!(d.report(14_200_000.0), None);
        assert_eq!(d.vfo, 14_074_000.0, "the VFO is untouched by the over");
        // Unkey: back to the receive frequency, RIT included.
        assert_eq!(d.end_tx(), Some(14_074_700.0));
        assert_eq!(d.end_tx(), None, "the dial is only given back once");
    }

    #[test]
    fn a_retune_during_an_over_waits_for_unkey() {
        let mut d = at(14_074_000.0);
        assert_eq!(d.begin_tx(14_200_000.0), Some(14_200_000.0));
        // Retuning mid-over would drag the transmitter off the frequency it was
        // cleared to use, so nothing is commanded…
        assert_eq!(d.set_vfo(14_080_000.0), None);
        assert_eq!(d.set_rit(-500.0), None);
        // …but both are remembered, and unkey lands on the new receive frequency.
        assert_eq!(d.end_tx(), Some(14_079_500.0));
    }
}
