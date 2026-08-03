//! The live connection to a Pluto: [`PlutoHandle`], the state its three
//! threads share, and the control thread that owns the AD9361.
//!
//! # Why three connections
//!
//! IIOD is strictly request/response on one socket, and `READBUF` blocks until
//! the device has filled a buffer — 63 ms at the lowest rate offered here. A
//! retune sharing that socket would queue behind it, so the dial would lag the
//! knob by a buffer. `iiod` is a thread-per-connection server, so instead this
//! opens three: control, receive and transmit, each owned by one blocking
//! thread. (`iiod` may still hold a per-device lock, so a retune issued
//! mid-buffer can wait that long inside the server; that is a bounded stall in
//! the right place, not a queue that grows.)
//!
//! # Half duplex, deliberately
//!
//! The AD9361 is a full-duplex part and this backend still transmits half
//! duplex: a Pluto is normally reached over a USB 2.0 Ethernet gadget, and that
//! link will not carry a megasample-per-second stream in both directions at
//! once. Receive is torn down for the length of an over so the whole link is
//! available to transmit — the same trade the HPSDR backend makes.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};

use sdroxide_types::PlutoConfig;

use crate::context::Context;
use crate::error::{Error, Result};
use crate::iiod::Connection;
use crate::phy::Phy;
pub use crate::phy::PlutoLimits;
use crate::stream;
use crate::trace::Trace;

/// How long the TCP handshake may take before the address counts as wrong.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How often a stream thread emits a throughput line (`RUST_LOG=…=debug`).
pub(crate) const STATS_INTERVAL: Duration = Duration::from_secs(2);

/// Transmit buffer length in complex samples. At 2 Msps this is ~2 ms per
/// `WRITEBUF`, short enough that key-up latency is inaudible and long enough
/// that the per-command round trip is not the bottleneck.
pub(crate) const TX_BUFFER_SAMPLES: usize = 4096;

/// Control messages from the [`PlutoHandle`] to its control thread.
pub(crate) enum Ctrl {
    RxFreq(f64),
    RxGain(f64),
    AgcMode(String),
    RxPort(String),
    TxPort(String),
    TxGain(f64),
    /// Reference trim in parts per million, applied in software to every LO we
    /// ask for. The device's own `xo_correction` is a persistent debug
    /// attribute, so writing it would outlive the session and surprise the next
    /// program to open the radio.
    Ppm(f64),
    TxOn(f64),
    TxOff,
    Shutdown,
}

/// State the three threads and the handle share.
pub(crate) struct Shared {
    pub phy: Phy,
    /// Receive buffer should be open. Cleared for the length of an over.
    pub rx_enabled: AtomicBool,
    /// Receive buffer *is* open — the acknowledgement the control thread waits
    /// for before letting transmit have the link.
    pub rx_active: AtomicBool,
    pub tx_enabled: AtomicBool,
    /// Transmit buffer *is* open, the mirror of [`Self::rx_active`]: receive
    /// must not reclaim the link until the transmit buffer has actually been
    /// closed, or the two overlap on a link that has room for one.
    pub tx_active: AtomicBool,
    /// Cleared when any thread gives up, which is what `needs_reopen()` reads.
    pub alive: AtomicBool,
    pub opened_at: Instant,
    /// Milliseconds since [`Self::opened_at`] when the receive thread last
    /// decoded samples, or 0 if it never has. Written by the stream thread
    /// rather than the reader, so a long over is not mistaken for a dead link.
    pub last_rx_ms: AtomicU64,
    pub transmitting: AtomicBool,
    pub buffer_samples: usize,
    pub rate_hz: f64,
    pub trace: Trace,
}

impl Shared {
    pub(crate) fn stamp_rx(&self) {
        self.last_rx_ms.store(self.opened_at.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// Report a thread's fatal error once and take the connection down, so the
    /// engine reopens instead of staring at a stream that will never resume.
    pub(crate) fn die(&self, what: &str, e: &Error) {
        if self.alive.swap(false, Ordering::Relaxed) {
            tracing::warn!("PlutoSDR: {what} stopped: {e}");
            self.trace.note(format!("!! {what} stopped: {e}"));
        }
    }
}

/// A live connection to a Pluto. Dropping it stops streaming and closes all
/// three sockets.
pub struct PlutoHandle {
    ctrl: Sender<Ctrl>,
    rx: Consumer<f32>,
    tx: Producer<f32>,
    joins: Vec<JoinHandle<()>>,
    shared: Arc<Shared>,

    /// Actual RX sample rate in Hz, after the hardware rounded the request.
    pub sample_rate_hz: f64,
    /// Actual TX sample rate. The AD9361 clocks both paths together, so this is
    /// normally the same number — read back rather than assumed.
    pub tx_rate_hz: f64,
    /// Analog filter bandwidth actually set, in Hz. The engine's LO-offset
    /// policy is decided against this.
    pub rf_bandwidth_hz: f64,
    /// What the device says it can do.
    pub limits: PlutoLimits,
    pub model: String,
    pub firmware: String,
    pub serial: String,
    /// Where it was reached, for labels and errors.
    pub addr: SocketAddr,

    rx_gain_db: f64,
    tx_gain_db: f64,
    rx_port: String,
    tx_port: String,
    /// A sentence for `IqSource::open_status`, or `None` when it came up clean.
    open_status: Option<String>,
    released: bool,
}

impl PlutoHandle {
    /// Open `address` (`host[:port]`), configure the front end from `cfg`, and
    /// start receiving at `center_hz`.
    pub fn open(address: &str, cfg: &PlutoConfig, center_hz: f64) -> Result<PlutoHandle> {
        let trace = Trace::new();
        crate::trace::remember(&trace);
        let addr = resolve(address)?;
        trace.note(format!("opening {addr} (from {address:?})"));
        tracing::info!(
            "PlutoSDR: opening {addr}, requested {:.3} Msps at {:.6} MHz",
            cfg.sample_rate_hz / 1e6,
            center_hz / 1e6
        );

        let mut control = Connection::connect(addr, CONNECT_TIMEOUT, trace.clone())?;
        let version = control.version()?;
        let xml = control.print_xml()?;
        let ctx = Context::parse(&xml)?;
        trace.set_context(ctx.summary());
        tracing::info!(
            "PlutoSDR: {addr} is \"{}\" firmware {} (iiod {version})",
            ctx.hw_model(),
            ctx.fw_version()
        );
        let phy = Phy::probe(&mut control, &ctx, &addr.to_string())?;

        // Order matters. The rate sets the clock tree, the bandwidth is chosen
        // against the rate, and the receive gain only takes effect once the AGC
        // is in manual — so each step depends on the one before it.
        let rate = phy.set_sample_rate(&mut control, cfg.sample_rate_hz)?;
        let want_bw = if cfg.rf_bandwidth_hz > 0.0 {
            cfg.rf_bandwidth_hz
        } else {
            // Wide enough that the engine's quarter-span LO offset still clears
            // the analog filter, which is what keeps the offset from being
            // quietly abandoned. See `sdroxide_radio::lo_offset_for`.
            rate * 0.9
        };
        let bandwidth = phy.set_bandwidth(&mut control, want_bw)?;
        phy.set_agc_mode(&mut control, cfg.agc.iio_name())?;
        phy.set_rx_gain(&mut control, cfg.rx_gain_db)?;
        if !cfg.rx_port.trim().is_empty() {
            phy.set_rx_port(&mut control, cfg.rx_port.trim())?;
        }
        if !cfg.tx_port.trim().is_empty() {
            phy.set_tx_port(&mut control, cfg.tx_port.trim())?;
        }
        // Silence the transmitter first, then set the operator's level.
        //
        // The order is the point. The AD9361 keeps its attenuator setting
        // across a host disconnect, so whatever the last program to touch this
        // Pluto left behind is live from the moment we connect; writing the
        // minimum first means the register is never something nobody in this
        // session chose. The SoapySDR backend does the same thing on open, for
        // the same reason.
        phy.silence_transmitter(&mut control)?;
        phy.set_tx_gain(&mut control, cfg.tx_gain_db)?;
        phy.set_rx_lo(&mut control, PlutoConfig::apply_ppm(center_hz, cfg.ppm))?;
        let tx_rate = phy.tx_sample_rate(&mut control).unwrap_or(rate);
        let rx_port = phy.rx_port(&mut control).unwrap_or_default();
        let tx_port = phy.tx_port(&mut control).unwrap_or_default();

        let mut warnings: Vec<String> = Vec::new();
        if let Some(n) = phy.limits.assumption_notice() {
            warnings.push(n);
        }
        if (rate - cfg.sample_rate_hz).abs() > 1.0 {
            let msg = format!(
                "PlutoSDR: {:.3} Msps requested, {:.3} Msps is what the hardware produced",
                cfg.sample_rate_hz / 1e6,
                rate / 1e6
            );
            tracing::info!("{msg}");
            warnings.push(msg);
        }
        tracing::info!(
            "PlutoSDR: {:.3} Msps, analog filter {:.3} MHz, AGC {}, RX gain {:.1} dB, \
             ports RX {rx_port} / TX {tx_port}",
            rate / 1e6,
            bandwidth / 1e6,
            cfg.agc.iio_name(),
            cfg.rx_gain_db,
        );

        let buffer_samples = cfg.buffer_samples.clamp(1024, 1 << 20);
        let shared = Arc::new(Shared {
            phy: phy.clone(),
            rx_enabled: AtomicBool::new(true),
            rx_active: AtomicBool::new(false),
            tx_enabled: AtomicBool::new(false),
            tx_active: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            opened_at: Instant::now(),
            last_rx_ms: AtomicU64::new(0),
            transmitting: AtomicBool::new(false),
            buffer_samples,
            rate_hz: rate,
            trace: trace.clone(),
        });

        // RX ring ~0.5 s at the RX rate; TX ring ~0.5 s at the TX rate.
        let rx_cap = ((rate * 2.0 * 0.5) as usize).next_power_of_two().max(1 << 16);
        let (rx_prod, rx_cons) = RingBuffer::<f32>::new(rx_cap);
        let tx_cap = ((tx_rate * 2.0 * 0.5) as usize).next_power_of_two().max(1 << 15);
        let (tx_prod, tx_cons) = RingBuffer::<f32>::new(tx_cap);
        tracing::debug!(
            "PlutoSDR: RX ring {rx_cap} floats, TX ring {tx_cap} floats, \
             {buffer_samples}-sample device buffers"
        );

        // The data connections are opened after the control one has proved the
        // device is a Pluto, so a wrong address costs one connection, not three.
        let rx_conn = Connection::connect(addr, CONNECT_TIMEOUT, trace.clone()).map_err(|e| {
            Error::Msg(format!(
                "the receive connection to {addr} was refused ({e}) — `iiod` accepted the \
                 first connection, so this is a per-connection limit rather than a wrong \
                 address"
            ))
        })?;
        let tx_conn = Connection::connect(addr, CONNECT_TIMEOUT, trace.clone()).map_err(|e| {
            Error::Msg(format!("the transmit connection to {addr} was refused ({e})"))
        })?;

        let (ctrl_tx, ctrl_rx) = crossbeam_channel::unbounded();
        let rx_shared = Arc::clone(&shared);
        let tx_shared = Arc::clone(&shared);
        let ctl_shared = Arc::clone(&shared);
        let ctl_cfg = cfg.clone();
        let joins = vec![
            spawn("sdroxide-pluto-rx", move || stream::rx_thread(rx_conn, rx_shared, rx_prod))?,
            spawn("sdroxide-pluto-tx", move || stream::tx_thread(tx_conn, tx_shared, tx_cons))?,
            spawn("sdroxide-pluto-ctl", move || {
                control_thread(control, ctl_shared, ctrl_rx, ctl_cfg, center_hz)
            })?,
        ];

        Ok(PlutoHandle {
            ctrl: ctrl_tx,
            rx: rx_cons,
            tx: tx_prod,
            joins,
            shared,
            sample_rate_hz: rate,
            tx_rate_hz: tx_rate,
            rf_bandwidth_hz: bandwidth,
            limits: phy.limits.clone(),
            model: phy.model.clone(),
            firmware: phy.firmware.clone(),
            serial: phy.serial.clone(),
            addr,
            rx_gain_db: cfg.rx_gain_db,
            tx_gain_db: cfg.tx_gain_db.clamp(phy.limits.tx_gain_db.0, phy.limits.tx_gain_db.1),
            rx_port,
            tx_port,
            open_status: (!warnings.is_empty()).then(|| warnings.join("; ")),
            released: false,
        })
    }

    /// One line naming the radio, for logs and the UI.
    pub fn label(&self) -> String {
        let model = if self.model.is_empty() { "PlutoSDR" } else { self.model.as_str() };
        format!("{model} @ {} ({:.3} Msps)", self.addr.ip(), self.sample_rate_hz / 1e6)
    }

    /// A warning captured while opening, or `None` when it came up clean.
    pub fn open_status(&self) -> Option<String> {
        self.open_status.clone()
    }

    pub fn trace(&self) -> &Trace {
        &self.shared.trace
    }

    pub fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::Relaxed)
    }

    /// Retune the receive local oscillator.
    pub fn set_rx_freq(&self, hz: f64) {
        let _ = self.ctrl.send(Ctrl::RxFreq(hz));
    }

    pub fn rx_gain_db(&self) -> f64 {
        self.rx_gain_db
    }

    pub fn set_rx_gain_db(&mut self, db: f64) {
        let db = db.clamp(self.limits.rx_gain_db.0, self.limits.rx_gain_db.1);
        self.rx_gain_db = db;
        let _ = self.ctrl.send(Ctrl::RxGain(db));
    }

    pub fn tx_gain_db(&self) -> f64 {
        self.tx_gain_db
    }

    /// Transmit gain in dB — negative, because the AD9361 expresses it as
    /// attenuation.
    pub fn set_tx_gain_db(&mut self, db: f64) {
        let db = db.clamp(self.limits.tx_gain_db.0, self.limits.tx_gain_db.1);
        self.tx_gain_db = db;
        let _ = self.ctrl.send(Ctrl::TxGain(db));
    }

    /// Switch the receive AGC mode (`manual`, `slow_attack`, `fast_attack`,
    /// `hybrid`).
    pub fn set_agc_mode(&self, mode: &str) {
        let _ = self.ctrl.send(Ctrl::AgcMode(mode.to_string()));
    }

    /// Reference trim in parts per million, applied to every LO from here on.
    pub fn set_ppm(&self, ppm: f64) {
        let _ = self.ctrl.send(Ctrl::Ppm(ppm));
    }

    pub fn rx_port(&self) -> &str {
        &self.rx_port
    }

    pub fn tx_port(&self) -> &str {
        &self.tx_port
    }

    pub fn set_rx_port(&mut self, port: &str) {
        self.rx_port = port.to_string();
        let _ = self.ctrl.send(Ctrl::RxPort(port.to_string()));
    }

    pub fn set_tx_port(&mut self, port: &str) {
        self.tx_port = port.to_string();
        let _ = self.ctrl.send(Ctrl::TxPort(port.to_string()));
    }

    /// Begin transmitting at `tx_freq_hz`; returns the TX I/Q rate to feed
    /// [`Self::tx_write`].
    pub fn tx_begin(&self, tx_freq_hz: f64) -> f64 {
        tracing::info!(
            "PlutoSDR: TX begin at {tx_freq_hz:.0} Hz ({:.3} Msps I/Q, {:.2} dB)",
            self.tx_rate_hz / 1e6,
            self.tx_gain_db
        );
        self.shared.transmitting.store(true, Ordering::Relaxed);
        let _ = self.ctrl.send(Ctrl::TxOn(tx_freq_hz));
        self.tx_rate_hz
    }

    pub fn tx_end(&self) {
        tracing::info!("PlutoSDR: TX end");
        self.shared.transmitting.store(false, Ordering::Relaxed);
        let _ = self.ctrl.send(Ctrl::TxOff);
    }

    /// Push interleaved I,Q transmit samples. Blocks briefly when the ring is
    /// full (pacing the caller); drops rather than hanging if the transmit
    /// thread has stalled.
    ///
    /// Writes go in whole I/Q pairs. Giving up mid-pair would put every later
    /// sample one slot out of step, so each Q would be encoded as an I — the
    /// wrong sideband for the rest of the over.
    pub fn tx_write(&mut self, iq: &[f32]) {
        for pair in iq.chunks_exact(2) {
            let mut tries = 0u32;
            let mut chunk = loop {
                match self.tx.write_chunk(2) {
                    Ok(c) => break c,
                    Err(_) => {
                        if tries > 2000 {
                            return; // ~200 ms: the thread has stalled
                        }
                        tries += 1;
                        std::thread::sleep(Duration::from_micros(100));
                    }
                }
            };
            let (head, tail) = chunk.as_mut_slices();
            for (slot, &v) in head.iter_mut().chain(tail.iter_mut()).zip(pair) {
                *slot = v;
            }
            chunk.commit_all();
        }
    }

    /// How many transmit floats are still queued, so PTT can be held until the
    /// tail has actually gone out (an FT8 burst needs every symbol).
    pub fn tx_pending(&self) -> usize {
        self.tx.buffer().capacity().saturating_sub(self.tx.slots())
    }

    /// Drain interleaved I,Q floats from the RX ring into `out`. Always returns
    /// an even count, so the stream stays aligned. 0 means nothing yet.
    pub fn rx_read(&mut self, out: &mut [f32]) -> usize {
        let take = self.rx.slots().min(out.len()) & !1;
        let mut n = 0;
        while n < take {
            match self.rx.pop() {
                Ok(v) => {
                    out[n] = v;
                    n += 1;
                }
                Err(_) => break,
            }
        }
        n
    }

    /// Drop whatever the receive thread queued. Receive is torn down for the
    /// length of an over, but a partial buffer can still be sitting in the ring
    /// when it resumes, and replaying it would put a burst of stale audio in
    /// front of the first live sample.
    pub fn discard_pending_rx(&mut self) {
        while self.rx.pop().is_ok() {}
    }

    /// How long the device has gone without delivering samples, measured from
    /// the last buffer decoded or — if none ever arrived — from when the
    /// connection opened. A stream that never starts is the failure that
    /// matters most here, so it ages just like one that stops. Always zero
    /// while transmitting, when receive is deliberately switched off.
    pub fn silent_for(&self) -> Duration {
        if self.shared.transmitting.load(Ordering::Relaxed) {
            return Duration::ZERO;
        }
        let since_open = self.shared.opened_at.elapsed();
        let last = Duration::from_millis(self.shared.last_rx_ms.load(Ordering::Relaxed));
        since_open.saturating_sub(last)
    }

    /// Stop the threads and close all three sockets, ahead of the engine
    /// building this front end's replacement.
    ///
    /// Idempotent, and leaves the handle callable: `rx_read` returns nothing and
    /// `is_alive` returns false, which is what the reopen path expects.
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.shared.alive.store(false, Ordering::Relaxed);
        self.shared.rx_enabled.store(false, Ordering::Relaxed);
        self.shared.tx_enabled.store(false, Ordering::Relaxed);
        let _ = self.ctrl.send(Ctrl::Shutdown);
        for j in self.joins.drain(..) {
            let _ = j.join();
        }
        tracing::debug!("PlutoSDR: released {}", self.addr);
    }
}

impl Drop for PlutoHandle {
    fn drop(&mut self) {
        self.release();
    }
}

fn spawn<F: FnOnce() + Send + 'static>(name: &str, body: F) -> Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(body)
        .map_err(|e| Error::Msg(format!("cannot spawn {name}: {e}")))
}

/// Resolve `host[:port]` to one socket address, preferring IPv4 — a Pluto's USB
/// gadget only has an IPv4 address, and `pluto.local` on a host with IPv6
/// otherwise resolves to something that cannot be reached.
pub(crate) fn resolve(address: &str) -> Result<SocketAddr> {
    let (host, port) = crate::split_addr(address).map_err(Error::Msg)?;
    let mut addrs = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| Error::Unreachable(format!("cannot resolve {host:?}: {e}")))?
        .collect::<Vec<_>>();
    addrs.sort_by_key(|a| !a.is_ipv4());
    addrs
        .into_iter()
        .next()
        .ok_or_else(|| Error::Unreachable(format!("{host:?} resolved to no addresses")))
}

/// Owns the control connection and the AD9361.
///
/// Everything that touches a front-end register happens here, on its own
/// socket, so nothing the operator does — a retune mid-drag, a gain slider —
/// waits behind a buffer read.
fn control_thread(
    mut conn: Connection,
    shared: Arc<Shared>,
    ctrl: Receiver<Ctrl>,
    cfg: PlutoConfig,
    center_hz: f64,
) {
    let phy = &shared.phy;
    let mut ppm = cfg.ppm;
    let mut rx_gain_db = cfg.rx_gain_db;
    // Seeded from where `open` left the oscillator, so a ppm trim made before
    // the operator has touched the dial still moves it.
    let mut rx_hz = center_hz;
    while shared.alive.load(Ordering::Relaxed) {
        let msg = match ctrl.recv_timeout(Duration::from_millis(200)) {
            Ok(m) => m,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        let outcome = match msg {
            Ctrl::RxFreq(hz) => {
                rx_hz = hz;
                phy.set_rx_lo(&mut conn, PlutoConfig::apply_ppm(hz, ppm))
            }
            Ctrl::RxGain(db) => {
                rx_gain_db = db;
                phy.set_rx_gain(&mut conn, db)
            }
            Ctrl::AgcMode(mode) => phy.set_agc_mode(&mut conn, &mode).and_then(|()| {
                // Manual gain is ignored while an attack mode is running, so
                // the value the operator last chose has to be replayed on the
                // way back into manual — otherwise the radio resumes at
                // whatever level the AGC happened to leave behind.
                if mode == "manual" { phy.set_rx_gain(&mut conn, rx_gain_db) } else { Ok(()) }
            }),
            Ctrl::RxPort(p) => phy.set_rx_port(&mut conn, &p),
            Ctrl::TxPort(p) => phy.set_tx_port(&mut conn, &p),
            Ctrl::TxGain(db) => phy.set_tx_gain(&mut conn, db),
            Ctrl::Ppm(v) => {
                ppm = v;
                // Take effect now rather than at the next retune: an operator
                // trimming ppm is watching a carrier while they drag.
                if rx_hz > 0.0 {
                    phy.set_rx_lo(&mut conn, PlutoConfig::apply_ppm(rx_hz, ppm))
                } else {
                    Ok(())
                }
            }
            Ctrl::TxOn(hz) => key_up(&mut conn, &shared, hz, ppm),
            Ctrl::TxOff => key_down(&mut conn, &shared, rx_hz, ppm),
            Ctrl::Shutdown => break,
        };
        if let Err(e) = outcome {
            // A rejected attribute write is not fatal on its own — a value out
            // of range, a mode this board does not have — but a socket that has
            // gone is. Distinguishing them is what keeps a bad slider from
            // tearing down a working radio.
            match e {
                Error::Remote { .. } | Error::Unsupported(_) => {
                    tracing::warn!("PlutoSDR: {e}");
                    shared.trace.note(format!("!! {e}"));
                }
                _ => {
                    shared.die("the control connection", &e);
                    break;
                }
            }
        }
    }
    shared.rx_enabled.store(false, Ordering::Relaxed);
    shared.tx_enabled.store(false, Ordering::Relaxed);
    shared.alive.store(false, Ordering::Relaxed);
    conn.exit();
    tracing::debug!("PlutoSDR: control thread finished");
}

/// Tune the transmit LO, silence the DDS, take receive down, then hand the link
/// to the transmit thread.
///
/// The DDS step is the one that cannot be skipped: the transmit path is fed by
/// four on-chip tone generators unless they are explicitly zeroed, and a Pluto
/// that skips it puts out a steady carrier pair at full power instead of the
/// modulation.
fn key_up(conn: &mut Connection, shared: &Shared, hz: f64, ppm: f64) -> Result<()> {
    let phy = &shared.phy;
    phy.set_tx_lo(conn, PlutoConfig::apply_ppm(hz, ppm))?;
    phy.silence_dds(conn)?;
    shared.rx_enabled.store(false, Ordering::Relaxed);
    // Wait for the receive thread to actually let go of its buffer. Bounded:
    // if it has died, transmit should still work.
    let deadline = Instant::now() + Duration::from_millis(500);
    while shared.rx_active.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    shared.tx_enabled.store(true, Ordering::Relaxed);
    Ok(())
}

/// Give the link back to receive, and put the receive LO back where the dial is
/// — the transmit LO may have moved it if the two share a synthesiser.
fn key_down(conn: &mut Connection, shared: &Shared, rx_hz: f64, ppm: f64) -> Result<()> {
    shared.tx_enabled.store(false, Ordering::Relaxed);
    // Wait for the transmit buffer to actually close before receive reclaims
    // the link: on a USB 2.0 gadget there is only room for one of them.
    let deadline = Instant::now() + Duration::from_millis(500);
    while shared.tx_active.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    shared.rx_enabled.store(true, Ordering::Relaxed);
    if rx_hz > 0.0 {
        shared.phy.set_rx_lo(conn, PlutoConfig::apply_ppm(rx_hz, ppm))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Pluto's gadget interface is IPv4-only. On a host where `pluto.local`
    /// also resolves to a link-local IPv6 address, picking that one produces a
    /// connection that can never succeed.
    #[test]
    fn resolution_prefers_ipv4() {
        let addr = resolve("127.0.0.1:30431").expect("loopback");
        assert!(addr.is_ipv4());
        assert_eq!(addr.port(), 30431);
        // The default port is filled in when the operator gives only a host.
        assert_eq!(resolve("127.0.0.1").expect("bare").port(), crate::DEFAULT_PORT);
    }

    #[test]
    fn a_bad_address_is_reported_not_guessed_at() {
        assert!(resolve("").is_err());
        assert!(resolve("192.168.2.1:not-a-port").is_err());
    }
}
