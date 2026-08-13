//! The `rtl_tcp` wire protocol.
//!
//! Twelve bytes of greeting, then an unframed stream of 8-bit unsigned I/Q —
//! byte for byte what the RTL2832U's bulk endpoint produces, which is why the
//! conversion path in [`crate::stream`] is shared between this backend and the
//! USB one. Control travels the other way as five-byte commands and is never
//! answered: the protocol has no replies, no acknowledgements and no error
//! reporting, so everything this end knows about the far end's state is what
//! it asked for rather than what happened.
//!
//! # Provenance
//!
//! Command numbering and the greeting layout are from osmocom `librtlsdr`
//! (`src/rtl_tcp.c`), with [`Cmd::BiasTee`] as the rtl-sdr-blog fork and
//! recent osmocom builds both number it. GPL-2.0-or-later, compatible with
//! this workspace's GPL-3.0-or-later.

use crate::error::{Error, Result};

/// The greeting's magic. `rtl_tcp` sends this; `rsp_tcp` (an SDRplay server
/// speaking a near-identical dialect) sends `RSP0` instead, which is worth
/// naming in the error because the two are easily confused.
pub const MAGIC: &[u8; 4] = b"RTL0";

/// Length of the greeting: magic, tuner type, gain count.
pub const GREETING_LEN: usize = 12;

/// A command opcode. The values are the wire numbers and must not be
/// renumbered.
///
/// Only the commands this backend sends are listed. The gaps are real
/// commands that are deliberately not used: `0x06` sets an individual IF
/// stage's gain (E4000 only), `0x07` is the demodulator's test-pattern
/// generator, `0x0a` is offset tuning (an E4000 feature; the R82xx ignores
/// it), and `0x0b`–`0x0d` override crystal frequencies, which is a far better
/// way to make a receiver silently wrong than to correct it — [`Cmd::Ppm`]
/// does the same job through the calibration the server keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Cmd {
    /// Centre frequency in Hz. On a server that is direct sampling this sets
    /// the DDC's IF instead, which is handled inside its tuning call.
    Freq = 0x01,
    SampleRate = 0x02,
    /// Tuner gain mode: 1 is manual, 0 hands the tuner its own AGC loop.
    /// Note the polarity — this is `rtlsdr_set_tuner_gain_mode(manual)`, so
    /// the value that reads like "automatic" is the one that isn't.
    GainMode = 0x03,
    /// Tuner gain in tenths of a dB. Ignored by the server while its tuner
    /// AGC is running, so [`Cmd::GainMode`] has to go first.
    Gain = 0x04,
    /// Crystal error in ppm, applied to the server's dongle.
    Ppm = 0x05,
    /// The demodulator's own digital AGC (not the tuner's).
    RtlAgc = 0x08,
    /// Direct sampling: 0 off, 1 the ADC's I branch, 2 its Q branch.
    DirectSampling = 0x09,
    /// ~4.5 V up the far end's coax. Absent from older servers, which ignore
    /// unknown opcodes silently.
    BiasTee = 0x0e,
}

/// Encode one command. Five bytes: the opcode, then the argument big-endian.
///
/// Signed arguments (ppm, gain) are sent as their two's-complement bit
/// pattern, which is what the server reads them back out as.
pub fn frame(cmd: Cmd, arg: u32) -> [u8; 5] {
    let a = arg.to_be_bytes();
    [cmd as u8, a[0], a[1], a[2], a[3]]
}

/// The tuner chip the server reports in its greeting.
///
/// This is the *far end's* hardware. It is the only thing a client ever learns
/// about the dongle — there is no serial, no product string and no rate
/// readback in the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tuner {
    Unknown,
    E4000,
    Fc0012,
    Fc0013,
    Fc2580,
    R820T,
    R828D,
}

impl Tuner {
    /// From the greeting's tuner-type word (`enum rtlsdr_tuner`).
    pub fn from_code(code: u32) -> Tuner {
        match code {
            1 => Tuner::E4000,
            2 => Tuner::Fc0012,
            3 => Tuner::Fc0013,
            4 => Tuner::Fc2580,
            5 => Tuner::R820T,
            6 => Tuner::R828D,
            _ => Tuner::Unknown,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tuner::Unknown => "unknown tuner",
            Tuner::E4000 => "E4000",
            Tuner::Fc0012 => "FC0012",
            Tuner::Fc0013 => "FC0013",
            Tuner::Fc2580 => "FC2580",
            // The protocol cannot tell an R820T from an R820T2: librtlsdr
            // reports both as this.
            Tuner::R820T => "R820T/R820T2",
            Tuner::R828D => "R828D",
        }
    }

    /// Whether a dongle with this tuner reaches HF by tuning alone, with no
    /// direct sampling asked of it.
    ///
    /// Only one dongle does: the RTL-SDR Blog V4, which upconverts in hardware
    /// below 28.8 MHz inside the blog fork's tuning call. Over USB this is
    /// read from the EEPROM, but the protocol carries no such thing, so the
    /// tuner is all there is to go on — and a V4 is always an R828D.
    ///
    /// The inference runs the other way for a plain R828D dongle, of which
    /// there are a few: it reports the same chip, cannot upconvert, and so
    /// hears nothing on HF until direct sampling is selected explicitly. That
    /// is why [`sdroxide_types::RtlTcpConfig::hf_mode`]'s `DirectQ` is obeyed
    /// even here.
    pub fn upconverts_hf(self) -> bool {
        self == Tuner::R828D
    }
}

/// What the server says about itself when the connection opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Greeting {
    pub tuner: Tuner,
    /// How many gain steps the far end's tuner has. Only the count is sent —
    /// never the values — so this is good for a log line and for telling a
    /// tuner with gain control from one without, and nothing else.
    pub gain_count: u32,
}

impl Greeting {
    /// Parse the twelve-byte greeting.
    pub fn parse(buf: &[u8]) -> Result<Greeting> {
        if buf.len() < GREETING_LEN {
            return Err(Error::Net(format!(
                "the server sent a {}-byte greeting; an rtl_tcp server sends {GREETING_LEN}",
                buf.len()
            )));
        }
        if &buf[..4] != MAGIC {
            let seen = String::from_utf8_lossy(&buf[..4]).to_string();
            let hint = if seen == "RSP0" {
                " — that is an rsp_tcp server (SDRplay); use the SDRplay interface \
                 against the RSP itself"
            } else {
                " — check that this is an rtl_tcp server and not another service \
                 on the same port"
            };
            return Err(Error::Net(format!(
                "the server greeted with {seen:?}, not \"RTL0\"{hint}"
            )));
        }
        Ok(Greeting {
            tuner: Tuner::from_code(u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]])),
            gain_count: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
        })
    }

    /// One-line description for logs and the device label.
    pub fn describe(&self) -> String {
        match self.gain_count {
            0 => self.tuner.name().to_string(),
            n => format!("{}, {n} gain steps", self.tuner.name()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_opcode_then_big_endian_argument() {
        // 100.1 MHz, as `rtl_tcp` receives it.
        assert_eq!(frame(Cmd::Freq, 100_100_000), [0x01, 0x05, 0xF7, 0x67, 0xA0]);
        assert_eq!(frame(Cmd::SampleRate, 1_024_000), [0x02, 0x00, 0x0F, 0xA0, 0x00]);
        assert_eq!(frame(Cmd::GainMode, 1), [0x03, 0, 0, 0, 1]);
        assert_eq!(frame(Cmd::DirectSampling, 2), [0x09, 0, 0, 0, 2]);
        assert_eq!(frame(Cmd::BiasTee, 0), [0x0e, 0, 0, 0, 0]);
    }

    /// Gain is tenths of a dB and ppm can be negative; both go out as the
    /// two's-complement pattern the server casts back to `int`.
    #[test]
    fn negative_arguments_survive_the_round_trip() {
        let f = frame(Cmd::Ppm, (-42i32) as u32);
        assert_eq!(f[0], 0x05);
        let arg = u32::from_be_bytes([f[1], f[2], f[3], f[4]]) as i32;
        assert_eq!(arg, -42);

        // 30.0 dB as the wire carries it.
        let f = frame(Cmd::Gain, 300);
        assert_eq!(u32::from_be_bytes([f[1], f[2], f[3], f[4]]), 300);
    }

    #[test]
    fn greeting_reports_the_far_end_tuner() {
        let mut buf = Vec::from(*MAGIC);
        buf.extend_from_slice(&5u32.to_be_bytes());
        buf.extend_from_slice(&29u32.to_be_bytes());
        let g = Greeting::parse(&buf).expect("parses");
        assert_eq!(g.tuner, Tuner::R820T);
        assert_eq!(g.gain_count, 29);
        assert!(g.describe().contains("R820T"));
        assert!(!g.tuner.upconverts_hf(), "a V3 needs direct sampling for HF");
    }

    #[test]
    fn an_r828d_is_taken_to_upconvert() {
        let mut buf = Vec::from(*MAGIC);
        buf.extend_from_slice(&6u32.to_be_bytes());
        buf.extend_from_slice(&29u32.to_be_bytes());
        let g = Greeting::parse(&buf).expect("parses");
        assert_eq!(g.tuner, Tuner::R828D);
        assert!(g.tuner.upconverts_hf());
    }

    /// The two failure modes worth telling apart: something that is not an
    /// rtl_tcp server, and a server that hung up mid-greeting.
    #[test]
    fn a_foreign_greeting_names_what_arrived() {
        let mut buf = Vec::from(*b"RSP0");
        buf.extend_from_slice(&[0u8; 8]);
        let e = Greeting::parse(&buf).expect_err("must not be taken for rtl_tcp");
        assert!(e.to_string().contains("rsp_tcp"), "{e}");

        let e = Greeting::parse(b"RTL0").expect_err("a short greeting is not a greeting");
        assert!(e.to_string().contains("4-byte"), "{e}");
    }

    #[test]
    fn unknown_tuner_codes_do_not_panic() {
        assert_eq!(Tuner::from_code(0), Tuner::Unknown);
        assert_eq!(Tuner::from_code(99), Tuner::Unknown);
        assert_eq!(Tuner::from_code(6), Tuner::R828D);
    }
}
