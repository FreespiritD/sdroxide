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
use crate::net::{STATS_INTERVAL, Shared, TX_BUFFER_SAMPLES};

/// I and Q. `Phy::probe` guarantees both buffers carry an I/Q pair at scan
/// indices 0 and 1, and every buffer is opened with only those two enabled —
/// a 2R2T device's second pair stays disabled and off the wire — so this is a
/// constant rather than a guess.
const IQ_CHANNELS: usize = 2;

/// `-EAGAIN`: the server's own device timeout expired with nothing to hand
/// over. Normal while a device is idle, and not a reason to tear anything down.
const EAGAIN: i64 = -11;
/// `-ETIMEDOUT`, the other spelling of the same thing.
const ETIMEDOUT: i64 = -110;

/// Periodic throughput accounting, emitted once per [`STATS_INTERVAL`] so a
/// tester can see whether samples are flowing — and at what rate — without a
/// per-buffer log flood. A wrong sample layout or a wrong rate shows up
/// immediately as an implausible ksps figure.
struct Stats {
    what: &'static str,
    nominal_hz: f64,
    started: Instant,
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
            started: Instant::now(),
            since: Instant::now(),
            win_buffers: 0,
            win_samples: 0,
            win_dropped: 0,
            total_samples: 0,
            total_dropped: 0,
        }
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
        let dt = self.started.elapsed().as_secs_f64();
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
pub(crate) fn rx_thread(mut conn: Connection, shared: Arc<Shared>, mut ring: Producer<f32>) {
    let phy = &shared.phy;
    let set_bytes = phy.rx_sample_bytes();
    let i_bytes = phy.rx_scan[0].bytes();
    let mut raw = vec![0u8; shared.buffer_samples * set_bytes];
    let mut iq: Vec<f32> = Vec::with_capacity(shared.buffer_samples * 2);
    let mut stats = Stats::new("RX", shared.rate_hz);
    let mut open = false;

    while shared.alive.load(Ordering::Relaxed) {
        if !shared.rx_enabled.load(Ordering::Relaxed) {
            if open {
                let _ = conn.close_buffer(&phy.rx_buffer_id);
                open = false;
                shared.rx_active.store(false, Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        if !open {
            if let Err(e) = conn.open_buffer(&phy.rx_buffer_id, shared.buffer_samples, IQ_CHANNELS)
            {
                shared.die("the receive buffer", &e);
                break;
            }
            open = true;
            shared.rx_active.store(true, Ordering::Relaxed);
            tracing::debug!(
                "PlutoSDR: receive buffer open, {} samples × {set_bytes} bytes",
                shared.buffer_samples
            );
        }
        let n = match conn.read_buf(&phy.rx_buffer_id, IQ_CHANNELS, &mut raw) {
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
        iq.clear();
        for s in 0..sets {
            let off = s * set_bytes;
            iq.push(phy.rx_scan[0].decode(&raw[off..off + i_bytes]));
            iq.push(phy.rx_scan[1].decode(&raw[off + i_bytes..off + set_bytes]));
        }
        push_iq(&mut ring, &iq, &mut stats);
        // Stamped by this thread rather than by the reader, so an over — during
        // which nothing drains the ring — is not read as a dead radio.
        shared.stamp_rx();
        stats.on_buffer(sets);
        stats.tick();
    }

    if open {
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
pub(crate) fn tx_thread(mut conn: Connection, shared: Arc<Shared>, mut ring: Consumer<f32>) {
    let phy = &shared.phy;
    let set_bytes = phy.tx_sample_bytes();
    let i_bytes = phy.tx_scan[0].bytes();
    let pairs = TX_BUFFER_SAMPLES;
    let mut raw = vec![0u8; pairs * set_bytes];
    let mut stats = Stats::new("TX", shared.rate_hz);
    let mut open = false;

    while shared.alive.load(Ordering::Relaxed) {
        if !shared.tx_enabled.load(Ordering::Relaxed) {
            if open {
                let _ = conn.close_buffer(&phy.tx_buffer_id);
                open = false;
                shared.tx_active.store(false, Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        if !open {
            if let Err(e) = conn.open_buffer(&phy.tx_buffer_id, pairs, IQ_CHANNELS) {
                shared.die("the transmit buffer", &e);
                break;
            }
            open = true;
            shared.tx_active.store(true, Ordering::Relaxed);
            tracing::debug!("PlutoSDR: transmit buffer open, {pairs} samples");
        }

        // Give the engine a moment to fill a whole buffer before padding with
        // silence. The first buffers of an over are legitimately short — the
        // ring starts empty — and sending them as-is would put a gap in the
        // middle of the modulation instead of only at the very start.
        let want = pairs * 2;
        let deadline = Instant::now() + Duration::from_millis(20);
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
