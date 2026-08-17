//! The two sample-carrying threads: one owns the receive buffer, one the
//! transmit buffer, and each has its own IIOD connection.
//!
//! Neither thread touches a front-end register — that is the control thread's
//! job, on a third connection. All either does is move bytes and convert them,
//! which is what keeps a `READBUF` that blocks for a whole buffer period from
//! delaying a retune.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer};

use crate::error::Error;
use crate::iiod::Connection;
use crate::net::{STATS_INTERVAL, Shared};

/// I and Q of the transmit buffer's one pair. `Phy::probe` guarantees it sits
/// at scan indices 0 and 1; a 2R2T device's second transmit pair stays
/// disabled and off the wire. The receive side is no longer a constant — it
/// opens with one or two pairs as streams attach (see `Shared::rx_pairs`).
const TX_IQ_CHANNELS: usize = 2;

/// `-EAGAIN`: the server's own device timeout expired with nothing to hand
/// over. Normal while a device is idle, and not a reason to tear anything down.
const EAGAIN: i64 = -11;
/// `-ETIMEDOUT`, the other spelling of the same thing.
const ETIMEDOUT: i64 = -110;

/// The shortest a receive buffer may go without producing a sample before it is
/// closed and reopened rather than read again — see [`silence_before_restart`].
const RESTART_FLOOR: Duration = Duration::from_millis(1_000);

/// How long a receive buffer that is answering "nothing yet" is given before it
/// is closed and reopened.
///
/// `-EAGAIN` on its own is ordinary and absorbed by reading again: it is what
/// the server says when its own device timeout expired with nothing to hand
/// over. What is *not* ordinary is that answer arriving over and over from a
/// buffer that was streaming a moment ago, which is the shape a stalled DMA
/// takes from this side of the link — and no number of further `READBUF`s on
/// the same buffer will restart it. Reopening might; it costs a few
/// milliseconds and it happens on the connection we already have, where the
/// alternative is the engine's watchdog tearing the whole radio down and
/// dialling it again.
///
/// Scaled by the buffer's own airtime rather than fixed, because a buffer only
/// produces once per that period: an operator who asks for a megasample buffer
/// at the lowest rate on offer is legitimately two seconds between deliveries,
/// and a fixed floor would have us reopening a perfectly healthy stream forever.
fn silence_before_restart(buffer_samples: usize, rate_hz: f64) -> Duration {
    let span = Duration::from_secs_f64(buffer_samples as f64 / rate_hz.max(1.0));
    (span * 3).max(RESTART_FLOOR)
}

/// Periodic throughput accounting, emitted once per [`STATS_INTERVAL`] so a
/// tester can see whether samples are flowing — and at what rate — without a
/// per-buffer log flood. A wrong sample layout or a wrong rate shows up
/// immediately as an implausible ksps figure.
struct Stats {
    what: &'static str,
    nominal_hz: f64,
    /// How long the buffer has actually been open, summed across every window.
    ///
    /// Wall time since the thread started is the wrong denominator: the
    /// transmit buffer is closed between overs and the receive buffer is closed
    /// *during* them, so counting the idle stretches reported clock errors
    /// hundreds of thousands of ppm out — a figure alarming enough in a debug
    /// log to send a tester after the wrong fault entirely.
    active: Duration,
    since: Instant,
    win_buffers: u64,
    win_samples: u64,
    win_dropped: u64,
    total_samples: u64,
    total_dropped: u64,
}

impl Stats {
    fn new(what: &'static str, nominal_hz: f64) -> Stats {
        Stats {
            what,
            nominal_hz,
            active: Duration::ZERO,
            since: Instant::now(),
            win_buffers: 0,
            win_samples: 0,
            win_dropped: 0,
            total_samples: 0,
            total_dropped: 0,
        }
    }

    /// The buffer just opened: start a fresh window, so the first figure after
    /// an idle stretch measures streaming and not the wait that preceded it.
    fn opened(&mut self) {
        self.since = Instant::now();
        self.win_buffers = 0;
        self.win_samples = 0;
        self.win_dropped = 0;
    }

    /// The buffer just closed: bank the part-window that will never be
    /// reported, so its samples and its time stay paired in the clock estimate.
    fn closed(&mut self) {
        self.active += self.since.elapsed();
    }

    fn on_buffer(&mut self, pairs: usize) {
        self.win_buffers += 1;
        self.win_samples += pairs as u64;
        self.total_samples += pairs as u64;
    }

    fn on_dropped(&mut self, pairs: usize) {
        self.win_dropped += pairs as u64;
        self.total_dropped += pairs as u64;
    }

    /// The device's sample clock measured against the host's, in ppm, once
    /// enough time has passed for the figure to mean anything. The same
    /// oscillator drives the AD9361's synthesiser, so this is also the tuning
    /// error: a Pluto reading tens of ppm here is tens of ppm off frequency,
    /// which is what the `ppm` setting on the Radio tab is for.
    fn clock_error(&self) -> String {
        let dt = self.active.as_secs_f64();
        if dt < 20.0 || self.total_samples == 0 || self.nominal_hz <= 0.0 {
            return "clock: measuring".to_string();
        }
        let measured = self.total_samples as f64 / dt;
        let ppm = (measured / self.nominal_hz - 1.0) * 1e6;
        format!("clock: {measured:.0} sps, {ppm:+.1} ppm vs nominal")
    }

    fn tick(&mut self) {
        let dt = self.since.elapsed();
        if dt < STATS_INTERVAL {
            return;
        }
        let ksps = self.win_samples as f64 / dt.as_secs_f64() / 1000.0;
        // A direction that cannot carry the full rate is the fault that used to
        // need an oscilloscope to find: on transmit the device empties its
        // buffer faster than the next one arrives, so the carrier drops out
        // between buffers and the envelope goes out chopped. Nothing else in
        // the program can see it, so it is said here.
        // 5 %, not a hair's breadth: a window ends on a buffer boundary while
        // its clock does not, and the first buffer of an over is waited for
        // from an empty ring, so a percent or two of shortfall is bookkeeping.
        if self.win_dropped == 0 && self.nominal_hz > 0.0 && ksps * 1e3 < self.nominal_hz * 0.95 {
            tracing::warn!(
                "PlutoSDR {}: {ksps:.1} of {:.1} ksps over {:.2}s — the link is not carrying the \
                 full sample rate. On transmit this leaves the modulation with gaps in it; \
                 lower the sample rate, or reach the radio over Ethernet rather than the USB \
                 gadget",
                self.what,
                self.nominal_hz / 1e3,
                dt.as_secs_f64(),
            );
        }
        if self.win_dropped > 0 {
            tracing::warn!(
                "PlutoSDR {}: {} buffers, {} samples ({ksps:.1} ksps) over {:.2}s; \
                 {} sample(s) DROPPED (ring full — the DSP thread is not keeping up; \
                 try a lower sample rate); {} dropped in total",
                self.what,
                self.win_buffers,
                self.win_samples,
                dt.as_secs_f64(),
                self.win_dropped,
                self.total_dropped,
            );
        } else {
            tracing::debug!(
                "PlutoSDR {}: {} buffers, {} samples ({ksps:.1} ksps) over {:.2}s; {}",
                self.what,
                self.win_buffers,
                self.win_samples,
                dt.as_secs_f64(),
                self.clock_error(),
            );
        }
        self.active += dt;
        self.since = Instant::now();
        self.win_buffers = 0;
        self.win_samples = 0;
        self.win_dropped = 0;
    }
}

/// Push one buffer's interleaved I/Q into the ring, keeping I and Q paired: if
/// the ring cannot take the whole buffer, the whole buffer is dropped. Pushing
/// what fits would leave the ring one float out of step, which swaps I with Q
/// for the rest of the session — a mirrored, unusable spectrum that reads as a
/// protocol bug rather than the overrun it is.
fn push_iq(ring: &mut Producer<f32>, iq: &[f32], stats: &mut Stats) {
    let Ok(mut chunk) = ring.write_chunk(iq.len()) else {
        stats.on_dropped(iq.len() / 2);
        return;
    };
    let (head, tail) = chunk.as_mut_slices();
    head.copy_from_slice(&iq[..head.len()]);
    tail.copy_from_slice(&iq[head.len()..]);
    chunk.commit_all();
}

/// Owns the receive buffer.
///
/// The buffer is opened with one or two I/Q pairs enabled, following
/// `Shared::rx_pairs` — a second radio attaching to the other chain widens
/// it, and its departure narrows it back. The channel mask is a property of
/// an *open* buffer, so a change means close-and-reopen: a brief gap on the
/// surviving stream, not a glitch in its alignment.
pub(crate) fn rx_thread(mut conn: Connection, shared: Arc<Shared>, mut ring: Producer<f32>) {
    let phy = &shared.phy;
    let max_pairs = phy.rx_pairs_available();
    let mut raw = vec![0u8; shared.buffer_samples * phy.rx_sample_bytes(max_pairs)];
    let mut iq: Vec<f32> = Vec::with_capacity(shared.buffer_samples * 2);
    let mut iq1: Vec<f32> = Vec::with_capacity(shared.buffer_samples * 2);
    let mut stats = Stats::new("RX", shared.rate_hz);
    let mut open_pairs = 0usize;
    // When this buffer last produced anything, and how long it may go without
    // before it is reopened. Reset on every open so a buffer is judged from
    // when it started, not from the last one's death.
    let patience = silence_before_restart(shared.buffer_samples, shared.rate_hz);
    let mut last_sets = Instant::now();

    while shared.alive.load(Ordering::Relaxed) {
        let want_pairs = shared.rx_pairs.load(Ordering::Relaxed).clamp(1, max_pairs);
        if !shared.rx_enabled.load(Ordering::Relaxed) {
            if open_pairs > 0 {
                let _ = conn.close_buffer(&phy.rx_buffer_id);
                open_pairs = 0;
                stats.closed();
                shared.rx_active.store(false, Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        if open_pairs > 0 && open_pairs != want_pairs {
            let _ = conn.close_buffer(&phy.rx_buffer_id);
            open_pairs = 0;
            stats.closed();
            tracing::debug!("PlutoSDR: receive buffer reopening for {want_pairs} pair(s)");
        }
        if open_pairs == 0 {
            if let Err(e) =
                conn.open_buffer(&phy.rx_buffer_id, shared.buffer_samples, want_pairs * 2)
            {
                shared.die("the receive buffer", &e);
                break;
            }
            open_pairs = want_pairs;
            stats.opened();
            last_sets = Instant::now();
            shared.rx_active.store(true, Ordering::Relaxed);
            tracing::debug!(
                "PlutoSDR: receive buffer open, {} samples × {} bytes ({} pair(s))",
                shared.buffer_samples,
                phy.rx_sample_bytes(open_pairs),
                open_pairs
            );
        }
        // Request exactly one device buffer's worth for the layout that is
        // open — `raw` is sized for the widest, and asking for more bytes
        // than the buffer carries would change the request on the wire.
        let set_bytes = phy.rx_sample_bytes(open_pairs);
        let want_bytes = shared.buffer_samples * set_bytes;
        // An open buffer that has stopped producing is reopened rather than
        // read again — see `silence_before_restart`. Checked here rather than
        // in the `-EAGAIN` arm below so that every shape of silence is caught,
        // including a server answering with an empty buffer. The clock is reset
        // by the reopen, so a device that is genuinely gone costs one restart
        // per window rather than a tight loop of them while the engine's
        // watchdog runs down.
        if last_sets.elapsed() >= patience {
            tracing::warn!(
                "PlutoSDR: no samples for {:.1}s — reopening the receive buffer",
                last_sets.elapsed().as_secs_f64()
            );
            shared.trace.note("~~ receive buffer reopened after a silence");
            let _ = conn.close_buffer(&phy.rx_buffer_id);
            open_pairs = 0;
            stats.closed();
            shared.rx_active.store(false, Ordering::Relaxed);
            last_sets = Instant::now();
            continue;
        }
        let n = match conn.read_buf(&phy.rx_buffer_id, open_pairs * 2, &mut raw[..want_bytes]) {
            Ok(n) => n,
            Err(Error::Remote { code, .. }) if code == EAGAIN || code == ETIMEDOUT => continue,
            Err(e) => {
                shared.die("the receive stream", &e);
                break;
            }
        };
        let sets = n / set_bytes;
        if sets == 0 {
            continue;
        }
        last_sets = Instant::now();
        let i_bytes = phy.rx_scan[0].bytes();
        iq.clear();
        iq1.clear();
        for s in 0..sets {
            let mut off = s * set_bytes;
            iq.push(phy.rx_scan[0].decode(&raw[off..off + i_bytes]));
            off += i_bytes;
            let q_bytes = phy.rx_scan[1].bytes();
            iq.push(phy.rx_scan[1].decode(&raw[off..off + q_bytes]));
            off += q_bytes;
            if open_pairs == 2 {
                let i2 = phy.rx_scan[2].bytes();
                iq1.push(phy.rx_scan[2].decode(&raw[off..off + i2]));
                off += i2;
                let q2 = phy.rx_scan[3].bytes();
                iq1.push(phy.rx_scan[3].decode(&raw[off..off + q2]));
            }
        }
        push_iq(&mut ring, &iq, &mut stats);
        // Stamped by this thread rather than by the reader, so an over — during
        // which nothing drains the ring — is not read as a dead radio.
        shared.stamp_rx();
        if open_pairs == 2 {
            // The second chain's radio may not have attached its ring yet (or
            // may just have dropped it); its samples then fall on the floor,
            // which is what "nobody is listening" should cost.
            let mut slot = shared.ring1.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r1) = slot.as_mut() {
                push_iq(r1, &iq1, &mut stats);
                shared.stamp_rx1();
            }
        }
        stats.on_buffer(sets);
        stats.tick();
    }

    if open_pairs > 0 {
        let _ = conn.close_buffer(&phy.rx_buffer_id);
    }
    shared.rx_active.store(false, Ordering::Relaxed);
    conn.exit();
    tracing::debug!("PlutoSDR: receive thread finished");
}

/// Owns the transmit buffer.
///
/// `WRITEBUF` does not return until the device has taken the data, so this loop
/// is paced by the hardware itself — there is no clock on this side to drift.
///
/// # What has to hold for the carrier to be continuous
///
/// The device plays nothing while a `WRITEBUF` is in flight, so one buffer must
/// cover *more airtime than the round trip costs* — see
/// [`crate::net::tx_buffer_samples`], which is what sizes it. Given that, the
/// device's queue depth is conserved: the engine produces one buffer's worth of
/// samples in exactly the airtime that buffer covers, so each push replaces
/// what was played since the last one, and the lead established by the first
/// push of an over survives to the end of it.
pub(crate) fn tx_thread(mut conn: Connection, shared: Arc<Shared>, mut ring: Consumer<f32>) {
    let phy = &shared.phy;
    let set_bytes = phy.tx_sample_bytes();
    let i_bytes = phy.tx_scan[0].bytes();
    let pairs = shared.tx_buffer_samples;
    let mut raw = vec![0u8; pairs * set_bytes];
    let mut stats = Stats::new("TX", shared.tx_rate_hz);
    let mut open = false;

    // How long a full buffer takes the engine to produce, which is what the
    // wait below is really waiting for. Scaled rather than fixed: a flat 20 ms
    // was shorter than a buffer at any useful rate once buffers grew past a few
    // milliseconds, so every single one would have been cut short and padded —
    // trading the old gap between buffers for a gap inside each of them.
    let buffer_span = Duration::from_secs_f64(pairs as f64 / shared.tx_rate_hz.max(1.0));
    let fill_wait = buffer_span * 2 + Duration::from_millis(20);

    while shared.alive.load(Ordering::Relaxed) {
        if !shared.tx_enabled.load(Ordering::Relaxed) {
            if open {
                let _ = conn.close_buffer(&phy.tx_buffer_id);
                open = false;
                stats.closed();
                shared.tx_active.store(false, Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        if !open {
            if let Err(e) = conn.open_buffer(&phy.tx_buffer_id, pairs, TX_IQ_CHANNELS) {
                shared.die("the transmit buffer", &e);
                break;
            }
            open = true;
            stats.opened();
            shared.tx_active.store(true, Ordering::Relaxed);
            tracing::debug!(
                "PlutoSDR: transmit buffer open, {pairs} samples ({:.1} ms)",
                buffer_span.as_secs_f64() * 1e3
            );
        }

        // Give the engine time to fill a whole buffer before padding with
        // silence. The first buffers of an over are legitimately short — the
        // ring starts empty — and sending them as-is would put a gap in the
        // middle of the modulation instead of only at the very start.
        let want = pairs * 2;
        let deadline = Instant::now() + fill_wait;
        while ring.slots() < want
            && Instant::now() < deadline
            && shared.tx_enabled.load(Ordering::Relaxed)
        {
            std::thread::sleep(Duration::from_micros(200));
        }

        let mut short = 0usize;
        for p in 0..pairs {
            let off = p * set_bytes;
            let (i, q) = match (ring.pop(), ring.pop()) {
                (Ok(i), Ok(q)) => (i, q),
                // Only ever an odd tail if the ring were pushed a half pair,
                // which `tx_write` makes impossible; treat it as silence.
                _ => {
                    short += 1;
                    (0.0, 0.0)
                }
            };
            phy.tx_scan[0].encode(i, &mut raw[off..off + i_bytes]);
            phy.tx_scan[1].encode(q, &mut raw[off + i_bytes..off + set_bytes]);
        }
        if short > 0 {
            tracing::trace!("PlutoSDR TX: padded {short} of {pairs} samples with silence");
        }
        match conn.write_buf(&phy.tx_buffer_id, &raw) {
            Ok(_) => {
                stats.on_buffer(pairs);
                stats.tick();
            }
            Err(Error::Remote { code, .. }) if code == EAGAIN || code == ETIMEDOUT => continue,
            Err(e) => {
                shared.die("the transmit stream", &e);
                break;
            }
        }
    }

    if open {
        let _ = conn.close_buffer(&phy.tx_buffer_id);
    }
    shared.tx_active.store(false, Ordering::Relaxed);
    conn.exit();
    tracing::debug!("PlutoSDR: transmit thread finished");
}
