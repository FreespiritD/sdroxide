//! Antenna-rotator client: sdroxide as a Hamlib **rotctld** client.
//!
//! The satellite lock computes azimuth and elevation a few times a second;
//! this crate is how those numbers reach a motorized antenna. It dials a
//! `rotctld` daemon (TCP, default port 4533) and speaks the short text
//! protocol — `P az el` to point, `p` to ask where the antenna is, `S` to
//! stop. One protocol reaches every rotator Hamlib can drive, which is
//! essentially all of them, and costs this codebase no serial drivers.
//!
//! The mirror image of `sdroxide-rigctld`: there we *are* the daemon and
//! GPredict connects to us; here the daemon already exists and we connect to
//! it.
//!
//! # Future backends
//!
//! For stations that would rather not run a daemon, the obvious additions are
//! **EasyComm II** (`AZnnn.n ELnnn.n` over a serial port — SatNOGS-style
//! homebrew controllers) and **GS-232A/B** (`Wnnn nnn` — Yaesu's own
//! interfaces). Both are line protocols simpler than rotctld's; they would
//! slot in behind the same [`RotctldClient`]-shaped API without the engine
//! noticing. Until someone owns the hardware to verify against, rotctld — via
//! `rotctld -m <model>` — already speaks to all of them.
//!
//! # Shape
//!
//! One worker thread owns the socket; the engine talks to it through a small
//! command channel and reads health back from a shared [`RotStatus`]. The
//! worker deduplicates movement (no point grinding motors over a fraction of
//! a degree), applies the configured azimuth offset, reconnects with backoff,
//! and polls the real antenna position once a second so the operator can see
//! the hardware answer.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use tracing::{debug, warn};

pub use sdroxide_types::RotatorConfig;

/// The client's health, as the engine reads it back: reachable or not, where
/// the daemon says the antenna points, and the last thing that went wrong.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RotStatus {
    pub connected: bool,
    /// Where the *hardware* reports itself — the commanded target lives in
    /// the satellite lock's own status.
    pub az_deg: f64,
    pub el_deg: f64,
    pub error: Option<String>,
}

enum RotCmd {
    Target { az: f64, el: f64 },
    Park,
}

/// Handle to the worker thread. Dropping it closes the command channel, which
/// is the shutdown signal; the drop joins the thread so a socket mid-write
/// cannot outlive the engine.
pub struct RotctldClient {
    tx: Option<Sender<RotCmd>>,
    status: Arc<Mutex<RotStatus>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl RotctldClient {
    pub fn start(cfg: RotatorConfig) -> RotctldClient {
        let (tx, rx) = bounded(8);
        let status = Arc::new(Mutex::new(RotStatus::default()));
        let st = status.clone();
        let join = std::thread::Builder::new()
            .name("sdroxide-rotctld".into())
            .spawn(move || worker(cfg, rx, st))
            .map_err(|e| warn!("could not start the rotctld client: {e}"))
            .ok();
        RotctldClient { tx: Some(tx), status, join }
    }

    /// Point the antenna. Safe to call at the tracking rate: the worker
    /// deduplicates against the last commanded position and the configured
    /// minimum movement, so a satellite crawling across the sky becomes a
    /// trickle of real commands rather than a 5 Hz stream.
    pub fn set_target(&self, az_deg: f64, el_deg: f64) {
        if let Some(tx) = &self.tx {
            // A full queue means the worker is mid-reconnect; the stale
            // target is better dropped — a fresher one follows shortly.
            let _ = tx.try_send(RotCmd::Target { az: az_deg, el: el_deg });
        }
    }

    /// Stop tracking: drive to the configured park position, or just stop
    /// moving when none is set. Idempotent — the worker remembers it already
    /// parked, so the engine can say this every tick the satellite is down.
    pub fn park(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(RotCmd::Park);
        }
    }

    pub fn status(&self) -> RotStatus {
        self.status.lock().unwrap().clone()
    }
}

impl Drop for RotctldClient {
    fn drop(&mut self) {
        self.tx = None; // closes the channel — the worker's shutdown signal
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// First retry after a connection failure, and the ceiling the spacing
/// doubles up to while the daemon stays unreachable.
const RECONNECT_FIRST: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
/// How often the real antenna position is read back.
const POSITION_POLL: Duration = Duration::from_secs(1);
/// How long the worker waits for a command before doing housekeeping —
/// paces the reconnect attempts and the position poll.
const TICK: Duration = Duration::from_millis(250);

fn worker(cfg: RotatorConfig, rx: Receiver<RotCmd>, status: Arc<Mutex<RotStatus>>) {
    let mut conn: Option<Conn> = None;
    let mut retry_at = Instant::now();
    let mut retry_every = RECONNECT_FIRST;
    let mut next_poll = Instant::now();

    loop {
        if conn.is_none() && Instant::now() >= retry_at {
            match Conn::open(&cfg) {
                Ok(c) => {
                    debug!("rotctld connected: {}", cfg.address());
                    retry_every = RECONNECT_FIRST;
                    set(&status, |s| {
                        s.connected = true;
                        s.error = None;
                    });
                    next_poll = Instant::now();
                    conn = Some(c);
                }
                Err(e) => {
                    set(&status, |s| {
                        s.connected = false;
                        s.error = Some(e);
                    });
                    retry_at = Instant::now() + retry_every;
                    retry_every = (retry_every * 2).min(RECONNECT_MAX);
                }
            }
        }

        let cmd = match rx.recv_timeout(TICK) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // A command while disconnected is dropped rather than queued: the
        // tracking loop sends fresh ones continuously, so replaying a stale
        // pointing after a minute of downtime would swing the antenna to
        // where the satellite used to be.
        let Some(c) = conn.as_mut() else { continue };

        let r = match cmd {
            Some(RotCmd::Target { az, el }) => c.point(&cfg, az, el),
            Some(RotCmd::Park) => c.park(&cfg),
            None => Ok(()),
        }
        .and_then(|()| {
            if Instant::now() >= next_poll {
                next_poll = Instant::now() + POSITION_POLL;
                let (az, el) = c.position()?;
                set(&status, |s| {
                    s.az_deg = az;
                    s.el_deg = el;
                });
            }
            Ok(())
        });
        if let Err(e) = r {
            warn!("rotctld: {e}");
            set(&status, |s| {
                s.connected = false;
                s.error = Some(e);
            });
            conn = None;
            retry_at = Instant::now() + RECONNECT_FIRST;
            retry_every = RECONNECT_FIRST;
        }
    }
}

fn set(status: &Mutex<RotStatus>, f: impl FnOnce(&mut RotStatus)) {
    if let Ok(mut s) = status.lock() {
        f(&mut s);
    }
}

/// One TCP connection to the daemon, with the per-connection movement state:
/// what was last commanded (for the dedup) and whether the antenna was left
/// parked (so a below-the-horizon satellite does not re-park every tick).
struct Conn {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    last_pointed: Option<(f64, f64)>,
    parked: bool,
}

impl Conn {
    fn open(cfg: &RotatorConfig) -> Result<Conn, String> {
        use std::net::ToSocketAddrs;
        let addr = cfg.address();
        let sock = addr
            .to_socket_addrs()
            .map_err(|e| format!("{addr}: {e}"))?
            .next()
            .ok_or_else(|| format!("{addr}: no address"))?;
        let stream = TcpStream::connect_timeout(&sock, Duration::from_secs(3))
            .map_err(|e| format!("{addr}: {e}"))?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_nodelay(true);
        let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        // Freshly connected, nothing commanded yet: `parked` so an idle
        // engine leaves the hardware exactly where it found it.
        Ok(Conn { stream, reader, last_pointed: None, parked: true })
    }

    fn point(&mut self, cfg: &RotatorConfig, az: f64, el: f64) -> Result<(), String> {
        let az = (az + cfg.az_offset_deg).rem_euclid(360.0);
        let el = el.clamp(0.0, 90.0);
        if !self.parked {
            if let Some((la, le)) = self.last_pointed {
                let min = cfg.min_move_deg.max(0.0);
                if az_dist(la, az) < min && (le - el).abs() < min {
                    return Ok(());
                }
            }
        }
        self.send(&format!("P {az:.2} {el:.2}\n"))?;
        self.rprt()?;
        self.parked = false;
        self.last_pointed = Some((az, el));
        Ok(())
    }

    fn park(&mut self, cfg: &RotatorConfig) -> Result<(), String> {
        if self.parked {
            return Ok(());
        }
        self.parked = true;
        self.last_pointed = None;
        match cfg.park {
            Some((az, el)) => {
                let az = (az + cfg.az_offset_deg).rem_euclid(360.0);
                self.send(&format!("P {az:.2} {:.2}\n", el.clamp(0.0, 90.0)))?;
                self.rprt()
            }
            // No park position: stop where it is rather than inventing one.
            None => {
                self.send("S\n")?;
                self.rprt()
            }
        }
    }

    /// `p` — where the daemon says the antenna is. Two bare lines on success;
    /// an `RPRT -n` in their place on failure.
    fn position(&mut self) -> Result<(f64, f64), String> {
        self.send("p\n")?;
        let first = self.read_line()?;
        let t = first.trim().to_string();
        if let Some(code) = t.strip_prefix("RPRT ") {
            return Err(format!("rotctld answered RPRT {} to a position query", code.trim()));
        }
        let az: f64 = t.parse().map_err(|_| format!("unparseable azimuth {t:?}"))?;
        let second = self.read_line()?;
        let t = second.trim();
        let el: f64 = t.parse().map_err(|_| format!("unparseable elevation {t:?}"))?;
        Ok((az, el))
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        self.stream.write_all(line.as_bytes()).map_err(|e| e.to_string())
    }

    fn read_line(&mut self) -> Result<String, String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => Err("rotctld closed the connection".into()),
            Ok(_) => Ok(line),
            Err(e) => Err(e.to_string()),
        }
    }

    fn rprt(&mut self) -> Result<(), String> {
        let line = self.read_line()?;
        let t = line.trim();
        match t.strip_prefix("RPRT ") {
            Some("0") => Ok(()),
            Some(code) => Err(format!("rotctld answered RPRT {code}")),
            None => Err(format!("unexpected rotctld reply {t:?}")),
        }
    }
}

/// Shortest angular distance between two azimuths, degrees.
fn az_dist(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A one-connection rotctld impersonation: answers `P` with the given
    /// report code, `p` with a fixed position, `S` with success, and sends
    /// every `P` line it saw down the channel for the test to inspect.
    fn mock_rotctld(rprt_for_p: &'static str) -> (u16, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = match listener.accept() {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut w = stream.try_clone().unwrap();
            let r = BufReader::new(stream);
            for line in r.lines() {
                let Ok(line) = line else { return };
                let t = line.trim().to_string();
                if t == "p" {
                    let _ = w.write_all(b"123.40\n45.60\n");
                } else if t.starts_with("P ") {
                    let _ = tx.send(t);
                    let _ = w.write_all(format!("RPRT {rprt_for_p}\n").as_bytes());
                } else if t == "S" {
                    let _ = tx.send(t);
                    let _ = w.write_all(b"RPRT 0\n");
                }
            }
        });
        (port, rx)
    }

    fn cfg(port: u16) -> RotatorConfig {
        RotatorConfig { enabled: true, host: "127.0.0.1".into(), port, ..Default::default() }
    }

    fn wait_for(mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for the client");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn points_polls_and_reports_the_hardware_position() {
        let (port, seen) = mock_rotctld("0");
        let client = RotctldClient::start(cfg(port));
        client.set_target(200.0, 30.0);
        let p = seen.recv_timeout(Duration::from_secs(5)).expect("a P command");
        assert_eq!(p, "P 200.00 30.00");
        // The position poll reads the mock's fixed answer back.
        wait_for(|| {
            let s = client.status();
            s.connected && (s.az_deg - 123.4).abs() < 0.01 && (s.el_deg - 45.6).abs() < 0.01
        });
        assert_eq!(client.status().error, None);
    }

    /// Sub-degree jitter must not become motor commands, and a real move must.
    #[test]
    fn small_movements_are_swallowed_by_the_dedup() {
        let (port, seen) = mock_rotctld("0");
        let client = RotctldClient::start(cfg(port)); // min_move_deg = 1.0 default
        client.set_target(100.0, 20.0);
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)).unwrap(), "P 100.00 20.00");
        client.set_target(100.4, 20.4); // inside the dead band
        client.set_target(103.0, 20.0); // outside it
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)).unwrap(), "P 103.00 20.00");
    }

    /// Parking with no park position stops the rotator; saying it twice
    /// sends nothing the second time.
    #[test]
    fn park_stops_once_and_only_after_tracking() {
        let (port, seen) = mock_rotctld("0");
        let client = RotctldClient::start(cfg(port));
        // Fresh connection is born parked: an idle engine's park chatter
        // must not touch hardware it never commanded.
        client.park();
        client.set_target(50.0, 10.0);
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)).unwrap(), "P 50.00 10.00");
        client.park();
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)).unwrap(), "S");
        client.park(); // already parked — nothing more may arrive
        client.set_target(60.0, 15.0); // ...and tracking resumes cleanly
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)).unwrap(), "P 60.00 15.00");
    }

    #[test]
    fn the_azimuth_offset_is_applied_and_wraps() {
        let (port, seen) = mock_rotctld("0");
        let mut c = cfg(port);
        c.az_offset_deg = 10.0;
        let client = RotctldClient::start(c);
        client.set_target(355.0, 5.0);
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)).unwrap(), "P 5.00 5.00");
    }

    #[test]
    fn a_daemon_error_reaches_the_status() {
        let (port, _seen) = mock_rotctld("-1");
        let client = RotctldClient::start(cfg(port));
        client.set_target(10.0, 10.0);
        wait_for(|| client.status().error.as_deref().is_some_and(|e| e.contains("RPRT -1")));
    }

    #[test]
    fn an_unreachable_daemon_is_a_status_not_a_hang() {
        // A port nothing listens on: bind-then-drop guarantees it is closed.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let client = RotctldClient::start(cfg(port));
        client.set_target(10.0, 10.0);
        wait_for(|| {
            let s = client.status();
            !s.connected && s.error.is_some()
        });
    }
}
