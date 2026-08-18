//! Native ELAD FDM-DUO / FDM-S2 / FDM-S1 driver.
//!
//! Pure Rust over `nusb`: no libusb, no gr-elad, no SoapySDR module, so this
//! backend is in every build variant on every platform.
//!
//! # Shape
//!
//! One blocking thread owns the device ([`stream`]), control reaches it over a
//! crossbeam channel, and samples come back through an `rtrb` ring of
//! interleaved `f32` — the same shape as [`sdroxide_airspyhf`] and
//! [`sdroxide_rtlsdr`]. [`protocol`] holds the wire with no `nusb` in it, which
//! is what makes the fiddly half of this driver testable without hardware.
//!
//! # What the hardware is
//!
//! All three devices are direct-sampling receivers — a 122.88 MHz ADC (61.44 on
//! the FDM-S1) behind a switchable low-pass bank and a 12 dB pad — with an FPGA
//! digital down-converter delivering one complex channel over a bulk endpoint.
//! Tuning is therefore not a synthesiser but a 32-bit phase increment written
//! into that DDC, which is why an FDM-S2 can hear above its own clock: the
//! modulo in [`protocol::tuning_word`] is an alias, on purpose.
//!
//! The FDM-DUO is a transceiver wrapped around the same receiver, and it is
//! **three USB devices**: this one, an FTDI bridge carrying its Kenwood-derived
//! CAT protocol, and a C-Media sound card carrying demodulated receive audio
//! and transmit audio. This crate drives only the first. Nothing here
//! transmits, on any model.
//!
//! # The sample rate cannot be set
//!
//! The DDC delivers 192, 384, 768, 1536, 3072 or 6144 kHz and **no request this
//! driver knows how to send selects between them**. ELAD's own GNU Radio module
//! does not set it either — it takes the rate as a parameter and uses it only to
//! scale — and the FDM-DUO has no front-panel menu for it, which together say
//! the decimation is programmed by ELAD's Windows software through some request
//! that has never been published. So `EladConfig::sample_rate_hz` says how the
//! stream is *read*, the device arrives in whatever mode it was left in, and the
//! driver measures the throughput a couple of seconds in and says so if the two
//! disagree (see `handle::RxStats::check_rate`).
//!
//! # Provenance
//!
//! The vendor protocol — device ids, the control transfers, the tuning
//! arithmetic, the EEPROM calibration map and the sample scaling — is
//! transcribed from ELAD's own [gr-elad](https://github.com/ELADIT/gr-elad),
//! `lib/fdm_source_c_impl.cc` (GPL-3.0-or-later, the same licence as this
//! workspace). No code is copied; the constants and the sequence are what
//! matter and both are cited where they are used. The FDM-DUO's CAT commands
//! come from the *ELAD FDM-DUO User Manual* v2.6, §6.
//!
//! # Not verified against hardware
//!
//! This driver was written from that reference, not on a bench. Every uncertain
//! point carries a comment saying so, and [`diagnostics`] renders a session
//! trace built for exactly one purpose: making a first bug report from an owner
//! actionable.

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
pub use handle::EladHandle;
pub use protocol::Model;
pub use trace::{FIELD_REPORT_HINT, diagnostics};
pub use usb::list;
