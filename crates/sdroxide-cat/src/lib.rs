//! Serial CAT control for non-SoapySDR rigs (Icom CI-V / Yaesu / Xiegu).
//!
//! NATIVE ONLY — links `serialport`; must never be a dependency of any
//! wasm-targeted crate. The rest of the app talks to it only through the
//! opaque [`CatHandle`] (a background serial thread), so no serial types leak
//! into the engine or UI.

mod civ;
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

    fn parse(&mut self, buf: &mut Vec<u8>) -> Vec<CatUpdate>;
}

/// CI-V protocol (Icom + Xiegu). `radio` is the CI-V transceiver address.
struct Civ {
    radio: u8,
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
                // Meter read (0x15): we only request the SWR sub-meter (0x12).
                0x15 => {
                    if let Some(swr) = civ::parse_swr_reply(&reply.data) {
                        out.push(CatUpdate::Swr(swr));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

fn make_protocol(cfg: &CatConfig) -> Box<dyn Protocol> {
    match cfg.family {
        CatFamily::Xiegu | CatFamily::Icom => Box::new(Civ { radio: cfg.icom_radio_id }),
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
                        CatUpdate::Swr(_) => {} // not requested during startup query
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
    // Asked of the framing before it goes to the thread, so the keyer can size
    // its chunks to the rig without reaching across the channel to find out.
    let cw_chunk_len =
        if cfg.cw_keying == CwKeying::Cat { make_protocol(&cfg).cw_chunk_len() } else { 0 };
    std::thread::Builder::new()
        .name("sdroxide-cat".into())
        .spawn(move || serial_thread(cfg, cmd_rx, event_tx, telem_tx))
        .expect("spawn cat thread");
    CatHandle { cmd_tx, event_rx, telem_rx, cw_chunk_len }
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
                info!(path = %cfg.serial.path, baud = cfg.serial.baud, "CAT port open");
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
        // Don't force a mode on connect — adopt the rig's current mode (read via
        // `query_once`/poll); the app commands mode only when the operator picks one.
        // RIT/XIT/split are the exception: those we do own, so clear the rig's
        // own copies rather than let them offset us invisibly.
        for f in protocol.clear_offsets() {
            let _ = port.write_all(&f);
        }

        let mut rx = Vec::with_capacity(256);
        let mut read_buf = [0u8; 256];
        let mut next_poll = Instant::now();
        // TX telemetry (SWR) is polled faster than freq/mode, but only while keyed.
        let mut next_telem = Instant::now();
        let mut ptt = false;
        let mut pending_freq: Option<f64> = None;
        let mut last_sent_freq: Option<f64> = None;
        let mut freq_deadline = Instant::now();
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
                            if port.write_all(&protocol.set_mode(mm)).is_err() {
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
                            if port.write_all(&protocol.set_freq(hz)).is_err() {
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
                            PttMethod::Cat => port.write_all(&protocol.ptt(on)).is_err(),
                        };
                        if failed {
                            break 'io true;
                        }
                        ptt = on;
                        if on {
                            next_telem = Instant::now(); // start polling SWR at once
                        } else {
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
                            if port.write_all(&f).is_err() {
                                break 'io true;
                            }
                        }
                    }
                    Ok(CatCmd::CwAbort) => {
                        for f in protocol.abort_cw() {
                            if port.write_all(&f).is_err() {
                                break 'io true;
                            }
                        }
                    }
                    Ok(CatCmd::CwWpm(wpm)) => {
                        for f in protocol.set_cw_wpm(wpm) {
                            if port.write_all(&f).is_err() {
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
                    if port.write_all(&protocol.set_freq(hz)).is_err() {
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
                    if port.write_all(&req).is_err() {
                        break 'io true;
                    }
                }
            }

            // While keyed, poll TX telemetry (SWR) at ~5 Hz.
            if ptt && Instant::now() >= next_telem {
                next_telem = Instant::now() + Duration::from_millis(200);
                for req in protocol.tx_telemetry_requests() {
                    if port.write_all(&req).is_err() {
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
                    for u in updates {
                        // SWR is telemetry, not a control change: route it to the
                        // telemetry channel and skip the freq/mode dedup below.
                        if let CatUpdate::Swr(v) = u {
                            let _ = telem_tx.send(TxTelemetry { fwd_w: None, swr: Some(v) });
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
                                let c = emit_mode != Some(m);
                                if c {
                                    emit_mode = Some(m);
                                }
                                c
                            }
                            CatUpdate::Swr(_) => false, // handled above
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
