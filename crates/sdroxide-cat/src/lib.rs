//! Serial CAT control for non-SoapySDR rigs (Icom CI-V / Yaesu / Xiegu).
//!
//! NATIVE ONLY — links `serialport`; must never be a dependency of any
//! wasm-targeted crate. The rest of the app talks to it only through the
//! opaque [`CatHandle`] (a background serial thread), so no serial types leak
//! into the engine or UI.

/// Icom CI-V framing and parsing. Public because the Icom LAN backend tunnels
/// the same protocol over UDP and must not carry a second copy of it.
pub mod civ;
mod elecraft;
mod kenwood;
mod rigctld;
mod yaesu;

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use sdroxide_types::{
    CatConfig, CatFamily, CwKeying, DigiMode, LineState, Mode, ModeControl, Parity, PttMethod,
    StopBits, TxTelemetry,
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
    /// The transmit power the rig is set to, as a `0..1` fraction of its
    /// maximum. Read once when the port opens, so the panel's Drive slider ends
    /// up where the radio's own power control already is.
    Power(f32),
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

    /// Set the rig's own receive filter, from audio-band edges in Hz either
    /// side of the dial.
    ///
    /// The mode is passed because no family expresses a filter without one: the
    /// same 500 Hz is one index in CW and a different one in SSB, and some rigs
    /// take a width in CW but a pair of slope frequencies in SSB. What each
    /// family can express varies too, so a protocol that cannot say this
    /// particular passband on this particular rig returns nothing — leaving the
    /// radio's filter where the operator last put it, which is a far better
    /// answer than a guess at an index.
    fn set_filter(&mut self, _mode: Mode, _lo_hz: f32, _hi_hz: f32) -> Vec<Vec<u8>> {
        Vec::new()
    }
    /// Whether [`Protocol::set_filter`] reaches this family at all.
    fn commands_filter(&self) -> bool {
        false
    }

    /// Set the transmitter's output power, as a `0..1` fraction of what the rig
    /// can do.
    ///
    /// This is the only transmit level a CAT rig has that means anything in
    /// *every* mode. The level of the audio we put into its sound card is not
    /// one: a rig in CW keys its own transmitter from its own keyer and never
    /// looks at the sound card, and one asked to TUNE holds a carrier the audio
    /// has no part in either. Empty for families with no such command, where
    /// there is nothing but the audio.
    fn set_power(&mut self, _frac: f32) -> Vec<Vec<u8>> {
        Vec::new()
    }
    /// Frames asking what the rig's power is set to, sent once when the port
    /// opens so the panel can *adopt* the rig's own level instead of imposing a
    /// remembered one on it. Empty for families with no such read.
    fn read_power(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }
    /// Whether [`Protocol::set_power`] reaches this family at all.
    fn commands_power(&self) -> bool {
        false
    }

    /// Whether a mode change can move this rig's *dial*.
    ///
    /// Most radios keep the VFO where it is when the mode changes. Some can be
    /// set not to: they shift the displayed frequency by the CW pitch when
    /// entering or leaving CW, so that zero-beat stays zero-beat, and the
    /// operator's 14.050 becomes 14.050 6 without anything having asked for it.
    /// A protocol that answers true has the frequency re-asserted behind every
    /// mode command it writes — which is what the radio's own documentation
    /// tells applications to do.
    fn mode_moves_dial(&self) -> bool {
        false
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
    /// The `1A` sub-command that switches DATA mode on this model, or `None`
    /// where the model has none — see [`civ::set_mode_frames`].
    data_sub: Option<u8>,
    /// The rig answered "NG" since this was last read (see
    /// [`Protocol::refused`]).
    nak: bool,
}

impl Protocol for Civ {
    fn set_freq(&mut self, hz: f64) -> Vec<u8> {
        civ::set_freq_frame(self.radio, hz)
    }
    fn set_mode(&mut self, m: Mode) -> Vec<u8> {
        civ::set_mode_frames(self.radio, m, self.data_sub).concat()
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
    fn set_filter(&mut self, mode: Mode, lo_hz: f32, hi_hz: f32) -> Vec<Vec<u8>> {
        civ::set_filter_frame(self.radio, mode, lo_hz, hi_hz).into_iter().collect()
    }
    fn commands_filter(&self) -> bool {
        true
    }
    fn set_power(&mut self, frac: f32) -> Vec<Vec<u8>> {
        vec![civ::set_power_frame(self.radio, frac)]
    }
    fn read_power(&self) -> Vec<Vec<u8>> {
        vec![civ::read_power_frame(self.radio)]
    }
    fn commands_power(&self) -> bool {
        true
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
                // Level read (0x14): only the transmit power (0x0A) is asked
                // for, and only when the port opens.
                0x14 => {
                    if let Some(frac) = civ::parse_power_reply(&reply.data) {
                        out.push(CatUpdate::Power(frac));
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

/// Output power Yaesu and Kenwood rigs are assumed to have at full scale, in
/// watts.
///
/// Both families' `PC` sets a *number of watts*, and neither has a command that
/// says how many the rig has — so a 0..1 fraction can only be turned into one
/// against an assumption. A hundred watts is what all but a handful of the rigs
/// these two dialects cover put out (the FT-891, FT-991A, FTDX10, FTDX101D,
/// FT-710, TS-590, TS-2000, TS-890 are all 100 W), and the exceptions are the
/// *bigger* ones — an FTDX101MP or a TS-480HX simply tops out at half the
/// slider. Erring low is the right way to be wrong about a transmitter.
///
/// The other two families need none of this. Icom's power level spans whatever
/// the radio has, so the fraction is the setting; and Elecraft, whose rigs run
/// from a 12 W KX2 to a 110 W K3, has an `OM` query that says which one is on
/// the other end of the cable.
const ASCII_FULL_POWER_W: f32 = 100.0;

/// Where to put the dial back after a mode change moved it, or `None` when
/// there is nothing to put back.
///
/// What we last *set* comes first: that is where the operator asked to be, and
/// it is right even if the rig has since shifted itself somewhere else. Only
/// when nothing has been set — a session that has done nothing but adopt the
/// radio's own dial — does the last frequency the rig *reported* stand in, and
/// it is the pre-shift one, because the reply carrying the shifted frequency
/// cannot have arrived before the mode command that caused it went out.
fn dial_to_restore(moves: bool, last_sent: Option<f64>, reported: Option<f64>) -> Option<f64> {
    moves.then(|| last_sent.or(reported)).flatten()
}

/// Piecewise-linear interpolation over a rising calibration table, clamped to
/// its ends.
///
/// Every meter a rig reports arrives as a number on a scale its manufacturer
/// chose and only ever published as a handful of points — "reading 130 is S9",
/// "reading 89 is 2:1" — so turning one into a decibel or a ratio is always
/// this: find the segment, interpolate, and refuse to extrapolate past either
/// end, where the curve is not just unknown but often not monotonic.
pub(crate) fn interp(cal: &[(f32, f32)], x: f32) -> f32 {
    if x <= cal[0].0 {
        return cal[0].1;
    }
    for w in cal.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if x <= x1 {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    cal[cal.len() - 1].1
}

/// dBm of an S9 signal on HF, the reference every S-meter curve here is
/// expressed against.
pub(crate) const S9_DBM: f32 = -73.0;

/// `PC` — set output power (Yaesu "new CAT" and Kenwood alike). The three-digit
/// field is in watts, and the families' documented minimum is 5 W: a rig cannot
/// be asked for less, so the bottom of the slider is as low as it goes.
fn pc_set_frame(frac: f32) -> Vec<u8> {
    let w = (frac.clamp(0.0, 1.0) * ASCII_FULL_POWER_W).round().clamp(5.0, 999.0) as u32;
    format!("PC{w:03};").into_bytes()
}

/// `PC;` — ask what the power is set to.
fn pc_read_frame() -> Vec<u8> {
    b"PC;".to_vec()
}

/// The payload of a `PC` reply (the digits after `PC`) as a 0..1 fraction, or
/// `None` when it isn't a number.
fn pc_parse(rest: &str) -> Option<f32> {
    let w: u32 = rest.trim().parse().ok()?;
    Some((w as f32 / ASCII_FULL_POWER_W).clamp(0.0, 1.0))
}

fn make_protocol(cfg: &CatConfig) -> Box<dyn Protocol> {
    match cfg.family {
        // A Xiegu speaks the dialect but is not an Icom: none of the model
        // table applies to it, so it gets the plain mode command.
        CatFamily::Xiegu => Box::new(Civ { radio: cfg.icom_radio_id, data_sub: None, nak: false }),
        CatFamily::Icom => Box::new(Civ {
            radio: cfg.icom_radio_id,
            data_sub: cfg.icom_model.data_mode_sub(),
            nak: false,
        }),
        CatFamily::Yaesu => Box::new(yaesu::Yaesu::new()),
        CatFamily::Kenwood => Box::new(kenwood::Kenwood::new(cfg.kenwood_send)),
        CatFamily::Elecraft => Box::new(elecraft::Elecraft::new()),
        CatFamily::Rigctld => Box::new(rigctld::Rigctld::new()),
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
    /// Output power as a 0..1 fraction of the rig's maximum.
    Power(f32),
    /// The rig's own receive filter: the mode it applies to, and the audio-band
    /// edges in Hz.
    Filter(Mode, f32, f32),
    Stop,
}

/// Opaque handle to the running serial thread.
pub struct CatHandle {
    cmd_tx: Sender<CatCmd>,
    event_rx: Receiver<CatUpdate>,
    telem_rx: Receiver<TxTelemetry>,
    signal_rx: Receiver<f32>,
    cw_chunk_len: usize,
    commands_power: bool,
    commands_filter: bool,
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
    /// Set the transmitter's output power, as a `0..1` fraction of what the rig
    /// can do. Silently ignored on a family with no power command, where the
    /// level of the audio going into the rig is the only control there is.
    pub fn set_power(&self, frac: f32) {
        if !self.commands_power {
            return;
        }
        let _ = self.cmd_tx.send(CatCmd::Power(frac));
    }
    /// Whether [`Self::set_power`] reaches this rig.
    pub fn commands_power(&self) -> bool {
        self.commands_power
    }
    /// Set the rig's own receive filter to the passband the operator chose,
    /// as audio-band edges in Hz. Silently ignored on a family — or a model —
    /// whose filter sdroxide cannot address.
    pub fn set_filter(&self, mode: Mode, lo_hz: f32, hi_hz: f32) {
        if !self.commands_filter {
            return;
        }
        let _ = self.cmd_tx.send(CatCmd::Filter(mode, lo_hz, hi_hz));
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
    let mut port = open_link(cfg).ok()?;
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
                        // Neither meter nor the power is requested during the
                        // startup query.
                        CatUpdate::Swr(_) | CatUpdate::Signal(_) | CatUpdate::Power(_) => {}
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
    // Same reason: the engine asks whether this rig's power can be commanded
    // before it commands anything.
    let commands_power = make_protocol(&cfg).commands_power();
    let commands_filter = make_protocol(&cfg).commands_filter();
    std::thread::Builder::new()
        .name("sdroxide-cat".into())
        .spawn(move || serial_thread(cfg, cmd_rx, event_tx, telem_tx, signal_tx))
        .expect("spawn cat thread");
    CatHandle {
        cmd_tx,
        event_rx,
        telem_rx,
        signal_rx,
        cw_chunk_len,
        commands_power,
        commands_filter,
    }
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

/// Shortest gap between two output-power writes. A slider drag is worth one
/// frame every so often, not one per pixel; a hundred milliseconds is faster
/// than anyone can read a wattmeter and slow enough that the queue in front of
/// the next PTT stays empty.
const POWER_GAP: Duration = Duration::from_millis(100);

/// Shortest gap between two receive-filter writes. Longer than the power's: a
/// filter edge is dragged rather than nudged, several frames can go out per
/// change on some families, and nothing about a receive filter is urgent.
const FILTER_GAP: Duration = Duration::from_millis(250);

/// Write one frame, leaving at least [`FRAME_GAP`] since the last one went out.
/// Returns true on a write error — the caller's signal to reconnect.
fn write_frame(port: &mut dyn Link, frame: &[u8], last_write: &mut Instant) -> bool {
    let since = last_write.elapsed();
    if since < FRAME_GAP {
        std::thread::sleep(FRAME_GAP - since);
    }
    let failed = port.write_all(frame).is_err();
    *last_write = Instant::now();
    failed
}

/// Write an output-power level, unless it is the one the rig was last given.
/// Returns true on a write error — the caller's signal to reconnect.
///
/// Skipping a level already written is what keeps the assertion before every
/// key-down free: the rig is served one frame at a time, and a needless one in
/// front of a PTT delays the transmitter coming up.
fn write_power(
    port: &mut dyn Link,
    protocol: &mut dyn Protocol,
    frac: f32,
    last_sent: &mut Option<f32>,
    last_write: &mut Instant,
) -> bool {
    if *last_sent == Some(frac) {
        return false;
    }
    let mut failed = false;
    for f in protocol.set_power(frac) {
        failed |= write_frame(port, &f, last_write);
    }
    if !failed {
        *last_sent = Some(frac);
    }
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

/// The link to the radio: a serial port, or a TCP connection to a `rigctld`.
///
/// Everything above this point is bytes in and bytes out, which is why the
/// network case fits at all — a rigctld speaks a line protocol over a socket
/// exactly as a transceiver speaks a frame protocol over a wire, and the
/// driver's rate limiting, coalescing and reconnection all mean the same thing
/// on both. The one thing that does not carry over is the pair of control
/// lines, which a socket simply does not have.
trait Link: Send {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()>;
    fn flush(&mut self) -> std::io::Result<()>;
    /// Drive RTS, where there is one. A network link has none, so a `PTT
    /// method` of RTS or DTR keys nothing there — see [`PttMethod`].
    fn set_rts(&mut self, _on: bool) {}
    fn set_dtr(&mut self, _on: bool) {}
}

impl Link for Box<dyn serialport::SerialPort> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(self, buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        std::io::Write::write_all(self, buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self)
    }
    fn set_rts(&mut self, on: bool) {
        let _ = self.write_request_to_send(on);
    }
    fn set_dtr(&mut self, on: bool) {
        let _ = self.write_data_terminal_ready(on);
    }
}

impl Link for std::net::TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(self, buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        std::io::Write::write_all(self, buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self)
    }
}

/// How long a read waits before giving the loop its turn back. The same on both
/// transports: the driver polls, so a read that blocks is a driver that cannot
/// write.
const READ_TIMEOUT: Duration = Duration::from_millis(50);

/// Open whichever link this configuration describes.
fn open_link(cfg: &CatConfig) -> std::io::Result<Box<dyn Link>> {
    if cfg.family == CatFamily::Rigctld {
        let stream = std::net::TcpStream::connect(cfg.rigctld_addr.trim())?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        // Every command here is a handful of bytes that wants to be on the wire
        // now: a key-down waiting on Nagle for another 40 ms is an over that
        // starts late.
        let _ = stream.set_nodelay(true);
        return Ok(Box::new(stream));
    }
    let s = &cfg.serial;
    let port = serialport::new(&s.path, s.baud)
        .data_bits(map_data_bits(s.data_bits))
        .parity(map_parity(s.parity))
        .stop_bits(map_stop(s.stop_bits))
        .timeout(READ_TIMEOUT)
        .open()
        .map_err(std::io::Error::other)?;
    Ok(Box::new(port))
}

/// What to call this link in the log — a port and a baud rate, or a host.
pub fn link_label(cfg: &CatConfig) -> String {
    match cfg.family {
        CatFamily::Rigctld => format!("rigctld at {}", cfg.rigctld_addr.trim()),
        _ => format!("{} at {} baud", cfg.serial.path, cfg.serial.baud),
    }
}

/// Apply a forced control-line level (ignored when `LineState::None`). If a
/// line is used for PTT, PTT owns it instead (handled in the loop).
fn apply_line(port: &mut dyn Link, forced: LineState, rts: bool) {
    let level = match forced {
        LineState::None => return,
        LineState::High => true,
        LineState::Low => false,
    };
    if rts { port.set_rts(level) } else { port.set_dtr(level) }
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
    //
    // `digi_mode` is a choice between two *sidebands* — plain USB or the rig's
    // DATA-U position — so it only makes sense for a mode that rides a
    // sideband. The carrier-centred modes (RIFP, VHF packet) frequency-modulate
    // the carrier instead: sending them as USB puts the rig in the wrong
    // modulation entirely, and nothing downstream would say so. Those fall
    // through to `mode_control`, where each protocol's own map answers DATA-FM.
    let mode_cmd = |app_mode: Mode| -> Option<Mode> {
        if app_mode.is_digital() && !app_mode.is_sstv() && !app_mode.is_carrier_centered() {
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
        let mut port = match open_link(&cfg) {
            Ok(p) => {
                // The PTT method belongs in this line: a rig that answers every
                // read and still refuses to key is nearly always one being asked
                // to key some way it isn't set up for, and this is where that
                // shows.
                info!(
                    link = %link_label(&cfg),
                    family = cfg.family.label(),
                    ptt = cfg.ptt.label(),
                    "CAT link open"
                );
                p
            }
            Err(e) => {
                warn!(link = %link_label(&cfg), "CAT open failed: {e}");
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
            PttMethod::Rts => port.set_rts(false),
            PttMethod::Dtr => port.set_dtr(false),
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
        // The transmit power is the rig's, not ours: ask what it is set to and
        // let the panel adopt it, the same way the dial and the mode are
        // adopted. Imposing a remembered level instead would put an operator
        // who has never touched the slider on the air at whatever it defaults
        // to — on a radio they had already set the power on.
        for f in protocol.read_power() {
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
        // Output power, coalesced and rate-limited exactly as the frequency is:
        // dragging the Drive slider produces a command per pixel, and a rig
        // whose control port is served one frame at a time must not be handed
        // hundreds of them — the PTT behind that queue is the thing that would
        // suffer. Only a level that differs from the last one written goes out,
        // so the assertion before every key-down is free once it has settled.
        let mut pending_power: Option<f32> = None;
        let mut last_sent_power: Option<f32> = None;
        let mut power_deadline = Instant::now();
        // The rig's own receive filter, on the same rate limit and the same
        // only-on-change rule. Unlike the power it is never asserted before a
        // key-down: it is a receive setting, and the one moment it must not
        // compete for the bus is the moment the transmitter is coming up.
        let mut pending_filter: Option<(Mode, f32, f32)> = None;
        let mut last_sent_filter: Option<(Mode, f32, f32)> = None;
        let mut filter_deadline = Instant::now();
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
                            if mode_memory.needs(&f) {
                                if write_frame(&mut *port, &f, &mut last_write) {
                                    break 'io true;
                                }
                                // On a rig that shifts its VFO with the mode,
                                // that command has just moved the dial out from
                                // under the operator. Put it back — before the
                                // next poll reads the shifted frequency and
                                // walks the app's dial to it, and before any
                                // key-down, which is the moment being a pitch
                                // off frequency actually costs something.
                                if let Some(hz) = dial_to_restore(
                                    protocol.mode_moves_dial(),
                                    last_sent_freq,
                                    emit_freq,
                                ) {
                                    last_sent_freq = None; // past the dedup
                                    pending_freq = Some(hz);
                                    freq_deadline = Instant::now();
                                }
                            }
                        }
                    }
                    Ok(CatCmd::Ptt(on)) => {
                        // Key-down has to land at the level the engine asserted
                        // for it — the drive, or the tune level under TUNE — so
                        // the rate limit below is not allowed to leave that
                        // sitting in the queue while the transmitter comes up.
                        if on
                            && let Some(frac) = pending_power.take()
                            && write_power(
                                &mut *port,
                                &mut *protocol,
                                frac,
                                &mut last_sent_power,
                                &mut last_write,
                            )
                        {
                            break 'io true;
                        }
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
                            PttMethod::Rts => {
                                port.set_rts(on);
                                false
                            }
                            PttMethod::Dtr => {
                                port.set_dtr(on);
                                false
                            }
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
                        // Nothing else asserts the level for this over: the rig
                        // keys itself, so there is no PTT here to hang it off —
                        // and CW is the mode where the rig's power is the only
                        // transmit control there is, the sound card having no
                        // part in it at all.
                        if let Some(frac) = pending_power.take()
                            && write_power(
                                &mut *port,
                                &mut *protocol,
                                frac,
                                &mut last_sent_power,
                                &mut last_write,
                            )
                        {
                            break 'io true;
                        }
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
                    // Coalesced exactly as the power is, and for the same
                    // reason: dragging a filter edge produces a command per
                    // pixel, and a rig served one frame at a time must not be
                    // handed hundreds of them.
                    Ok(CatCmd::Filter(m, lo, hi)) => pending_filter = Some((m, lo, hi)),
                    Ok(CatCmd::Power(frac)) => pending_power = Some(frac), // coalesce
                    Ok(CatCmd::Stop) => return,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            // Rate-limited power write (only on change), on the same principle
            // as the frequency below it.
            if let Some(frac) = pending_power {
                let now = Instant::now();
                if now >= power_deadline {
                    pending_power = None;
                    power_deadline = now + POWER_GAP;
                    if write_power(
                        &mut *port,
                        &mut *protocol,
                        frac,
                        &mut last_sent_power,
                        &mut last_write,
                    ) {
                        break 'io true;
                    }
                }
            }

            // Rate-limited filter write, on the same principle — and skipped
            // entirely while transmitting, where a receive setting has nothing
            // to do and the bus belongs to the meter.
            if let Some(f) = pending_filter
                && !ptt
            {
                let now = Instant::now();
                if now >= filter_deadline {
                    pending_filter = None;
                    filter_deadline = now + FILTER_GAP;
                    if last_sent_filter != Some(f) {
                        let mut failed = false;
                        for frame in protocol.set_filter(f.0, f.1, f.2) {
                            failed |= write_frame(&mut *port, &frame, &mut last_write);
                        }
                        if failed {
                            break 'io true;
                        }
                        last_sent_filter = Some(f);
                    }
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
                        // The power the rig reports goes straight out: it is
                        // asked for once per connection, and the engine adopts
                        // it without answering, so there is nothing here to
                        // dedup and no loop to break. Recording it as sent is
                        // what makes the adoption free — the engine asserts the
                        // level it adopted before the first key-down, and that
                        // is now a level the rig is already on.
                        if let CatUpdate::Power(frac) = u {
                            last_sent_power = Some(frac);
                            let _ = event_tx.send(u);
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
                            // Both meters and the power are handled above.
                            CatUpdate::Swr(_) | CatUpdate::Signal(_) | CatUpdate::Power(_) => false,
                        };
                        if changed {
                            let _ = event_tx.send(u);
                        }
                    }
                }
                // A read that found nothing in its window. Which of the two
                // kinds arrives is the transport's business — a serial port
                // times out, a socket would block — and neither is an error.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
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
            icom_model: sdroxide_types::IcomModel::Ic7300,
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

    /// A mode change on an Elecraft can shift the dial by the CW pitch, so the
    /// frequency is re-asserted behind every mode command. Which frequency is
    /// the whole question: the operator's, not the rig's.
    #[test]
    fn the_dial_put_back_after_a_mode_change_is_the_one_that_was_asked_for() {
        // What we last set wins — that is where the operator asked to be.
        assert_eq!(
            dial_to_restore(true, Some(14_050_000.0), Some(14_050_600.0)),
            Some(14_050_000.0)
        );
        // With nothing set this session, the last frequency the rig reported
        // stands in. It is the pre-shift one: the reply carrying the shifted
        // frequency cannot have arrived before the mode command that caused it.
        assert_eq!(dial_to_restore(true, None, Some(7_030_000.0)), Some(7_030_000.0));
        // Nothing known at all is nothing to put back.
        assert_eq!(dial_to_restore(true, None, None), None);
        // And a family whose dial does not move is not written to at all — a
        // needless frame in front of the next key-down is exactly what the
        // rate limiting elsewhere in this file exists to avoid.
        assert_eq!(dial_to_restore(false, Some(14_050_000.0), Some(14_050_600.0)), None);
    }

    /// Only the family whose radios document the behaviour asks for it.
    #[test]
    fn only_elecraft_re_asserts_the_dial_after_a_mode_change() {
        let family = |f| make_protocol(&CatConfig { family: f, ..CatConfig::default() });
        assert!(family(CatFamily::Elecraft).mode_moves_dial());
        for f in [CatFamily::Icom, CatFamily::Xiegu, CatFamily::Yaesu, CatFamily::Kenwood] {
            assert!(!family(f).mode_moves_dial(), "{f:?}");
        }
    }

    /// `PC` is watts, and the families' documented floor is 5 W — a rig cannot
    /// be asked for nothing, so the bottom of the slider is as low as it goes
    /// rather than a `PC000;` the radio would reject outright.
    #[test]
    fn the_ascii_families_send_power_as_three_digits_of_watts() {
        let sent = |frac: f32| String::from_utf8(pc_set_frame(frac)).unwrap();
        assert_eq!(sent(1.0), "PC100;");
        assert_eq!(sent(0.5), "PC050;");
        assert_eq!(sent(0.05), "PC005;");
        assert_eq!(sent(0.0), "PC005;");
        // A slider cannot ask for more than the assumed full scale.
        assert_eq!(sent(2.0), "PC100;");
    }

    #[test]
    fn a_pc_reply_reads_back_as_the_fraction_that_set_it() {
        assert_eq!(pc_parse("100"), Some(1.0));
        assert_eq!(pc_parse("050"), Some(0.5));
        assert_eq!(pc_parse("005"), Some(0.05));
        // A rig that puts out more than the assumed full scale reports more
        // watts than we would ever ask for; the slider still tops out at 1.
        assert_eq!(pc_parse("200"), Some(1.0));
        // Not a number is not a power.
        assert_eq!(pc_parse(""), None);
        assert_eq!(pc_parse("abc"), None);
    }
}
