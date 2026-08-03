//! An in-process `iiod` that a real [`PlutoHandle`] is driven against.
//!
//! This backend was written without a Pluto to test it on, which makes this the
//! main defence: it exercises the actual client — three connections, the
//! control thread, the receive decoder, the transmit encoder — against a server
//! that answers the way libiio's own daemon documents.
//!
//! Two things it deliberately does *not* do:
//!
//! - It does not use this crate's encoder to build the samples it serves. A
//!   self-consistent encode/decode pair passes its own test while both are
//!   wrong; the bytes below are written out by hand from the AD9361 wire format
//!   (12 significant bits, two's complement, little-endian, in a 16-bit slot).
//! - It does not prove the framing is what a real `iiod` produces. That is a
//!   claim about hardware, and only hardware can settle it — see the trace's
//!   `first READBUF chunk` line.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sdroxide_pluto::PlutoHandle;
use sdroxide_types::{PlutoAgc, PlutoConfig};

/// A trimmed but structurally faithful Pluto context.
const CONTEXT_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<context name="xml" description="Linux (none) 5.10.0 #1 SMP PREEMPT armv7l">
  <context-attribute name="hw_model" value="Analog Devices PlutoSDR Rev.B (Z7010-AD9364)" />
  <context-attribute name="hw_serial" value="1044735411960005" />
  <context-attribute name="fw_version" value="v0.39" />
  <device id="iio:device0" name="ad9361-phy">
    <channel id="voltage0" type="input">
      <attribute name="rf_port_select" filename="in_voltage0_rf_port_select" />
      <attribute name="rf_port_select_available" filename="in_voltage0_rf_port_select_available" />
      <attribute name="hardwaregain" filename="in_voltage0_hardwaregain" />
      <attribute name="hardwaregain_available" filename="in_voltage0_hardwaregain_available" />
      <attribute name="gain_control_mode" filename="in_voltage0_gain_control_mode" />
      <attribute name="gain_control_mode_available" filename="in_voltage0_gain_control_mode_available" />
      <attribute name="rf_bandwidth" filename="in_voltage0_rf_bandwidth" />
      <attribute name="sampling_frequency" filename="in_voltage0_sampling_frequency" />
    </channel>
    <channel id="voltage0" type="output">
      <attribute name="rf_port_select" filename="out_voltage0_rf_port_select" />
      <attribute name="rf_port_select_available" filename="out_voltage0_rf_port_select_available" />
      <attribute name="hardwaregain" filename="out_voltage0_hardwaregain" />
      <attribute name="hardwaregain_available" filename="out_voltage0_hardwaregain_available" />
      <attribute name="rf_bandwidth" filename="out_voltage0_rf_bandwidth" />
      <attribute name="sampling_frequency" filename="out_voltage0_sampling_frequency" />
    </channel>
    <channel id="altvoltage0" name="RX_LO" type="output">
      <attribute name="frequency" filename="out_altvoltage0_RX_LO_frequency" />
      <attribute name="frequency_available" filename="out_altvoltage0_RX_LO_frequency_available" />
    </channel>
    <channel id="altvoltage1" name="TX_LO" type="output">
      <attribute name="frequency" filename="out_altvoltage1_TX_LO_frequency" />
      <attribute name="frequency_available" filename="out_altvoltage1_TX_LO_frequency_available" />
    </channel>
    <attribute name="ensm_mode" filename="ensm_mode" />
  </device>
  <device id="iio:device3" name="cf-ad9361-lpc">
    <channel id="voltage0" type="input">
      <scan-element index="0" format="le:s12/16&gt;&gt;0" />
    </channel>
    <channel id="voltage1" type="input">
      <scan-element index="1" format="le:s12/16&gt;&gt;0" />
    </channel>
  </device>
  <device id="iio:device4" name="cf-ad9361-dds-core-lpc">
    <channel id="voltage0" type="output">
      <scan-element index="0" format="le:s16/16&gt;&gt;0" />
    </channel>
    <channel id="voltage1" type="output">
      <scan-element index="1" format="le:s16/16&gt;&gt;0" />
    </channel>
    <channel id="altvoltage0" name="TX1_I_F1" type="output">
      <attribute name="raw" filename="out_altvoltage0_TX1_I_F1_raw" />
    </channel>
    <channel id="altvoltage1" name="TX1_Q_F1" type="output">
      <attribute name="raw" filename="out_altvoltage1_TX1_Q_F1_raw" />
    </channel>
  </device>
</context>"#;

/// What the fake device remembers, so the test can assert on what the client
/// actually did to it.
#[derive(Default)]
struct DeviceState {
    attrs: Vec<(String, String)>,
    /// Every `WRITEBUF` payload the client sent.
    tx_payloads: Vec<Vec<u8>>,
    rx_buffer_opens: usize,
    tx_buffer_opens: usize,
    rx_buffer_open: bool,
    tx_buffer_open: bool,
}

impl DeviceState {
    fn get(&self, key: &str) -> Option<&str> {
        self.attrs.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn writes_of(&self, key: &str) -> Vec<&str> {
        self.attrs.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str()).collect()
    }
}

struct Fake {
    addr: std::net::SocketAddr,
    state: Arc<Mutex<DeviceState>>,
    stop: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
}

impl Fake {
    fn start() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let state = Arc::new(Mutex::new(DeviceState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(AtomicUsize::new(0));
        {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            let connections = Arc::clone(&connections);
            std::thread::spawn(move || {
                for sock in listener.incoming() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(sock) = sock else { break };
                    connections.fetch_add(1, Ordering::Relaxed);
                    let state = Arc::clone(&state);
                    let stop = Arc::clone(&stop);
                    std::thread::spawn(move || serve(sock, state, stop));
                }
            });
        }
        Fake { addr, state, stop, connections }
    }

    fn address(&self) -> String {
        self.addr.to_string()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock the accept loop.
        let _ = TcpStream::connect(self.addr);
    }
}

/// The default value of every attribute the client reads before it writes one.
fn default_attr(key: &str) -> &'static str {
    match key {
        "ad9361-phy/OUTPUT/altvoltage0/frequency_available" => "[70000000 1 6000000000]",
        "ad9361-phy/OUTPUT/altvoltage1/frequency_available" => "[70000000 1 6000000000]",
        "ad9361-phy/INPUT/voltage0/hardwaregain_available" => "[0 1 71]",
        "ad9361-phy/OUTPUT/voltage0/hardwaregain_available" => "[-89.750000 0.250000 0.000000]",
        "ad9361-phy/INPUT/voltage0/gain_control_mode_available" => {
            "manual fast_attack slow_attack hybrid"
        }
        "ad9361-phy/INPUT/voltage0/rf_port_select_available" => "A_BALANCED B_BALANCED",
        "ad9361-phy/OUTPUT/voltage0/rf_port_select_available" => "A B",
        "ad9361-phy/INPUT/voltage0/rf_port_select" => "A_BALANCED",
        "ad9361-phy/OUTPUT/voltage0/rf_port_select" => "A",
        "ad9361-phy/INPUT/voltage0/sampling_frequency" => "2000000",
        "ad9361-phy/OUTPUT/voltage0/sampling_frequency" => "2000000",
        "ad9361-phy/INPUT/voltage0/rf_bandwidth" => "1800000",
        _ => "0",
    }
}

/// One I/Q sample set as a real device would put it on the wire: two 12-bit
/// two's-complement values, each little-endian in a 16-bit slot.
///
/// Written out by hand rather than by calling this crate's encoder, so an
/// encoder that has the byte order or the sign extension wrong cannot make this
/// test agree with it.
/// +2047, positive full scale — `0x07FF` on the wire.
const SAMPLE_I: i16 = 2047;
/// -2048, negative full scale. On the wire that is `0x0800`: in *12-bit* two's
/// complement the top bit of the 12 is the sign, so this reads as +2048 if the
/// decoder sign-extends from the 16-bit container instead.
const SAMPLE_Q: i16 = -2048;
const SAMPLE_BYTES: [u8; 4] = [0xFF, 0x07, 0x00, 0x08];

fn serve(sock: TcpStream, state: Arc<Mutex<DeviceState>>, stop: Arc<AtomicBool>) {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));
    let mut writer = sock.try_clone().expect("clone");
    let mut reader = BufReader::new(sock);
    let mut line = String::new();
    while !stop.load(Ordering::Relaxed) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(_) => break,
        }
        let cmd = line.trim_end_matches(['\r', '\n']).to_string();
        if cmd.is_empty() {
            continue;
        }
        let words: Vec<&str> = cmd.split_whitespace().collect();
        let ok = match words.first().copied() {
            Some("VERSION") => writer.write_all(b"0.25 (git tag:v0.25)\n").is_ok(),
            Some("TIMEOUT") => writer.write_all(b"0\n").is_ok(),
            Some("PRINT") => writer
                .write_all(format!("{}\n{CONTEXT_XML}\n", CONTEXT_XML.len()).as_bytes())
                .is_ok(),
            Some("READ") => {
                let key = attr_key(&words[1..]);
                let value = {
                    let g = state.lock().expect("lock");
                    g.get(&key).map(str::to_string)
                }
                .unwrap_or_else(|| default_attr(&key).to_string());
                let mut payload = value.into_bytes();
                payload.push(0);
                writer
                    .write_all(format!("{}\n", payload.len()).as_bytes())
                    .and_then(|()| writer.write_all(&payload))
                    .and_then(|()| writer.write_all(b"\n"))
                    .is_ok()
            }
            Some("WRITE") => {
                // The trailing count is the payload length; everything before
                // it names the attribute.
                let len: usize = words.last().and_then(|s| s.parse().ok()).unwrap_or(0);
                let key = attr_key(&words[1..words.len() - 1]);
                let mut payload = vec![0u8; len];
                if reader.read_exact(&mut payload).is_err() {
                    break;
                }
                let value =
                    String::from_utf8_lossy(&payload).trim_end_matches('\0').trim().to_string();
                state.lock().expect("lock").attrs.push((key, value));
                writer.write_all(format!("{len}\n").as_bytes()).is_ok()
            }
            Some("OPEN") => {
                let mut g = state.lock().expect("lock");
                if words[1] == "iio:device3" {
                    g.rx_buffer_opens += 1;
                    g.rx_buffer_open = true;
                } else {
                    g.tx_buffer_opens += 1;
                    g.tx_buffer_open = true;
                }
                drop(g);
                writer.write_all(b"0\n").is_ok()
            }
            Some("CLOSE") => {
                let mut g = state.lock().expect("lock");
                if words[1] == "iio:device3" {
                    g.rx_buffer_open = false;
                } else {
                    g.tx_buffer_open = false;
                }
                drop(g);
                writer.write_all(b"0\n").is_ok()
            }
            Some("READBUF") => {
                if !state.lock().expect("lock").rx_buffer_open {
                    let _ = writer.write_all(b"-9\n");
                    continue;
                }
                let want: usize = words[2].parse().unwrap_or(0);
                let sets = want / SAMPLE_BYTES.len();
                let mut data = Vec::with_capacity(sets * SAMPLE_BYTES.len());
                for _ in 0..sets {
                    data.extend_from_slice(&SAMPLE_BYTES);
                }
                // Deliberately answered in two chunks, each with its own mask,
                // because that is the shape the protocol allows and the client
                // has to survive it.
                let split = (data.len() / 2) & !3;
                let ok = write_chunk(&mut writer, &data[..split])
                    && write_chunk(&mut writer, &data[split..]);
                if ok {
                    let _ = writer.write_all(b"0\n");
                }
                ok
            }
            Some("WRITEBUF") => {
                let len: usize = words[2].parse().unwrap_or(0);
                if writer.write_all(b"0\n").is_err() {
                    break;
                }
                let mut payload = vec![0u8; len];
                if reader.read_exact(&mut payload).is_err() {
                    break;
                }
                state.lock().expect("lock").tx_payloads.push(payload);
                writer.write_all(format!("{len}\n").as_bytes()).is_ok()
            }
            Some("EXIT") => break,
            _ => writer.write_all(b"-22\n").is_ok(),
        };
        if !ok || writer.flush().is_err() {
            break;
        }
    }
    let _ = writer.shutdown(Shutdown::Both);
}

fn write_chunk(writer: &mut TcpStream, data: &[u8]) -> bool {
    writer
        .write_all(format!("{}\n00000003\n", data.len()).as_bytes())
        .and_then(|()| writer.write_all(data))
        .is_ok()
}

/// `["iio:device0", "INPUT", "voltage0", "hardwaregain"]` →
/// `"ad9361-phy/INPUT/voltage0/hardwaregain"`, so the fake keys on names a
/// reader recognises rather than on `iio:deviceN`.
fn attr_key(words: &[&str]) -> String {
    let mut parts: Vec<String> = words.iter().map(|s| s.to_string()).collect();
    if let Some(first) = parts.first_mut() {
        *first = match first.as_str() {
            "iio:device0" => "ad9361-phy".into(),
            "iio:device3" => "cf-ad9361-lpc".into(),
            "iio:device4" => "cf-ad9361-dds-core-lpc".into(),
            other => other.to_string(),
        };
    }
    parts.join("/")
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

fn config() -> PlutoConfig {
    PlutoConfig {
        sample_rate_hz: 2_000_000.0,
        rx_gain_db: 40.0,
        agc: PlutoAgc::Manual,
        tx_gain_db: -30.0,
        buffer_samples: 4096,
        ..PlutoConfig::default()
    }
}

#[test]
fn a_pluto_comes_up_and_streams() {
    let fake = Fake::start();
    let mut handle =
        PlutoHandle::open(&fake.address(), &config(), 435_000_000.0).expect("open the fake Pluto");

    assert_eq!(handle.sample_rate_hz, 2_000_000.0);
    assert!(handle.model.contains("AD9364"));
    assert_eq!(handle.firmware, "v0.39");
    // The limits are the ones the device published, not the fallback table.
    assert_eq!(handle.limits.rx_lo_hz, (70e6, 6e9));
    assert_eq!(handle.limits.rx_gain_db, (0.0, 71.0, 1.0));
    assert_eq!(handle.limits.tx_gain_db, (-89.75, 0.0, 0.25));
    assert_eq!(handle.limits.rx_ports, vec!["A_BALANCED", "B_BALANCED"]);
    assert!(handle.limits.assumed.is_empty(), "{:?}", handle.limits.assumed);
    // Everything came from the device, so there is nothing to warn about.
    assert_eq!(handle.open_status(), None);

    // Three connections: control, receive, transmit.
    assert_eq!(fake.connections.load(Ordering::Relaxed), 3);

    let mut buf = vec![0f32; 4096];
    let mut got = 0;
    wait_for("samples", || {
        got = handle.rx_read(&mut buf);
        got > 0
    });
    assert_eq!(got % 2, 0, "an odd count would swap I and Q for good");
    // The hand-written wire bytes decode to positive and negative full scale.
    assert!((buf[0] - (SAMPLE_I as f32 / 2048.0)).abs() < 1e-6, "I was {}", buf[0]);
    assert!((buf[1] - (SAMPLE_Q as f32 / 2048.0)).abs() < 1e-6, "Q was {}", buf[1]);
    assert!((buf[1] - -1.0).abs() < 1e-6);

    handle.release();
}

#[test]
fn the_front_end_is_configured_in_an_order_that_works() {
    let fake = Fake::start();
    let handle =
        PlutoHandle::open(&fake.address(), &config(), 435_000_000.0).expect("open the fake Pluto");
    wait_for("the receive buffer", || fake.state.lock().unwrap().rx_buffer_open);
    drop(handle);

    let g = fake.state.lock().expect("lock");
    // The analog filter is opened up to 0.9 × the rate, so the engine's
    // quarter-span LO offset still clears it. Leaving the AD9361 default here
    // is what makes that offset silently do nothing.
    assert_eq!(g.get("ad9361-phy/INPUT/voltage0/rf_bandwidth"), Some("1800000"));
    assert_eq!(g.get("ad9361-phy/OUTPUT/voltage0/rf_bandwidth"), Some("1800000"));
    assert_eq!(g.get("ad9361-phy/INPUT/voltage0/sampling_frequency"), Some("2000000"));
    assert_eq!(g.get("ad9361-phy/INPUT/voltage0/gain_control_mode"), Some("manual"));
    assert_eq!(g.get("ad9361-phy/OUTPUT/altvoltage0/frequency"), Some("435000000"));

    // The transmitter is silenced before the operator's level is applied, so
    // whatever the previous program left in the attenuator is never live.
    let tx_gains = g.writes_of("ad9361-phy/OUTPUT/voltage0/hardwaregain");
    assert_eq!(tx_gains.len(), 2, "{tx_gains:?}");
    assert!(tx_gains[0].starts_with("-89.75"), "{tx_gains:?}");
    assert!(tx_gains[1].starts_with("-30"), "{tx_gains:?}");
}

#[test]
fn transmitting_takes_the_link_and_gives_it_back() {
    let fake = Fake::start();
    let mut handle =
        PlutoHandle::open(&fake.address(), &config(), 145_500_000.0).expect("open the fake Pluto");
    wait_for("the receive buffer", || fake.state.lock().unwrap().rx_buffer_open);

    let rate = handle.tx_begin(145_500_000.0);
    assert_eq!(rate, 2_000_000.0);

    // Full-scale I, silence in Q — a tone this crate's encoder has to put on
    // the wire as +32767 in the I slot.
    let iq: Vec<f32> = (0..8192).map(|i| if i % 2 == 0 { 1.0 } else { 0.0 }).collect();
    handle.tx_write(&iq);

    wait_for("a transmit buffer", || !fake.state.lock().unwrap().tx_payloads.is_empty());

    {
        let g = fake.state.lock().expect("lock");
        // Receive let go of the link before transmit took it.
        assert!(!g.rx_buffer_open, "receive was still running during the over");
        assert!(g.tx_buffer_open);
        // The DDS tone generators are silenced, or the Pluto transmits a
        // carrier pair instead of the modulation — at full power, on frequency.
        assert_eq!(g.get("cf-ad9361-dds-core-lpc/OUTPUT/altvoltage0/raw"), Some("0"));
        assert_eq!(g.get("cf-ad9361-dds-core-lpc/OUTPUT/altvoltage1/raw"), Some("0"));
        assert_eq!(g.get("ad9361-phy/OUTPUT/altvoltage1/frequency"), Some("145500000"));
        // 16-bit little-endian, full scale in I and nothing in Q.
        let payload = &g.tx_payloads[0];
        assert_eq!(
            &payload[..4],
            &[0xFF, 0x7F, 0x00, 0x00],
            "first sample was {:?}",
            &payload[..4]
        );
    }

    handle.tx_end();
    wait_for("receive to resume", || fake.state.lock().unwrap().rx_buffer_open);
    {
        let g = fake.state.lock().expect("lock");
        assert!(!g.tx_buffer_open, "transmit was still running after the over");
        // The receive oscillator is put back where the dial is.
        assert_eq!(g.get("ad9361-phy/OUTPUT/altvoltage0/frequency"), Some("145500000"));
        assert!(g.rx_buffer_opens >= 2, "receive should have reopened");
    }

    handle.release();
}

#[test]
fn a_retune_reaches_the_oscillator_while_samples_are_flowing() {
    let fake = Fake::start();
    let handle =
        PlutoHandle::open(&fake.address(), &config(), 100_000_000.0).expect("open the fake Pluto");
    wait_for("the receive buffer", || fake.state.lock().unwrap().rx_buffer_open);

    handle.set_rx_freq(144_200_000.0);
    wait_for("the retune", || {
        fake.state.lock().unwrap().get("ad9361-phy/OUTPUT/altvoltage0/frequency")
            == Some("144200000")
    });

    // ppm is applied in software to the requested frequency, not written to the
    // device's own persistent trim.
    handle.set_ppm(10.0);
    wait_for("the ppm trim", || {
        fake.state.lock().unwrap().get("ad9361-phy/OUTPUT/altvoltage0/frequency")
            == Some("144201442")
    });
    assert!(
        fake.state.lock().unwrap().get("ad9361-phy/xo_correction").is_none(),
        "the device's persistent trim must not be touched"
    );

    drop(handle);
}

/// An IIO device that is not a Pluto has to be named, not driven into nonsense.
#[test]
fn a_non_pluto_iio_device_is_refused_by_name() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let (sock, _) = listener.accept().expect("accept");
        let mut writer = sock.try_clone().expect("clone");
        let mut reader = BufReader::new(sock);
        let xml = r#"<context name="xml" description="x">
            <device id="iio:device0" name="adc081c" />
        </context>"#;
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            let cmd = line.trim().to_string();
            line.clear();
            let _ = match cmd.split_whitespace().next() {
                Some("TIMEOUT") => writer.write_all(b"0\n"),
                Some("VERSION") => writer.write_all(b"0.25 (git tag:v0.25)\n"),
                Some("PRINT") => writer.write_all(format!("{}\n{xml}\n", xml.len()).as_bytes()),
                _ => writer.write_all(b"-22\n"),
            };
            let _ = writer.flush();
        }
    });

    let text = match PlutoHandle::open(&addr.to_string(), &config(), 435_000_000.0) {
        Ok(_) => panic!("an ADC is not a Pluto and must not open as one"),
        Err(e) => e.to_string(),
    };
    assert!(text.contains("not a PlutoSDR"), "{text}");
    assert!(text.contains("adc081c"), "{text}");
}

/// A firmware that publishes no `_available` attributes still has to work — and
/// has to say which figures are guesses rather than quoting them as fact.
#[test]
fn a_silent_firmware_falls_back_and_says_so() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    // The same context with every `_available` attribute removed, and an
    // AD9363 model string.
    let xml = CONTEXT_XML
        .replace("(Z7010-AD9364)", "(Z7010-AD9363)")
        .lines()
        .filter(|l| !l.contains("_available"))
        .collect::<Vec<_>>()
        .join("\n");
    let state = Arc::new(Mutex::new(DeviceState::default()));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let state = Arc::clone(&state);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            for sock in listener.incoming().take(3) {
                let Ok(sock) = sock else { break };
                let state = Arc::clone(&state);
                let stop = Arc::clone(&stop);
                let xml = xml.clone();
                std::thread::spawn(move || serve_with_xml(sock, state, stop, xml));
            }
        });
    }

    let handle = PlutoHandle::open(&addr.to_string(), &config(), 435_000_000.0)
        .expect("a silent firmware still opens");
    // The AD9363 range, because that is what the model string names.
    assert_eq!(handle.limits.rx_lo_hz, (325_000_000.0, 3_800_000_000.0));
    let status = handle.open_status().expect("a guessed limit has to announce itself");
    assert!(status.contains("assuming"), "{status}");
    assert!(status.contains("tuning range"), "{status}");
    drop(handle);
    stop.store(true, Ordering::Relaxed);
}

/// The same server as [`serve`], with the context document substituted.
fn serve_with_xml(
    sock: TcpStream,
    state: Arc<Mutex<DeviceState>>,
    stop: Arc<AtomicBool>,
    xml: String,
) {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));
    let mut writer = sock.try_clone().expect("clone");
    let mut reader = BufReader::new(sock);
    let mut line = String::new();
    while !stop.load(Ordering::Relaxed) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => break,
        }
        let cmd = line.trim_end_matches(['\r', '\n']).to_string();
        if cmd.is_empty() {
            continue;
        }
        let words: Vec<&str> = cmd.split_whitespace().collect();
        let ok = match words.first().copied() {
            Some("PRINT") => writer.write_all(format!("{}\n{xml}\n", xml.len()).as_bytes()).is_ok(),
            Some("VERSION") => writer.write_all(b"0.25 (git tag:v0.25)\n").is_ok(),
            Some("TIMEOUT") => writer.write_all(b"0\n").is_ok(),
            Some("READ") => {
                let key = attr_key(&words[1..]);
                let value = {
                    let g = state.lock().expect("lock");
                    g.get(&key).map(str::to_string)
                }
                .unwrap_or_else(|| default_attr(&key).to_string());
                let mut payload = value.into_bytes();
                payload.push(0);
                writer
                    .write_all(format!("{}\n", payload.len()).as_bytes())
                    .and_then(|()| writer.write_all(&payload))
                    .and_then(|()| writer.write_all(b"\n"))
                    .is_ok()
            }
            Some("WRITE") => {
                let len: usize = words.last().and_then(|s| s.parse().ok()).unwrap_or(0);
                let key = attr_key(&words[1..words.len() - 1]);
                let mut payload = vec![0u8; len];
                if reader.read_exact(&mut payload).is_err() {
                    break;
                }
                let value =
                    String::from_utf8_lossy(&payload).trim_end_matches('\0').trim().to_string();
                state.lock().expect("lock").attrs.push((key, value));
                writer.write_all(format!("{len}\n").as_bytes()).is_ok()
            }
            Some("OPEN") | Some("CLOSE") => writer.write_all(b"0\n").is_ok(),
            Some("READBUF") | Some("WRITEBUF") => writer.write_all(b"-11\n").is_ok(),
            Some("EXIT") => break,
            _ => writer.write_all(b"-22\n").is_ok(),
        };
        if !ok || writer.flush().is_err() {
            break;
        }
    }
}
