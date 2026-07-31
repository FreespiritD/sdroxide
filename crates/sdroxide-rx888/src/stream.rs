//! The two threads that own the receiver.
//!
//! # Why two
//!
//! Every other USB backend in the workspace converts samples inline on the
//! thread servicing the endpoint. That works at the RTL-SDR's 4.8 MB/s. This
//! device delivers **129.6 MB/s** at the default rate, and the conversion is not
//! a byte-swap but an 8192-point FFT roughly sixteen thousand times a second. A
//! single thread would stop servicing the endpoint for the duration of each FFT,
//! and the FX3 does not wait.
//!
//! So the USB thread does as little as it possibly can — drain completions,
//! hand the buffer on, resubmit — and a second thread does the arithmetic.
//! Buffers are recycled between the two rather than allocated, because at this
//! rate the allocator is a real cost.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use nusb::MaybeFuture;
use nusb::transfer::{Bulk, In, TransferError};
use rtrb::Producer;
use sdroxide_dsp::{Complex32, WbDdc};

use crate::convert;
use crate::device::{Device, Settings};
use crate::error::{Error, Result};
use crate::handle::{Ctrl, Pending, Rx888Handle, RxStats, Shared, push_iq, ring_for};
use crate::protocol::{BULK_EP, BULK_PACKET_HS, BULK_PACKET_SS};

/// Input block size for the downconverter. 8192 gives a 7.9 kHz tuning grid at
/// 64.8 Msps, which is fine enough that the residual mixer never has far to go.
pub const DDC_BLOCK: usize = 8192;
/// Bins selected, and so the inverse FFT size. 256 of 8192 at 64.8 Msps is a
/// 2.025 Msps output, comfortably inside what the engine's own DDC expects.
pub const DDC_BINS: usize = 256;

/// Bounds on the transfer geometry, so a bad config cannot wedge the stream.
const MIN_TRANSFERS: usize = 4;
const MAX_TRANSFERS: usize = 64;
const MIN_TRANSFER_KIB: usize = 16;
const MAX_TRANSFER_KIB: usize = 1024;

/// How long the USB thread waits for a completion before serving control.
const COMPLETE_TIMEOUT: Duration = Duration::from_millis(5);

/// How many filled buffers may be in flight to the converter thread.
///
/// Small on purpose: this queue is not a buffer, it is a hand-off. If the
/// converter falls behind, the honest outcome is to drop a buffer and say so,
/// not to grow an unbounded backlog that turns into latency.
const HANDOFF_DEPTH: usize = 8;

/// Open the device and start streaming.
///
/// The device is opened *on the USB thread* so all control stays there; this
/// function blocks on a handshake so the caller still gets a synchronous
/// `Result`.
pub fn spawn(settings: &Settings, center_hz: f64) -> Result<Rx888Handle> {
    let (ctrl_tx, ctrl_rx) = crossbeam_channel::unbounded::<Ctrl>();
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<DeviceInfo>>(1);

    let shared = Arc::new(Shared {
        alive: AtomicBool::new(true),
        last_rx_ms: AtomicU64::new(0),
        out_rate_milli_hz: AtomicU64::new(0),
        vga_tenth_db: AtomicI64::new(i64::MIN),
        att_tenth_db: AtomicI64::new(i64::MIN),
        dropped: AtomicU64::new(0),
    });

    let out_rate = settings.adc_rate_hz * DDC_BINS as f64 / DDC_BLOCK as f64;
    let (rx_prod, rx_cons) = ring_for(out_rate);

    let settings = settings.clone();
    let thread_shared = Arc::clone(&shared);
    let join = std::thread::Builder::new()
        .name("sdroxide-rx888".into())
        .spawn(move || {
            run(settings, center_hz, ctrl_rx, rx_prod, Arc::clone(&thread_shared), ready_tx);
            thread_shared.alive.store(false, Ordering::Relaxed);
        })
        .map_err(|e| Error::Access(format!("could not start the RX-888 thread: {e}")))?;

    match ready_rx.recv() {
        Ok(Ok(info)) => Ok(Rx888Handle::from_parts(
            rx_cons,
            ctrl_tx,
            shared,
            join,
            info.label,
            info.serial,
            info.adc_rate_hz,
            info.out_rate_hz,
            info.bin_hz,
            info.warning,
        )),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            let _ = join.join();
            Err(Error::Access("the RX-888 thread stopped before it opened the device".into()))
        }
    }
}

struct DeviceInfo {
    label: String,
    serial: Option<String>,
    adc_rate_hz: f64,
    out_rate_hz: f64,
    bin_hz: f64,
    warning: Option<String>,
}

/// One buffer travelling between the USB thread and the converter.
struct Filled {
    buf: Vec<u8>,
}

fn run(
    settings: Settings,
    center_hz: f64,
    ctrl: Receiver<Ctrl>,
    rx: Producer<f32>,
    shared: Arc<Shared>,
    ready: crossbeam_channel::Sender<Result<DeviceInfo>>,
) {
    let mut dev = match Device::open(&settings, None) {
        Ok(d) => d,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    let adc_rate = dev.adc_rate_hz();
    let mut ddc = WbDdc::new(adc_rate, DDC_BLOCK, DDC_BINS);
    ddc.set_center_hz(center_hz);
    let out_rate = ddc.out_rate();
    shared.out_rate_milli_hz.store((out_rate * 1000.0) as u64, Ordering::Relaxed);

    let _ = ready.send(Ok(DeviceInfo {
        label: dev.label().to_string(),
        serial: dev.serial().map(str::to_string),
        adc_rate_hz: adc_rate,
        out_rate_hz: out_rate,
        bin_hz: ddc.bin_hz(),
        warning: dev.warning().map(str::to_string),
    }));

    tracing::info!(
        "RX-888 streaming: {:.3} Msps real in -> {:.4} Msps complex out ({:.1} MB/s over USB)",
        adc_rate / 1e6,
        out_rate / 1e6,
        adc_rate * 2.0 / 1e6,
    );

    // Hand-off queues: `full` carries data to the converter, `empty` returns
    // the buffers so nothing is allocated in the steady state.
    let (full_tx, full_rx) = crossbeam_channel::bounded::<Filled>(HANDOFF_DEPTH);
    let (empty_tx, empty_rx) = crossbeam_channel::bounded::<Vec<u8>>(HANDOFF_DEPTH + 4);

    let conv_shared = Arc::clone(&shared);
    let randomized = dev.randomized();
    let (ddc_ctrl_tx, ddc_ctrl_rx) = crossbeam_channel::unbounded::<f64>();
    let converter =
        std::thread::Builder::new().name("sdroxide-rx888-ddc".into()).spawn(move || {
            convert_loop(ddc, randomized, full_rx, empty_tx, rx, conv_shared, ddc_ctrl_rx);
        });
    let converter = match converter {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("could not start the RX-888 converter thread: {e}");
            return;
        }
    };

    if let Err(e) = pump(&mut dev, &ctrl, &full_tx, &empty_rx, &ddc_ctrl_tx, &settings) {
        tracing::warn!("RX-888 stream stopped: {e}");
    }

    // Dropping the sender ends the converter's loop.
    drop(full_tx);
    let _ = converter.join();
    dev.shutdown();
}

/// Transfer geometry, clamped and rounded to something the endpoint accepts.
fn geometry(packet: usize) -> (usize, usize) {
    let transfers = 16usize.clamp(MIN_TRANSFERS, MAX_TRANSFERS);
    let kib = 256usize.clamp(MIN_TRANSFER_KIB, MAX_TRANSFER_KIB);
    // A transfer length that is not a whole number of packets is rejected.
    let bytes = (kib * 1024 / packet).max(1) * packet;
    (transfers, bytes)
}

fn pump(
    dev: &mut Device,
    ctrl: &Receiver<Ctrl>,
    full: &Sender<Filled>,
    empty: &Receiver<Vec<u8>>,
    ddc_ctrl: &Sender<f64>,
    settings: &Settings,
) -> Result<()> {
    let packet = match dev.usb().speed() {
        Some(nusb::Speed::Super | nusb::Speed::SuperPlus) => BULK_PACKET_SS,
        _ => BULK_PACKET_HS,
    };
    let (in_flight, xfer_bytes) = geometry(packet);

    // STARTFX3 before the endpoint exists — see `Device::start`.
    dev.start()?;

    let mut ep = dev
        .usb()
        .interface()
        .endpoint::<Bulk, In>(BULK_EP)
        .map_err(|e| Error::Access(format!("cannot open the RX-888 bulk endpoint: {e}")))?;

    for _ in 0..in_flight {
        ep.submit(ep.allocate(xfer_bytes));
    }

    let mut settings = settings.clone();
    let started = Instant::now();
    let mut overruns = 0u64;

    loop {
        // 1. Collapse the control channel and apply each field once.
        let mut pending = Pending::default();
        while let Ok(c) = ctrl.try_recv() {
            pending.absorb(c);
        }
        if pending.shutdown {
            break;
        }
        if !pending.is_empty() {
            while ep.pending() < in_flight {
                ep.submit(ep.allocate(xfer_bytes));
            }
            apply(dev, &pending, &mut settings, ddc_ctrl);
        }

        // 2. Refill before draining, so the queue is never empty while the
        //    device is producing.
        while ep.pending() < in_flight {
            ep.submit(ep.allocate(xfer_bytes));
        }

        // 3. Drain every ready completion, not just one.
        let mut drained = 0usize;
        loop {
            let timeout = if drained == 0 { COMPLETE_TIMEOUT } else { Duration::ZERO };
            let Some(completion) = ep.wait_next_complete(timeout) else {
                break;
            };
            match completion.status {
                Ok(()) => {
                    let data = &completion.buffer[..];
                    if !data.is_empty() {
                        let mut buf = empty.try_recv().unwrap_or_default();
                        buf.clear();
                        buf.extend_from_slice(data);
                        match full.try_send(Filled { buf }) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => {
                                // The converter is behind. Dropping here is
                                // better than stalling the endpoint.
                                overruns += 1;
                                if overruns.is_power_of_two() {
                                    tracing::warn!(
                                        "RX-888: converter behind, dropped {overruns} buffers"
                                    );
                                }
                            }
                            Err(TrySendError::Disconnected(_)) => {
                                return Ok(());
                            }
                        }
                    }
                }
                Err(TransferError::Cancelled) => {}
                Err(TransferError::Disconnected) => {
                    return Err(Error::NotFound("the RX-888 was unplugged".into()));
                }
                Err(TransferError::Stall) => {
                    tracing::warn!("RX-888 endpoint stalled; clearing and resynchronising");
                    let _ = ep.clear_halt().wait();
                }
                Err(e) => tracing::debug!("RX-888 transfer error: {e}"),
            }

            let mut buf = completion.buffer;
            buf.clear();
            ep.submit(buf);

            drained += 1;
            if drained >= in_flight {
                break;
            }
        }
        let _ = started;
    }

    ep.cancel_all();
    let deadline = Instant::now() + Duration::from_millis(500);
    while ep.pending() > 0 && Instant::now() < deadline {
        if ep.wait_next_complete(Duration::from_millis(50)).is_none() {
            break;
        }
    }
    let _ = dev.stop();
    Ok(())
}

fn apply(dev: &mut Device, p: &Pending, settings: &mut Settings, ddc_ctrl: &Sender<f64>) {
    // Retuning is the converter's business: there is no hardware LO to move.
    if let Some(hz) = p.center {
        let _ = ddc_ctrl.send(hz);
    }
    if let Some(db) = p.vga {
        settings.vga_db = db;
        if let Err(e) = dev.set_vga_db(db) {
            tracing::warn!("RX-888: {e}");
        }
    }
    if let Some(db) = p.att {
        settings.attenuator_db = db;
        if let Err(e) = dev.set_attenuator_db(db) {
            tracing::warn!("RX-888: {e}");
        }
    }
    if let Some(on) = p.dither {
        settings.dither = on;
        if let Err(e) = dev.set_dither(on) {
            tracing::warn!("RX-888: {e}");
        }
    }
    if let Some(on) = p.bias_tee {
        settings.bias_tee_hf = on;
        if let Err(e) = dev.set_bias_tee(on) {
            tracing::warn!("RX-888: {e}");
        }
    }
    if let Some(on) = p.pga {
        settings.pga = on;
        if let Err(e) = dev.set_pga(on) {
            tracing::warn!("RX-888: {e}");
        }
    }
}

/// The arithmetic half: bytes in, complex baseband out.
#[allow(clippy::too_many_arguments)]
fn convert_loop(
    mut ddc: WbDdc,
    randomized: bool,
    full: Receiver<Filled>,
    empty: Sender<Vec<u8>>,
    mut rx: Producer<f32>,
    shared: Arc<Shared>,
    retune: Receiver<f64>,
) {
    let mut stats = RxStats::new(ddc.out_rate());
    let mut carry: Option<u8> = None;
    let mut real: Vec<f32> = Vec::with_capacity(1 << 20);
    let mut cplx: Vec<Complex32> = Vec::with_capacity(1 << 16);
    let mut inter: Vec<f32> = Vec::with_capacity(1 << 17);
    let started = Instant::now();

    while let Ok(filled) = full.recv() {
        // Last retune wins, and it is free: no hardware is involved.
        let mut want = None;
        while let Ok(hz) = retune.try_recv() {
            want = Some(hz);
        }
        if let Some(hz) = want {
            ddc.set_center_hz(hz);
        }

        convert::to_f32(&filled.buf, randomized, &mut carry, &mut real);
        cplx.clear();
        ddc.process(&real, &mut cplx);

        if !cplx.is_empty() {
            inter.clear();
            inter.reserve(cplx.len() * 2);
            for v in &cplx {
                inter.push(v.re);
                inter.push(v.im);
            }
            let dropped = push_iq(&mut rx, &inter, &mut stats);
            if dropped > 0 {
                shared.dropped.fetch_add(dropped as u64, Ordering::Relaxed);
            }
            shared.last_rx_ms.store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
        }

        // Give the buffer back. A full return queue just means the USB thread
        // has plenty, so dropping it is harmless.
        let _ = empty.try_send(filled.buf);
        stats.tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_lengths_are_whole_packets() {
        for packet in [BULK_PACKET_HS, BULK_PACKET_SS] {
            let (n, bytes) = geometry(packet);
            assert!((MIN_TRANSFERS..=MAX_TRANSFERS).contains(&n));
            assert_eq!(bytes % packet, 0, "a partial packet is rejected outright");
            assert!(bytes >= MIN_TRANSFER_KIB * 1024);
        }
    }

    #[test]
    fn the_ddc_geometry_gives_the_expected_output_rate() {
        // 64.8 Msps real in, 256 of 8192 bins out.
        let out = 64.8e6 * DDC_BINS as f64 / DDC_BLOCK as f64;
        assert!((out - 2_025_000.0).abs() < 1.0, "{out}");
    }
}
