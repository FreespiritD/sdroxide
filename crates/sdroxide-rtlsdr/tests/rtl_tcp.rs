//! The `rtl_tcp` client against a server that is only pretending.
//!
//! The far end of this backend is a program on another machine, which is
//! exactly the sort of thing that never gets exercised until it is on a mast.
//! A fake server is cheap — twelve bytes of greeting and a socket — and it
//! pins down the two halves that matter: that the opening handshake configures
//! the dongle in the right order with the right numbers, and that the byte
//! stream comes back out as I/Q in the right order with nothing lost at a
//! segment boundary.
//!
//! What it cannot check is a real `rtl_tcp`'s behaviour. It answers nothing,
//! as the protocol requires, so a server that ignores a command looks
//! identical here to one that obeys it.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sdroxide_rtlsdr::RtlSdrHandle;
use sdroxide_types::{RtlSdrAgc, RtlSdrHfMode, RtlTcpConfig};

/// Command opcodes, spelled out here rather than imported: a test that shares
/// the constants with the code under test cannot catch a renumbering.
const FREQ: u8 = 0x01;
const RATE: u8 = 0x02;
const GAIN_MODE: u8 = 0x03;
const GAIN: u8 = 0x04;
const PPM: u8 = 0x05;
const RTL_AGC: u8 = 0x08;
const DIRECT: u8 = 0x09;
const BIAS_TEE: u8 = 0x0e;

/// How long a test waits for the far end to catch up before giving up.
const DEADLINE: Duration = Duration::from_secs(5);

struct Fake {
    addr: SocketAddr,
    cmds: Arc<Mutex<Vec<(u8, u32)>>>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Fake {
    /// Start a server that greets as `tuner` and then streams `stream_bytes`
    /// of a known pattern, recording every command it is sent.
    fn start(tuner: u32, stream_bytes: usize) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let cmds = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread_cmds = Arc::clone(&cmds);
        let thread_stop = Arc::clone(&stop);
        let join = std::thread::spawn(move || {
            let Ok((sock, _)) = listener.accept() else { return };
            serve(sock, tuner, stream_bytes, &thread_cmds, &thread_stop);
        });

        Fake { addr, cmds, stop, join: Some(join) }
    }

    fn endpoint(&self) -> String {
        self.addr.to_string()
    }

    fn cmds(&self) -> Vec<(u8, u32)> {
        self.cmds.lock().expect("lock").clone()
    }

    /// Wait until at least `n` commands have arrived, then return them all.
    /// Returns whatever it has if the deadline passes, so the assertion that
    /// follows reports the real difference rather than a timeout.
    fn wait_for_cmds(&self, n: usize) -> Vec<(u8, u32)> {
        let until = Instant::now() + DEADLINE;
        loop {
            let c = self.cmds();
            if c.len() >= n || Instant::now() > until {
                return c;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The flag is only looked at between reads, so a server still waiting
        // for its first client would never see it. One connection that is
        // never used gets it out of `accept` and into the loop that can.
        let _ = TcpStream::connect(self.addr);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn serve(
    mut sock: TcpStream,
    tuner: u32,
    mut stream_bytes: usize,
    cmds: &Mutex<Vec<(u8, u32)>>,
    stop: &AtomicBool,
) {
    let mut greeting = Vec::from(*b"RTL0");
    greeting.extend_from_slice(&tuner.to_be_bytes());
    greeting.extend_from_slice(&29u32.to_be_bytes());
    if sock.write_all(&greeting).is_err() {
        return;
    }
    sock.set_read_timeout(Some(Duration::from_millis(5))).expect("timeout");

    let mut acc: Vec<u8> = Vec::new();
    let mut pattern: u8 = 0;
    let mut buf = [0u8; 256];
    while !stop.load(Ordering::Relaxed) {
        match sock.read(&mut buf) {
            // The client hung up. Anything it sent before that has already
            // been delivered, so the record is complete.
            Ok(0) => return,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
        while acc.len() >= 5 {
            let rest = acc.split_off(5);
            let arg = u32::from_be_bytes([acc[1], acc[2], acc[3], acc[4]]);
            cmds.lock().expect("lock").push((acc[0], arg));
            acc = rest;
        }

        // Deliberately awkward chunk lengths, none of them even: a client that
        // drops the odd byte at the end of a read instead of carrying it into
        // the next one swaps I with Q and fails the comparison downstream.
        if stream_bytes > 0 {
            let n = stream_bytes.min(997);
            let chunk: Vec<u8> = (0..n)
                .map(|_| {
                    let b = pattern;
                    pattern = pattern.wrapping_add(1);
                    b
                })
                .collect();
            if sock.write_all(&chunk).is_err() {
                return;
            }
            stream_bytes -= n;
        }
    }
}

fn config(endpoint: &str) -> RtlTcpConfig {
    RtlTcpConfig {
        address: endpoint.to_string(),
        sample_rate_hz: 1_024_000.0,
        ppm: -7,
        tuner_gain_db: 30.0,
        agc: RtlSdrAgc::Manual,
        hf_mode: RtlSdrHfMode::Auto,
        bias_tee: false,
        iq_correction: true,
    }
}

/// The value the sample lookup table produces for one wire byte. Written out
/// rather than shared with the driver, for the same reason as the opcodes.
fn expect_sample(b: u8) -> f32 {
    (b as f32 - 127.4) / 128.0
}

/// The opening sequence, in the order the far end needs it: the crystal
/// correction before anything is computed against it, the rate before the
/// tuning, the HF path before the frequency it decides how to reach, and the
/// gains after the path switch that would have reset them.
#[test]
fn the_handshake_configures_the_far_end_in_dependency_order() {
    let fake = Fake::start(5, 0);
    let handle = RtlSdrHandle::connect(&config(&fake.endpoint()), 100_100_000.0).expect("connects");

    let cmds = fake.wait_for_cmds(8);
    assert_eq!(
        cmds,
        vec![
            (PPM, (-7i32) as u32),
            (RATE, 1_024_000),
            // 100 MHz is well above the crossover, so the far end is told to
            // use its tuner — explicitly, because what a previous client left
            // it in cannot be read back.
            (DIRECT, 0),
            (FREQ, 100_100_000),
            // Manual gain: mode 1, then 30.0 dB as tenths, then the
            // demodulator's own AGC off.
            (GAIN_MODE, 1),
            (GAIN, 300),
            (RTL_AGC, 0),
            (BIAS_TEE, 0),
        ]
    );
    drop(handle);
}

/// Tuning to HF has to switch the far end's front end *and* restate the gains,
/// because the tuner re-init that comes with the switch resets them.
#[test]
fn tuning_below_the_crossover_switches_the_far_end_to_direct_sampling() {
    let fake = Fake::start(5, 0);
    let handle = RtlSdrHandle::connect(&config(&fake.endpoint()), 100_100_000.0).expect("connects");
    let opening = fake.wait_for_cmds(8).len();

    handle.set_center_hz(7_100_000.0);

    let cmds = fake.wait_for_cmds(opening + 5);
    assert_eq!(
        &cmds[opening..],
        &[(DIRECT, 2), (GAIN_MODE, 1), (GAIN, 300), (RTL_AGC, 0), (FREQ, 7_100_000)],
        "the gains have to follow the path switch, and the frequency the path"
    );
    drop(handle);
}

/// A Blog V4 upconverts on the far end, and the protocol gives no way to tell
/// one from a plain R828D — so the tuner is what decides, and it is left alone.
#[test]
fn an_r828d_is_not_switched_to_direct_sampling() {
    let fake = Fake::start(6, 0);
    let handle = RtlSdrHandle::connect(&config(&fake.endpoint()), 100_100_000.0).expect("connects");
    let opening = fake.wait_for_cmds(8).len();

    handle.set_center_hz(7_100_000.0);

    let cmds = fake.wait_for_cmds(opening + 1);
    assert_eq!(&cmds[opening..], &[(FREQ, 7_100_000)], "an upconverting dongle just tunes");
    drop(handle);
}

/// Gain and AGC changes go out as a group, because a server running its tuner
/// AGC discards a gain it is sent while the mode still says automatic.
#[test]
fn a_gain_change_restates_the_mode_with_it() {
    let fake = Fake::start(5, 0);
    let handle = RtlSdrHandle::connect(&config(&fake.endpoint()), 100_100_000.0).expect("connects");
    let opening = fake.wait_for_cmds(8).len();

    handle.set_gain_db(12.5);
    let cmds = fake.wait_for_cmds(opening + 3);
    assert_eq!(&cmds[opening..], &[(GAIN_MODE, 1), (GAIN, 125), (RTL_AGC, 0)]);

    // Handing the far end its own loops: no gain value at all, since it owns
    // the figure now.
    let n = cmds.len();
    handle.set_agc(RtlSdrAgc::Both);
    let cmds = fake.wait_for_cmds(n + 2);
    assert_eq!(&cmds[n..], &[(GAIN_MODE, 0), (RTL_AGC, 1)]);
    assert_eq!(handle.effective_gain_db(), None, "no figure to report under the far end's AGC");
    drop(handle);
}

/// The bias tee outlives the connection — `rtl_tcp` keeps the dongle open for
/// the next client — so leaving it on at shutdown would leave DC on a coax
/// that is somewhere else entirely.
#[test]
fn the_remote_bias_tee_is_turned_off_on_the_way_out() {
    let fake = Fake::start(5, 0);
    let mut handle =
        RtlSdrHandle::connect(&config(&fake.endpoint()), 100_100_000.0).expect("connects");
    let opening = fake.wait_for_cmds(8).len();

    handle.set_bias_tee(true);
    let cmds = fake.wait_for_cmds(opening + 1);
    assert_eq!(&cmds[opening..], &[(BIAS_TEE, 1)]);

    handle.release();
    let cmds = fake.wait_for_cmds(opening + 2);
    assert_eq!(cmds.last(), Some(&(BIAS_TEE, 0)), "the far end was left powering the coax");
}

/// Every byte the server sent, in order, as interleaved I/Q — the check that
/// nothing is dropped or transposed at a read boundary. The chunk lengths the
/// fake uses are odd on purpose, so a read almost always ends between an I and
/// its Q.
#[test]
fn the_sample_stream_survives_the_segment_boundaries() {
    const BYTES: usize = 8_191;
    let fake = Fake::start(5, BYTES);
    let mut handle =
        RtlSdrHandle::connect(&config(&fake.endpoint()), 100_100_000.0).expect("connects");

    // One byte of the odd total stays behind, held for a partner that never
    // comes.
    let want = BYTES - 1;
    let mut got: Vec<f32> = Vec::with_capacity(want);
    let mut buf = vec![0f32; 4096];
    let until = Instant::now() + DEADLINE;
    while got.len() < want && Instant::now() < until {
        let n = handle.rx_read(&mut buf);
        assert_eq!(n % 2, 0, "an odd float count would put I and Q out of step");
        got.extend_from_slice(&buf[..n]);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    assert_eq!(got.len(), want, "the stream stopped short");
    for (i, v) in got.iter().enumerate() {
        let expect = expect_sample((i % 256) as u8);
        assert!(
            (v - expect).abs() < 1e-6,
            "sample {i}: {v} != {expect} — the stream slipped a byte here"
        );
    }
    handle.release();
}

/// The two ways this goes wrong in the field, and what each has to say.
#[test]
fn a_server_that_is_not_rtl_tcp_says_so() {
    // Something else listening on the port.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut g = Vec::from(*b"RSP0");
            g.extend_from_slice(&[0u8; 8]);
            let _ = sock.write_all(&g);
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    let Err(e) = RtlSdrHandle::connect(&config(&addr.to_string()), 100e6) else {
        panic!("an rsp_tcp greeting must not be taken for rtl_tcp");
    };
    assert!(e.to_string().contains("rsp_tcp"), "unhelpful: {e}");
}

#[test]
fn nothing_listening_names_the_thing_to_start() {
    // A port that was ours a moment ago is a port nothing is listening on.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);

    let Err(e) = RtlSdrHandle::connect(&config(&addr.to_string()), 100e6) else {
        panic!("connected to a port nothing is listening on");
    };
    assert!(e.to_string().contains("rtl_tcp -a 0.0.0.0"), "unhelpful: {e}");
}

/// The port may be left off the address, because the protocol has a default
/// and every rtl_tcp instruction on the internet uses it.
#[test]
fn the_default_port_is_supplied() {
    // The address the client dials, rather than a connection: 1234 is not a
    // port a test may assume it can bind.
    assert_eq!(config("raspberrypi.local").endpoint(), "raspberrypi.local:1234");
    assert_eq!(config("192.168.1.5").endpoint(), "192.168.1.5:1234");
    // One that already carries a port is dialled as written...
    assert_eq!(config("192.168.1.5:5678").endpoint(), "192.168.1.5:5678");
    // ...and an IPv6 literal is all colons, so the brackets are what decide.
    assert_eq!(config("[fe80::1]").endpoint(), "[fe80::1]:1234");
    assert_eq!(config("[fe80::1]:5678").endpoint(), "[fe80::1]:5678");
}
