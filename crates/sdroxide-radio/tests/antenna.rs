//! Antenna selection: a multi-port front end (a LimeSDR receives on
//! LNAH/LNAL/LNAW and transmits on BAND1/BAND2) must end up on the port the
//! operator asked for — at start, when they switch it, and again after a
//! reconnect, which reopens the device on whatever its driver defaults to.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sdroxide_radio::{Complex32, EngineConfig, IqSource, Result, start_engine};
use sdroxide_types::{Command, DeviceCaps, Direction, RadioEvent};

const RATE: f64 = 48_000.0;
const CENTER: f64 = 14_100_000.0;

/// The ports a front end is on, shared with the test so it can see what the
/// engine actually asked the hardware for — as opposed to what the published
/// state says, which is only the readback of it.
#[derive(Clone)]
struct Ports {
    rx: Arc<Mutex<String>>,
    tx: Arc<Mutex<String>>,
    /// Every selection the engine made, in order, tagged with its direction.
    asked: Arc<Mutex<Vec<(Direction, String)>>>,
}

impl Ports {
    /// A device that powers up on its first port, as a driver does.
    fn fresh() -> Self {
        Ports {
            rx: Arc::new(Mutex::new("LNAH".into())),
            tx: Arc::new(Mutex::new("BAND1".into())),
            asked: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn rx(&self) -> String {
        self.rx.lock().unwrap().clone()
    }

    fn tx(&self) -> String {
        self.tx.lock().unwrap().clone()
    }

    fn asked(&self) -> Vec<(Direction, String)> {
        self.asked.lock().unwrap().clone()
    }
}

struct Rig {
    ports: Ports,
}

impl IqSource for Rig {
    fn sample_rate(&self) -> f64 {
        RATE
    }
    fn center_hz(&self) -> f64 {
        CENTER
    }
    fn set_center_hz(&mut self, _hz: f64) -> Result<()> {
        Ok(())
    }
    fn read(&mut self, buf: &mut [Complex32]) -> Result<usize> {
        std::thread::sleep(Duration::from_millis(5));
        let n = buf.len().min(256);
        buf[..n].fill(Complex32::new(0.0, 0.0));
        Ok(n)
    }
    fn describe(&self) -> String {
        "multi-port rig".into()
    }
    fn set_antenna(&mut self, name: &str) -> Result<()> {
        self.ports.asked.lock().unwrap().push((Direction::Rx, name.into()));
        *self.ports.rx.lock().unwrap() = name.into();
        Ok(())
    }
    fn current_antenna(&self) -> String {
        self.ports.rx()
    }
    fn set_tx_antenna(&mut self, name: &str) -> Result<()> {
        self.ports.asked.lock().unwrap().push((Direction::Tx, name.into()));
        *self.ports.tx.lock().unwrap() = name.into();
        Ok(())
    }
    fn current_tx_antenna(&self) -> String {
        self.ports.tx()
    }
}

fn caps() -> DeviceCaps {
    DeviceCaps {
        driver: "lime".into(),
        label: "LimeSDR".into(),
        rx_channels: 1,
        tx_channels: 1,
        sample_rates: vec![RATE],
        freq_ranges_rx: vec![(0.0, 60_000_000.0)],
        freq_ranges_tx: vec![(0.0, 60_000_000.0)],
        antennas_rx: vec!["LNAH".into(), "LNAL".into(), "LNAW".into()],
        antennas_tx: vec!["BAND1".into(), "BAND2".into()],
        ..DeviceCaps::default()
    }
}

/// Wait for a published state that satisfies `want`, so the assertions are
/// about what every UI sees rather than about internal timing.
fn wait_for_state(
    rx: &crossbeam_channel::Receiver<RadioEvent>,
    want: impl Fn(&sdroxide_types::RadioState) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(RadioEvent::State(s)) if want(&s) => return true,
            Ok(_) => {}
            Err(_) => {}
        }
    }
    false
}

/// `--antenna` / `--tx-antenna` on a headless server: nobody is at the machine
/// to pick a port, so the one named on the command line has to be selected as
/// the engine comes up.
#[test]
fn the_startup_antenna_is_selected_on_the_front_end() {
    let ports = Ports::fresh();
    let cfg = EngineConfig {
        initial_antenna: (Some("LNAW".into()), Some("BAND2".into())),
        ..Default::default()
    };
    let mut h = start_engine(Box::new(Rig { ports: ports.clone() }), caps(), cfg);
    let thread = h.thread.take();

    assert!(
        wait_for_state(&h.event_rx, |s| s.antenna_rx == "LNAW" && s.antenna_tx == "BAND2"),
        "the opening state must show the ports that were asked for, not the driver's defaults"
    );
    assert_eq!(ports.rx(), "LNAW", "the RX port reached the hardware");
    assert_eq!(ports.tx(), "BAND2", "the TX port reached the hardware");

    drop(h);
    let _ = thread.map(|t| t.join());
}

/// A port the front end does not have is somebody else's radio — the preference
/// outlived an interface switch. Asking for it would only make the driver log an
/// error, so it is skipped and the device is left where it opened.
#[test]
fn an_antenna_this_device_does_not_have_is_left_alone() {
    let ports = Ports::fresh();
    let cfg = EngineConfig {
        initial_antenna: (Some("VERTICAL".into()), Some("ATU".into())),
        ..Default::default()
    };
    let mut h = start_engine(Box::new(Rig { ports: ports.clone() }), caps(), cfg);
    let thread = h.thread.take();

    assert!(wait_for_state(&h.event_rx, |s| s.antenna_rx == "LNAH"));
    assert!(ports.asked().is_empty(), "nothing should have been asked of the device");
    assert_eq!(ports.rx(), "LNAH");
    assert_eq!(ports.tx(), "BAND1");

    drop(h);
    let _ = thread.map(|t| t.join());
}

/// The TX port used to be recorded in the state and never sent anywhere. A
/// LimeSDR that transmits on the wrong band port puts the power into a filter
/// that isn't for this band, so the command has to reach the hardware.
#[test]
fn selecting_the_tx_antenna_reaches_the_hardware() {
    let ports = Ports::fresh();
    let mut h =
        start_engine(Box::new(Rig { ports: ports.clone() }), caps(), EngineConfig::default());
    let thread = h.thread.take();

    h.cmd_tx.send(Command::SetAntenna { dir: Direction::Tx, name: "BAND2".into() }).unwrap();
    assert!(wait_for_state(&h.event_rx, |s| s.antenna_tx == "BAND2"));
    assert_eq!(ports.tx(), "BAND2");
    assert_eq!(ports.asked(), vec![(Direction::Tx, "BAND2".to_string())]);

    drop(h);
    let _ = thread.map(|t| t.join());
}

/// A rig that drops out and comes back is reopened by the engine, and a freshly
/// opened device is on its driver's default port. The antenna belongs to the
/// station's coax rather than to the front end, so it has to be restored — an
/// operator who came back to a working waterfall would have no reason to suspect
/// they were listening on the wrong feedline.
#[test]
fn a_reconnect_returns_to_the_selected_antenna() {
    // The replacement the factory hands back: a fresh device on LNAH/BAND1.
    let after = Ports::fresh();
    let handed = after.clone();
    let reopen: sdroxide_radio::ReopenFn = Box::new(move |_center: f64| {
        Ok((Box::new(Rig { ports: handed.clone() }) as Box<dyn IqSource>, caps()))
    });

    let before = Ports::fresh();
    let cfg = EngineConfig {
        initial_antenna: (Some("LNAL".into()), None),
        reopen: Some(reopen),
        ..Default::default()
    };
    let mut h = start_engine(Box::new(Rig { ports: before.clone() }), caps(), cfg);
    let thread = h.thread.take();
    assert!(wait_for_state(&h.event_rx, |s| s.antenna_rx == "LNAL"));

    // The operator moves to a third port, then the interface is rebuilt under
    // them: what they chose by hand is what must come back, not what they
    // started on.
    h.cmd_tx.send(Command::SetAntenna { dir: Direction::Rx, name: "LNAW".into() }).unwrap();
    assert!(wait_for_state(&h.event_rx, |s| s.antenna_rx == "LNAW"));
    assert_eq!(before.rx(), "LNAW");

    h.swap_tx.send(sdroxide_radio::EngineSwap::ReopenSource).unwrap();
    assert!(
        wait_for_state(&h.event_rx, |s| s.antenna_rx == "LNAW"),
        "the reopened front end must be put back on the operator's port"
    );
    assert_eq!(after.rx(), "LNAW", "and the new device is the one that was asked");

    drop(h);
    let _ = thread.map(|t| t.join());
}
