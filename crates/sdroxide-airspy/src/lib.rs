//! Native Airspy R2 / Mini driver.
//!
//! Pure Rust over `nusb`: no libairspy, no libusb, no SoapySDR module, so this
//! backend is in every build variant on every platform.
//!
//! # Not the Airspy HF+
//!
//! `sdroxide-airspyhf` next door drives a *different radio*. The two share a
//! vendor and nothing else: different silicon, different USB id, a different
//! protocol, and a different host-DSP problem. Nothing here is a port of that
//! crate and the two should not be made to look like one.
//!
//! # What the host has to do
//!
//! This receiver's ADC is **real**, and the wanted signal sits at a quarter of
//! the sample rate. So the host does the downconversion: DC removal, a
//! multiply-free fs/4 rotation, and a half-band decimator — see
//! [`convert::HostDsp`]. That is also why the rate programmed into the receiver
//! is twice the complex rate the operator picks; see
//! [`protocol::program_rate_hz`].
//!
//! # Provenance
//!
//! The protocol, the rate arithmetic and the gain curves are transcribed from
//! Airspy's [libairspy](https://github.com/airspy/airspyone_host) —
//! `libairspy/src/airspy.c` and `airspy_commands.h` (BSD-3-Clause, compatible
//! with this workspace's GPL-3.0-or-later). The half-band filter is **not**
//! transcribed: it is designed here and measured by a test, because a copied
//! coefficient table cannot tell you what it actually achieves.
//!
//! # Not verified against hardware
//!
//! This driver was written from the reference implementation, not on a bench.

pub mod convert;
pub mod device;
pub mod error;
pub mod handle;
pub mod protocol;
pub mod stream;
pub mod trace;
pub mod usb;

pub use device::Device;
pub use error::{Error, Result};
pub use handle::AirspyHandle;
pub use trace::{FIELD_REPORT_HINT, diagnostics};
pub use usb::list;
