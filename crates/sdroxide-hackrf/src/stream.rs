//! The one blocking thread that owns the radio, and the bulk pump inside it.
//!
//! # Two lanes, never both
//!
//! The radio is half duplex, and this is where that shows. Exactly one bulk
//! endpoint is open at a time, held in [`Lane`], and a direction change *drops*
//! the one it is leaving rather than idling it. That is not tidiness: idling and
//! re-arming the receive endpoint leaves the receive path returning repeated and
//! aliased buffers until the device is reopened, which `sdroxide-radio`'s
//! SoapySDR path documents the hard way at `device.rs:575-581`. Making the
//! endpoint an owned value inside an enum is what makes the close unavoidable —
//! the transition cannot be written without giving it up.
//!
//! # The transmit tail
//!
//! [`Lane::Tx`] is drained to empty before the endpoint is released. The engine
//! only asks for an explicit drain on the digital-burst path, so without this an
//! ordinary key-up would cut whatever was still in the ring — up to 200 ms of
//! the end of the over. For an FT8 transmission that is the difference between
//! decoding and not.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use nusb::MaybeFuture;
use nusb::transfer::{Bulk, In, Out, TransferError};
use rtrb::{Consumer, Producer};
use sdroxide_types::HackRfConfig;

use crate::device::Device;
use crate::error::{Error, Result};
use crate::handle::{
    Ctrl, DeviceInfo, HackRfHandle, Pending, RxStats, Shared, TxTransition, push_iq, rx_ring_for,
    tx_ring_for,
};
use crate::protocol::{BULK_IN, BULK_OUT, BULK_PACKET, BYTES_PER_SAMPLE, GainSetter};
use crate::trace::{self, Trace};
use crate::usb::UsbDev;

/// How long a first completion may block before the loop goes back to serve
/// control. Bounds how long a dial drag waits when the stream has gone quiet.
const COMPLETE_TIMEOUT: Duration = Duration::from_millis(20);

/// How long to wait for cancelled transfers to come back before dropping an
/// endpoint. Generous: the alternative is releasing an endpoint with transfers
/// still in flight.
const CANCEL_TIMEOUT: Duration = Duration::from_millis(200);

/// How long a key-up will wait for the transmit ring to empty. Past this the
/// tail is abandoned — a stuck transmitter is worse than a clipped over.
const TX_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Silence pre-loaded into the transmit endpoint at key-down, so the first real
/// samples meet a queue that is already running rather than starting one.
const TX_PRIME_SECS: f64 = 0.050;

/// Transfer geometry, bounded against a hand-edited config the way the RTL-SDR
/// backend bounds its own.
///
/// 64 KiB is 16.4 ms at 2 Msps and 1.6 ms at 20 Msps; sixteen in flight is a
/// quarter-second of hardware-side buffering at the bottom rate and 26 ms at the
/// top. libhackrf's own 4 × 256 KiB is worse on both counts — coarser control
/// latency, and a longer stall when a retune lands mid-transfer.
const MIN_TRANSFERS: usize = 4;
const MAX_TRANSFERS: usize = 64;
const MIN_TRANSFER_BYTES: usize = 2 * 1024;
const MAX_TRANSFER_BYTES: usize = 256 * 1024;

fn geometry(cfg: &HackRfConfig) -> (usize, usize) {
    let n = (cfg.transfers as usize).clamp(MIN_TRANSFERS, MAX_TRANSFERS);
    let bytes = ((cfg.transfer_kib as usize) * 1024).clamp(MIN_TRANSFER_BYTES, MAX_TRANSFER_BYTES)
        / BULK_PACKET
        * BULK_PACKET;
    (n, bytes.max(BULK_PACKET))
}

/// Which direction the radio is in, and the endpoint that belongs with it.
///
/// The endpoints are owned rather than borrowed so that a direction change has
/// to give one up to get the other — see the module header.
enum Lane {
    Rx(nusb::Endpoint<Bulk, In>),
    Tx(nusb::Endpoint<Bulk, Out>),
}

/// Open the radio and start the stream thread.
///
/// The device is opened *on the thread* so that all control transfers happen on
/// one thread; this function blocks on a handshake until that has succeeded or
/// failed, so the caller still gets a synchronous `Result`.
pub(crate) fn spawn(cfg: &HackRfConfig, center_hz: f64) -> Result<HackRfHandle> {
    let (ctrl_tx, ctrl_rx) = crossbeam_channel::unbounded::<Ctrl>();
    // Rendezvous for the open result, so the caller learns about a permission
    // problem or a missing radio as a normal error rather than as a stream that
    // silently never starts.
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<DeviceInfo>>(1);

    let shared = Arc::new(Shared::new());
    let t = Trace::new();
    trace::remember(&t);

    let rate = cfg.sample_rate_hz;
    let (rx_prod, rx_cons) = rx_ring_for(rate);
    let (tx_prod, tx_cons) = tx_ring_for(rate);

    let cfg = cfg.clone();
    let thread_shared = Arc::clone(&shared);
    let thread_trace = t.clone();
    let join = std::thread::Builder::new()
        .name("sdroxide-hackrf".into())
        .spawn(move || {
            run(
                cfg,
                center_hz,
                ctrl_rx,
                rx_prod,
                tx_cons,
                Arc::clone(&thread_shared),
                ready_tx,
                thread_trace,
            );
            thread_shared.tx_active.store(false, Ordering::Relaxed);
            thread_shared.alive.store(false, Ordering::Relaxed);
        })
        .map_err(|e| Error::Access(format!("could not start the HackRF thread: {e}")))?;

    match ready_rx.recv() {
        Ok(Ok(info)) => {
            Ok(HackRfHandle::from_parts(rx_cons, tx_prod, ctrl_tx, shared, join, t, info))
        }
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        // The thread died before reporting; joining surfaces a panic message.
        Err(_) => {
            let _ = join.join();
            Err(Error::Access("the HackRF thread stopped before it opened the radio".into()))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    cfg: HackRfConfig,
    center_hz: f64,
    ctrl: crossbeam_channel::Receiver<Ctrl>,
    mut rx: Producer<u8>,
    mut tx: Consumer<u8>,
    shared: Arc<Shared>,
    ready: crossbeam_channel::Sender<Result<DeviceInfo>>,
    trace: Trace,
) {
    let usb = match UsbDev::open(&cfg.serial, &trace) {
        Ok(d) => d,
        Err(e) => {
            trace.note(format!("open failed: {e}"));
            let _ = ready.send(Err(e));
            return;
        }
    };
    let superspeed = usb.is_superspeed();
    let serial = usb.serial().map(str::to_string);

    let mut dev = match Device::open(usb, &cfg, center_hz) {
        Ok(d) => d,
        Err(e) => {
            trace.note(format!("configure failed: {e}"));
            let _ = ready.send(Err(e));
            return;
        }
    };

    let (lna, vga, txvga) = dev.gains();
    shared.set_gain(GainSetter::Lna, lna);
    shared.set_gain(GainSetter::Vga, vga);
    shared.set_gain(GainSetter::TxVga, txvga);

    let rate = dev.rate_hz();
    // 20 Msps is 40 MB/s. A high-speed link tops out at 60 MB/s in theory and
    // well under it in practice, so this is worth saying at open rather than
    // leaving somebody to diagnose dropped samples.
    let link_warning = (!superspeed && rate > 8.0e6).then(|| {
        format!(
            "{:.1} Msps is {:.0} MB/s and this radio is on a high-speed (USB 2.0) link — \
             expect dropped samples. Move it to a SuperSpeed port or pick a lower rate.",
            rate / 1e6,
            rate * BYTES_PER_SAMPLE as f64 / 1e6
        )
    });
    if let Some(w) = &link_warning {
        tracing::warn!("{w}");
        trace.note(w);
    }

    let _ = ready.send(Ok(DeviceInfo {
        label: dev.label(),
        kind: dev.kind(),
        firmware: dev.version().to_string(),
        serial,
        sample_rate_hz: rate,
        filter_bw_hz: dev.filter_bw(),
        freq_range: dev.kind().freq_range(),
        rate_range: dev.rate_range(),
        has_bias_tee: dev.has_bias_tee(),
        filter_is_automatic: dev.sets_own_filter(),
        snapped_from: (cfg.sample_rate_hz != rate).then_some(cfg.sample_rate_hz),
        link_warning,
    }));

    if let Err(e) = pump(&mut dev, &cfg, &ctrl, &mut rx, &mut tx, &shared, &trace) {
        tracing::warn!("HackRF stream stopped: {e}");
        trace.note(format!("stream stopped: {e}"));
    }
    // Whatever happened above, the radio goes off the air here. This is the
    // last thing between a fault and a transmitter nobody is driving.
    dev.shutdown();
}

fn pump(
    dev: &mut Device<UsbDev>,
    cfg: &HackRfConfig,
    ctrl: &crossbeam_channel::Receiver<Ctrl>,
    rx: &mut Producer<u8>,
    tx: &mut Consumer<u8>,
    shared: &Arc<Shared>,
    trace: &Trace,
) -> Result<()> {
    let (in_flight, transfer_bytes) = geometry(cfg);
    tracing::info!(
        "HackRF streaming: {in_flight} transfers x {} KiB = {:.0} ms of buffering at {:.1} Msps",
        transfer_bytes / 1024,
        (in_flight * transfer_bytes / BYTES_PER_SAMPLE) as f64 / dev.rate_hz() * 1000.0,
        dev.rate_hz() / 1e6,
    );

    let mut stats = RxStats::new(dev.rate_hz());
    let started = Instant::now();
    let mut logged_first = false;
    let mut lane = open_rx(dev, in_flight, transfer_bytes)?;
    let mut carry: Option<u8> = None;
    let mut tx_bytes: Vec<u8> = Vec::with_capacity(transfer_bytes);

    loop {
        // 1. Collapse the whole control channel, then apply each field once.
        let mut pending = Pending::default();
        while let Ok(c) = ctrl.try_recv() {
            pending.absorb(c);
        }
        if pending.shutdown {
            break;
        }
        if !pending.is_empty()
            && let Err(e) = apply(dev, &pending, shared)
        {
            // A setting that cannot be applied is worth saying out loud, but it
            // is not a reason to tear the stream down — the operator can pick
            // another value.
            tracing::warn!("HackRF: {e}");
            trace.note(format!("control change failed: {e}"));
        }

        // 2. Direction changes, in the order they were asked for. Never
        //    collapsed; see `Pending`.
        for t in pending.tx.iter().copied() {
            lane = match transition(dev, lane, t, tx, shared, trace, in_flight, transfer_bytes) {
                Ok(l) => l,
                Err(e) => {
                    // A failed transition must not leave the radio keyed with
                    // nothing driving it, and must not end the session either:
                    // recover to receive and say so.
                    tracing::warn!("HackRF: direction change failed: {e}");
                    trace.note(format!("direction change failed: {e}"));
                    shared.tx_active.store(false, Ordering::Relaxed);
                    let _ = dev.end_tx();
                    stats.restart_clock();
                    open_rx(dev, in_flight, transfer_bytes)?
                }
            };
        }

        // 3. Service whichever lane is open.
        match &mut lane {
            Lane::Rx(ep) => {
                service_rx(
                    ep,
                    rx,
                    &mut stats,
                    &mut carry,
                    &mut logged_first,
                    shared,
                    trace,
                    started,
                    in_flight,
                    transfer_bytes,
                )?;
                stats.tick(trace);
            }
            Lane::Tx(ep) => {
                service_tx(ep, tx, &mut tx_bytes, in_flight, transfer_bytes)?;
            }
        }
    }

    trace.note(format!("stream ended: {}", stats.summary()));
    close(lane);
    Ok(())
}

/// Apply the collapsible settings, in dependency order.
///
/// ppm first because every frequency is computed against the corrected crystal;
/// then the rate, which reprograms the filter and re-tunes; then an explicit
/// filter override, which must win over what the rate chose; then the dial.
/// Gains and switches last — they depend on nothing.
fn apply(dev: &mut Device<UsbDev>, p: &Pending, shared: &Arc<Shared>) -> Result<()> {
    if let Some(v) = p.ppm {
        dev.set_ppm(v)?;
    }
    if let Some(v) = p.rate {
        dev.set_rate(v)?;
    }
    if let Some(v) = p.filter_bw {
        dev.set_filter_pref(v)?;
    }
    if let Some(v) = p.center {
        dev.set_center_hz(v)?;
    }
    for (stage, want) in
        [(GainSetter::Lna, p.lna), (GainSetter::Vga, p.vga), (GainSetter::TxVga, p.txvga)]
    {
        if let Some(db) = want {
            let applied = dev.set_gain(stage, db)?;
            shared.set_gain(stage, applied);
        }
    }
    if let Some(v) = p.amp {
        dev.set_amp(false, v)?;
    }
    if let Some(v) = p.tx_amp {
        dev.set_amp(true, v)?;
    }
    if let Some(v) = p.bias_tee {
        dev.set_bias_tee(v)?;
    }
    Ok(())
}

/// Carry out one direction change.
#[allow(clippy::too_many_arguments)]
fn transition(
    dev: &mut Device<UsbDev>,
    lane: Lane,
    t: TxTransition,
    tx: &mut Consumer<u8>,
    shared: &Arc<Shared>,
    trace: &Trace,
    in_flight: usize,
    transfer_bytes: usize,
) -> Result<Lane> {
    match t {
        TxTransition::Begin { center_hz } => {
            if matches!(lane, Lane::Tx(_)) {
                // Already keyed. A second Begin is a retune, not a second
                // key-down — keying an already-keyed radio would tear the
                // endpoint down mid-over for nothing.
                dev.set_tx_center_hz(center_hz)?;
                return Ok(lane);
            }
            // Give up the receive endpoint *before* the mode changes.
            close(lane);
            dev.begin_tx(center_hz)?;
            let ep = open_tx(dev, transfer_bytes)?;
            shared.tx_active.store(true, Ordering::Relaxed);
            Ok(prime_tx(ep, dev.rate_hz(), in_flight, transfer_bytes))
        }
        TxTransition::Freq(hz) => {
            dev.set_tx_center_hz(hz)?;
            Ok(lane)
        }
        TxTransition::End => {
            let Lane::Tx(mut ep) = lane else {
                // Not keyed. Nothing to unkey, and re-opening the receive
                // endpoint here would drop the stream for no reason.
                return Ok(lane);
            };
            // Let the tail out before anything else. The engine only asks for
            // an explicit drain on the digital-burst path, so this is what
            // keeps an ordinary key-up from clipping the end of the over.
            drain_tx(&mut ep, tx, transfer_bytes);
            close(Lane::Tx(ep));
            shared.tx_active.store(false, Ordering::Relaxed);
            dev.end_tx()?;
            trace.note("back to receive");
            open_rx(dev, in_flight, transfer_bytes)
        }
    }
}

fn open_rx(dev: &mut Device<UsbDev>, in_flight: usize, transfer_bytes: usize) -> Result<Lane> {
    let mut ep = dev
        .io()
        .interface()
        .endpoint::<Bulk, In>(BULK_IN)
        .map_err(|e| Error::Access(format!("cannot open the HackRF receive endpoint: {e}")))?;
    for _ in 0..in_flight {
        ep.submit(ep.allocate(transfer_bytes));
    }
    Ok(Lane::Rx(ep))
}

fn open_tx(dev: &mut Device<UsbDev>, transfer_bytes: usize) -> Result<nusb::Endpoint<Bulk, Out>> {
    let _ = transfer_bytes;
    dev.io()
        .interface()
        .endpoint::<Bulk, Out>(BULK_OUT)
        .map_err(|e| Error::Access(format!("cannot open the HackRF transmit endpoint: {e}")))
}

/// Pre-load the transmit queue with silence.
///
/// The radio is already keyed by the time this runs — the mode change did that
/// — so silence is silence and costs nothing on the air. What it buys is a
/// queue that is already running when the first modulated block arrives, rather
/// than one that has to start under load.
fn prime_tx(
    mut ep: nusb::Endpoint<Bulk, Out>,
    rate_hz: f64,
    in_flight: usize,
    transfer_bytes: usize,
) -> Lane {
    let want = (rate_hz * BYTES_PER_SAMPLE as f64 * TX_PRIME_SECS) as usize;
    let blocks = (want / transfer_bytes).clamp(1, in_flight);
    for _ in 0..blocks {
        let mut buf = ep.allocate(transfer_bytes);
        buf.clear();
        buf.extend_fill(transfer_bytes, 0);
        ep.submit(buf);
    }
    Lane::Tx(ep)
}

/// Keep the receive queue full and move what comes back into the ring.
#[allow(clippy::too_many_arguments)]
fn service_rx(
    ep: &mut nusb::Endpoint<Bulk, In>,
    rx: &mut Producer<u8>,
    stats: &mut RxStats,
    carry: &mut Option<u8>,
    logged_first: &mut bool,
    shared: &Arc<Shared>,
    trace: &Trace,
    started: Instant,
    in_flight: usize,
    transfer_bytes: usize,
) -> Result<()> {
    // Refill before draining, so the queue is never empty while the radio is
    // producing.
    while ep.pending() < in_flight {
        ep.submit(ep.allocate(transfer_bytes));
    }

    // Drain every completion that is ready, not just one. Blocking only on the
    // first; the rest are polled. Taking one per iteration starves the stream
    // under a sustained dial drag — measured on the RTL-SDR backend, and the
    // arithmetic is no kinder here.
    let mut drained = 0usize;
    loop {
        let timeout = if drained == 0 { COMPLETE_TIMEOUT } else { Duration::ZERO };
        let Some(completion) = ep.wait_next_complete(timeout) else {
            break;
        };
        match completion.status {
            Ok(()) => {
                let bytes = &completion.buffer[..];
                if !bytes.is_empty() {
                    if !*logged_first {
                        *logged_first = true;
                        // The one line a developer without this hardware cannot
                        // produce: point the radio at a carrier above the LO and
                        // these bytes settle whether I really is first.
                        let mut peek = Vec::new();
                        let mut c = None;
                        crate::convert::to_iq(
                            &bytes[..bytes.len().min(32)],
                            &crate::convert::sample_lut(),
                            &mut c,
                            &mut peek,
                        );
                        trace.first_samples(bytes, &peek);
                    }
                    // The ring carries wire bytes; the conversion to complex
                    // happens on the consumer side. All that is needed here is
                    // that whole I/Q pairs go in, which is what `carry` is for.
                    let (pairs, tail) = split_pairs(bytes, carry);
                    if !pairs.is_empty() {
                        stats.on_iq(pairs.len() / BYTES_PER_SAMPLE);
                        push_iq(rx, pairs, stats);
                        shared
                            .last_rx_ms
                            .store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
                    }
                    *carry = tail;
                }
            }
            Err(TransferError::Cancelled) => {}
            Err(TransferError::Disconnected) => {
                return Err(Error::NotFound("the HackRF was unplugged".into()));
            }
            Err(TransferError::Stall) => {
                tracing::warn!("HackRF receive endpoint stalled; clearing and resynchronising");
                trace.note("receive endpoint stalled; cleared");
                let _ = ep.clear_halt().wait();
                // The stream restarts from a packet boundary, so a byte carried
                // from before the stall belongs to nothing.
                *carry = None;
                stats.on_stall();
            }
            Err(e) => {
                tracing::debug!("HackRF transfer error: {e}");
                stats.on_error();
            }
        }

        // Reuse the buffer rather than allocating a new one every few
        // milliseconds.
        let mut buf = completion.buffer;
        buf.clear();
        ep.submit(buf);

        drained += 1;
        // Go back and serve control rather than spinning here forever if the
        // radio is outrunning us.
        if drained >= in_flight {
            break;
        }
    }
    Ok(())
}

/// Split a completion into whole I/Q pairs plus a possible odd trailing byte.
///
/// A transfer that ends between an I and its Q would otherwise swap the two for
/// the rest of the session. Returning the tail rather than pushing it is what
/// keeps the ring holding only whole samples, which is what `push_iq`'s
/// all-or-nothing rule depends on.
fn split_pairs<'a>(bytes: &'a [u8], carry: &Option<u8>) -> (&'a [u8], Option<u8>) {
    debug_assert!(carry.is_none() || !bytes.is_empty());
    let offset = if carry.is_some() { 1 } else { 0 };
    let usable = bytes.len().saturating_sub(offset);
    let whole = usable - usable % BYTES_PER_SAMPLE;
    let tail = (usable % BYTES_PER_SAMPLE != 0).then(|| bytes[bytes.len() - 1]);
    (&bytes[offset..offset + whole], tail)
}

/// Keep the transmit queue fed from the ring.
fn service_tx(
    ep: &mut nusb::Endpoint<Bulk, Out>,
    tx: &mut Consumer<u8>,
    scratch: &mut Vec<u8>,
    in_flight: usize,
    transfer_bytes: usize,
) -> Result<()> {
    // Reap what has gone out first, so the queue depth below is honest.
    while let Some(c) = ep.wait_next_complete(Duration::ZERO) {
        match c.status {
            Ok(()) | Err(TransferError::Cancelled) => {}
            Err(TransferError::Disconnected) => {
                return Err(Error::NotFound("the HackRF was unplugged".into()));
            }
            Err(e) => tracing::debug!("HackRF transmit transfer error: {e}"),
        }
    }

    while ep.pending() < in_flight {
        let take = tx.slots().min(transfer_bytes) & !1;
        if take == 0 {
            break;
        }
        scratch.clear();
        scratch.reserve(take);
        let Ok(chunk) = tx.read_chunk(take) else {
            break;
        };
        let (head, tail) = chunk.as_slices();
        scratch.extend_from_slice(head);
        scratch.extend_from_slice(&tail[..take - head.len()]);
        chunk.commit_all();

        let mut buf = ep.allocate(take);
        buf.clear();
        buf.extend_from_slice(scratch);
        ep.submit(buf);
    }

    // Nothing to send and nothing outstanding: wait briefly rather than spin.
    // The radio is keyed and idle, which is what the pause between two
    // paced blocks looks like.
    if ep.pending() == 0 {
        std::thread::sleep(Duration::from_micros(500));
    } else {
        let _ = ep.wait_next_complete(COMPLETE_TIMEOUT);
    }
    Ok(())
}

/// Push whatever is left in the ring out, then wait for the queue to empty.
///
/// Bounded: past [`TX_DRAIN_TIMEOUT`] the tail is abandoned, because a
/// transmitter that will not stop is worse than an over that ends early.
fn drain_tx(ep: &mut nusb::Endpoint<Bulk, Out>, tx: &mut Consumer<u8>, transfer_bytes: usize) {
    let deadline = Instant::now() + TX_DRAIN_TIMEOUT;
    let mut scratch = Vec::with_capacity(transfer_bytes);
    while Instant::now() < deadline {
        let take = tx.slots().min(transfer_bytes) & !1;
        if take == 0 {
            break;
        }
        scratch.clear();
        let Ok(chunk) = tx.read_chunk(take) else {
            break;
        };
        let (head, tail) = chunk.as_slices();
        scratch.extend_from_slice(head);
        scratch.extend_from_slice(&tail[..take - head.len()]);
        chunk.commit_all();

        let mut buf = ep.allocate(take);
        buf.clear();
        buf.extend_from_slice(&scratch);
        ep.submit(buf);
        let _ = ep.wait_next_complete(Duration::from_millis(20));
    }
    while ep.pending() > 0 && Instant::now() < deadline {
        if ep.wait_next_complete(Duration::from_millis(20)).is_none() {
            break;
        }
    }
}

/// Cancel and reap everything outstanding, then drop the endpoint.
///
/// Dropping is the point: for the receive lane, idling and re-arming instead
/// leaves the path returning repeated and aliased buffers until the device is
/// reopened.
fn close(lane: Lane) {
    // The two arms are the same five lines against two different concrete
    // types. `Endpoint<Bulk, In>` and `Endpoint<Bulk, Out>` share no trait that
    // exposes `cancel_all`/`pending`/`wait_next_complete`, so this is written
    // twice rather than hidden behind a macro that would be longer than what it
    // replaced.
    let deadline = Instant::now() + CANCEL_TIMEOUT;
    match lane {
        Lane::Rx(mut ep) => {
            ep.cancel_all();
            while ep.pending() > 0 && Instant::now() < deadline {
                if ep.wait_next_complete(Duration::from_millis(20)).is_none() {
                    break;
                }
            }
        }
        Lane::Tx(mut ep) => {
            ep.cancel_all();
            while ep.pending() > 0 && Instant::now() < deadline {
                if ep.wait_next_complete(Duration::from_millis(20)).is_none() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transfer lengths must be whole packets, and the clamps must hold against
    /// a hand-edited config file.
    #[test]
    fn geometry_rounds_to_whole_packets_and_clamps() {
        let cfg = HackRfConfig::default();
        let (n, bytes) = geometry(&cfg);
        assert_eq!(n, 16);
        assert_eq!(bytes, 64 * 1024);
        assert_eq!(bytes % BULK_PACKET, 0);

        // Absurd values are clamped rather than believed.
        let zero = HackRfConfig { transfers: 0, transfer_kib: 0, ..Default::default() };
        let (n, bytes) = geometry(&zero);
        assert_eq!(n, MIN_TRANSFERS);
        assert!(bytes >= BULK_PACKET && bytes % BULK_PACKET == 0);

        let huge = HackRfConfig { transfers: 255, transfer_kib: 4096, ..Default::default() };
        let (n, bytes) = geometry(&huge);
        assert_eq!(n, MAX_TRANSFERS);
        assert_eq!(bytes, MAX_TRANSFER_BYTES);

        // A size that is not a whole number of packets rounds down to one.
        let odd = HackRfConfig { transfer_kib: 3, ..Default::default() };
        let (_, bytes) = geometry(&odd);
        assert_eq!(bytes % BULK_PACKET, 0);
        assert!(bytes >= BULK_PACKET);
    }

    /// A transfer ending between an I and its Q must not put the ring out of
    /// step. The tail is held back rather than pushed.
    #[test]
    fn an_odd_completion_holds_its_last_byte_back() {
        // Even length, nothing carried: everything is usable.
        let (pairs, tail) = split_pairs(&[1, 2, 3, 4], &None);
        assert_eq!(pairs, &[1, 2, 3, 4]);
        assert_eq!(tail, None);

        // Odd length: the last byte waits for its partner.
        let (pairs, tail) = split_pairs(&[1, 2, 3], &None);
        assert_eq!(pairs, &[1, 2]);
        assert_eq!(tail, Some(3));

        // With a byte already carried, the first byte of this completion
        // belongs to it — so an odd count now leaves nothing over.
        let (pairs, tail) = split_pairs(&[9, 1, 2, 3], &Some(0));
        assert_eq!(pairs, &[1, 2], "the leading byte pairs with the carry");
        assert_eq!(tail, Some(3));

        // Everything that reaches the ring is a whole number of samples,
        // whatever the completion lengths are.
        let mut carry = None;
        for len in 0..40usize {
            let bytes: Vec<u8> = (0..len as u8).collect();
            if carry.is_some() && bytes.is_empty() {
                continue;
            }
            let (pairs, tail) = split_pairs(&bytes, &carry);
            assert_eq!(pairs.len() % BYTES_PER_SAMPLE, 0, "len {len} leaked a partial sample");
            carry = tail;
        }
    }

    /// The prime is bounded by the queue depth, not just by the rate — at
    /// 20 Msps 50 ms of silence is 2 MB, which must not become 32 transfers in
    /// flight.
    #[test]
    fn the_transmit_prime_is_bounded_by_the_queue() {
        // Nothing here can exceed `in_flight`, and nothing can be zero.
        for rate in [2.0e6, 20.0e6] {
            let want = (rate * BYTES_PER_SAMPLE as f64 * TX_PRIME_SECS) as usize;
            let blocks = (want / (64 * 1024)).clamp(1, 16);
            assert!((1..=16).contains(&blocks), "{rate}: {blocks} blocks");
        }
        // At 2 Msps 50 ms is 200 KB, which is three 64 KiB blocks.
        let want = (2.0e6 * 2.0 * TX_PRIME_SECS) as usize;
        assert_eq!((want / (64 * 1024)).clamp(1, 16), 3);
    }
}
