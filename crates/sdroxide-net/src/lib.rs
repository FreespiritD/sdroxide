//! Native network-cockpit clients for sdroxide: DX-cluster telnet, POTA/SOTA
//! and PSK Reporter spot feeds, QRZ/HamQTH callsign lookup, and QSO upload /
//! QSL-confirmation download (eQSL, QRZ Logbook, Club Log, LoTW).
//!
//! NATIVE ONLY — this crate does blocking network I/O on its own threads and
//! must never be a dependency of any wasm-targeted crate. The GUI keeps its
//! threadless model: the [`SpotManager`] owns every feed/worker thread and the
//! engine only drains it (non-blocking) on its existing poll loop.

mod cluster;
mod event;
mod http;
mod lookup;
mod manager;
mod poll;
mod pota;
mod pskreporter;
mod sota;
mod upload;

pub use event::NetEvent;
pub use manager::SpotManager;

// On-demand blocking helpers, exposed for tests / direct use.
pub use lookup::lookup as lookup_callsign;
pub use upload::{sync_confirmations, upload as upload_qso};
