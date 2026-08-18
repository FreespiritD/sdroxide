//! The LimeRFE wire format, with no serial port in it.
//!
//! Every exchange is a fixed 16-byte buffer out and a fixed 16-byte buffer
//! back. `buf[0]` is the command and the board echoes it; `buf[1]` in the reply
//! is the status, zero for success. Short commands are padded rather than
//! truncated — a reply of any other length means the link has lost sync, not
//! that the board had less to say.
//!
//! Keeping this module free of `serialport` is what makes the fiddly half of
//! the driver testable with nothing plugged in, the same split every native USB
//! driver here makes with its `protocol.rs`.

use sdroxide_types::{RFE_BUFFER_SIZE, RfeChannel, RfeMode, RfePort};

use crate::error::{Error, Result};

/// Command codes, from LimeSuite's `limeRFE_constants.h`.
pub mod cmd {
    pub const HELLO: u8 = 0x00;
    pub const MODE: u8 = 0xd1;
    pub const CONFIG: u8 = 0xd2;
    pub const READ_ADC1: u8 = 0xa1;
    pub const READ_ADC2: u8 = 0xa2;
    pub const READ_TEMP: u8 = 0xa3;
    pub const FAN: u8 = 0xc1;
    pub const GET_INFO: u8 = 0xe1;
    pub const RESET: u8 = 0xe2;
    pub const GET_CONFIG: u8 = 0xe3;
}

/// How many times to say hello before deciding nothing is listening, and how
/// long to leave between tries. Both are LimeSuite's numbers
/// (`RFE_MAX_HELLO_ATTEMPTS`, `RFE_TIME_BETWEEN_HELLO_MS`): the board's
/// microcontroller can be mid-boot when the port opens.
pub const MAX_HELLO_ATTEMPTS: u32 = 10;
pub const HELLO_INTERVAL_MS: u64 = 200;

/// What the board reports about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RfeInfo {
    pub firmware: u8,
    pub hardware: u8,
}

/// Everything the board is set to, in one transaction.
///
/// The Rust mirror of LimeSuite's `rfe_boardState`, with real types instead of
/// nine `char`s. Both transports consume this: the serial one encodes it into
/// a `CONFIG` frame, the board one memcpys it into the C struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RfeState {
    pub channel_rx: RfeChannel,
    pub channel_tx: RfeChannel,
    pub port_rx: RfePort,
    pub port_tx: RfePort,
    pub mode: RfeMode,
    pub notch: bool,
    /// Receive attenuation in 2 dB steps, 0–7.
    pub atten_steps: u8,
    pub swr_enable: bool,
    /// SWR pickup source: false = external coupler, true = the cellular
    /// amplifier's internal detector.
    pub swr_source_cell: bool,
}

impl Default for RfeState {
    fn default() -> Self {
        RfeState {
            channel_rx: RfeChannel::Wb1000,
            channel_tx: RfeChannel::Wb1000,
            port_rx: RfePort::J3,
            port_tx: RfePort::J4,
            mode: RfeMode::Rx,
            notch: false,
            atten_steps: 0,
            swr_enable: false,
            swr_source_cell: false,
        }
    }
}

/// A command with no arguments.
pub fn encode_bare(command: u8) -> [u8; RFE_BUFFER_SIZE] {
    let mut buf = [0u8; RFE_BUFFER_SIZE];
    buf[0] = command;
    buf
}

/// `CONFIG` — channels, ports, mode, notch and attenuation in one go.
///
/// The byte order is LimeSuite's and is not guessable: getting `selPortRX` and
/// `selPortTX` the wrong way round would route receive into the transmit
/// amplifier's filter and look, from the spectrum, like a dead antenna.
pub fn encode_config(st: &RfeState) -> [u8; RFE_BUFFER_SIZE] {
    let mut buf = [0u8; RFE_BUFFER_SIZE];
    buf[0] = cmd::CONFIG;
    buf[1] = st.channel_rx.code();
    buf[2] = st.channel_tx.code();
    buf[3] = st.port_rx.code();
    buf[4] = st.port_tx.code();
    buf[5] = st.mode.code();
    buf[6] = u8::from(st.notch);
    buf[7] = st.atten_steps.min(7);
    buf[8] = u8::from(st.swr_enable);
    buf[9] = u8::from(st.swr_source_cell);
    buf
}

/// `MODE` — the relays only. This is the one that has to happen at key-down,
/// which is why it is a command of its own rather than a whole `CONFIG`.
pub fn encode_mode(mode: RfeMode) -> [u8; RFE_BUFFER_SIZE] {
    let mut buf = [0u8; RFE_BUFFER_SIZE];
    buf[0] = cmd::MODE;
    buf[1] = mode.code();
    buf
}

pub fn encode_fan(on: bool) -> [u8; RFE_BUFFER_SIZE] {
    let mut buf = [0u8; RFE_BUFFER_SIZE];
    buf[0] = cmd::FAN;
    buf[1] = u8::from(on);
    buf
}

/// Check a reply belongs to the command that was sent and carries no error.
///
/// The echo check is load-bearing on a serial link: a reply left in the buffer
/// from a previous exchange has the right length and the wrong meaning, and
/// accepting it would report success for a command the board never saw.
pub fn check_reply(sent: u8, reply: &[u8]) -> Result<()> {
    if reply.len() != RFE_BUFFER_SIZE {
        return Err(Error::ShortReply { want: RFE_BUFFER_SIZE, got: reply.len() });
    }
    if reply[0] != sent {
        return Err(Error::Desync { sent, echoed: reply[0] });
    }
    match reply[1] {
        0 => Ok(()),
        code => Err(Error::from_board(code)),
    }
}

/// Decode a `GET_INFO` reply.
pub fn decode_info(reply: &[u8]) -> Result<RfeInfo> {
    check_reply(cmd::GET_INFO, reply)?;
    Ok(RfeInfo { firmware: reply[2], hardware: reply[3] })
}

/// Decode a `GET_CONFIG` reply — the board's own account of its state, in the
/// same byte order [`encode_config`] writes.
pub fn decode_state(reply: &[u8]) -> Result<RfeState> {
    check_reply(cmd::GET_CONFIG, reply)?;
    Ok(RfeState {
        channel_rx: RfeChannel::from_code(reply[2]),
        channel_tx: RfeChannel::from_code(reply[3]),
        port_rx: port_from_code(reply[4]),
        port_tx: port_from_code(reply[5]),
        mode: mode_from_code(reply[6]),
        notch: reply[7] != 0,
        atten_steps: reply[8].min(7),
        swr_enable: reply[9] != 0,
        swr_source_cell: reply[10] != 0,
    })
}

/// Decode an ADC reading. The board answers with a 10-bit count, high byte
/// first.
pub fn decode_adc(sent: u8, reply: &[u8]) -> Result<u16> {
    check_reply(sent, reply)?;
    Ok((u16::from(reply[2]) << 8 | u16::from(reply[3])) & 0x03ff)
}

fn port_from_code(code: u8) -> RfePort {
    match code {
        2 => RfePort::J4,
        3 => RfePort::J5,
        _ => RfePort::J3,
    }
}

fn mode_from_code(code: u8) -> RfeMode {
    match code {
        1 => RfeMode::Tx,
        2 => RfeMode::None,
        3 => RfeMode::TxRx,
        _ => RfeMode::Rx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes for a known state. Written out literally rather than
    /// computed, because a test that builds the expectation the same way the
    /// code does would agree with any byte order at all.
    #[test]
    fn a_config_frame_is_the_bytes_limesuite_sends() {
        let st = RfeState {
            channel_rx: RfeChannel::Ham0145,
            channel_tx: RfeChannel::Ham0435,
            port_rx: RfePort::J3,
            port_tx: RfePort::J4,
            mode: RfeMode::TxRx,
            notch: true,
            atten_steps: 3,
            swr_enable: true,
            swr_source_cell: false,
        };
        assert_eq!(
            encode_config(&st),
            [0xd2, 5, 7, 1, 2, 3, 1, 3, 1, 0, 0, 0, 0, 0, 0, 0],
            "command, rx ch, tx ch, rx port, tx port, mode, notch, att, swr, swr src"
        );
    }

    /// Attenuation past the board's range is clamped, not wrapped — a wrapped
    /// value would ask for a different attenuation rather than the largest one.
    #[test]
    fn attenuation_is_clamped_into_the_frame() {
        let st = RfeState { atten_steps: 200, ..RfeState::default() };
        assert_eq!(encode_config(&st)[7], 7);
    }

    #[test]
    fn mode_and_fan_frames_carry_one_argument() {
        assert_eq!(encode_mode(RfeMode::Tx)[..2], [0xd1, 1]);
        assert_eq!(encode_mode(RfeMode::Rx)[..2], [0xd1, 0]);
        assert_eq!(encode_fan(true)[..2], [0xc1, 1]);
        assert_eq!(encode_fan(false)[..2], [0xc1, 0]);
        assert_eq!(encode_bare(cmd::RESET)[..2], [0xe2, 0]);
    }

    /// A reply left over from an earlier exchange has the right length and the
    /// wrong meaning. Taking it would report success for a command the board
    /// never saw.
    #[test]
    fn a_reply_that_does_not_echo_the_command_is_rejected() {
        let mut reply = [0u8; RFE_BUFFER_SIZE];
        reply[0] = cmd::MODE;
        let err = check_reply(cmd::CONFIG, &reply).unwrap_err();
        assert!(matches!(err, Error::Desync { sent: 0xd2, echoed: 0xd1 }), "{err}");
    }

    /// A truncated read is a timeout, never a small success.
    #[test]
    fn a_short_reply_is_an_error_not_a_partial_success() {
        let reply = [cmd::CONFIG, 0, 0, 0];
        let err = check_reply(cmd::CONFIG, &reply).unwrap_err();
        assert!(matches!(err, Error::ShortReply { got: 4, .. }), "{err}");
    }

    /// Every refusal the board can give back has to arrive as a sentence, not
    /// a number — these are the ones an operator can actually act on.
    #[test]
    fn every_board_error_code_becomes_something_actionable() {
        let mut reply = [0u8; RFE_BUFFER_SIZE];
        reply[0] = cmd::CONFIG;
        for code in [1u8, 2, 3, 4, 5, 6] {
            reply[1] = code;
            let err = check_reply(cmd::CONFIG, &reply).unwrap_err();
            let text = err.to_string();
            assert!(text.len() > 20, "code {code} gave only {text:?}");
            assert!(!text.contains("unknown"), "code {code} fell through: {text}");
        }
        // And one the firmware might invent later still says something.
        reply[1] = 99;
        assert!(check_reply(cmd::CONFIG, &reply).is_err());
    }

    /// A state written and read back is the same state — the readback path
    /// uses a different byte offset from the write path (the reply carries the
    /// echo and status first), which is exactly the kind of thing that drifts.
    #[test]
    fn a_state_survives_the_board_reporting_it_back() {
        let st = RfeState {
            channel_rx: RfeChannel::Ham0030,
            channel_tx: RfeChannel::Ham0030,
            port_rx: RfePort::J5,
            port_tx: RfePort::J5,
            mode: RfeMode::Rx,
            notch: true,
            atten_steps: 7,
            swr_enable: true,
            swr_source_cell: true,
        };
        // Build the reply the board would send: echo, status, then the same
        // nine fields the config frame carries.
        let sent = encode_config(&st);
        let mut reply = [0u8; RFE_BUFFER_SIZE];
        reply[0] = cmd::GET_CONFIG;
        reply[1] = 0;
        reply[2..11].copy_from_slice(&sent[1..10]);
        assert_eq!(decode_state(&reply).unwrap(), st);
    }

    #[test]
    fn info_and_adc_replies_decode() {
        let mut reply = [0u8; RFE_BUFFER_SIZE];
        reply[0] = cmd::GET_INFO;
        reply[2] = 4;
        reply[3] = 7;
        assert_eq!(decode_info(&reply).unwrap(), RfeInfo { firmware: 4, hardware: 7 });

        let mut reply = [0u8; RFE_BUFFER_SIZE];
        reply[0] = cmd::READ_ADC1;
        reply[2] = 0x03;
        reply[3] = 0xff;
        assert_eq!(decode_adc(cmd::READ_ADC1, &reply).unwrap(), 1023);
        // The top six bits are not part of a 10-bit reading.
        reply[2] = 0xff;
        assert_eq!(decode_adc(cmd::READ_ADC1, &reply).unwrap(), 1023);
    }
}
