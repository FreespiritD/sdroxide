//! Bresser Weather Center sensors on 868.3 MHz — the 5-in-1 and 6-in-1 families.
//!
//! # Source
//!
//! Layout transcribed from the protocol documentation comment in `rtl_433`'s
//! `src/devices/bresser_5in1.c`, which describes the device and publishes
//! seventeen captured frames — several annotated with the readings the sensor was
//! showing. Five of those are reproduced as test vectors below, and the ones with
//! stated readings (37.4 m/s gust, 27.5 m/s average, −7.0 °C, battery low)
//! reproduce exactly. That comment in turn credits
//! <https://github.com/andreafabrizi/BresserWeatherCenter>. No code was taken
//! from either.
//!
//! # Why this one is the safest frame in the band to accept
//!
//! Twenty-six payload bytes, of which the **first thirteen are the bitwise
//! complement of the last thirteen**. That is 104 bits of pure redundancy, on top
//! of a checksum byte holding the population count of the twelve data bytes. A
//! frame passing both is not a coincidence in any quantity of noise this band can
//! produce — which is worth having, because unlike the Fine Offset family there is
//! no CRC here at all.
//!
//! The complement is checked *loosely* — eleven of thirteen bytes rather than all
//! thirteen. One published capture differs in a single bit, and a single bit error
//! is exactly what a 13 dB burst produces; demanding all 104 bits agree would
//! throw away most real frames while the check remains overwhelming at eleven.
//!
//! # Frame
//!
//! ```text
//! byte  0 … 12   13   14   15    16  17    18  19    20  21   22   23 24   25
//!      ~13 … ~25 ck   id   s|typ gust dir|g wind w   temp t    hum  rain    B|t
//! ```
//!
//! Wind gust is plain binary in tenths of a metre per second, nine bits with the
//! top one in the low nibble of byte 17. Wind average and temperature are **BCD**,
//! two digits in one byte plus a hundreds digit in the low nibble of the next —
//! that structure is confirmed by the wind field, whose published 27.5 m/s only
//! comes out of the low nibble, and the temperature field follows it.
//!
//! Rainfall is deliberately not decoded: it is two BCD bytes whose digit order no
//! published capture states a value for, and a rain total is exactly the sort of
//! number that is useless if the digits are the wrong way round.
//!
//! # The 6-in-1 family
//!
//! A different frame behind the same preamble, sync word and symbol rate:
//! eighteen bytes carrying an LFSR-16 digest and an add-to-`0xff` checksum. Both
//! were verified against the four published captures annotated with readings, and
//! both reproduce exactly — which matters, because the add-checksum alone is eight
//! bits and would pass one noise frame in 256.
//!
//! ```text
//! byte 0 1   2 … 5   6      7 … 11   12 13    14   15 16   17
//!      digest  id    type   wind     temp  flags  hum  uv/rain  checksum
//! ```
//!
//! These sensors alternate message types: one carries temperature, humidity and
//! rain, the next carries only wind. The wind-only variant is a *different length*
//! and neither published check validates it at this one, so it is not decoded —
//! and, being rejected by the checks rather than misread, it costs nothing. Wind
//! from the temperature message is left alone too: its nibbles are interleaved and
//! partly inverted, and no capture annotated with a wind speed also validates
//! here, so there is nothing to check an implementation against.

use sdroxide_types::{IsmProtocol, IsmQuantity, IsmReading};

use super::Decoded;
use crate::slice::Phy;

/// 2-FSK, 8.22 kbaud, `0xaa…` preamble then `0x2dd4`.
///
/// The rate is what makes this family invisible to a decoder that assumes the
/// 17.24 kbaud of the Fine Offset sensors it shares the channel and the sync word
/// with — see the note on symbol-rate candidates in [`crate::slice::frames`].
///
/// The length range spans both families: eighteen bytes for a 6-in-1, twenty-six
/// for a 5-in-1, and [`parse`] tries each at the length it expects.
pub const PHY: Phy = Phy {
    baud: 8_220.0,
    sync: 0x2DD4,
    sync_bits: 16,
    sync_errors: 1,
    preamble_bits: 16,
    payload_min: 18,
    payload_max: 26,
};

pub const CHANNEL_HZ: f64 = 868_300_000.0;

/// Complement bytes that must agree, of thirteen. See the module note.
const MIN_COMPLEMENT: usize = 11;

/// Sensor types that are a rain gauge rather than the 5-in-1 array, and so carry
/// a payload this module does not read.
const RAIN_GAUGE_TYPES: [u8; 3] = [0x9, 0xA, 0xB];

/// Two BCD digits, or `None` if either nibble is not a decimal digit.
fn bcd(b: u8) -> Option<u32> {
    let (hi, lo) = (b >> 4, b & 0x0F);
    (hi <= 9 && lo <= 9).then_some(u32::from(hi) * 10 + u32::from(lo))
}

pub fn parse(p: &[u8]) -> Option<Decoded> {
    // Longest first: a shorter frame's checks cannot accidentally pass on the head
    // of a longer one as easily as the reverse.
    if p.len() >= 26 {
        if let Some(d) = five_in_one(p) {
            return Some(d);
        }
    }
    six_in_one(p)
}

/// LFSR-16 digest, generator `0x8810` (`poly` here — `gen` is a reserved word in
/// edition 2024), as the 6-in-1 frames carry.
///
/// One pass over the data, most-significant bit first: each set bit folds the
/// running key into the sum, and the key advances by a Galois shift every bit
/// whether or not it was set. Verified against four published captures.
fn lfsr_digest16(data: &[u8], poly: u16, key: u16) -> u16 {
    let mut sum = 0u16;
    let mut k = key;
    for &b in data {
        for bit in (0..8).rev() {
            if (b >> bit) & 1 != 0 {
                sum ^= k;
            }
            k = if k & 1 != 0 { (k >> 1) ^ poly } else { k >> 1 };
        }
    }
    sum
}

/// Bresser 6-in-1: eighteen bytes, digest plus add-checksum.
fn six_in_one(p: &[u8]) -> Option<Decoded> {
    if p.len() < 18 {
        return None;
    }
    let body = &p[2..17];

    // Add to 0xff. Cheap, so it goes first.
    if body.iter().fold(0u8, |a, &b| a.wrapping_add(b)).wrapping_add(p[17]) != 0xFF {
        return None;
    }
    // And the digest, which is what makes accepting the frame safe.
    if lfsr_digest16(body, 0x8810, 0x5412) != (u16::from(p[0]) << 8 | u16::from(p[1])) {
        return None;
    }

    // Temperature is three BCD digits: two in byte 12, the tenth in the high
    // nibble of byte 13, with the sign and the battery flag in its low nibble.
    let tens = bcd(p[12])?;
    let tenth = u32::from(p[13] >> 4);
    if tenth > 9 {
        return None;
    }
    let magnitude = i64::from(tens * 10 + tenth);
    let temp_c = if p[13] & 0x08 != 0 { -magnitude } else { magnitude } as f64 / 10.0;
    if !(-40.0..=80.0).contains(&temp_c) {
        return None;
    }

    let mut readings = vec![IsmReading::new(IsmQuantity::TempC, temp_c)];
    if let Some(h) = bcd(p[14]) {
        if h <= 100 {
            readings.push(IsmReading::new(IsmQuantity::HumidityPct, f64::from(h)));
        }
    }

    let mut extra = Vec::new();
    if p[13] & 0x02 != 0 {
        extra.push(("battery".to_string(), "low".to_string()));
    }

    Some(Decoded {
        protocol: IsmProtocol::Bresser,
        model: Some("6-in-1".to_string()),
        // Four bytes of it, which is what distinguishes two of these in one garden.
        device: format!("{:02x}{:02x}{:02x}{:02x}", p[2], p[3], p[4], p[5]),
        readings,
        extra,
        encrypted: false,
        raw_hex: String::new(),
    })
}

/// Bresser 5-in-1: twenty-six bytes, complement copy plus population count.
fn five_in_one(p: &[u8]) -> Option<Decoded> {
    if p.len() < 26 {
        return None;
    }

    // The checksum: the population count of the twelve data bytes.
    let ck: u32 = p[14..26].iter().map(|b| b.count_ones()).sum();
    if ck != u32::from(p[13]) {
        return None;
    }

    // And the complement copy, loosely — see the module note.
    let agree = (0..13).filter(|&i| p[i] == !p[i + 13]).count();
    if agree < MIN_COMPLEMENT {
        return None;
    }

    let id = p[14];
    let sensor_type = p[15] & 0x0F;
    let battery_low = (p[25] >> 4) & 0x08 != 0;

    let mut extra = Vec::new();
    if battery_low {
        extra.push(("battery".to_string(), "low".to_string()));
    }

    if RAIN_GAUGE_TYPES.contains(&sensor_type) {
        // A real, validated Bresser frame whose payload means something else.
        // Reported rather than dropped: knowing a rain gauge is there is worth
        // having, and inventing a rainfall from an unverified digit order is not.
        extra.push(("payload".to_string(), "rain gauge, not decoded".to_string()));
        return Some(Decoded {
            protocol: IsmProtocol::Bresser,
            model: Some("rain gauge".to_string()),
            device: format!("{id:02x}"),
            readings: Vec::new(),
            extra,
            encrypted: false,
            raw_hex: String::new(),
        });
    }

    let mut readings = Vec::new();

    // Temperature: two BCD digits plus a hundreds digit, sign in the low nibble
    // of the last byte.
    if let Some(tens) = bcd(p[20]) {
        let hundreds = u32::from(p[21] & 0x0F);
        if hundreds <= 9 {
            let tenths = i64::from(hundreds * 100 + tens);
            let signed = if p[25] & 0x0F != 0 { -tenths } else { tenths };
            let temp_c = signed as f64 / 10.0;
            if (-40.0..=80.0).contains(&temp_c) {
                readings.push(IsmReading::new(IsmQuantity::TempC, temp_c));
            }
        }
    }

    if let Some(h) = bcd(p[22]) {
        if h <= 100 {
            readings.push(IsmReading::new(IsmQuantity::HumidityPct, f64::from(h)));
        }
    }

    // Wind average, BCD, same two-digits-plus-hundreds shape as the temperature.
    if let Some(tens) = bcd(p[18]) {
        let hundreds = u32::from(p[19] & 0x0F);
        if hundreds <= 9 {
            let ms = f64::from(hundreds * 100 + tens) / 10.0;
            if ms <= MAX_WIND_MS {
                readings.push(IsmReading::new(IsmQuantity::WindAvgMs, ms));
            }
        }
    }

    // Gust, plain binary in tenths, nine bits.
    let gust = f64::from((u32::from(p[17] & 0x0F) << 8) | u32::from(p[16])) / 10.0;
    if gust <= MAX_WIND_MS {
        readings.push(IsmReading::new(IsmQuantity::WindGustMs, gust));
    }

    // Sixteen compass points.
    readings.push(IsmReading::new(IsmQuantity::WindDirDeg, f64::from(p[17] >> 4) * 22.5));

    // A frame that validated but yielded nothing readable is a decode failure, not
    // a device with no measurements.
    if readings.len() < 2 {
        return None;
    }

    Some(Decoded {
        protocol: IsmProtocol::Bresser,
        model: Some("5-in-1".to_string()),
        device: format!("{id:02x}"),
        readings,
        extra,
        encrypted: false,
        raw_hex: String::new(),
    })
}

/// Fastest wind worth believing, m/s. The encoding reaches 99.9; the strongest
/// surface gust ever recorded is near 113, and a domestic anemometer reporting
/// anything close is reporting a decode error.
const MAX_WIND_MS: f64 = 80.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(hex: &str) -> Vec<u8> {
        hex.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    fn get(d: &Decoded, q: IsmQuantity) -> Option<f64> {
        d.readings.iter().find(|r| r.quantity == q).map(|r| r.value)
    }

    /// The published capture annotated "Large Wind Values, Gust=37.4m/s
    /// Avg=27.5m/s". Both come out exactly, which is what pins the BCD
    /// hundreds-digit nibble — and with it the temperature field, which has the
    /// same shape.
    #[test]
    fn the_published_wind_capture_reproduces_its_stated_speeds() {
        let f =
            frame("e3 fd 7f 89 7e 8a ed 68 fe af 9b fd ff 1c 02 80 76 81 75 12 97 01 50 64 02 00");
        let d = parse(&f).expect("the sample did not decode");
        assert_eq!(d.protocol, IsmProtocol::Bresser);
        assert_eq!(d.model.as_deref(), Some("5-in-1"));
        assert_eq!(d.device, "02");
        assert!((get(&d, IsmQuantity::WindGustMs).unwrap() - 37.4).abs() < 1e-9);
        assert!((get(&d, IsmQuantity::WindAvgMs).unwrap() - 27.5).abs() < 1e-9);
        assert!((get(&d, IsmQuantity::WindDirDeg).unwrap() - 180.0).abs() < 1e-9);
        assert!((get(&d, IsmQuantity::HumidityPct).unwrap() - 50.0).abs() < 1e-9);
        assert!(d.extra.is_empty(), "battery was fine in this capture");
    }

    /// The captures annotated with a battery state and a temperature sign.
    #[test]
    fn the_published_temperature_captures_reproduce_their_signs_and_batteries() {
        // "low batt -ve temp -7.0C"
        let f =
            frame("ed a1 ff ff 1f ff ef 8f ff d6 df ff 77 12 5e 00 00 e0 00 10 70 00 29 20 00 88");
        let d = parse(&f).expect("did not decode");
        assert!((get(&d, IsmQuantity::TempC).unwrap() + 7.0).abs() < 1e-9);
        assert!((get(&d, IsmQuantity::HumidityPct).unwrap() - 29.0).abs() < 1e-9);
        assert_eq!(d.extra, vec![("battery".to_string(), "low".to_string())]);

        // "low batt +ve temp"
        let f =
            frame("ef a1 ff ff 1f ff ef dc ff de df ff 7f 10 5e 00 00 e0 00 10 23 00 21 20 00 80");
        let d = parse(&f).expect("did not decode");
        assert!(get(&d, IsmQuantity::TempC).unwrap() > 0.0, "sign bit clear means positive");
        assert!((get(&d, IsmQuantity::TempC).unwrap() - 2.3).abs() < 1e-9);
        assert_eq!(d.extra, vec![("battery".to_string(), "low".to_string())]);

        // "good batt -ve temp" — and this is the one whose complement copy differs
        // by a single bit, so it is also the test that the loose check exists for.
        let f =
            frame("ec 91 ff ff 3f fb ef e7 fe ad ed ff f7 13 6e 00 00 e0 04 10 18 01 52 12 00 08");
        let agree = (0..13).filter(|&i| f[i] == !f[i + 13]).count();
        assert_eq!(agree, 12, "this capture is one byte out; that is the point of it");
        let d = parse(&f).expect("a one-bit slip must not lose the frame");
        assert!(get(&d, IsmQuantity::TempC).unwrap() < 0.0);
        assert!(d.extra.is_empty(), "battery was fine here");
    }

    /// A rain gauge is a different payload behind the same framing. It has to be
    /// reported as present without inventing a rainfall.
    #[test]
    fn a_rain_gauge_is_reported_but_not_read() {
        let f =
            frame("ed ee 46 ff ff ff ef 9f ff 8b 7d eb ff 12 11 b9 00 00 00 10 60 00 74 82 14 00");
        let d = parse(&f).expect("did not decode");
        assert_eq!(d.model.as_deref(), Some("rain gauge"));
        assert_eq!(d.device, "11");
        assert!(d.readings.is_empty(), "the rain digit order is unverified");
        assert!(d.extra.iter().any(|(k, _)| k == "payload"));
    }

    /// Both checks have to bite, independently.
    #[test]
    fn each_check_rejects_on_its_own() {
        let good =
            frame("e3 fd 7f 89 7e 8a ed 68 fe af 9b fd ff 1c 02 80 76 81 75 12 97 01 50 64 02 00");
        assert!(parse(&good).is_some());

        // Wrong checksum only.
        let mut bad = good.clone();
        bad[13] ^= 0x01;
        assert!(parse(&bad).is_none(), "a wrong population count was accepted");

        // Complement broken in three bytes, with the checksum left valid — the
        // data half is untouched, so only the redundancy check can reject it.
        let mut bad = good.clone();
        for i in 0..3 {
            bad[i] ^= 0xFF;
        }
        assert!(parse(&bad).is_none(), "a broken complement copy was accepted");

        for n in 0..26 {
            assert!(parse(&good[..n]).is_none(), "{n} bytes should not parse");
        }
    }

    /// The four published 6-in-1 captures annotated with readings. Both of that
    /// frame's checks — the LFSR-16 digest and the add-to-0xff checksum — have to
    /// reproduce, and the temperature and humidity have to come out as stated.
    #[test]
    fn the_published_6in1_captures_reproduce_their_readings() {
        for hex in [
            "5e aa 18 80 02 c3 18 fa 8f fb 27 68 11 84 81 ff f0 72 00",
            "f8 2e 18 80 02 c3 18 fc c6 fd 26 38 11 84 81 ff f0 68 00",
            "21 e8 18 80 02 c3 18 fb 9c fc 33 08 11 84 81 ff f0 b7 f8",
            "5c e4 18 80 02 c3 18 fb ba fc 26 98 11 84 81 ff f0 16 00",
        ] {
            let f = frame(hex);
            let d = parse(&f).unwrap_or_else(|| panic!("{hex} did not decode"));
            assert_eq!(d.protocol, IsmProtocol::Bresser);
            assert_eq!(d.model.as_deref(), Some("6-in-1"));
            assert_eq!(d.device, "188002c3", "the four-byte id");
            assert!(
                (get(&d, IsmQuantity::TempC).unwrap() - 11.8).abs() < 1e-9,
                "temp was {:?}",
                get(&d, IsmQuantity::TempC)
            );
            assert!((get(&d, IsmQuantity::HumidityPct).unwrap() - 81.0).abs() < 1e-9);
        }
    }

    /// The digest is the check that makes an 8-bit add-checksum safe to trust, so
    /// it has to actually reject.
    #[test]
    fn the_6in1_digest_rejects_a_tampered_frame() {
        let good = frame("5e aa 18 80 02 c3 18 fa 8f fb 27 68 11 84 81 ff f0 72 00");
        assert!(parse(&good).is_some());

        // Move a data byte and repair the add-checksum, so only the digest can
        // catch it. This is the case an add-checksum alone lets through.
        let mut bad = good.clone();
        bad[12] = 0x22;
        let body: u8 = bad[2..17].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        bad[17] = 0xFFu8.wrapping_sub(body);
        assert_eq!(
            bad[2..17].iter().fold(0u8, |a, &b| a.wrapping_add(b)).wrapping_add(bad[17]),
            0xFF,
            "the add-checksum must be valid, or this tests the wrong thing"
        );
        assert!(parse(&bad).is_none(), "the digest failed to catch a tampered payload");

        // And the add-checksum alone must also bite.
        let mut bad = good.clone();
        bad[17] ^= 0x01;
        assert!(parse(&bad).is_none());
    }

    /// The wind-only alternating message is a different length and validates at
    /// neither check here. It must be rejected, not misread as a temperature.
    #[test]
    fn the_wind_only_message_is_rejected_rather_than_misread() {
        for hex in [
            "ae d1 18 80 02 c3 18 fa 8d fb 26 78 ff ff ff fe 02 db f0",
            "c4 7d 18 80 02 c3 18 fc 78 fd 29 28 ff ff ff fe 03 97 f0",
            "28 1e 18 80 02 c3 18 fb b7 fc 26 58 ff ff ff fe 02 c3 f0",
        ] {
            assert!(parse(&frame(hex)).is_none(), "{hex} should not have decoded");
        }
    }

    /// Noise must not pass. A population count plus eleven complement bytes is
    /// around a hundred bits of redundancy, so this should be exactly zero.
    #[test]
    fn random_frames_never_pass() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut accepted = 0;
        for _ in 0..200_000 {
            let mut f = [0u8; 26];
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
        assert_eq!(accepted, 0, "{accepted} random frames passed a 100-bit redundancy check");
    }

    /// And the same for the 6-in-1 length, whose protection is a 16-bit digest plus
    /// an 8-bit checksum — one in sixteen million, so a few hundred thousand tries
    /// should find none.
    #[test]
    fn random_6in1_length_frames_never_pass() {
        let mut seed = 0x1357_9BDF_0246_8ACEu64;
        let mut accepted = 0;
        for _ in 0..300_000 {
            let mut f = [0u8; 18];
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
        assert_eq!(accepted, 0, "{accepted} random 18-byte frames were accepted");
    }
}
