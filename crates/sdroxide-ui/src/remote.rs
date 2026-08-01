//! `RemoteController`: the same UI seam as `LocalController`, but over a
//! WebSocket speaking `sdroxide-proto`. Compiles for wasm32 and native.

use std::collections::VecDeque;

use ewebsock::{WsEvent, WsMessage, WsReceiver, WsSender};
use sdroxide_proto::{AudioCaps, AudioCodec, ClientMsg, PROTO_VERSION, ServerMsg, decode, encode};
use sdroxide_types::{AudioDevices, Command, RadioController, RadioEvent};

/// Platform audio glue: playback of received PCM and microphone capture.
/// The wasm client backs this with an AudioWorklet bridge.
pub trait AudioBridge {
    fn caps(&self) -> AudioCaps;
    /// Play mono 48 kHz PCM.
    fn play(&mut self, pcm: &[f32]);
    /// Append captured mic samples (mono 48 kHz) to `out`.
    fn pull_mic(&mut self, out: &mut Vec<f32>);
    /// Switchable sound devices, when the platform has any (native cpal
    /// bridge). The browser bridge keeps the default `None` — the browser
    /// owns device routing there.
    fn devices(&self) -> Option<AudioDevices> {
        None
    }
    /// Switch the output (`output = true`) or input device; `None` = default.
    fn set_device(&mut self, output: bool, name: Option<String>) {
        let _ = (output, name);
    }
}

/// How many pre-open messages to hold. The window is one connect round-trip
/// wide and the UI sends a handful of config commands in it, so this only ever
/// bites if the socket never opens at all.
const OUTBOX_LIMIT: usize = 64;

/// Open a socket that wakes the UI on every event.
fn dial(
    url: &str,
    wake: &std::sync::Arc<dyn Fn() + Send + Sync>,
) -> Result<(WsSender, WsReceiver), String> {
    let wake = std::sync::Arc::clone(wake);
    ewebsock::connect_with_wakeup(url, ewebsock::Options::default(), move || wake())
        .map_err(|e| e.to_string())
}

/// Outbound gate: `Hello` has to be the first message the server reads, and the
/// UI starts issuing commands (a first-frame `SetSpectrumCfg`, with no
/// debounce) before the socket has finished opening. Anything sent that early
/// waits here and flushes in order *behind* `Hello`.
///
/// Without it the two platforms fail differently and neither is acceptable:
/// ewebsock's native sender queues pre-open messages, so the command reaches
/// the server ahead of `Hello` and the session is closed with
/// "expected Hello"; its web sender calls `send()` on a still-CONNECTING
/// socket, which throws and drops the command on the floor.
#[derive(Default)]
struct Outbox {
    opened: bool,
    queued: VecDeque<ClientMsg>,
}

impl Outbox {
    /// `Some(msg)` to write now, `None` if it was held back.
    fn send(&mut self, msg: ClientMsg) -> Option<ClientMsg> {
        if self.opened {
            return Some(msg);
        }
        if self.queued.len() == OUTBOX_LIMIT {
            // Oldest first: these are latest-wins config commands.
            self.queued.pop_front();
        }
        self.queued.push_back(msg);
        None
    }

    /// The socket opened: `hello`, then everything that was waiting.
    fn open(&mut self, hello: ClientMsg) -> Vec<ClientMsg> {
        self.opened = true;
        let mut out = Vec::with_capacity(self.queued.len() + 1);
        out.push(hello);
        out.extend(self.queued.drain(..));
        out
    }
}

pub struct RemoteController {
    sender: WsSender,
    receiver: WsReceiver,
    /// What to dial, and how to wake the UI when the socket has something —
    /// both kept so the session can be re-established in place after the link
    /// drops, without rebuilding the app around a new controller.
    url: String,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    outbox: Outbox,
    audio: Option<Box<dyn AudioBridge>>,
    pending: VecDeque<RadioEvent>,
    tx_codec: Option<AudioCodec>,
    transmitting: bool,
    /// The engine is recording a voice-keyer message. Its microphone is *our*
    /// microphone, so the uplink has to run for this too — otherwise a remote
    /// operator's recording comes back silent.
    voice_recording: bool,
    mic_buf: Vec<f32>,
    mic_seq: u32,
}

impl RemoteController {
    /// `wake` is called from the socket thread whenever an event arrives —
    /// pass `ctx.request_repaint` so the UI wakes immediately instead of
    /// waiting for its next scheduled poll.
    pub fn connect(
        url: &str,
        audio: Option<Box<dyn AudioBridge>>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(wake);
        let (sender, receiver) = dial(url, &wake)?;
        Ok(RemoteController {
            sender,
            receiver,
            url: url.to_string(),
            wake,
            outbox: Outbox::default(),
            audio,
            pending: VecDeque::new(),
            tx_codec: None,
            transmitting: false,
            voice_recording: false,
            mic_buf: Vec::new(),
            mic_seq: 0,
        })
    }

    /// Write straight to the socket, bypassing the gate. Only for messages the
    /// gate has already released.
    fn write(&mut self, msg: &ClientMsg) {
        if let Ok(bytes) = encode(msg) {
            self.sender.send(WsMessage::Binary(bytes));
        }
    }

    fn send_msg(&mut self, msg: ClientMsg) {
        if let Some(msg) = self.outbox.send(msg) {
            self.write(&msg);
        }
    }

    fn on_server_msg(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::HelloAck { caps, state, tx_codec, .. } => {
                self.tx_codec = Some(tx_codec);
                self.pending.push_back(RadioEvent::Capabilities(caps));
                self.pending.push_back(RadioEvent::State(state));
            }
            ServerMsg::State(s) => {
                self.transmitting = s.tx.ptt || s.tx.tune;
                self.pending.push_back(RadioEvent::State(s));
            }
            ServerMsg::Spectrum(f) => self.pending.push_back(RadioEvent::Spectrum(f)),
            ServerMsg::WideSpectrum(f) => self.pending.push_back(RadioEvent::WideSpectrum(f)),
            ServerMsg::Meters(m) => self.pending.push_back(RadioEvent::Meters(m)),
            ServerMsg::Memories(m) => self.pending.push_back(RadioEvent::Memories(m)),
            ServerMsg::RxAudio { payload, .. } => {
                if let Some(bridge) = self.audio.as_mut() {
                    // Only the PCM16 downlink is decoded client-side; an
                    // Opus-capable bridge would advertise it in Hello.
                    let pcm: Vec<f32> = payload
                        .chunks_exact(2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                        .collect();
                    bridge.play(&pcm);
                }
            }
            ServerMsg::Pong(_) => {}
            ServerMsg::Busy => self.pending.push_back(RadioEvent::ConnectionLost(
                "server busy — another client is connected".into(),
            )),
            ServerMsg::Error(e) => self.pending.push_back(RadioEvent::ConnectionLost(e)),
            ServerMsg::Notice(n) => self.pending.push_back(RadioEvent::Notice(n)),
            ServerMsg::Ft8Decodes(d) => self.pending.push_back(RadioEvent::Ft8Decodes(d)),
            ServerMsg::Ft8Status(s) => self.pending.push_back(RadioEvent::Ft8Status(s)),
            ServerMsg::Ft8QsoLogged(r) => self.pending.push_back(RadioEvent::Ft8QsoLogged(r)),
            ServerMsg::SkimmerSpots(s) => self.pending.push_back(RadioEvent::SkimmerSpots(s)),
            ServerMsg::SstvLine { image_id, y, rgb } => {
                self.pending.push_back(RadioEvent::SstvLine { image_id, y, rgb })
            }
            ServerMsg::SstvImage { image_id, mode, w, h, png } => {
                self.pending.push_back(RadioEvent::SstvImage { image_id, mode, w, h, png })
            }
            ServerMsg::SstvStatus(s) => self.pending.push_back(RadioEvent::SstvStatus(s)),
            ServerMsg::WefaxLine { image_id, y, gray } => {
                self.pending.push_back(RadioEvent::WefaxLine { image_id, y, gray })
            }
            ServerMsg::WefaxImage { image_id, w, h, png } => {
                self.pending.push_back(RadioEvent::WefaxImage { image_id, w, h, png })
            }
            ServerMsg::WefaxStatus(s) => self.pending.push_back(RadioEvent::WefaxStatus(s)),
            ServerMsg::RifpRows { image_id, y, w, h, rows } => {
                self.pending.push_back(RadioEvent::RifpRows { image_id, y, w, h, rows })
            }
            ServerMsg::RifpImage { image_id, meta, png } => {
                self.pending.push_back(RadioEvent::RifpImage { image_id, meta, png })
            }
            ServerMsg::RifpStatus(s) => self.pending.push_back(RadioEvent::RifpStatus(s)),
            ServerMsg::DigiImage { png } => self.pending.push_back(RadioEvent::DigiImage { png }),
            ServerMsg::HellColumns { seq, rows, cols } => {
                self.pending.push_back(RadioEvent::HellColumns { seq, rows, cols })
            }
            ServerMsg::VoiceStatus(v) => {
                self.voice_recording = v.recording.is_some();
                self.pending.push_back(RadioEvent::VoiceStatus(v));
            }
            ServerMsg::Spots(s) => self.pending.push_back(RadioEvent::Spots(s)),
            ServerMsg::NetStatus(s) => self.pending.push_back(RadioEvent::NetStatus(s)),
            ServerMsg::CallsignResult(c) => self.pending.push_back(RadioEvent::CallsignResult(c)),
            ServerMsg::Upload(r) => self.pending.push_back(RadioEvent::Upload(r)),
            ServerMsg::Confirmations(r) => self.pending.push_back(RadioEvent::Confirmations(r)),
            ServerMsg::RigctldStatus { running, addr, clients, error } => {
                self.pending.push_back(RadioEvent::RigctldStatus { running, addr, clients, error })
            }
            ServerMsg::TciServerStatus { running, addr, clients, error } => self
                .pending
                .push_back(RadioEvent::TciServerStatus { running, addr, clients, error }),
            ServerMsg::ImagePresets(p) => self.pending.push_back(RadioEvent::ImagePresets(p)),
            ServerMsg::ImageSlotSource { slot, version, png } => {
                self.pending.push_back(RadioEvent::ImageSlotSource { slot, version, png })
            }
            ServerMsg::ImageListing(l) => self.pending.push_back(RadioEvent::ImageListing(l)),
            ServerMsg::ImageFile { kind, name, png } => {
                self.pending.push_back(RadioEvent::ImageFile { kind, name, png })
            }
            ServerMsg::ImageSaved(e) => self.pending.push_back(RadioEvent::ImageSaved(e)),
            ServerMsg::ImageDeleted { kind, name } => {
                self.pending.push_back(RadioEvent::ImageDeleted { kind, name })
            }
            ServerMsg::StationConfig(c) => self.pending.push_back(RadioEvent::StationConfig(c)),
            ServerMsg::TleSubStatus(s) => self.pending.push_back(RadioEvent::TleSubStatus(s)),
        }
    }

    fn pump_mic(&mut self) {
        let Some(bridge) = self.audio.as_mut() else { return };
        if !self.transmitting && !self.voice_recording {
            self.mic_buf.clear();
            // Keep draining the capture ring so it doesn't back up.
            let mut scratch = Vec::new();
            bridge.pull_mic(&mut scratch);
            return;
        }
        bridge.pull_mic(&mut self.mic_buf);
        while self.mic_buf.len() >= 960 {
            let payload: Vec<u8> = self.mic_buf[..960]
                .iter()
                .flat_map(|&s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
                .collect();
            self.mic_seq = self.mic_seq.wrapping_add(1);
            let msg = ClientMsg::MicFrame { seq: self.mic_seq, payload };
            self.send_msg(msg);
            self.mic_buf.drain(..960);
        }
    }
}

impl RadioController for RemoteController {
    fn send(&mut self, cmd: Command) {
        self.send_msg(ClientMsg::Command(cmd));
    }

    fn poll_event(&mut self) -> Option<RadioEvent> {
        while let Some(ev) = self.receiver.try_recv() {
            match ev {
                WsEvent::Opened => {
                    let caps = self
                        .audio
                        .as_ref()
                        .map(|a| a.caps())
                        .unwrap_or(AudioCaps { opus_decode: false, opus_encode: false });
                    let hello = ClientMsg::Hello { proto: PROTO_VERSION, audio: caps };
                    for msg in self.outbox.open(hello) {
                        self.write(&msg);
                    }
                }
                WsEvent::Message(WsMessage::Binary(bytes)) => match decode::<ServerMsg>(&bytes) {
                    Ok(msg) => self.on_server_msg(msg),
                    Err(e) => self
                        .pending
                        .push_back(RadioEvent::ConnectionLost(format!("protocol error: {e}"))),
                },
                WsEvent::Message(_) => {}
                WsEvent::Error(e) => {
                    self.pending.push_back(RadioEvent::ConnectionLost(e));
                }
                WsEvent::Closed => {
                    self.pending.push_back(RadioEvent::ConnectionLost("connection closed".into()));
                }
            }
        }
        self.pump_mic();
        self.pending.pop_front()
    }

    fn wants_repaint_soon(&self) -> bool {
        !self.pending.is_empty()
    }

    fn can_reconnect(&self) -> bool {
        true
    }

    fn engine_is_remote(&self) -> bool {
        true
    }

    fn reconnect(&mut self) -> Result<(), String> {
        // Close first, and only then dial: the server allows one control
        // session at a time, so a new socket opened while the old one is still
        // registered is answered with `Busy` — the reconnect would fail on the
        // strength of the connection it is replacing.
        self.sender.close();
        let (sender, receiver) = dial(&self.url, &self.wake)?;
        self.sender = sender;
        self.receiver = receiver;
        // Everything below is per-session: the outbox gate has to hold commands
        // behind a fresh `Hello`, the codec is renegotiated in the handshake,
        // and neither queued events nor a half-sent microphone block from the
        // dead session may be carried into the new one.
        self.outbox = Outbox::default();
        self.pending.clear();
        self.tx_codec = None;
        self.transmitting = false;
        self.voice_recording = false;
        self.mic_buf.clear();
        self.mic_seq = 0;
        Ok(())
    }

    fn audio_devices(&self) -> Option<AudioDevices> {
        self.audio.as_ref().and_then(|a| a.devices())
    }

    fn set_audio_device(&mut self, output: bool, name: Option<String>) {
        if let Some(a) = self.audio.as_mut() {
            a.set_device(output, name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdroxide_types::SpectrumConfig;

    fn hello() -> ClientMsg {
        ClientMsg::Hello {
            proto: PROTO_VERSION,
            audio: AudioCaps { opus_decode: false, opus_encode: false },
        }
    }

    /// The first-frame `SetSpectrumCfg` that the capture from the bug report
    /// caught on the wire ahead of `Hello`.
    fn first_frame_cfg() -> ClientMsg {
        ClientMsg::Command(Command::SetSpectrumCfg(SpectrumConfig {
            fft_size: 32768,
            fps: 60,
            avg_tc: 0.0,
            db_floor: -120.0,
            db_ceil: -20.0,
            viewport: Some((14_584_000.0, 14_968_000.0)),
        }))
    }

    /// The regression: on a link with real latency the UI issues commands
    /// before `Opened` arrives. Nothing may reach the socket until `Hello` has,
    /// or the server closes the session with "expected Hello".
    #[test]
    fn nothing_precedes_hello_on_the_wire() {
        let mut ob = Outbox::default();
        assert!(ob.send(first_frame_cfg()).is_none(), "a pre-open command must not be written");

        let flushed = ob.open(hello());
        assert_eq!(flushed.len(), 2);
        assert!(matches!(flushed[0], ClientMsg::Hello { .. }), "Hello must be first");
        assert_eq!(flushed[1], first_frame_cfg(), "the held command must follow it, not be lost");
    }

    /// Ordering is preserved across the gate, and once open there is no
    /// buffering left to reorder anything.
    #[test]
    fn queued_commands_keep_their_order_and_then_pass_through() {
        let mut ob = Outbox::default();
        for cmd in [Command::SetPtt(true), Command::SetPtt(false), Command::SetCenter(14_074_000.0)]
        {
            assert!(ob.send(ClientMsg::Command(cmd)).is_none());
        }
        let flushed = ob.open(hello());
        assert_eq!(
            flushed[1..],
            [
                ClientMsg::Command(Command::SetPtt(true)),
                ClientMsg::Command(Command::SetPtt(false)),
                ClientMsg::Command(Command::SetCenter(14_074_000.0)),
            ]
        );
        // After the handshake the gate is transparent.
        let passed = ob.send(ClientMsg::Ping(7));
        assert_eq!(passed, Some(ClientMsg::Ping(7)));
    }

    /// A socket that never opens must not grow the queue without bound.
    #[test]
    fn the_outbox_is_bounded_and_drops_the_stalest_first() {
        let mut ob = Outbox::default();
        for f in 0..(OUTBOX_LIMIT as u64 + 10) {
            ob.send(ClientMsg::Ping(f));
        }
        let flushed = ob.open(hello());
        assert_eq!(flushed.len(), OUTBOX_LIMIT + 1);
        // The 10 oldest were dropped; the newest survived.
        assert_eq!(flushed[1], ClientMsg::Ping(10));
        assert_eq!(flushed[OUTBOX_LIMIT], ClientMsg::Ping(OUTBOX_LIMIT as u64 + 9));
    }
}
