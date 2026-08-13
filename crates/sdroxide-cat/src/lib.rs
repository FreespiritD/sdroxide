//! Serial CAT control for non-SoapySDR rigs (Icom CI-V / Yaesu / Xiegu).
//!
//! NATIVE ONLY — links `serialport`; must never be a dependency of any
//! wasm-targeted crate. The rest of the app talks to it only through the
//! opaque [`CatHandle`] (a background serial thread), so no serial types leak
//! into the engine or UI.

/// Icom CI-V framing and parsing. Public because the Icom LAN backend tunnels
/// the same protocol over UDP and must not carry a second copy of it.
pub mod civ;
mod kenwood;
mod yaesu;

use std::io::Write;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use sdroxide_types::{
    CatConfig, CatFamily, CwKeying, DigiMode, LineState, Mode, ModeControl, Parity, PttMethod,
    SerialConfig, StopBits, TxTelemetry,
};
use tracing::{info, warn};

/// A change the rig reported (external dial/mode movement) or that we read back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CatUpdate {
    Freq(f64),
    Mode(Mode),
    /// TX SWR reading (routed to the telemetry channel, not the control channel).
    Swr(f32),
    /// RX S-meter reading in dBm, from the rig's own meter (routed to the
    /// signal channel, not the control channel).
    Signal(f32),
}

/// Enumerate serial ports for the settings UI. USB-style ports (ttyACM/ttyUSB,
/// where CAT rigs like the X6100 appear) are listed first; the many legacy
/// `/dev/ttyS*` entries — which the non-libudev sysfs scan can't filter to
/// only present ones — sort to the end.
pub fn available_ports() -> Vec<String> {
    let mut ports: Vec<String> = serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default();
    let rank = |p: &str| -> u8 {
        if p.contains("ttyACM") || p.contains("ttyUSB") {
            0
        } else if p.contains("ttyS") {
            2
        } else {
            1
        }
    };
    ports.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    ports
}

/// Per-family framing. `parse` consumes complete frames from a rolling buffer.
trait Protocol: Send {
    fn set_freq(&mut self, hz: f64) -> Vec<u8>;
    fn set_mode(&mut self, m: Mode) -> Vec<u8>;
    /// CAT-command PTT (only used when `PttMethod::Cat`).
    fn ptt(&self, on: bool) -> Vec<u8>;
    /// Frames that request the rig's current freq + mode.
    fn poll_requests(&self) -> Vec<Vec<u8>>;
    /// Frames requesting TX telemetry (SWR / power), polled only while keyed.
    /// Empty for families with no such read.
    fn tx_telemetry_requests(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }
    /// Frames requesting the rig's S-meter, polled only while receiving.
    ///
    /// A CAT rig sends us audio it has already demodulated, filtered and
    /// levelled — there is no signal left on this side of the sound card to
    /// measure — so its own meter is the only S-meter the operator can be
    /// shown. Empty for families with no such read, which fall back to the
    /// level of the audio itself.
    fn rx_telemetry_requests(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }
    /// Frames that switch the rig's *own* RIT, XIT and split off, sent once
    /// when the port opens. sdroxide carries all three on the dial (the rig's
    /// dial is the only frequency control a CAT rig gives us), so anything the
    /// radio is still holding would add to ours unseen. Empty for families with
    /// no such command.
    fn clear_offsets(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }

    /// Longest run of CW text this rig's keyer takes in one go, or 0 when it
    /// cannot be keyed from text at all. The caller sends no more than this per
    /// [`Protocol::send_cw`], so nothing has to be truncated on the way out.
    fn cw_chunk_len(&self) -> usize {
        0
    }
    /// Hand `text` to the rig's own keyer. The rig keys itself — this is *not*
    /// wrapped in PTT, and must not be: a transmitter already keyed by CAT is
    /// one the keyer cannot key.
    fn send_cw(&mut self, _text: &str) -> Vec<Vec<u8>> {
        Vec::new()
    }
    /// Stop a message the rig is part way through sending. Empty for families
    /// with no such command — there, an abort can only stop the *next* chunk.
    fn abort_cw(&mut self) -> Vec<Vec<u8>> {
        Vec::new()
    }
    /// Set the rig keyer's speed. The rig keys at its own speed, so the panel's
    /// WPM has no effect on the air until it has been sent here.
    fn set_cw_wpm(&mut self, _wpm: f32) -> Vec<Vec<u8>> {
        Vec::new()
    }

    /// True when [`Protocol::parse`] learned something about the rig's framing
    /// that invalidates frames written before it — the Yaesu frequency-field
    /// width. Reported once, to whoever can re-issue the frame.
    fn reframed(&mut self) -> bool {
        false
    }

    /// True when [`Protocol::parse`] saw the rig refuse a command since the
    /// last call. Which command it refused is not in the answer — CI-V's "NG"
    /// carries nothing but itself — so this is only worth reporting where the
    /// caller knows what it just sent. Reported once, then cleared.
    fn refused(&mut self) -> bool {
        false
    }

    fn parse(&mut self, buf: &mut Vec<u8>) -> Vec<CatUpdate>;
}

/// CI-V protocol (Icom + Xiegu). `radio` is the CI-V transceiver address.
struct Civ {
    radio: u8,
    /// The rig answered "NG" since this was last read (see
    /// [`Protocol::refused`]).
    nak: bool,
}

impl Protocol for Civ {
    fn set_freq(&mut self, hz: f64) -> Vec<u8> {
        civ::set_freq_frame(self.radio, hz)
    }
    fn set_mode(&mut self, m: Mode) -> Vec<u8> {
        civ::set_mode_frame(self.radio, m)
    }
    fn ptt(&self, on: bool) -> Vec<u8> {
        civ::ptt_frame(self.radio, on)
    }
    fn poll_requests(&self) -> Vec<Vec<u8>> {
        vec![civ::read_freq_frame(self.radio), civ::read_mode_frame(self.radio)]
    }
    fn tx_telemetry_requests(&self) -> Vec<Vec<u8>> {
        vec![civ::read_swr_frame(self.radio)]
    }
    fn rx_telemetry_requests(&self) -> Vec<Vec<u8>> {
        vec![civ::read_smeter_frame(self.radio)]
    }
    fn clear_offsets(&self) -> Vec<Vec<u8>> {
        civ::clear_offsets_frames(self.radio)
    }
    fn cw_chunk_len(&self) -> usize {
        civ::CW_MAX
    }
    fn send_cw(&mut self, text: &str) -> Vec<Vec<u8>> {
        civ::send_cw_frame(self.radio, text).into_iter().collect()
    }
    fn abort_cw(&mut self) -> Vec<Vec<u8>> {
        vec![civ::stop_cw_frame(self.radio)]
    }
    fn set_cw_wpm(&mut self, wpm: f32) -> Vec<Vec<u8>> {
        vec![civ::keyer_speed_frame(self.radio, wpm)]
    }
    fn refused(&mut self) -> bool {
        std::mem::take(&mut self.nak)
    }
    fn parse(&mut self, buf: &mut Vec<u8>) -> Vec<CatUpdate> {
        let mut out = Vec::new();
        for reply in civ::parse_frames(buf) {
            // Ignore our own echoes (controller-sourced frames).
            if reply.from == civ::CONTROLLER_ADDR {
                continue;
            }
            match reply.cmd {
                0x03 => {
                    if let Some(hz) = civ::decode_freq(&reply.data) {
                        out.push(CatUpdate::Freq(hz));
                    }
                }
                0x04 => {
                    if let Some(&b) = reply.data.first() {
                        if let Some(m) = civ::civ_to_mode(b) {
                            out.push(CatUpdate::Mode(m));
                        }
                    }
                }
                // Meter read (0x15): the SWR sub-meter (0x12) while transmitting,
                // the S-meter (0x02) while receiving. The sub-command byte in the
                // reply says which arrived — nothing else does, since both are
                // answered on the one command.
                0x15 => {
                    if let Some(swr) = civ::parse_swr_reply(&reply.data) {
                        out.push(CatUpdate::Swr(swr));
                    } else if let Some(dbm) = civ::parse_smeter_reply(&reply.data) {
                        out.push(CatUpdate::Signal(dbm));
                    }
                }
                // "NG": the rig would not do what it was asked. Plenty of these
                // are expected — every sub-command a given model doesn't
                // implement answers this way — so it is only noted here, for a
                // caller that knows it just sent something that mattered.
                civ::NG => self.nak = true,
                _ => {}
            }
        }
        out
    }
}

fn make_protocol(cfg: &CatConfig) -> Box<dyn Protocol> {
    match cfg.family {
        CatFamily::Xiegu | CatFamily::Icom => {
            Box::new(Civ { radio: cfg.icom_radio_id, nak: false })
        }
        CatFamily::Yaesu => Box::new(yaesu::Yaesu::new()),
        CatFamily::Kenwood => Box::new(kenwood::Kenwood::new(cfg.kenwood_send)),
    }
}

enum CatCmd {
    Freq(f64),
    Mode(Mode),
    Ptt(bool),
    /// Text for the rig's own keyer to send.
    Cw(String),
    /// Stop a message the rig is part way through.
    CwAbort,
    CwWpm(f32),
    Stop,
}

/// Opaque handle to the running serial thread.
pub struct CatHandle {
    cmd_tx: Sender<CatCmd>,
    event_rx: Receiver<CatUpdate>,
    telem_rx: Receiver<TxTelemetry>,
    signal_rx: Receiver<f32>,
    cw_chunk_len: usize,
}

impl CatHandle {
    pub fn set_freq(&self, hz: f64) {
        let _ = self.cmd_tx.send(CatCmd::Freq(hz));
    }
    pub fn set_mode(&self, m: Mode) {
        let _ = self.cmd_tx.send(CatCmd::Mode(m));
    }
    pub fn set_ptt(&self, on: bool) {
        let _ = self.cmd_tx.send(CatCmd::Ptt(on));
    }
    /// How much CW text this rig's keyer takes at a time, or `None` if it
    /// cannot be keyed from text.
    pub fn cw_chunk_len(&self) -> Option<usize> {
        (self.cw_chunk_len > 0).then_some(self.cw_chunk_len)
    }
    /// Hand `text` to the rig's keyer. No more than [`Self::cw_chunk_len`] at a
    /// time, and not again until the rig has finished the last lot — a rig part
    /// way through a message has nowhere to put a second one.
    pub fn send_cw(&self, text: String) {
        let _ = self.cmd_tx.send(CatCmd::Cw(text));
    }
    pub fn abort_cw(&self) {
        let _ = self.cmd_tx.send(CatCmd::CwAbort);
    }
    /// Set the rig keyer's speed — but only when the rig is the one doing the
    /// sending. A rig has one keyer and its paddle uses it too, so an operator
    /// who is not keying from here keeps their own speed.
    pub fn set_cw_wpm(&self, wpm: f32) {
        if self.cw_chunk_len == 0 {
            return;
        }
        let _ = self.cmd_tx.send(CatCmd::CwWpm(wpm));
    }
    /// Non-blocking drain of rig-reported freq/mode changes.
    pub fn poll(&self) -> Vec<CatUpdate> {
        self.event_rx.try_iter().collect()
    }
    /// Latest TX telemetry (SWR) the rig reported, or `None` if nothing new
    /// arrived since the last call. A default (all-`None`) value is pushed when
    /// PTT drops, so the reading clears on unkey.
    pub fn poll_telemetry(&self) -> Option<TxTelemetry> {
        self.telem_rx.try_iter().last()
    }

    /// The rig's own S-meter in dBm, or `None` if nothing new arrived since the
    /// last call. Only rigs whose family has such a read report one; the rest
    /// never send here at all.
    pub fn poll_signal(&self) -> Option<f32> {
        self.signal_rx.try_iter().last()
    }
}

impl Drop for CatHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(CatCmd::Stop);
    }
}

/// Blocking one-shot query of the rig's current frequency + mode, used at
/// startup so the app adopts the radio's state instead of overwriting it.
/// Returns `None` if the port can't be opened or the rig doesn't answer.
pub fn query_once(cfg: &CatConfig) -> Option<(Option<f64>, Option<Mode>)> {
    let mut port = open_port(&cfg.serial).ok()?;
    let mut protocol = make_protocol(cfg);
    for req in protocol.poll_requests() {
        let _ = port.write_all(&req);
    }
    let _ = port.flush();
    let mut rx = Vec::new();
    let mut buf = [0u8; 128];
    let (mut freq, mut mode) = (None, None);
    let deadline = Instant::now() + Duration::from_millis(600);
    while Instant::now() < deadline && (freq.is_none() || mode.is_none()) {
        if let Ok(n) = port.read(&mut buf) {
            if n > 0 {
                rx.extend_from_slice(&buf[..n]);
                for u in protocol.parse(&mut rx) {
                    match u {
                        CatUpdate::Freq(hz) => freq = Some(hz),
                        CatUpdate::Mode(m) => mode = Some(m),
                        // Neither meter is requested during the startup query.
                        CatUpdate::Swr(_) | CatUpdate::Signal(_) => {}
                    }
                }
            }
        }
    }
    (freq.is_some() || mode.is_some()).then_some((freq, mode))
}

/// Spawn the serial CAT thread from a persisted [`CatConfig`].
pub fn spawn(cfg: CatConfig) -> CatHandle {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let (telem_tx, telem_rx) = crossbeam_channel::unbounded();
    let (signal_tx, signal_rx) = crossbeam_channel::unbounded();
    // Asked of the framing before it goes to the thread, so the keyer can size
    // its chunks to the rig without reaching across the channel to find out.
    let cw_chunk_len =
        if cfg.cw_keying == CwKeying::Cat { make_protocol(&cfg).cw_chunk_len() } else { 0 };
    std::thread::Builder::new()
        .name("sdroxide-cat".into())
        .spawn(move || serial_thread(cfg, cmd_rx, event_tx, telem_tx, signal_tx))
        .expect("spawn cat thread");
    CatHandle { cmd_tx, event_rx, telem_rx, signal_rx, cw_chunk_len }
}

fn map_parity(p: Parity) -> serialport::Parity {
    match p {
        Parity::None => serialport::Parity::None,
        Parity::Even => serialport::Parity::Even,
        Parity::Odd => serialport::Parity::Odd,
    }
}
fn map_stop(s: StopBits) -> serialport::StopBits {
    match s {
        StopBits::One => serialport::StopBits::One,
        StopBits::Two => serialport::StopBits::Two,
    }
}
fn map_data_bits(n: u8) -> serialport::DataBits {
    match n {
        5 => serialport::DataBits::Five,
        6 => serialport::DataBits::Six,
        7 => serialport::DataBits::Seven,
        _ => serialport::DataBits::Eight,
    }
}

/// The shortest gap left between two frames written to the rig.
///
/// A transceiver serves its control port with the same processor that runs the
/// radio, and it acts on one command at a time: a frame that arrives while the
/// rig is still working through the previous one can simply be missed, and
/// nothing on the wire says so. The case that matters is key-down, which asserts
/// the mode (and, with split or XIT, the transmit frequency) and then keys —
/// changing mode is among the slowest things a rig does, and a PTT lost behind
/// one is an over that never reaches the air while everything else about the
/// link looks healthy.
///
/// Only consecutive writes wait; a frame sent on its own goes out at once. The
/// whole traffic here is a handful of short frames a second, so this costs
/// nothing that can be noticed — and [`ModeMemory`] keeps the mode off the wire
/// entirely when the rig is already in it, which is what makes key-down a single
/// frame in the ordinary case.
const FRAME_GAP: Duration = Duration::from_millis(30);

/// Write one frame, leaving at least [`FRAME_GAP`] since the last one went out.
/// Returns true on a write error — the caller's signal to reconnect.
fn write_frame(
    port: &mut dyn serialport::SerialPort,
    frame: &[u8],
    last_write: &mut Instant,
) -> bool {
    let since = last_write.elapsed();
    if since < FRAME_GAP {
        std::thread::sleep(FRAME_GAP - since);
    }
    let failed = port.write_all(frame).is_err();
    *last_write = Instant::now();
    failed
}

/// What mode the rig is in, held as the frame that would put it there — either
/// because it was told so, or because it said so on its last poll.
///
/// Every key-down asserts the mode, so that an over cannot go out in whatever
/// the rig happens to have been left in. Asserting it is not free, though: the
/// rig acts on the command every time, which on an Icom also re-selects filter
/// 1 under an operator who chose another, and leaves the radio busy at exactly
/// the moment the PTT frame arrives behind it. So the command is only written
/// when it would actually change something.
///
/// What is compared is the frame, not the mode: two of the app's modes can be
/// one thing to a rig (DIGU rides on USB, and that is what goes on the wire),
/// and the rig can only report back the one it has.
#[derive(Default)]
struct ModeMemory(Option<Vec<u8>>);

impl ModeMemory {
    /// True when `frame` still needs sending — the rig is in some other mode,
    /// or has not said which. Records it as sent.
    fn needs(&mut self, frame: &[u8]) -> bool {
        if self.0.as_deref() == Some(frame) {
            return false;
        }
        self.0 = Some(frame.to_vec());
        true
    }

    /// The rig reported the mode `frame` would have set. That is where it is,
    /// whoever put it there — a mode the operator selected on the radio itself
    /// is one there is no need to command back onto it.
    fn reported(&mut self, frame: &[u8]) {
        if self.0.as_deref() != Some(frame) {
            self.0 = Some(frame.to_vec());
        }
    }
}

fn open_port(s: &SerialConfig) -> serialport::Result<Box<dyn serialport::SerialPort>> {
    let port = serialport::new(&s.path, s.baud)
        .data_bits(map_data_bits(s.data_bits))
        .parity(map_parity(s.parity))
        .stop_bits(map_stop(s.stop_bits))
        .timeout(Duration::from_millis(50))
        .open()?;
    Ok(port)
}

/// Apply a forced control-line level (ignored when `LineState::None`). If a
/// line is used for PTT, PTT owns it instead (handled in the loop).
fn apply_line(port: &mut dyn serialport::SerialPort, forced: LineState, rts: bool) {
    let level = match forced {
        LineState::None => return,
        LineState::High => true,
        LineState::Low => false,
    };
    let _ =
        if rts { port.write_request_to_send(level) } else { port.write_data_terminal_ready(level) };
}

fn serial_thread(
    cfg: CatConfig,
    cmd_rx: Receiver<CatCmd>,
    event_tx: Sender<CatUpdate>,
    telem_tx: Sender<TxTelemetry>,
    signal_tx: Sender<f32>,
) {
    let mut protocol = make_protocol(&cfg);
    let poll_period = Duration::from_secs_f32((1.0 / cfg.poll_hz.max(0.2)).min(5.0));
    // What mode to command the rig into for a given app mode. FT8/FT4 use the
    // separate `digi_mode` setting; every other mode obeys `mode_control`
    // (CAT = mirror the selected mode to the rig; Radio = don't touch it).
    let mode_cmd = |app_mode: Mode| -> Option<Mode> {
        if app_mode.is_digital() && !app_mode.is_sstv() {
            return match cfg.digi_mode {
                DigiMode::Radio => None,
                DigiMode::Usb => Some(Mode::Usb),
                DigiMode::Data => Some(Mode::Digu),
            };
        }
        match cfg.mode_control {
            ModeControl::Cat => Some(app_mode),
            ModeControl::Radio => None,
        }
    };

    loop {
        // (Re)open the port, retrying on failure.
        let mut port = match open_port(&cfg.serial) {
            Ok(p) => {
                // The PTT method belongs in this line: a rig that answers every
                // read and still refuses to key is nearly always one being asked
                // to key some way it isn't set up for, and this is where that
                // shows.
                info!(
                    path = %cfg.serial.path,
                    baud = cfg.serial.baud,
                    family = cfg.family.label(),
                    ptt = cfg.ptt.label(),
                    "CAT port open"
                );
                p
            }
            Err(e) => {
                warn!(path = %cfg.serial.path, "CAT open failed: {e}");
                // Wait, but still honor a Stop.
                match cmd_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(CatCmd::Stop) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        return;
                    }
                    _ => continue,
                }
            }
        };
        // Forced control lines (unless the line is the PTT method).
        if cfg.ptt != PttMethod::Rts {
            apply_line(&mut *port, cfg.serial.force_rts, true);
        }
        if cfg.ptt != PttMethod::Dtr {
            apply_line(&mut *port, cfg.serial.force_dtr, false);
        }
        // Deassert PTT line at start.
        match cfg.ptt {
            PttMethod::Rts => {
                let _ = port.write_request_to_send(false);
            }
            PttMethod::Dtr => {
                let _ = port.write_data_terminal_ready(false);
            }
            _ => {}
        }
        // When the last frame went out, so consecutive writes can be spaced
        // (see `FRAME_GAP`). Backdated: the first write waits for nothing.
        let mut last_write = Instant::now() - FRAME_GAP;
        // Don't force a mode on connect — adopt the rig's current mode (read via
        // `query_once`/poll); the app commands mode only when the operator picks one.
        // RIT/XIT/split are the exception: those we do own, so clear the rig's
        // own copies rather than let them offset us invisibly.
        for f in protocol.clear_offsets() {
            write_frame(&mut *port, &f, &mut last_write);
        }

        let mut rx = Vec::with_capacity(256);
        let mut read_buf = [0u8; 256];
        let mut next_poll = Instant::now();
        // The meters are polled faster than freq/mode, and which meter is asked
        // for depends on what the rig is doing: SWR while keyed, S-meter while
        // receiving.
        let mut next_meter = Instant::now();
        let mut ptt = false;
        // When a CAT key-down was last written, so the rig's refusal of one can
        // be told from the refusals its unimplemented sub-commands answer with.
        let mut ptt_written: Option<Instant> = None;
        let mut pending_freq: Option<f64> = None;
        let mut last_sent_freq: Option<f64> = None;
        let mut freq_deadline = Instant::now();
        let mut mode_memory = ModeMemory::default();
        // Only forward genuine changes so the engine isn't re-notified every poll.
        let mut emit_freq: Option<f64> = None;
        let mut emit_mode: Option<Mode> = None;

        let broke = 'io: loop {
            // Drain commands.
            loop {
                match cmd_rx.try_recv() {
                    Ok(CatCmd::Freq(hz)) => pending_freq = Some(hz), // coalesce
                    Ok(CatCmd::Mode(m)) => {
                        if let Some(mm) = mode_cmd(m) {
                            let f = protocol.set_mode(mm);
                            if mode_memory.needs(&f) && write_frame(&mut *port, &f, &mut last_write)
                            {
                                break 'io true;
                            }
                        }
                    }
                    Ok(CatCmd::Ptt(on)) => {
                        // Key-down has to land on the transmit frequency. With
                        // XIT or split the engine queues the transmit dial
                        // immediately before PTT, and the debounce below would
                        // otherwise let the first moment of the over go out
                        // where we were listening — so flush it first.
                        if on
                            && let Some(hz) = pending_freq.take()
                            && last_sent_freq != Some(hz)
                        {
                            let f = protocol.set_freq(hz);
                            if write_frame(&mut *port, &f, &mut last_write) {
                                break 'io true;
                            }
                            last_sent_freq = Some(hz);
                            emit_freq = Some(hz); // suppress the poll echo
                            freq_deadline = Instant::now() + Duration::from_millis(50);
                        }
                        let failed = match cfg.ptt {
                            PttMethod::Vox => false,
                            PttMethod::Rts => port.write_request_to_send(on).is_err(),
                            PttMethod::Dtr => port.write_data_terminal_ready(on).is_err(),
                            PttMethod::Cat => {
                                let f = protocol.ptt(on);
                                ptt_written = on.then(Instant::now);
                                write_frame(&mut *port, &f, &mut last_write)
                            }
                        };
                        if failed {
                            break 'io true;
                        }
                        ptt = on;
                        // Ask the meter that belongs to the new state straight
                        // away, rather than showing the other one's last reading
                        // for the rest of the current period.
                        next_meter = Instant::now();
                        if !on {
                            // Clear the reading so the meter drops SWR on unkey.
                            let _ = telem_tx.send(TxTelemetry::default());
                        }
                    }
                    // CW the rig keys itself. Deliberately outside the PTT
                    // interlock above: the rig switches to transmit for the
                    // length of the message on its own, and asserting CAT PTT
                    // around it would hold a carrier the keyer cannot key.
                    Ok(CatCmd::Cw(text)) => {
                        for f in protocol.send_cw(&text) {
                            if write_frame(&mut *port, &f, &mut last_write) {
                                break 'io true;
                            }
                        }
                    }
                    Ok(CatCmd::CwAbort) => {
                        for f in protocol.abort_cw() {
                            if write_frame(&mut *port, &f, &mut last_write) {
                                break 'io true;
                            }
                        }
                    }
                    Ok(CatCmd::CwWpm(wpm)) => {
                        for f in protocol.set_cw_wpm(wpm) {
                            if write_frame(&mut *port, &f, &mut last_write) {
                                break 'io true;
                            }
                        }
                    }
                    Ok(CatCmd::Stop) => return,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            // Debounced frequency write (rate-limit to ~50 ms, only on change).
            if let Some(hz) = pending_freq {
                let now = Instant::now();
                if last_sent_freq != Some(hz) && now >= freq_deadline {
                    let f = protocol.set_freq(hz);
                    if write_frame(&mut *port, &f, &mut last_write) {
                        break 'io true;
                    }
                    last_sent_freq = Some(hz);
                    emit_freq = Some(hz); // suppress the poll echo of our own set
                    pending_freq = None;
                    freq_deadline = now + Duration::from_millis(50);
                }
            }

            // Poll the rig for external changes.
            if Instant::now() >= next_poll {
                next_poll = Instant::now() + poll_period;
                for req in protocol.poll_requests() {
                    if write_frame(&mut *port, &req, &mut last_write) {
                        break 'io true;
                    }
                }
            }

            // Poll the meter that applies right now, at ~5 Hz: the SWR while
            // keyed, the rig's S-meter while receiving. Both ride the same
            // command on CI-V and only one of them is meaningful at a time, so
            // they take turns rather than sharing the bus.
            if Instant::now() >= next_meter {
                next_meter = Instant::now() + Duration::from_millis(200);
                let reqs = if ptt {
                    protocol.tx_telemetry_requests()
                } else {
                    protocol.rx_telemetry_requests()
                };
                for req in reqs {
                    if write_frame(&mut *port, &req, &mut last_write) {
                        break 'io true;
                    }
                }
            }

            // Read whatever arrived; parse and emit updates.
            match port.read(&mut read_buf) {
                Ok(0) => {}
                Ok(n) => {
                    rx.extend_from_slice(&read_buf[..n]);
                    let mut updates = protocol.parse(&mut rx);
                    // A reply can teach the framing how this particular rig
                    // addresses its frequency (see `Protocol::reframed`). What
                    // we sent before that was refused and the rig never moved,
                    // so the operator's last dial has to go out again — now in
                    // terms the rig accepts.
                    if protocol.reframed()
                        && let Some(hz) = last_sent_freq.take()
                    {
                        pending_freq = Some(hz);
                        freq_deadline = Instant::now();
                        // The frequency in this same batch is where the refused
                        // set left the rig — not somewhere the operator asked
                        // to be. Reporting it would walk the app's dial back to
                        // it for the moment before the re-issue lands.
                        updates.retain(|u| !matches!(u, CatUpdate::Freq(_)));
                    }
                    // A refusal on its own says nothing — rigs answer that way
                    // for every sub-command they don't have, and the offsets
                    // cleared at open collect a few. One arriving on the heels
                    // of a key-down is worth saying out loud: the operator is
                    // looking at a transmitter that did not key, with no other
                    // sign of why.
                    if protocol.refused()
                        && ptt_written.is_some_and(|t| t.elapsed() < Duration::from_millis(500))
                    {
                        ptt_written = None;
                        warn!(
                            "the radio refused a command at key-down — if it did not transmit, \
                             check its CI-V settings, or the PTT method in Settings → Radio"
                        );
                    }
                    for u in updates {
                        // The meters are telemetry, not control changes: they go
                        // to their own channels and skip the freq/mode dedup
                        // below — a reading that repeats is still current, and
                        // dropping it would freeze the meter.
                        if let CatUpdate::Swr(v) = u {
                            let _ = telem_tx.send(TxTelemetry { fwd_w: None, swr: Some(v) });
                            continue;
                        }
                        if let CatUpdate::Signal(dbm) = u {
                            let _ = signal_tx.send(dbm);
                            continue;
                        }
                        // Forward only genuine changes (poll repeats otherwise).
                        let changed = match u {
                            CatUpdate::Freq(hz) => {
                                let c = emit_freq.map(|f| (f - hz).abs() >= 1.0).unwrap_or(true);
                                if c {
                                    emit_freq = Some(hz);
                                }
                                c
                            }
                            CatUpdate::Mode(m) => {
                                // Also where the rig's mode is learned: what it
                                // reports is the truth about what it is in, and
                                // anything that isn't what we last set means the
                                // next mode command has to go out for real.
                                mode_memory.reported(&protocol.set_mode(m));
                                let c = emit_mode != Some(m);
                                if c {
                                    emit_mode = Some(m);
                                }
                                c
                            }
                            // Both meters are handled above.
                            CatUpdate::Swr(_) | CatUpdate::Signal(_) => false,
                        };
                        if changed {
                            let _ = event_tx.send(u);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    warn!("CAT read error: {e}");
                    break 'io true;
                }
            }

            std::thread::sleep(Duration::from_millis(5));
        };

        if broke {
            warn!("CAT link error; reconnecting");
            std::thread::sleep(Duration::from_secs(1));
        } else {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdroxide_types::CatFamily;

    fn icom() -> Box<dyn Protocol> {
        make_protocol(&CatConfig {
            family: CatFamily::Icom,
            icom_radio_id: 0x94,
            ..CatConfig::default()
        })
    }

    /// The rig's mode is asserted on every key-down. Writing it every time is
    /// what this guards against: the rig acts on each one — re-selecting its
    /// filter, and busy for as long as it takes — with the PTT frame right
    /// behind it.
    #[test]
    fn the_mode_is_only_commanded_when_it_would_change_something() {
        let mut p = icom();
        let mut m = ModeMemory::default();
        // Nothing is known about the rig yet, so the mode goes out.
        assert!(m.needs(&p.set_mode(Mode::Usb)));
        // Asserting the same mode again — every subsequent key-down — does not.
        assert!(!m.needs(&p.set_mode(Mode::Usb)));
        // DIGU is USB on the wire for this family, so it is not a change either.
        assert!(!m.needs(&p.set_mode(Mode::Digu)));
        // A mode that really is different is written.
        assert!(m.needs(&p.set_mode(Mode::Cw)));
    }

    #[test]
    fn what_the_rig_reports_is_where_the_rig_is() {
        let mut p = icom();
        let mut m = ModeMemory::default();
        assert!(m.needs(&p.set_mode(Mode::Cw)));
        // The operator turns the mode knob on the radio itself. The app follows
        // it there, and commanding it back onto a mode it is already in is
        // exactly the wasted write this avoids.
        m.reported(&p.set_mode(Mode::Lsb));
        assert!(!m.needs(&p.set_mode(Mode::Lsb)));
        // And a mode the rig is *not* in still goes out.
        assert!(m.needs(&p.set_mode(Mode::Usb)));
    }
}
