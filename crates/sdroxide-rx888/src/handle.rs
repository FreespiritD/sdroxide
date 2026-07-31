//! The handle the rest of the program holds, and the accounting behind it.
//!
//! Same shape as the RTL-SDR and HPSDR backends: blocking threads own the
//! device, control goes in over a crossbeam channel, samples come back out
//! through an `rtrb` ring of interleaved `f32`.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use rtrb::{Consumer, Producer, RingBuffer};

/// How often the stream thread emits a throughput line.
const STATS_INTERVAL: Duration = Duration::from_secs(5);

/// A control message for the stream thread.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Ctrl {
    Center(f64),
    Vga(f64),
    Att(f64),
    Dither(bool),
    BiasTee(bool),
    Pga(bool),
    Shutdown,
}

/// Control messages accumulated over one pass of the thread loop.
///
/// Dragging the panadapter emits hundreds of `Center` messages a second. Unlike
/// the RTL-SDR, retuning here is nearly free — it is a change of FFT bin, not an
/// I2C transaction — but the coalescing still matters: applying a hundred
/// retunes per pass would reset the downconverter's block phase a hundred times.
/// Last value wins, which is the right semantics for a dial.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Pending {
    pub center: Option<f64>,
    pub vga: Option<f64>,
    pub att: Option<f64>,
    pub dither: Option<bool>,
    pub bias_tee: Option<bool>,
    pub pga: Option<bool>,
    pub shutdown: bool,
}

impl Pending {
    pub(crate) fn absorb(&mut self, c: Ctrl) {
        match c {
            Ctrl::Center(v) => self.center = Some(v),
            Ctrl::Vga(v) => self.vga = Some(v),
            Ctrl::Att(v) => self.att = Some(v),
            Ctrl::Dither(v) => self.dither = Some(v),
            Ctrl::BiasTee(v) => self.bias_tee = Some(v),
            Ctrl::Pga(v) => self.pga = Some(v),
            Ctrl::Shutdown => self.shutdown = true,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        *self == Pending::default()
    }
}

/// State the stream thread publishes for the handle to read.
pub(crate) struct Shared {
    pub alive: AtomicBool,
    pub last_rx_ms: AtomicU64,
    /// Achieved output rate, in millihertz, so the adapter can report what the
    /// downconverter actually produces rather than what was asked for.
    pub out_rate_milli_hz: AtomicU64,
    /// Gains the hardware settled on, in tenths of a dB; `i64::MIN` for unset.
    pub vga_tenth_db: AtomicI64,
    pub att_tenth_db: AtomicI64,
    /// Samples the ring could not take because the consumer fell behind.
    pub dropped: AtomicU64,
    /// Latest full-band spectrum frame, in dBFS ascending from DC.
    ///
    /// Latest-wins rather than queued: a display that falls behind wants the
    /// newest picture, not a backlog of stale ones. A mutex is fine here — it is
    /// taken twenty times a second, not per sample.
    pub wide: Mutex<Option<Vec<f32>>>,
}

/// Throughput and health accounting.
pub(crate) struct RxStats {
    nominal_hz: f64,
    since: Instant,
    win_samples: u64,
    win_dropped: u64,
}

impl RxStats {
    pub(crate) fn new(nominal_hz: f64) -> RxStats {
        RxStats { nominal_hz, since: Instant::now(), win_samples: 0, win_dropped: 0 }
    }

    pub(crate) fn on_iq(&mut self, samples: usize) {
        self.win_samples += samples as u64;
    }

    pub(crate) fn on_dropped(&mut self, samples: usize) {
        self.win_dropped += samples as u64;
    }

    /// Emit a periodic line and reset the window.
    pub(crate) fn tick(&mut self) {
        let elapsed = self.since.elapsed();
        if elapsed < STATS_INTERVAL {
            return;
        }
        let secs = elapsed.as_secs_f64();
        let rate = self.win_samples as f64 / secs;
        if self.win_dropped > 0 {
            tracing::warn!(
                "RX-888: {:.3} Msps out ({:.1}% of nominal), {} samples dropped",
                rate / 1e6,
                100.0 * rate / self.nominal_hz,
                self.win_dropped,
            );
        } else {
            tracing::debug!(
                "RX-888: {:.3} Msps out ({:.1}% of nominal)",
                rate / 1e6,
                100.0 * rate / self.nominal_hz
            );
        }
        self.since = Instant::now();
        self.win_samples = 0;
        self.win_dropped = 0;
    }
}

/// Push interleaved complex samples into the ring, counting what will not fit.
///
/// Dropping is the right failure here: the alternative is blocking the thread
/// that is servicing USB, which turns a brief consumer stall into a hardware
/// overrun and loses far more than the samples that would not fit.
pub(crate) fn push_iq(ring: &mut Producer<f32>, data: &[f32], stats: &mut RxStats) -> usize {
    let mut written = 0;
    if let Ok(mut chunk) = ring.write_chunk_uninit(data.len().min(ring.slots())) {
        let (a, b) = chunk.as_mut_slices();
        let split = a.len();
        for (dst, src) in a.iter_mut().zip(&data[..split.min(data.len())]) {
            dst.write(*src);
        }
        if data.len() > split {
            for (dst, src) in b.iter_mut().zip(&data[split..]) {
                dst.write(*src);
            }
        }
        written = a.len() + b.len();
        unsafe { chunk.commit_all() };
    }
    stats.on_iq(written / 2);
    let dropped = (data.len() - written) / 2;
    if dropped > 0 {
        stats.on_dropped(dropped);
    }
    dropped
}

/// Ring sized for half a second of interleaved `f32` at `rate`, rounded up to a
/// power of two.
pub(crate) fn ring_for(rate: f64) -> (Producer<f32>, Consumer<f32>) {
    let slots = ((rate * 2.0 * 0.5) as usize).next_power_of_two().clamp(1 << 14, 1 << 24);
    RingBuffer::new(slots)
}

/// A running RX-888.
pub struct Rx888Handle {
    rx: Consumer<f32>,
    ctrl: Sender<Ctrl>,
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
    label: String,
    serial: Option<String>,
    adc_rate_hz: f64,
    out_rate_hz: f64,
    bin_hz: f64,
    warning: Option<String>,
    opened_at: Instant,
    released: bool,
}

impl Rx888Handle {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts(
        rx: Consumer<f32>,
        ctrl: Sender<Ctrl>,
        shared: Arc<Shared>,
        join: JoinHandle<()>,
        label: String,
        serial: Option<String>,
        adc_rate_hz: f64,
        out_rate_hz: f64,
        bin_hz: f64,
        warning: Option<String>,
    ) -> Rx888Handle {
        Rx888Handle {
            rx,
            ctrl,
            shared,
            join: Some(join),
            label,
            serial,
            adc_rate_hz,
            out_rate_hz,
            bin_hz,
            warning,
            opened_at: Instant::now(),
            released: false,
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    /// The ADC clock, i.e. the real-sample rate on the wire.
    pub fn adc_rate_hz(&self) -> f64 {
        self.adc_rate_hz
    }

    /// The complex baseband rate delivered to the caller.
    pub fn out_rate_hz(&self) -> f64 {
        let milli = self.shared.out_rate_milli_hz.load(Ordering::Relaxed);
        if milli > 0 { milli as f64 / 1000.0 } else { self.out_rate_hz }
    }

    /// Coarse tuning grid, in Hz.
    pub fn bin_hz(&self) -> f64 {
        self.bin_hz
    }

    /// Centre and span of the full-band spectrum, in Hz. A real ADC stream
    /// covers DC to Nyquist, so this is a quarter and a half of the ADC clock.
    pub fn wide_span_hz(&self) -> (f64, f64) {
        (self.adc_rate_hz / 4.0, self.adc_rate_hz / 2.0)
    }

    /// Take the newest full-band spectrum frame, if one is waiting.
    pub fn take_wide_spectrum(&self) -> Option<Vec<f32>> {
        self.shared.wide.lock().ok().and_then(|mut g| g.take())
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// Whether the stream thread is still running.
    pub fn is_alive(&self) -> bool {
        !self.released && self.shared.alive.load(Ordering::Relaxed)
    }

    /// Complex samples the ring could not take because the consumer fell
    /// behind. Non-zero here means the DSP thread is not keeping up, which is a
    /// different fault from the converter falling behind the USB endpoint.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// How long the receiver has delivered nothing.
    ///
    /// `last_rx_ms` is a *timestamp* measured from when the stream thread
    /// started, not an age, so it has to be subtracted from the elapsed time
    /// rather than used directly — reading it as an age reports a perfectly
    /// healthy receiver as dead a few seconds after it starts.
    pub fn silent_for(&self) -> Duration {
        let since_open = self.opened_at.elapsed();
        let last = Duration::from_millis(self.shared.last_rx_ms.load(Ordering::Relaxed));
        since_open.saturating_sub(last)
    }

    /// The gain the hardware settled on, if it has reported one yet.
    pub fn effective_vga_db(&self) -> Option<f64> {
        match self.shared.vga_tenth_db.load(Ordering::Relaxed) {
            i64::MIN => None,
            v => Some(v as f64 / 10.0),
        }
    }

    pub fn effective_att_db(&self) -> Option<f64> {
        match self.shared.att_tenth_db.load(Ordering::Relaxed) {
            i64::MIN => None,
            v => Some(v as f64 / 10.0),
        }
    }

    /// Read interleaved complex samples. Returns how many `f32` were taken.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        let n = out.len().min(self.rx.slots());
        // Keep I and Q together: an odd count would swap them for good.
        let n = n - (n % 2);
        if n == 0 {
            return 0;
        }
        match self.rx.read_chunk(n) {
            Ok(chunk) => {
                let (a, b) = chunk.as_slices();
                out[..a.len()].copy_from_slice(a);
                out[a.len()..a.len() + b.len()].copy_from_slice(b);
                chunk.commit_all();
                n
            }
            Err(_) => 0,
        }
    }

    pub fn set_center_hz(&self, hz: f64) {
        let _ = self.ctrl.send(Ctrl::Center(hz));
    }

    pub fn set_vga_db(&self, db: f64) {
        let _ = self.ctrl.send(Ctrl::Vga(db));
    }

    pub fn set_att_db(&self, db: f64) {
        let _ = self.ctrl.send(Ctrl::Att(db));
    }

    pub fn set_dither(&self, on: bool) {
        let _ = self.ctrl.send(Ctrl::Dither(on));
    }

    pub fn set_bias_tee(&self, on: bool) {
        let _ = self.ctrl.send(Ctrl::BiasTee(on));
    }

    pub fn set_pga(&self, on: bool) {
        let _ = self.ctrl.send(Ctrl::Pga(on));
    }

    /// Stop the thread and let go of the device.
    ///
    /// Idempotent, and leaves the handle callable but inert — the engine's
    /// reopen path requires exactly that, because a replacement is built only
    /// after the outgoing source has stood down.
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let _ = self.ctrl.send(Ctrl::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        self.shared.alive.store(false, Ordering::Relaxed);
    }
}

impl Drop for Rx888Handle {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_keeps_only_the_last_value_per_field() {
        let mut p = Pending::default();
        assert!(p.is_empty());
        p.absorb(Ctrl::Center(1.0));
        p.absorb(Ctrl::Center(2.0));
        p.absorb(Ctrl::Vga(10.0));
        assert_eq!(p.center, Some(2.0));
        assert_eq!(p.vga, Some(10.0));
        assert_eq!(p.att, None);
        assert!(!p.is_empty());
    }

    #[test]
    fn shutdown_survives_later_messages() {
        let mut p = Pending::default();
        p.absorb(Ctrl::Shutdown);
        p.absorb(Ctrl::Center(1.0));
        assert!(p.shutdown, "a shutdown must not be forgotten by a later retune");
    }

    #[test]
    fn the_ring_holds_about_half_a_second() {
        let (p, _c) = ring_for(2_025_000.0);
        // Half a second of interleaved f32 is 2.025 M slots; the next power of
        // two is 4 Mi, and it must be comfortably more than a single block.
        assert!(p.slots() >= 2_025_000);
        assert!(p.slots().is_power_of_two());
    }

    #[test]
    fn the_ring_is_clamped_for_absurd_rates() {
        let (p, _c) = ring_for(1e12);
        assert!(p.slots() <= 1 << 24);
        let (p, _c) = ring_for(1.0);
        assert!(p.slots() >= 1 << 14);
    }

    #[test]
    fn silence_is_an_age_not_a_timestamp() {
        let (_p, cons) = RingBuffer::<f32>::new(16);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared {
            alive: AtomicBool::new(true),
            last_rx_ms: AtomicU64::new(0),
            out_rate_milli_hz: AtomicU64::new(0),
            vga_tenth_db: AtomicI64::new(i64::MIN),
            att_tenth_db: AtomicI64::new(i64::MIN),
            dropped: AtomicU64::new(0),
            wide: Mutex::new(None),
        });
        let join = std::thread::spawn(|| {});
        let mut h = Rx888Handle::from_parts(
            cons,
            tx,
            shared,
            join,
            "test".into(),
            None,
            64.8e6,
            2.025e6,
            7910.0,
            None,
        );
        // A receiver that delivered a sample "just now" has near-zero silence,
        // however long the handle has been open. Reading the raw timestamp
        // instead would grow without bound and trip the reopen threshold.
        let age = h.opened_at.elapsed();
        h.shared.last_rx_ms.store(age.as_millis() as u64, Ordering::Relaxed);
        assert!(h.silent_for() < Duration::from_millis(50), "{:?}", h.silent_for());
        h.released = true;
    }

    #[test]
    fn reads_never_split_an_iq_pair() {
        let (mut prod, cons) = RingBuffer::<f32>::new(64);
        for i in 0..9 {
            prod.push(i as f32).unwrap();
        }
        let (tx, _rx) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared {
            alive: AtomicBool::new(true),
            last_rx_ms: AtomicU64::new(0),
            out_rate_milli_hz: AtomicU64::new(0),
            vga_tenth_db: AtomicI64::new(i64::MIN),
            att_tenth_db: AtomicI64::new(i64::MIN),
            dropped: AtomicU64::new(0),
            wide: Mutex::new(None),
        });
        let join = std::thread::spawn(|| {});
        let mut h = Rx888Handle::from_parts(
            cons,
            tx,
            shared,
            join,
            "test".into(),
            None,
            64.8e6,
            2.025e6,
            7910.0,
            None,
        );
        let mut buf = [0f32; 32];
        // Nine samples are available; an even count must come back.
        let n = h.read(&mut buf);
        assert_eq!(n % 2, 0, "read {n} f32, which splits an I/Q pair");
        assert_eq!(n, 8);
        h.released = true; // do not try to join in Drop
    }
}
