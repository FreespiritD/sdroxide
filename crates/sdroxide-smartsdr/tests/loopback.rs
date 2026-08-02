//! The client driven against the simulator over real sockets.
//!
//! Nobody here has a FLEX-6600 on the bench, so this is the only thing standing
//! between the protocol code and a user's radio. It deliberately asserts on
//! *wire* outcomes — the commands the radio received, the IQ that came back, the
//! audio packets that arrived — rather than on the client's internal state,
//! because internal state would still agree with itself if the bytes were wrong.

use std::time::Duration;

use sdroxide_smartsdr::sim::{SimConfig, SimRadio};
use sdroxide_smartsdr::{FlexHandle, FlexUpdate};
use sdroxide_types::Mode;

/// Long enough to absorb a loaded CI machine; every wait exits as soon as its
/// condition holds, so a passing run does not spend it.
const PATIENCE: Duration = Duration::from_secs(5);

fn sim() -> SimRadio {
    SimRadio::start(SimConfig::default()).expect("start simulator")
}

fn connect(sim: &SimRadio, rate: f64) -> FlexHandle {
    FlexHandle::connect(&sim.address(), rate, 1, "test").expect("connect to simulator")
}

/// Drain IQ until `want` samples have arrived or patience runs out.
fn collect_iq(h: &mut FlexHandle, want: usize) -> Vec<f32> {
    let mut got = Vec::with_capacity(want);
    let mut buf = vec![0f32; 8192];
    let deadline = std::time::Instant::now() + PATIENCE;
    while got.len() < want && std::time::Instant::now() < deadline {
        let n = h.rx_read(&mut buf);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        got.extend_from_slice(&buf[..n]);
    }
    got
}

#[test]
fn connects_and_reports_the_radio_identity() {
    let sim = sim();
    let h = connect(&sim, 48_000.0);

    assert_eq!(h.ident.version, "3.3.28.0");
    assert_ne!(h.ident.handle, 0, "the radio must assign a client handle");
    assert!(h.is_alive());

    // The model arrives in the `radio` status that answers `client gui`, not in
    // the V/H handshake, so it is only present if the connect path reads the
    // status burst it read past on the way to that reply. It decides which
    // frequency ranges the caller advertises, so an empty one is not cosmetic.
    assert_eq!(h.ident.model, "FLEX-6600");
    assert_eq!(h.ident.nickname, "sdroxide-sim");
    assert_eq!(h.ident.display_name(), "sdroxide-sim (FLEX-6600)");

    // The handshake order is the radio's protocol state machine, not a
    // preference: identity, then GUI registration, then the UDP port, then
    // subscriptions. Getting it wrong is refused by real firmware.
    let cmds = sim.stats.commands();
    let pos = |prefix: &str| {
        cmds.iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("never sent {prefix:?}; sent: {cmds:#?}"))
    };
    assert!(pos("client program") < pos("client gui"));
    assert!(pos("client gui") < pos("client udpport"));
    assert!(pos("client gui") < pos("sub slice"));
}

#[test]
fn dax_iq_flows_and_carries_the_synthetic_scene() {
    let sim = sim();
    let mut h = connect(&sim, 48_000.0);

    let iq = collect_iq(&mut h, 4096);
    assert!(iq.len() >= 4096, "only {} IQ samples arrived", iq.len());
    assert_eq!(iq.len() % 2, 0, "I/Q must stay paired");

    // The scene has a carrier on the tuned frequency, so the samples must carry
    // real power. A byte-order mistake in the DAX IQ decoder yields denormals
    // ≈ 0 here rather than an error, which is exactly the failure this catches.
    let power: f64 = iq.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / iq.len() as f64;
    assert!(power > 1e-4, "IQ power {power:e} is at the noise floor — byte order?");
    assert!(iq.iter().all(|v| v.is_finite()));
}

#[test]
fn the_radio_delivers_the_requested_iq_rate() {
    let sim = sim();
    // A FLEX creates every dax_iq stream at 48 kHz and only moves on the
    // follow-up `stream set daxiq_rate`. The rate the packets actually carry is
    // read back off their class codes, so this checks the second command landed.
    let mut h = connect(&sim, 192_000.0);
    collect_iq(&mut h, 2048);

    assert!(sim.stats.saw_command("stream set"), "never asked for a rate change");
    assert_eq!(h.actual_rate_hz(), 192_000.0, "radio is streaming at the wrong rate");
}

#[test]
fn tuning_moves_the_radios_slice() {
    let sim = sim();
    let h = connect(&sim, 48_000.0);

    h.set_center(7_074_000.0);
    // The radio's own slice has to follow our centre, or a second client (and
    // the radio's front panel) shows a frequency we are not listening on.
    assert!(
        sim.wait_for(PATIENCE, |s| (s.slice_freq_hz() - 7_074_000.0).abs() < 1.0),
        "slice sat at {:.0} Hz, expected 7074000",
        sim.stats.slice_freq_hz()
    );

    // An IF offset puts the slice inside the span without moving the centre —
    // the panadapter must not be dragged along, or the offset never converges.
    h.set_if_offset(2_500.0);
    assert!(
        sim.wait_for(PATIENCE, |s| (s.slice_freq_hz() - 7_076_500.0).abs() < 1.0),
        "slice sat at {:.0} Hz, expected 7076500",
        sim.stats.slice_freq_hz()
    );
    assert!(
        sim.stats.commands().iter().any(|c| c.contains("autopan=0")),
        "slice tune must suppress autopan"
    );
}

#[test]
fn a_tuned_carrier_lands_where_it_should() {
    let sim = sim();
    let mut h = connect(&sim, 48_000.0);

    // The simulator anchors its carriers in RF, so retuning genuinely moves them
    // through the passband. Park the centre 10 kHz below a carrier and the
    // baseband tone must come out at +10 kHz — which a client that confuses the
    // centre with the dial gets wrong.
    h.set_center(14_090_000.0); // carrier at 14.100 MHz → +10 kHz
    std::thread::sleep(Duration::from_millis(250)); // let the retune take effect
    let mut buf = vec![0f32; 65536];
    let _ = h.rx_read(&mut buf); // discard samples from before the retune
    let iq = collect_iq(&mut h, 16384);
    assert!(iq.len() >= 16384);

    let rate = 48_000.0;
    let n = iq.len() / 2;
    // Correlate against the expected tone; a matched frequency integrates
    // coherently, a wrong one averages away.
    let power_at = |hz: f64| -> f64 {
        use std::f64::consts::TAU;
        let (mut re, mut im) = (0.0, 0.0);
        for k in 0..n {
            let ph = TAU * hz * k as f64 / rate;
            let (i, q) = (iq[2 * k] as f64, iq[2 * k + 1] as f64);
            re += i * ph.cos() + q * ph.sin();
            im += q * ph.cos() - i * ph.sin();
        }
        (re * re + im * im) / (n * n) as f64
    };

    let want = power_at(10_000.0);
    let decoy = power_at(-10_000.0);
    assert!(
        want > decoy * 10.0,
        "carrier not at +10 kHz: power there {want:e} vs {decoy:e} at −10 kHz \
         (a sign error in the tuning or a conjugated spectrum)"
    );
}

#[test]
fn transmit_audio_reaches_the_radio() {
    let sim = sim();
    let mut h = connect(&sim, 48_000.0);
    collect_iq(&mut h, 512); // let the session settle

    let rate = h.tx_begin(14_074_000.0);
    assert_eq!(rate, 24_000.0, "DAX TX audio is 24 kHz");

    // A second of a 1 kHz tone at half scale.
    let tone: Vec<f32> = (0..24_000)
        .map(|k| (std::f64::consts::TAU * 1000.0 * k as f64 / 24_000.0).sin() as f32 * 0.5)
        .collect();
    h.tx_write(&tone);

    assert!(
        sim.wait_for(PATIENCE, |s| s.tx_frames.load(std::sync::atomic::Ordering::Relaxed) > 4096),
        "the radio received only {} TX frames",
        sim.stats.tx_frames.load(std::sync::atomic::Ordering::Relaxed)
    );
    // Amplitude has to survive the trip: a byte-order mistake on the TX side
    // would deliver frames full of denormals, and the radio would transmit
    // silence at full power.
    let peak = sim.stats.tx_peak();
    assert!((0.4..=0.6).contains(&peak), "TX peak {peak} — expected ≈0.5");

    assert!(sim.stats.keyed.load(std::sync::atomic::Ordering::Relaxed), "radio is not keyed");
    // The modulator has to be switched to DAX, or the radio transmits mic audio.
    assert!(sim.stats.saw_command("transmit set dax=1"));

    h.tx_end();
    assert!(sim.wait_for(PATIENCE, |s| !s.keyed.load(std::sync::atomic::Ordering::Relaxed)));
    // And handed back, or the operator's next local transmission is silent.
    // Strictly after the unkey: releasing the modulator while still keyed would
    // put a moment of shack ambience on the air.
    assert!(sim.wait_for(PATIENCE, |s| s.saw_command("transmit set dax=0")));
    let cmds = sim.stats.commands();
    let pos = |c: &str| cmds.iter().rposition(|x| x == c).expect("command not sent");
    assert!(pos("xmit 0") < pos("transmit set dax=0"), "modulator released before unkeying");
}

#[test]
fn transmit_audio_is_paced_by_the_clock_not_by_receive_packets() {
    use std::sync::atomic::Ordering;

    let sim = sim();
    // 24 kHz IQ is the worst case for the coupling this guards against: the
    // radio sends only ~190 IQ packets a second, and a transmit pump that
    // emitted one audio packet per received one could not reach the 187
    // packets/s that 24 kHz DAX audio needs. Under-feeding the radio's buffer
    // does not sound like silence, it sounds like a chopped transmission.
    let mut h = connect(&sim, 24_000.0);
    collect_iq(&mut h, 512);

    h.tx_begin(14_074_000.0);
    // Two seconds of audio, handed over all at once the way the engine hands
    // over an FT8 burst.
    let tone: Vec<f32> = (0..48_000)
        .map(|k| (std::f64::consts::TAU * 700.0 * k as f64 / 24_000.0).sin() as f32 * 0.5)
        .collect();
    let started = std::time::Instant::now();
    h.tx_write(&tone);

    // After one second of wall clock, roughly one second of audio must have
    // reached the radio — not a fraction of it, and not all two seconds.
    assert!(
        sim.wait_for(Duration::from_millis(1400), |s| s.tx_frames.load(Ordering::Relaxed)
            >= 20_000)
    );
    let elapsed = started.elapsed().as_secs_f64();
    let frames = sim.stats.tx_frames.load(Ordering::Relaxed);
    let rate = frames as f64 / elapsed;
    assert!(
        (18_000.0..32_000.0).contains(&rate),
        "TX ran at {rate:.0} frames/s, expected ≈24000 ({frames} frames in {elapsed:.2} s)"
    );

    h.tx_end();
}

#[test]
fn swr_telemetry_arrives_while_keyed() {
    let sim = sim();
    let mut h = connect(&sim, 48_000.0);
    collect_iq(&mut h, 512);

    h.set_drive(0.5);
    h.tx_begin(14_074_000.0);
    let tone: Vec<f32> = (0..48_000).map(|k| ((k % 24) as f32 / 24.0) - 0.5).collect();
    h.tx_write(&tone);

    // Meter ids are per-session, so this only works if the client learned them
    // from the `meter` definition status — the `#`-separated one.
    let deadline = std::time::Instant::now() + PATIENCE;
    let mut swr = None;
    while std::time::Instant::now() < deadline && swr.is_none() {
        if let Some(t) = h.poll_telemetry() {
            swr = t.swr;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let swr = swr.expect("no SWR reading arrived while transmitting");
    assert!((swr - 1.3).abs() < 0.05, "SWR {swr}, expected ≈1.3");

    h.tx_end();
}

#[test]
fn a_mode_change_reaches_the_radio_and_comes_back() {
    let sim = sim();
    let h = connect(&sim, 48_000.0);

    h.set_mode(Mode::Cw);
    assert!(sim.wait_for(PATIENCE, |s| s.saw_command("slice set 0 mode=CW")));

    // Every sdroxide digital mode asks the radio only for a sideband — the
    // decoding happens here, from the IQ.
    h.set_mode(Mode::Ft8);
    assert!(sim.wait_for(PATIENCE, |s| s.saw_command("slice set 0 mode=DIGU")));
}

#[test]
fn connecting_adopts_the_radios_current_frequency() {
    let sim = sim();
    let h = connect(&sim, 48_000.0);

    // Opening sdroxide must not yank a running station off frequency to
    // whatever was last in its own config; the radio is authoritative about
    // where it is tuned.
    let deadline = std::time::Instant::now() + PATIENCE;
    let mut adopted = None;
    while std::time::Instant::now() < deadline && adopted.is_none() {
        for u in h.poll_updates() {
            if let FlexUpdate::Freq(hz) = u {
                adopted = Some(hz);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(adopted, Some(14_074_000.0), "did not adopt the radio's dial on connect");
}

#[test]
fn operator_changes_on_the_radio_are_reported_back() {
    let sim = sim();
    let h = connect(&sim, 48_000.0);
    // Let the client adopt slice 0 and settle, so the echo-suppression window
    // from its own setup commands has passed and the connect-time adoption
    // (see above) is out of the way.
    std::thread::sleep(Duration::from_millis(400));
    let _ = h.poll_updates();

    // Stand in for somebody turning the radio's own dial.
    h.send_raw("slice tune 0 21.074000");

    let deadline = std::time::Instant::now() + PATIENCE;
    let mut seen = None;
    while std::time::Instant::now() < deadline && seen.is_none() {
        for u in h.poll_updates() {
            if let FlexUpdate::Freq(hz) = u {
                seen = Some(hz);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(seen, Some(21_074_000.0), "the client did not follow the radio's dial");
}

#[test]
fn a_refused_gui_registration_explains_itself() {
    let sim = SimRadio::start(SimConfig { refuse_gui: true, ..SimConfig::default() })
        .expect("start simulator");
    let msg = match FlexHandle::connect(&sim.address(), 48_000.0, 1, "test") {
        Ok(_) => panic!("registration should have been refused"),
        Err(e) => e.to_string(),
    };
    // The bare hex code is the most common thing in a Flex bug report and the
    // least useful one; the message has to say what to do about it.
    assert!(msg.contains("multiFLEX"), "unhelpful message: {msg}");
    assert!(msg.contains("F3000001"), "message drops the code: {msg}");
}

#[test]
fn the_session_trace_records_both_directions() {
    let sim = sim();
    let mut h = connect(&sim, 48_000.0);
    collect_iq(&mut h, 2048);

    let dump = h.trace.dump();
    // This is what a user is asked to send in, so its usefulness is a tested
    // property, not a hope.
    assert!(dump.contains("→ C"), "no outbound commands in the trace");
    assert!(dump.contains("← V3.3.28.0"), "no inbound lines in the trace");
    assert!(dump.contains("client gui"), "the handshake is missing");
    assert!(dump.contains("first packet stream="), "no VITA-49 stream record");
    assert!(dump.contains("--- streams ---"));
}

#[test]
fn dropping_the_handle_releases_the_radios_streams() {
    let sim = sim();
    {
        let mut h = connect(&sim, 48_000.0);
        collect_iq(&mut h, 512);
    } // dropped here

    // A radio whose streams are never removed keeps transmitting them to a port
    // nobody is listening on, and keeps the DAX channel out of the next
    // session's reach.
    assert!(sim.wait_for(PATIENCE, |s| s.saw_command("stream remove")));
    assert!(sim.stats.saw_command("display panafall remove"));
}
