//! The AX.25 lane behind the `Transport` seam, with no radio.
//!
//! `session::run_into` is generic over `Read + Write` and has no idea a radio
//! exists, so a fake gateway on the other end of a `port_pair` exercises
//! `Ax25Transport` exactly as a real link would — the blocking read, the
//! clean-disconnect path, the failure path — while the B2F conversation itself
//! stays the one already pinned by `session.rs`'s own tests.
//!
//! What this does **not** prove is that a real RMS gateway answers. That needs
//! the air, and is the operator's step.

use std::io::{Read, Write};
use std::time::Duration;

use sdroxide_ax25::{Addr, LinkConfig, PortEvent, PortRequest, port_pair};
use sdroxide_winlink::transport::{Ax25Transport, Transport};

fn link() -> (sdroxide_ax25::PortHandle, sdroxide_ax25::PortEndpoint) {
    port_pair(LinkConfig { me: Addr::new("OE3JJS-10").unwrap(), paclen: 128, maxframe: 4 })
}

/// Wait for the client's connect request, then report the link up.
fn answer(endpoint: &sdroxide_ax25::PortEndpoint) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if endpoint.take_requests().iter().any(|r| matches!(r, PortRequest::Connect { .. })) {
            endpoint.emit(PortEvent::Connected);
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("no connect request arrived");
}

/// The transport connects, carries bytes both ways, and names the gateway —
/// which is what `SessionConfig::target_call` now uses for the greeting instead
/// of the hard-coded `wl2k`.
#[test]
fn the_transport_carries_a_conversation() {
    let (handle, endpoint) = link();
    let g = std::thread::spawn(move || {
        answer(&endpoint);
        endpoint.emit(PortEvent::Data(b"hello from the gateway\r".to_vec()));

        let mut seen = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !seen.contains("MY TURN") && std::time::Instant::now() < deadline {
            for r in endpoint.take_requests() {
                if let PortRequest::Data(d) = r {
                    seen.push_str(&String::from_utf8_lossy(&d));
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(seen.contains("MY TURN"), "the gateway never heard us: {seen:?}");
        endpoint.emit(PortEvent::Data(b"ack\r".to_vec()));
        endpoint.emit(PortEvent::Disconnected);
    });

    let mut t =
        Ax25Transport::connect(handle, "OE1XAR-10", &[], Duration::from_secs(5)).expect("connect");
    assert_eq!(t.target_call(), "OE1XAR-10");
    assert!(t.describe().contains("OE1XAR-10"));

    let mut buf = [0u8; 64];
    let n = t.read(&mut buf).expect("read");
    assert_eq!(&buf[..n], b"hello from the gateway\r");

    t.write_all(b"MY TURN\r").expect("write");
    let n = t.read(&mut buf).expect("read");
    assert_eq!(&buf[..n], b"ack\r");

    drop(t);
    g.join().unwrap();
}

/// A clean disconnect must surface as `Ok(0)` *after* the buffered bytes, not
/// instead of them. A gateway that sends its last block and hangs up in the
/// same breath is ordinary, and losing that block would lose the end of a
/// message — silently, because `Ok(0)` reads as a tidy end of session.
#[test]
fn a_clean_hangup_delivers_the_last_bytes_first() {
    let (handle, endpoint) = link();
    std::thread::spawn(move || {
        answer(&endpoint);
        endpoint.emit(PortEvent::Data(b"FQ\r".to_vec()));
        endpoint.emit(PortEvent::Disconnected);
    });

    let mut t =
        Ax25Transport::connect(handle, "OE1XAR-10", &[], Duration::from_secs(5)).expect("connect");
    let mut buf = [0u8; 16];
    let n = t.read(&mut buf).expect("read");
    assert_eq!(&buf[..n], b"FQ\r", "the last block was lost to the hangup");
    assert_eq!(t.read(&mut buf).expect("read"), 0, "a clean hangup must read as Ok(0)");
}

/// A link that gives up is an error, not an end of stream. The session has to
/// tell "the gateway said goodbye" from "the path died", and only the second is
/// worth reporting as a failure.
#[test]
fn a_failed_link_is_an_error_not_an_eof() {
    let (handle, endpoint) = link();
    std::thread::spawn(move || {
        answer(&endpoint);
        endpoint.emit(PortEvent::Failed("retries exhausted".into()));
    });

    let mut t =
        Ax25Transport::connect(handle, "OE1XAR-10", &[], Duration::from_secs(5)).expect("connect");
    let mut buf = [0u8; 16];
    let e = t.read(&mut buf).expect_err("a dead link must not read as EOF");
    assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe);
    assert!(e.to_string().contains("retries exhausted"), "the reason must survive: {e}");
}

/// A gateway that never answers times out with something the operator can read,
/// rather than blocking the worker for ever.
#[test]
fn an_unanswered_call_times_out() {
    let (handle, _endpoint) = link();
    let e = Ax25Transport::connect(handle, "OE1XAR-10", &[], Duration::from_millis(200))
        .expect_err("an unanswered call must not succeed");
    assert!(e.to_string().contains("OE1XAR-10"), "the error should name the gateway: {e}");
}

/// One session at a time on one radio.
#[test]
fn a_second_session_is_refused() {
    let (handle, endpoint) = link();
    std::thread::spawn(move || {
        answer(&endpoint);
        std::thread::sleep(Duration::from_secs(2));
    });

    let first = Ax25Transport::connect(handle.clone(), "OE1XAR-10", &[], Duration::from_secs(5))
        .expect("first");
    let e = Ax25Transport::connect(handle, "OE1XAR-10", &[], Duration::from_millis(200))
        .expect_err("two sessions took one radio");
    assert!(e.to_string().contains("already in use"), "{e}");
    drop(first);
}

/// A mistyped callsign is caught before anything is keyed.
#[test]
fn a_bad_gateway_callsign_is_refused_up_front() {
    let (handle, _endpoint) = link();
    let e = Ax25Transport::connect(handle, "not a callsign", &[], Duration::from_secs(1))
        .expect_err("garbage accepted as a callsign");
    assert!(e.to_string().to_lowercase().contains("callsign"), "{e}");
}
