//! The 868 MHz ISM burst decoder: reads the unattended digital traffic that
//! fills the European ISM band — weather and soil sensors, utility meters, home
//! automation — and reports each device it hears with its readings in real
//! units.
//!
//! Native-only, like the skimmers: it runs in the engine and reaches the UI as
//! [`sdroxide_types::IsmReport`]s over the ordinary event path.
//!
//! # Shape of the thing
//!
//! ```text
//! wide IQ window  ──►  one Ddc per channel in the plan  ──►  Gate
//!                                                              │ burst
//!                            Frame ◄── slice ◄── discriminate ◄─┘
//!                              │
//!                              └──► protocol registry ──► CRC ──► IsmReport
//! ```
//!
//! [`plan`] says which channels exist and which of them the receiver can reach;
//! [`gate`] cuts the stream into transmissions; [`demod`] and [`slice`] recover
//! bits; [`proto`] turns validated frames into readings. [`controller`] wraps the
//! lot in a worker thread.
//!
//! # Provenance
//!
//! Every protocol module is written from that protocol's published
//! specification or public reverse-engineering write-up, cited in its own
//! header, and not ported from an existing decoder. `rtl_433` is the obvious
//! prior art and is GPL-2.0-or-later; field layouts are facts about the devices
//! and cross-checking against it is fair, but no code came from it.

mod controller;
mod crc;
mod demod;
mod gate;
mod plan;
pub mod probe;
mod proto;
mod slice;

pub use controller::{IsmAction, IsmController};
pub use gate::Burst;
pub use plan::{CHANNELS, Channel, USABLE_FRACTION, ideal_center_hz, span_hz, window_center_hz};
pub use probe::{BurstReport, Probe, Survey};
