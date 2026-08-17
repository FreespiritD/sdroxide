//! Fine Offset Electronics sensors on 868.3 MHz, and the many names they are
//! sold under — Ecowitt, Froggit, Ambient Weather, SwitchDoc, Misol.
//!
//! # Source
//!
//! The frame layouts are transcribed from the protocol documentation comments in
//! `rtl_433`'s `src/devices/fineoffset.c`, which describe the devices rather than
//! the decoder. Each one is reproduced here as a test vector using the sample
//! frame and the decoded values published beside it, so what is implemented is
//! checked against a capture from a real sensor and not against a reading of the
//! prose. No code was taken from there.
//!
//! # One family, several frame lengths
//!
//! These sensors share a physical layer with the LaCrosse units — 2-FSK at
//! 17.24 kbps behind an `0xaa` preamble and an `0x2dd4` sync word — and then a
//! common framing:
//!
//! ```text
//! FF <payload …> CC SS
//!  |              |  `- sum of every preceding byte, mod 256
//!  |              `---- CRC-8, poly 0x31, init 0, over every byte before it
//!  `------------------- family code, which says what the payload means
//! ```
//!
//! Two independent checks, which is what makes this family safe to decode at all:
//! a CRC-8 alone passes one noise frame in 256, and the burst gate on a busy band
//! offers plenty of those. With the sum as well it is one in 65 536, on top of the
//! preamble and sync-word constraints the slicer has already applied.
//!
//! The members differ in length, so the parser tries each layout at the length
//! that layout expects.
//!
//! # Recognised but not read
//!
//! A frame whose family code is not one of the mapped layouts still gets reported
//! when both checks pass, with its identity and its bytes but no readings. That is
//! not a guess: the CRC and the sum agreeing means this really is a Fine Offset
//! sensor of *some* model, and saying "there is a WH57 here that I cannot read
//! yet" is more use than saying nothing.

use sdroxide_types::{IsmProtocol, IsmQuantity, IsmReading};

use super::Decoded;
use crate::crc::crc8;
use crate::slice::Phy;

/// Shared with the LaCrosse sensors; only the framing after the sync word differs.
pub const PHY: Phy = Phy {
    baud: 17_241.0,
    sync: 0x2DD4,
    sync_bits: 16,
    sync_errors: 1,
    preamble_bits: 16,
    // The shortest mapped layout is fourteen bytes (WH51) and the longest is
    // seventeen (WH24/WH65).
    payload_min: 14,
    payload_max: 17,
};

pub const CHANNEL_HZ: f64 = 868_300_000.0;

const CRC_POLY: u8 = 0x31;

/// Family codes seen on this channel, and what to call them.
///
/// Reported even for the layouts that are not decoded — see the module note. The
/// list is deliberately short: a code only belongs here once there is a source
/// saying which sensor sends it.
const FAMILIES: &[(u8, &str)] =
    &[(0x24, "WH24/WH65 or WH25/WH32"), (0x51, "WH51"), (0x40, "WH40"), (0x41, "WH57")];

/// Whether `n` bytes of `p` pass both the CRC and the byte sum.
///
/// `n` is the length of the whole frame *including* the two check bytes, so the
/// CRC covers `n - 2` bytes and the sum covers `n - 1`.
fn checks_pass(p: &[u8], n: usize) -> bool {
    if p.len() < n || n < 3 {
        return false;
    }
    let data = &p[..n - 2];
    let (crc, sum) = (p[n - 2], p[n - 1]);
    if crc8(CRC_POLY, 0x00, data) != crc {
        return false;
    }
    // Mod-256 sum of everything up to and including the CRC. The source comment
    // calls this a "bitsum (XOR)", which it is not: the published sample only
    // agrees with an ordinary sum, and the test vector below is what settles it.
    let want: u8 = p[..n - 1].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    want == sum
}

pub fn parse(p: &[u8]) -> Option<Decoded> {
    let family = *p.first()?;

    // Longest first: a shorter layout's checks could in principle pass on the
    // leading bytes of a longer frame, and the longer reading is the right one.
    if checks_pass(p, 17) && family == 0x24 {
        return wh24(p);
    }
    if checks_pass(p, 14) && family == 0x51 {
        return wh51(p);
    }

    // Neither mapped layout, but if some length checks out this is still a real
    // sensor of this family.
    for n in [17usize, 16, 15, 14] {
        if checks_pass(p, n) {
            let name = FAMILIES.iter().find(|(f, _)| *f == family).map(|(_, n)| *n);
            return Some(Decoded {
                protocol: IsmProtocol::FineOffset,
                model: Some(match name {
                    Some(n) => n.to_string(),
                    None => format!("family {family:#04x}"),
                }),
                // Every layout in this family puts the sensor id straight after
                // the family code; how many bytes of it there are is what varies,
                // so the widest safe reading is the one byte they all have.
                device: format!("{:02x}", p[1]),
                readings: Vec::new(),
                extra: vec![("payload".to_string(), "not decoded".to_string())],
                encrypted: false,
                raw_hex: String::new(),
            });
        }
    }
    None
}

/// WH51 / WN31 soil moisture, and the SwitchDoc SM23.
///
/// ```text
/// FF II II II TB YY MM ZA AA XX XX XX CC SS
/// ```
///
/// `T` is a transmission-period boost flag, `B` the battery voltage in tenths,
/// `MM` the moisture percentage, and `AAA` a nine-bit raw ADC reading. `YY` and
/// the three `XX` bytes are fixed padding.
fn wh51(p: &[u8]) -> Option<Decoded> {
    let id = (u32::from(p[1]) << 16) | (u32::from(p[2]) << 8) | u32::from(p[3]);
    let battery_v = f64::from(p[4] & 0x1F) / 10.0;
    let moisture = p[6];
    if moisture > 100 {
        return None;
    }

    let mut readings = vec![IsmReading::new(IsmQuantity::SoilMoisturePct, f64::from(moisture))];
    // A plausible cell voltage only. Zero means the field was not populated, and
    // anything above 2 V is not a single AA cell, so neither is worth reporting as
    // a measurement.
    if (0.1..=2.0).contains(&battery_v) {
        readings.push(IsmReading::new(IsmQuantity::BatteryVolts, battery_v));
    }

    Some(Decoded {
        protocol: IsmProtocol::FineOffset,
        model: Some("WH51 soil".to_string()),
        device: format!("{id:06x}"),
        readings,
        extra: Vec::new(),
        encrypted: false,
        raw_hex: String::new(),
    })
}

/// WH24 / WH65 / WS69 / HP1000 outdoor arrays.
///
/// ```text
/// FF II DD VT TT HH WW GG RR RR UU UU LL LL LL CC BB
/// ```
///
/// # Why there is no wind speed here
///
/// The layout is confirmed by the published sample for the identity,
/// temperature, humidity, wind direction and rainfall — every one of those
/// reproduces the value stated beside the capture, and the test below asserts it.
/// The wind *speed* fields do not: the sample has `WW = 8` and `GG = 2` while the
/// readings quoted with it are 1.12 m/s average and 2.24 m/s gust, which no single
/// scale factor produces, and a gust below the average is not a reading a
/// weather station can take. Something in that example is wrong, and with no
/// capture of our own to settle it the honest thing is to leave the two fields
/// out rather than publish a number derived from an inconsistency.
fn wh24(p: &[u8]) -> Option<Decoded> {
    let id = p[1];
    let flags = p[3] >> 4;
    // Nine bits: the byte, plus one bit carried in the flags nibble.
    let wind_dir = u32::from(p[2]) | (u32::from(flags & 0x08) << 5);

    // Twelve bits spanning the low nibble of byte 3 and byte 4, of which the top
    // one is a low-battery flag.
    let raw = (u32::from(p[3] & 0x0F) << 8) | u32::from(p[4]);
    let low_battery = raw & 0x800 != 0;
    let temp_c = f64::from(raw & 0x7FF) / 10.0 - 40.0;
    if !(-40.0..=80.0).contains(&temp_c) {
        return None;
    }

    let humidity = p[5];
    let rain_counter = (u32::from(p[8]) << 8) | u32::from(p[9]);

    let mut readings = vec![IsmReading::new(IsmQuantity::TempC, temp_c)];
    if humidity <= 100 {
        readings.push(IsmReading::new(IsmQuantity::HumidityPct, f64::from(humidity)));
    }
    if wind_dir <= 360 {
        readings.push(IsmReading::new(IsmQuantity::WindDirDeg, f64::from(wind_dir)));
    }
    // 0.3 mm per tick, which the published sample confirms: 74 ticks is the
    // 22.2 mm quoted with it.
    readings.push(IsmReading::new(IsmQuantity::RainMm, f64::from(rain_counter) * 0.3));

    let mut extra = Vec::new();
    if low_battery {
        extra.push(("battery".to_string(), "low".to_string()));
    }

    Some(Decoded {
        protocol: IsmProtocol::FineOffset,
        model: Some("WH24/WH65".to_string()),
        device: format!("{id:02x}"),
        readings,
        extra,
        encrypted: false,
        raw_hex: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The WH51 sample published with the layout, and the fields it states.
    ///
    /// Both checks passing on this byte alignment is strong evidence on its own:
    /// a CRC and an independent sum agreeing over thirteen bytes does not happen
    /// if the frame has been split up wrongly.
    #[test]
    fn the_published_wh51_capture_decodes() {
        // aa aa aa 2d d4 | 51 00 6b 58 6e 7f 24 f8 d2 ff ff ff 3c 28
        let f =
            [0x51, 0x00, 0x6B, 0x58, 0x6E, 0x7F, 0x24, 0xF8, 0xD2, 0xFF, 0xFF, 0xFF, 0x3C, 0x28];
        assert!(checks_pass(&f, 14), "the CRC and sum of the published sample must agree");

        let d = parse(&f).expect("the sample did not decode");
        assert_eq!(d.protocol, IsmProtocol::FineOffset);
        assert_eq!(d.model.as_deref(), Some("WH51 soil"));
        assert_eq!(d.device, "006b58");
        // Moisture is the documented 0x24 = 36 %.
        assert_eq!(d.readings[0].quantity, IsmQuantity::SoilMoisturePct);
        assert!((d.readings[0].value - 36.0).abs() < 1e-9);
        // Battery is the low five bits of 0x6e in tenths of a volt.
        assert_eq!(d.readings[1].quantity, IsmQuantity::BatteryVolts);
        assert!((d.readings[1].value - 1.4).abs() < 1e-9, "got {}", d.readings[1].value);
    }

    /// The WH24 sample, and the readings published beside it: id 191, 11.8 °C,
    /// 78 %RH, wind direction 266°, rainfall 22.2 mm.
    #[test]
    fn the_published_wh24_capture_decodes_to_its_stated_readings() {
        let f = [
            0x24, 0xBF, 0x0A, 0xE2, 0x06, 0x4E, 0x08, 0x02, 0x00, 0x4A, 0x00, 0x01, 0x00, 0x00,
            0x00, 0x8F, 0x07,
        ];
        assert!(checks_pass(&f, 17), "the CRC and sum of the published sample must agree");

        let d = parse(&f).expect("the sample did not decode");
        assert_eq!(d.model.as_deref(), Some("WH24/WH65"));
        assert_eq!(d.device, "bf", "id 191 is 0xbf");

        let by = |q: IsmQuantity| d.readings.iter().find(|r| r.quantity == q).map(|r| r.value);
        assert!((by(IsmQuantity::TempC).unwrap() - 11.8).abs() < 1e-9);
        assert!((by(IsmQuantity::HumidityPct).unwrap() - 78.0).abs() < 1e-9);
        assert!((by(IsmQuantity::WindDirDeg).unwrap() - 266.0).abs() < 1e-9);
        assert!((by(IsmQuantity::RainMm).unwrap() - 22.2).abs() < 1e-6);
        // The two fields the published example contradicts itself about.
        assert!(by(IsmQuantity::WindAvgMs).is_none(), "wind speed must not be guessed");
        assert!(by(IsmQuantity::WindGustMs).is_none(), "gust must not be guessed");
    }

    /// Both checks have to reject, independently. A decoder that only really
    /// applied one of them would report noise as soil moisture once every 256
    /// bursts, and on this band that is often.
    #[test]
    fn each_check_rejects_on_its_own() {
        let good =
            [0x51, 0x00, 0x6B, 0x58, 0x6E, 0x7F, 0x24, 0xF8, 0xD2, 0xFF, 0xFF, 0xFF, 0x3C, 0x28];
        assert!(parse(&good).is_some());

        // Break the CRC, leaving the sum consistent with the broken CRC byte.
        let mut bad = good;
        bad[12] ^= 0x01;
        bad[13] = bad[..13].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        assert!(parse(&bad).is_none(), "a wrong CRC was accepted");

        // Break only the sum.
        let mut bad = good;
        bad[13] ^= 0x01;
        assert!(parse(&bad).is_none(), "a wrong sum was accepted");

        // A payload value the sensor cannot produce.
        let mut bad = good;
        bad[6] = 200; // 200 % moisture
        bad[12] = crc8(CRC_POLY, 0x00, &bad[..12]);
        bad[13] = bad[..13].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        assert!(parse(&bad).is_none(), "200 % soil moisture was accepted");

        for n in 0..14 {
            assert!(parse(&good[..n]).is_none(), "{n} bytes should not parse");
        }
    }

    /// An unmapped family whose checks pass is a real sensor and has to be
    /// reported — with no readings, because none are known.
    #[test]
    fn an_unmapped_family_is_reported_without_readings() {
        // Build a well-formed 14-byte frame with a family code nothing maps.
        let mut f = [0u8; 14];
        f[0] = 0x41; // WH57, named but not decoded
        f[1] = 0xAB;
        for (i, b) in f.iter_mut().enumerate().take(12).skip(2) {
            *b = (i as u8) * 7;
        }
        f[12] = crc8(CRC_POLY, 0x00, &f[..12]);
        f[13] = f[..13].iter().fold(0u8, |a, &b| a.wrapping_add(b));

        let d = parse(&f).expect("a valid frame of an unknown layout must still be reported");
        assert_eq!(d.model.as_deref(), Some("WH57"));
        assert_eq!(d.device, "ab");
        assert!(d.readings.is_empty());
        assert_eq!(d.extra, vec![("payload".to_string(), "not decoded".to_string())]);

        // And a family code with no name at all still reports the code itself,
        // rather than pretending to know the model.
        f[0] = 0x9E;
        f[12] = crc8(CRC_POLY, 0x00, &f[..12]);
        f[13] = f[..13].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        let d = parse(&f).expect("should still be reported");
        assert_eq!(d.model.as_deref(), Some("family 0x9e"));
    }

    /// Noise must not pass both checks. Two independent eight-bit checks put the
    /// odds at one in 65 536, so a few hundred thousand random frames is enough
    /// to notice a decoder that is only applying one of them.
    #[test]
    fn random_frames_essentially_never_pass_both_checks() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut accepted = 0;
        for _ in 0..200_000 {
            let mut f = [0u8; 17];
            for b in f.iter_mut() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                *b = (seed >> 33) as u8;
            }
            if parse(&f).is_some() {
                accepted += 1;
            }
        }
        // Four lengths are tried, so the expectation is about 12 in 200 000.
        assert!(accepted < 40, "{accepted} random frames were accepted as sensors");
    }
}
