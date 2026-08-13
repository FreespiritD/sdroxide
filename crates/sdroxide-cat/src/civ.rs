//! Icom CI-V framing — also used by the Xiegu X6100, which speaks a CI-V
//! dialect. A frame is `FE FE <to> <from> <cmd> [data…] FD`.

use sdroxide_types::Mode;

pub const PREAMBLE: u8 = 0xFE;
pub const END: u8 = 0xFD;
/// The rig's "NG" answer: it will not do what the last command asked. Carries
/// no indication of *which* command, and models answer it for every
/// sub-command they don't implement.
pub const NG: u8 = 0xFA;
/// Controller (this software) address — conventional default.
pub const CONTROLLER_ADDR: u8 = 0xE0;

/// Encode a frequency (Hz) as 5 little-endian BCD bytes (CI-V cmd 0x05/0x03).
pub fn encode_freq(hz: f64) -> [u8; 5] {
    let mut v = hz.round().max(0.0) as u64;
    let mut out = [0u8; 5];
    for b in out.iter_mut() {
        let lo = (v % 10) as u8;
        v /= 10;
        let hi = (v % 10) as u8;
        v /= 10;
        *b = (hi << 4) | lo;
    }
    out
}

/// Decode 5 little-endian BCD bytes back to a frequency in Hz.
pub fn decode_freq(bytes: &[u8]) -> Option<f64> {
    if bytes.len() < 5 {
        return None;
    }
    let mut hz: u64 = 0;
    for &b in bytes[..5].iter().rev() {
        let hi = (b >> 4) as u64;
        let lo = (b & 0x0f) as u64;
        if hi > 9 || lo > 9 {
            return None;
        }
        hz = hz * 100 + hi * 10 + lo;
    }
    Some(hz as f64)
}

/// The app's `Mode` → CI-V mode byte. Digital modes ride on their sideband.
pub fn mode_to_civ(m: Mode) -> u8 {
    match m {
        Mode::Lsb | Mode::Digl => 0x00,
        Mode::Usb
        | Mode::Digu
        | Mode::Ft8
        | Mode::Js8
        | Mode::Wspr
        | Mode::Ft4
        | Mode::Ft2
        | Mode::Psk
        | Mode::Rtty
        | Mode::Sstv
        | Mode::Wefax
        | Mode::Olivia
        | Mode::Thor
        | Mode::Fsq
        | Mode::Hell
        | Mode::RfPaint
        | Mode::Rade => 0x01,
        Mode::Am | Mode::Sam | Mode::Dsb => 0x02,
        Mode::Cw => 0x03,
        // RIFP is FSK on the carrier, so a CAT rig has to be in FM for the
        // dial to mean what RIFP means by it.
        Mode::Nfm | Mode::Wfm | Mode::Rifp => 0x05,
        Mode::Spec => 0x01,
    }
}

/// CI-V mode byte → the app's `Mode`.
pub fn civ_to_mode(b: u8) -> Option<Mode> {
    Some(match b {
        0x00 => Mode::Lsb,
        0x01 => Mode::Usb,
        0x02 => Mode::Am,
        0x03 | 0x07 => Mode::Cw,
        0x05 | 0x06 => Mode::Nfm,
        _ => return None,
    })
}

/// Build a CI-V frame addressed to `radio`.
pub fn frame(radio: u8, cmd: u8, data: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(6 + data.len());
    f.extend_from_slice(&[PREAMBLE, PREAMBLE, radio, CONTROLLER_ADDR, cmd]);
    f.extend_from_slice(data);
    f.push(END);
    f
}

pub fn set_freq_frame(radio: u8, hz: f64) -> Vec<u8> {
    frame(radio, 0x05, &encode_freq(hz))
}
pub fn read_freq_frame(radio: u8) -> Vec<u8> {
    frame(radio, 0x03, &[])
}
pub fn set_mode_frame(radio: u8, m: Mode) -> Vec<u8> {
    // mode byte + filter 1; the X6100 accepts the two-byte form.
    frame(radio, 0x06, &[mode_to_civ(m), 0x01])
}
pub fn read_mode_frame(radio: u8) -> Vec<u8> {
    frame(radio, 0x04, &[])
}
pub fn ptt_frame(radio: u8, on: bool) -> Vec<u8> {
    frame(radio, 0x1C, &[0x00, on as u8])
}
/// Read the SWR meter (Icom cmd `0x15` sub `0x12`). Only meaningful while
/// transmitting; the rig answers with a 0..255 reading (see [`swr_from_reading`]).
pub fn read_swr_frame(radio: u8) -> Vec<u8> {
    frame(radio, 0x15, &[0x12])
}
/// Read the S-meter (Icom cmd `0x15` sub `0x02`). The rig answers with a 0..255
/// reading on its own calibrated scale (see [`dbm_from_smeter`]).
///
/// A CAT rig hands us audio it has already demodulated and levelled, so nothing
/// on this side of the sound card can measure a signal strength — the rig's own
/// meter is the only S-meter there is. Only meaningful while receiving.
pub fn read_smeter_frame(radio: u8) -> Vec<u8> {
    frame(radio, 0x15, &[0x02])
}

/// Longest CW message one "send CW" frame carries. The rig buffers what it is
/// given and keys it out at its own speed; more than this in a frame is
/// refused, so the sender chunks to it.
pub const CW_MAX: usize = 30;

/// Reduce `text` to the characters an Icom keyer will send: upper case, the
/// letters, digits and punctuation it has Morse for. A character it does not
/// know would be refused along with the rest of the frame, so it is dropped
/// here instead.
fn cw_text(text: &str) -> String {
    text.trim()
        .chars()
        // A line break is a word break, not nothing: dropping it would run the
        // end of one line into the start of the next and send them as one word.
        .map(|c| if c.is_ascii_whitespace() { ' ' } else { c.to_ascii_uppercase() })
        .filter(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ' ' | '/' | '?' | '.' | ',' | '-' | '=')
        })
        .scan(false, |prev_space, c| {
            let space = c == ' ';
            let keep = !(space && *prev_space);
            *prev_space = space;
            Some(keep.then_some(c))
        })
        .flatten()
        .take(CW_MAX)
        .collect()
}

/// Send `text` as CW from the rig's own keyer (cmd `0x17`). `None` when nothing
/// in `text` is sendable — an empty frame would be a rejected frame.
///
/// The rig keys itself for the length of the message (its break-in decides how
/// it switches), so this must not be wrapped in PTT.
pub fn send_cw_frame(radio: u8, text: &str) -> Option<Vec<u8>> {
    let msg = cw_text(text);
    (!msg.is_empty()).then(|| frame(radio, 0x17, msg.as_bytes()))
}

/// Stop a message part way through: `0xFF` is the abort payload of the same
/// send-CW command.
pub fn stop_cw_frame(radio: u8) -> Vec<u8> {
    frame(radio, 0x17, &[0xFF])
}

/// Set the keyer speed (cmd `0x14` sub `0x0C`). Icom carries it as its generic
/// 0–255 level over a 6–48 WPM range, so the panel's speed is mapped onto that
/// scale rather than sent as a number of words.
pub fn keyer_speed_frame(radio: u8, wpm: f32) -> Vec<u8> {
    let wpm = wpm.clamp(6.0, 48.0);
    let level = (((wpm - 6.0) * (255.0 / 42.0)).round() as u32).min(255);
    let (hi, lo) = ((level / 100) as u8, (level % 100) as u8);
    let bcd = |v: u8| ((v / 10) << 4) | (v % 10);
    frame(radio, 0x14, &[0x0C, bcd(hi), bcd(lo)])
}

/// Hand the rig's *own* RIT, ΔTX (XIT) and split back to neutral.
///
/// sdroxide carries all three on the dial itself (see `AudioCatSource`), so an
/// offset the rig is still holding — from a previous session, or from the
/// operator's own RIT knob — would stack on top of ours where nothing in the
/// software could see it. Sent once when the port opens; a rig that doesn't
/// implement these sub-commands just NAKs them, which the parser ignores.
pub fn clear_offsets_frames(radio: u8) -> Vec<Vec<u8>> {
    vec![
        // RIT and ΔTX share one offset register (cmd 0x21 sub 0x00): the offset
        // as two little-endian BCD bytes, then a sign byte.
        frame(radio, 0x21, &[0x00, 0x00, 0x00, 0x00]),
        frame(radio, 0x21, &[0x01, 0x00]), // RIT off
        frame(radio, 0x21, &[0x02, 0x00]), // ΔTX (XIT) off
        frame(radio, 0x0F, &[0x00]),       // simplex (split off)
    ]
}

/// Decode Icom's 2-byte BCD meter reading (`0000..0255`) to a plain integer.
/// `data` is the payload after the meter sub-command byte.
fn decode_meter(data: &[u8]) -> Option<u32> {
    let bcd = |b: u8| -> Option<u32> {
        let (hi, lo) = ((b >> 4) as u32, (b & 0x0f) as u32);
        (hi <= 9 && lo <= 9).then_some(hi * 10 + lo)
    };
    let (a, b) = (data.first()?, data.get(1)?);
    Some(bcd(*a)? * 100 + bcd(*b)?)
}

/// Map an Icom SWR-meter reading (`0..255`) to an SWR ratio via piecewise-linear
/// interpolation over the standard calibration breakpoints (matching Hamlib's
/// Icom SWR curve: 0→1.0, 48→1.5, 80→2.0, 120→3.0), extended past 3:1 for the
/// rare high-SWR case. Clamped to the table ends.
fn swr_from_reading(reading: u32) -> f32 {
    const CAL: &[(f32, f32)] =
        &[(0.0, 1.0), (48.0, 1.5), (80.0, 2.0), (120.0, 3.0), (160.0, 5.0), (255.0, 10.0)];
    let r = reading as f32;
    if r <= CAL[0].0 {
        return CAL[0].1;
    }
    for w in CAL.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        if r <= x1 {
            return y0 + (y1 - y0) * (r - x0) / (x1 - x0);
        }
    }
    CAL[CAL.len() - 1].1
}

/// Parse an SWR-meter reply payload (Icom cmd `0x15`): the sub-command byte
/// followed by the BCD reading. Returns the SWR ratio, or `None` if the reply
/// isn't the SWR meter (`0x12`) or is malformed.
pub fn parse_swr_reply(data: &[u8]) -> Option<f32> {
    if data.first() != Some(&0x12) {
        return None;
    }
    Some(swr_from_reading(decode_meter(&data[1..])?))
}

/// Map an Icom S-meter reading (`0..255`) to dBm, over the calibration Icom
/// states and Hamlib carries for this family: reading 0 is S0, 120 is S9, and
/// 241 is S9+60 dB. S9 is −73 dBm and an S-unit is 6 dB, so the three points are
/// −127, −73 and −13 dBm; between and beyond them the scale is interpolated and
/// clamped, as the SWR curve is.
///
/// This is the rig's *own* meter, in the units its manufacturer calibrated it
/// in — not a level derived from the sound card — so it needs no dBFS→dBm
/// offset applying on top.
fn dbm_from_smeter(reading: u32) -> f32 {
    const CAL: &[(f32, f32)] = &[(0.0, -127.0), (120.0, -73.0), (241.0, -13.0)];
    let r = reading as f32;
    if r <= CAL[0].0 {
        return CAL[0].1;
    }
    for w in CAL.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        if r <= x1 {
            return y0 + (y1 - y0) * (r - x0) / (x1 - x0);
        }
    }
    CAL[CAL.len() - 1].1
}

/// Parse an S-meter reply payload (Icom cmd `0x15`): the sub-command byte
/// followed by the BCD reading. Returns the signal level in dBm, or `None` if
/// the reply isn't the S-meter (`0x02`) or is malformed.
pub fn parse_smeter_reply(data: &[u8]) -> Option<f32> {
    if data.first() != Some(&0x02) {
        return None;
    }
    Some(dbm_from_smeter(decode_meter(&data[1..])?))
}

/// A parsed reply from the rig (payload after `<cmd>`, addresses stripped).
#[derive(Debug, Clone, PartialEq)]
pub struct CivReply {
    pub from: u8,
    pub to: u8,
    pub cmd: u8,
    pub data: Vec<u8>,
}

/// Pull complete CI-V frames out of a rolling byte buffer, consuming them.
/// Tolerates junk between frames (CI-V is a shared bus with echoes).
pub fn parse_frames(buf: &mut Vec<u8>) -> Vec<CivReply> {
    let mut out = Vec::new();
    loop {
        // Find a preamble pair.
        let Some(start) = buf.windows(2).position(|w| w == [PREAMBLE, PREAMBLE]) else {
            // No frame start; keep at most the last byte (could be a lone FE).
            if buf.len() > 1 {
                let keep = buf.split_off(buf.len() - 1);
                buf.clear();
                buf.extend_from_slice(&keep);
            }
            break;
        };
        // Find the terminator after the preamble.
        let Some(rel_end) = buf[start + 2..].iter().position(|&b| b == END) else {
            // Incomplete frame — drop everything before `start` and wait.
            if start > 0 {
                buf.drain(..start);
            }
            break;
        };
        let end = start + 2 + rel_end;
        // Frame body is buf[start+2 ..= end]; need at least to,from,cmd.
        if end >= start + 5 {
            let to = buf[start + 2];
            let from = buf[start + 3];
            let cmd = buf[start + 4];
            let data = buf[start + 5..end].to_vec();
            out.push(CivReply { from, to, cmd, data });
        }
        buf.drain(..=end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freq_roundtrips() {
        for hz in [14_074_000.0, 7_055_000.0, 28_500_000.0, 1_800_000.0, 145_500_000.0] {
            let b = encode_freq(hz);
            assert_eq!(decode_freq(&b), Some(hz), "freq {hz}");
        }
    }

    #[test]
    fn known_freq_bytes() {
        // 14.074000 MHz → little-endian BCD.
        assert_eq!(encode_freq(14_074_000.0), [0x00, 0x40, 0x07, 0x14, 0x00]);
    }

    #[test]
    fn set_freq_frame_shape() {
        let f = set_freq_frame(0x70, 14_074_000.0);
        assert_eq!(f, vec![0xFE, 0xFE, 0x70, 0xE0, 0x05, 0x00, 0x40, 0x07, 0x14, 0x00, 0xFD]);
    }

    #[test]
    fn parses_freq_reply_amid_echo() {
        // An echo of our own read request, then the rig's freq answer.
        let mut buf = Vec::new();
        buf.extend_from_slice(&read_freq_frame(0x70)); // echo (to=70,from=E0)
        buf.extend_from_slice(&frame(0x70, 0x03, &encode_freq(7_055_000.0))); // "reply"
        let frames = parse_frames(&mut buf);
        // Both parse; the freq one is cmd 0x03 with 5 data bytes.
        let freqs: Vec<f64> = frames
            .iter()
            .filter(|f| f.cmd == 0x03 && f.data.len() >= 5)
            .filter_map(|f| decode_freq(&f.data))
            .collect();
        assert_eq!(freqs, vec![7_055_000.0]);
        assert!(buf.is_empty());
    }

    #[test]
    fn swr_meter_decodes_and_scales() {
        // Reply payload = sub-command 0x12 followed by the 2-byte BCD reading.
        // Calibration breakpoints map exactly: 0→1.0, 48→1.5, 80→2.0, 120→3.0.
        let swr = |reading_bcd: [u8; 2]| parse_swr_reply(&[0x12, reading_bcd[0], reading_bcd[1]]);
        assert_eq!(swr([0x00, 0x00]), Some(1.0)); // reading 0
        assert_eq!(swr([0x00, 0x48]), Some(1.5)); // reading 48
        assert_eq!(swr([0x00, 0x80]), Some(2.0)); // reading 80
        assert_eq!(swr([0x01, 0x20]), Some(3.0)); // reading 120
        // Midpoint of the 0..48 segment interpolates linearly to 1.25.
        assert_eq!(swr([0x00, 0x24]), Some(1.25)); // reading 24
        // A malformed reading (bad BCD nibble) yields None, not a bogus SWR.
        assert_eq!(swr([0x00, 0x0f]), None);
        // The wrong meter sub-command is ignored (we only read SWR / 0x12).
        assert_eq!(parse_swr_reply(&[0x11, 0x00, 0x50]), None);
        // The S-meter shares command 0x15 and must not be read as an SWR.
        assert_eq!(parse_swr_reply(&[0x02, 0x01, 0x20]), None);
    }

    #[test]
    fn s_meter_decodes_to_dbm() {
        let dbm =
            |reading_bcd: [u8; 2]| parse_smeter_reply(&[0x02, reading_bcd[0], reading_bcd[1]]);
        // The calibration points: S0, S9, S9+60.
        assert_eq!(dbm([0x00, 0x00]), Some(-127.0));
        assert_eq!(dbm([0x01, 0x20]), Some(-73.0));
        assert_eq!(dbm([0x02, 0x41]), Some(-13.0));
        // S9 is 120 and an S-unit is 6 dB, so half of S9's reading is S4.5 —
        // 27 dB below S9 on the interpolated scale.
        assert_eq!(dbm([0x00, 0x60]), Some(-100.0));
        // Past the top of the table the reading clamps rather than running on.
        assert_eq!(dbm([0x02, 0x55]), Some(-13.0));
        // A malformed reading is no reading, and the SWR sub-meter is not ours.
        assert_eq!(dbm([0x00, 0x0f]), None);
        assert_eq!(parse_smeter_reply(&[0x12, 0x00, 0x48]), None);
    }

    #[test]
    fn the_two_meter_reads_are_distinct_frames() {
        assert_eq!(read_smeter_frame(0x94), vec![0xFE, 0xFE, 0x94, 0xE0, 0x15, 0x02, 0xFD]);
        assert_eq!(read_swr_frame(0x94), vec![0xFE, 0xFE, 0x94, 0xE0, 0x15, 0x12, 0xFD]);
    }

    #[test]
    fn clearing_offsets_neutralises_rit_xit_and_split() {
        let f = clear_offsets_frames(0x70);
        let body = |i: usize| f[i][4..f[i].len() - 1].to_vec();
        assert_eq!(f.len(), 4);
        // Every frame is addressed to the rig from the controller.
        for frame in &f {
            assert_eq!(frame[..4], [0xFE, 0xFE, 0x70, 0xE0]);
            assert_eq!(*frame.last().unwrap(), 0xFD);
        }
        assert_eq!(body(0), vec![0x21, 0x00, 0x00, 0x00, 0x00], "RIT/ΔTX offset → 0 Hz, +");
        assert_eq!(body(1), vec![0x21, 0x01, 0x00], "RIT off");
        assert_eq!(body(2), vec![0x21, 0x02, 0x00], "ΔTX (XIT) off");
        assert_eq!(body(3), vec![0x0F, 0x00], "simplex");
    }

    #[test]
    fn cw_is_sent_as_text_the_rig_can_key() {
        // Command 0x17 with the message as plain upper-case ASCII.
        let f = send_cw_frame(0x94, "cq de w1aw").unwrap();
        assert_eq!(f[..5], [0xFE, 0xFE, 0x94, 0xE0, 0x17]);
        assert_eq!(&f[5..f.len() - 1], b"CQ DE W1AW");
        assert_eq!(*f.last().unwrap(), 0xFD);
        // Nothing sendable is no frame at all, not an empty one.
        assert!(send_cw_frame(0x94, "  <>  ").is_none());
        // A line break is a word break — dropping it would send one word.
        let wrapped = send_cw_frame(0x94, "tnx fer call\nur 599").unwrap();
        assert_eq!(&wrapped[5..wrapped.len() - 1], b"TNX FER CALL UR 599");
        // A message longer than one frame carries is cut to fit.
        let long = send_cw_frame(0x94, &"a".repeat(50)).unwrap();
        assert_eq!(long.len() - 6, CW_MAX);
        // Abort is the same command with the escape payload.
        assert_eq!(stop_cw_frame(0x94), vec![0xFE, 0xFE, 0x94, 0xE0, 0x17, 0xFF, 0xFD]);
    }

    #[test]
    fn keyer_speed_maps_wpm_onto_the_rigs_level_scale() {
        let level = |wpm: f32| {
            let f = keyer_speed_frame(0x94, wpm);
            assert_eq!(f[4..6], [0x14, 0x0C]);
            decode_meter(&f[6..f.len() - 1]).unwrap()
        };
        // The ends of the rig's range, and a speed in the middle of it. The
        // reference table Hamlib carries has 20 WPM at 84.
        assert_eq!(level(6.0), 0);
        assert_eq!(level(48.0), 255);
        assert_eq!(level(20.0), 85);
        // Speeds the panel offers that the rig's keyer does not reach clamp
        // rather than wrapping round its 0..255 scale.
        assert_eq!(level(4.0), 0);
        assert_eq!(level(60.0), 255);
    }

    #[test]
    fn handles_partial_frame() {
        let full = set_freq_frame(0x70, 14_074_000.0);
        let (head, tail) = full.split_at(6);
        let mut buf = head.to_vec();
        assert!(parse_frames(&mut buf).is_empty()); // incomplete, buffered
        buf.extend_from_slice(tail);
        assert_eq!(parse_frames(&mut buf).len(), 1);
    }
}
