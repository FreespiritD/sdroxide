//! The seam between a connected-mode link and whatever wants a byte stream.
//!
//! A Winlink forwarding session is written against `Read`/`Write` and blocks on
//! its own worker thread. The link layer lives on the engine's audio thread,
//! because that is where PTT and the sample clock are. This is the pair of ends
//! that joins them, and nothing but bytes and intentions crosses it.
//!
//! ```text
//! worker thread                         engine thread
//! ─────────────                         ─────────────
//! session::run_into(&mut transport)     PacketController
//!         │                                  ├─ ax25::state + timers
//!         └── PortHandle ⇄ channels ⇄ PortEndpoint ─┤
//!             Connect/Data/Disconnect            ├─ modem
//!             Connected/Data/Disconnected/Failed └─ CSMA + PTT
//! ```
//!
//! # Two deliberate choices
//!
//! **The request channel is bounded.** `Write::write` blocking when the window
//! is full *is* the backpressure: without it, B2F would hand over a whole
//! compressed message in a moment and the session would believe it had been
//! sent while nothing had left the antenna.
//!
//! **A lease, not a mutex.** Two owners interleaving on one link would produce
//! traffic neither understands, so [`PortHandle::try_claim`] hands out one lease
//! at a time and the second caller gets a clear refusal — the same shape as the
//! engine's transmit gate.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

use crate::addr::Addr;

/// How many outstanding writes the session may run ahead by.
///
/// Eight frames' worth: enough that the link never starves waiting for the
/// session between overs, small enough that a session which stops reading
/// cannot bank a message the radio has not sent.
const WINDOW: usize = 8;

/// What the session asks the link to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortRequest {
    Connect { peer: Addr, via: Vec<Addr>, ext: bool },
    Data(Vec<u8>),
    Disconnect,
}

/// What the link tells the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortEvent {
    Connected,
    Data(Vec<u8>),
    /// A clean shutdown — DISC/UA, or the peer went away politely.
    Disconnected,
    /// The link gave up: retries exhausted, the mode changed under it, the
    /// operator aborted. Carries something worth putting in the session log.
    Failed(String),
}

/// Settings the link needs that are not the state machine's business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkConfig {
    /// Our own address. A link cannot be opened without one.
    pub me: Addr,
    /// Longest information field we send.
    pub paclen: u16,
    /// Frames outstanding before an acknowledgement is required.
    pub maxframe: u8,
}

/// The session's end. Cloneable so a manager can hold one while a worker uses
/// another, but only one lease may be live at a time.
#[derive(Debug, Clone)]
pub struct PortHandle {
    tx: Sender<PortRequest>,
    rx: Receiver<PortEvent>,
    claimed: Arc<AtomicBool>,
}

/// Proof of exclusive use. Releases on drop.
#[derive(Debug)]
pub struct PortLease {
    claimed: Arc<AtomicBool>,
}

impl Drop for PortLease {
    fn drop(&mut self) {
        self.claimed.store(false, Ordering::SeqCst);
    }
}

impl PortHandle {
    /// Take exclusive use of the link, or `None` if somebody already has it.
    #[must_use]
    pub fn try_claim(&self) -> Option<PortLease> {
        if self.claimed.swap(true, Ordering::SeqCst) {
            return None;
        }
        Some(PortLease { claimed: Arc::clone(&self.claimed) })
    }

    /// Ask the link to do something. `Err` means the engine end is gone — the
    /// operator changed mode, or the radio was closed.
    pub fn send(&self, req: PortRequest) -> Result<(), PortClosed> {
        self.tx.send(req).map_err(|_| PortClosed)
    }

    /// Non-blocking send, for a caller that must not stall.
    pub fn try_send(&self, req: PortRequest) -> Result<(), TrySendError<PortRequest>> {
        self.tx.try_send(req)
    }

    /// Block for the next event. `Err` means the link is gone.
    pub fn recv(&self) -> Result<PortEvent, PortClosed> {
        self.rx.recv().map_err(|_| PortClosed)
    }

    /// Block for the next event, giving up after `timeout`.
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<PortEvent> {
        self.rx.recv_timeout(timeout).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the packet link is gone")]
pub struct PortClosed;

/// The engine's end, held by the controller.
#[derive(Debug)]
pub struct PortEndpoint {
    rx: Receiver<PortRequest>,
    tx: Sender<PortEvent>,
    pub cfg: LinkConfig,
}

impl PortEndpoint {
    /// Take whatever the session has asked for. Never blocks — this runs on the
    /// audio thread, which owes the transmitter a block every 10 ms.
    pub fn take_requests(&self) -> Vec<PortRequest> {
        self.rx.try_iter().collect()
    }

    /// Tell the session something. Dropped silently when the session has gone,
    /// which is normal: it may have given up while the link was still tidying.
    pub fn emit(&self, ev: PortEvent) {
        let _ = self.tx.send(ev);
    }
}

/// Make a linked pair.
#[must_use]
pub fn port_pair(cfg: LinkConfig) -> (PortHandle, PortEndpoint) {
    let (req_tx, req_rx) = bounded(WINDOW);
    // Events are bounded too, and more generously: a burst of received frames
    // must not block the audio thread, but an unbounded queue would let a
    // stalled session grow one without limit.
    let (ev_tx, ev_rx) = bounded(WINDOW * 8);
    (
        PortHandle { tx: req_tx, rx: ev_rx, claimed: Arc::new(AtomicBool::new(false)) },
        PortEndpoint { rx: req_rx, tx: ev_tx, cfg },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LinkConfig {
        LinkConfig { me: Addr::new("OE3JJS-10").unwrap(), paclen: 128, maxframe: 4 }
    }

    #[test]
    fn requests_and_events_cross() {
        let (h, e) = port_pair(cfg());
        h.send(PortRequest::Data(b"hello".to_vec())).unwrap();
        assert_eq!(e.take_requests(), vec![PortRequest::Data(b"hello".to_vec())]);
        e.emit(PortEvent::Connected);
        assert_eq!(h.recv().unwrap(), PortEvent::Connected);
    }

    /// The window is the backpressure. Without it a session believes a whole
    /// message has been sent while none of it has left the antenna.
    #[test]
    fn the_request_window_is_bounded() {
        let (h, _e) = port_pair(cfg());
        for _ in 0..WINDOW {
            h.try_send(PortRequest::Data(vec![0; 8])).expect("within the window");
        }
        assert!(h.try_send(PortRequest::Data(vec![0; 8])).is_err(), "the window did not bound");
    }

    /// One owner at a time, and the lease comes back on drop.
    #[test]
    fn only_one_owner_at_a_time() {
        let (h, _e) = port_pair(cfg());
        let lease = h.try_claim().expect("first claim");
        assert!(h.try_claim().is_none(), "two owners on one link");
        drop(lease);
        assert!(h.try_claim().is_some(), "the lease did not come back");
    }

    /// A dropped controller must surface as an error, not a hang: the mode
    /// changed under the session and it needs to unwind with a transcript.
    #[test]
    fn a_dropped_link_is_an_error_not_a_hang() {
        let (h, e) = port_pair(cfg());
        drop(e);
        assert_eq!(h.send(PortRequest::Disconnect), Err(PortClosed));
        assert_eq!(h.recv(), Err(PortClosed));
    }
}
