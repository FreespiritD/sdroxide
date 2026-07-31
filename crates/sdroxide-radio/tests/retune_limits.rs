//! A tune the front end cannot take must not cost the operator their radio.
//!
//! The report this comes from: a LimeSDR tuned below its range answered
//! "SoapyLMS7::setFrequency() failed", and from then on the only way back was
//! restarting the server. Two things went wrong. The dial stayed on a frequency
//! the hardware could not receive, so every later retune tried the same
//! impossible tune again; and the failure was announced as a lost connection,
//! which every attached UI renders as a dead radio with nothing to click.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use sdroxide_radio::{Complex32, EngineConfig, IqSource, RadioError, Result, start_engine};
use sdroxide_types::{Command, DeviceCaps, RadioEvent, Vfo};

const RATE: f64 = 48_000.0;
const CENTER: f64 = 14_100_000.0;
/// Outside every range below — the LimeSDR's 4.584 MHz, in spirit.
const TOO_LOW: f64 = 4_584_000.0;

/// A front end that accepts every tune, and records where it was sent.
struct Tunable {
    center_hz: f64,
    rate_hz: f64,
    lo_offset_hz: f64,
    /// Set if the source is ever asked for a frequency it cannot receive; the
    /// engine is supposed to spare it the call entirely.
    asked_out_of_range: Arc<AtomicBool>,
    /// Where the last accepted tune put the LO.
    landed: Arc<Mutex<f64>>,
    /// Whether the hardware refuses tunes away from where it started.
    refuses: bool,
}

impl Tunable {
    fn new(asked_out_of_range: &Arc<AtomicBool>) -> Self {
        Tunable {
            center_hz: CENTER,
            rate_hz: RATE,
            lo_offset_hz: 0.0,
            asked_out_of_range: Arc::clone(asked_out_of_range),
            landed: Arc::new(Mutex::new(CENTER)),
            refuses: false,
        }
    }
}

impl IqSource for Tunable {
    fn sample_rate(&self) -> f64 {
        self.rate_hz
    }
    fn center_hz(&self) -> f64 {
        self.center_hz
    }
    fn lo_offset_hz(&self) -> f64 {
        self.lo_offset_hz
    }
    fn set_center_hz(&mut self, hz: f64) -> Result<()> {
        if !(10_000_000.0..=60_000_000.0).contains(&hz) {
            self.asked_out_of_range.store(true, Ordering::SeqCst);
        }
        if self.refuses && (hz - CENTER).abs() > 1.0 {
            return Err(RadioError::Msg("SoapyLMS7::setFrequency() failed".into()));
        }
        self.center_hz = hz;
        *self.landed.lock().unwrap() = hz;
        Ok(())
    }
    fn read(&mut self, buf: &mut [Complex32]) -> Result<usize> {
        std::thread::sleep(Duration::from_millis(5));
        let n = buf.len().min(256);
        buf[..n].fill(Complex32::new(0.0, 0.0));
        Ok(n)
    }
    fn describe(&self) -> String {
        "test front end".into()
    }
}

fn caps(rx_ranges: Vec<(f64, f64)>) -> DeviceCaps {
    DeviceCaps {
        driver: "test".into(),
        label: "test".into(),
        rx_channels: 1,
        sample_rates: vec![RATE],
        freq_ranges_rx: rx_ranges,
        ..DeviceCaps::default()
    }
}

/// Collect events for `secs`, returning the notices seen, any lost-connection
/// report, and where the dial ended up.
fn drain(rx: &Receiver<RadioEvent>, secs: f64) -> (Vec<String>, Option<String>, f64) {
    let mut notices = Vec::new();
    let mut lost = None;
    let mut dial = f64::NAN;
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < deadline {
        while let Ok(ev) = rx.try_recv() {
            match ev {
                RadioEvent::Notice(Some(n)) => notices.push(n),
                RadioEvent::ConnectionLost(e) => lost = Some(e),
                RadioEvent::State(s) => dial = s.active_freq_hz(),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    (notices, lost, dial)
}

/// The driver is never asked for a frequency its own capabilities rule out —
/// that call is exactly the one that leaves a LimeSDR unusable.
#[test]
fn a_tune_outside_the_advertised_range_never_reaches_the_driver() {
    let asked = Arc::new(AtomicBool::new(false));
    let source = Tunable::new(&asked);
    let mut h = start_engine(
        Box::new(source),
        caps(vec![(10_000_000.0, 60_000_000.0)]),
        EngineConfig::default(),
    );
    let thread = h.thread.take();

    h.cmd_tx.send(Command::SetVfo { vfo: Vfo::A, hz: TOO_LOW }).unwrap();
    let (notices, lost, dial) = drain(&h.event_rx, 1.0);

    assert!(!asked.load(Ordering::SeqCst), "the driver was asked to tune out of its range");
    assert_eq!(lost, None, "a refused tune is not a lost connection");
    assert!(
        notices.iter().any(|n| n.contains("outside") && n.contains("4.584")),
        "the operator should be told why, got {notices:?}"
    );
    assert!(
        (dial - CENTER).abs() < 1.0,
        "the dial should be back on the last frequency that worked, not {dial}"
    );

    drop(h.cmd_tx);
    if let Some(t) = thread {
        let _ = t.join();
    }
}

/// A driver that refuses a tune it was allowed to attempt (the range it
/// advertises is wider than what it can really do) leaves the operator on a
/// working frequency, with the session intact.
#[test]
fn a_driver_that_refuses_a_tune_puts_the_dial_back() {
    let asked = Arc::new(AtomicBool::new(false));
    let source = Tunable { refuses: true, ..Tunable::new(&asked) };
    // Wide open: nothing stops the request reaching the driver here.
    let mut h = start_engine(Box::new(source), caps(vec![(0.0, 6e9)]), EngineConfig::default());
    let thread = h.thread.take();

    // Far enough to leave the 48 kHz span, so the engine has to retune for it.
    h.cmd_tx.send(Command::SetVfo { vfo: Vfo::A, hz: CENTER + 500_000.0 }).unwrap();
    let (notices, lost, dial) = drain(&h.event_rx, 1.0);

    assert_eq!(lost, None, "a refused tune must not read as a dead radio");
    assert!(
        notices.iter().any(|n| n.contains("refused")),
        "the operator should be told the radio refused, got {notices:?}"
    );
    assert!((dial - CENTER).abs() < 1.0, "the dial should have gone back to {CENTER}, not {dial}");

    // And the engine is still running: a tune it can take still works.
    h.cmd_tx.send(Command::SetVfo { vfo: Vfo::A, hz: CENTER + 5_000.0 }).unwrap();
    let (_, lost, dial) = drain(&h.event_rx, 0.5);
    assert_eq!(lost, None);
    assert!((dial - (CENTER + 5_000.0)).abs() < 1.0, "still tunable within the span, got {dial}");

    drop(h.cmd_tx);
    if let Some(t) = thread {
        let _ = t.join();
    }
}

/// A front end that parks its LO clear of the VFO can still work the top of its
/// range: the LO goes to the mirror position rather than off the end, and the
/// operator keeps the frequency they asked for.
#[test]
fn an_offset_lo_flips_below_the_vfo_at_the_top_of_the_range() {
    const TOP: f64 = 1_766_000_000.0;
    const SPAN: f64 = 2_400_000.0;
    const OFFSET: f64 = 600_000.0;
    // Close enough to the ceiling that the LO cannot be parked above it.
    let dial = TOP - OFFSET / 2.0;

    let asked = Arc::new(AtomicBool::new(false));
    let source = Tunable {
        center_hz: 100_000_000.0,
        rate_hz: SPAN,
        lo_offset_hz: OFFSET,
        ..Tunable::new(&asked)
    };
    let landed = Arc::clone(&source.landed);
    let mut h =
        start_engine(Box::new(source), caps(vec![(24_000_000.0, TOP)]), EngineConfig::default());
    let thread = h.thread.take();

    h.cmd_tx.send(Command::SetVfo { vfo: Vfo::A, hz: dial }).unwrap();
    let (notices, lost, ended_on) = drain(&h.event_rx, 1.0);

    assert_eq!(lost, None);
    assert!(notices.is_empty(), "this frequency is reachable, got {notices:?}");
    assert!((ended_on - dial).abs() < 1.0, "the dial should have stayed on {dial}, not {ended_on}");
    let lo = *landed.lock().unwrap();
    assert!((lo - (dial - OFFSET)).abs() < 1.0, "the LO should sit below the VFO, at {lo}");

    drop(h.cmd_tx);
    if let Some(t) = thread {
        let _ = t.join();
    }
}
