//! The protocol registry: what a decoded frame means, and which decoders to try
//! on a burst.
//!
//! A protocol is a channel, a physical layer, and a function from validated
//! bytes to readings. Adding one is adding a row to [`PROTOCOLS`] and a module
//! beside this one; nothing else in the crate has to know it exists.

use sdroxide_types::{IsmProtocol, IsmReading};

use crate::gate::Burst;
use crate::slice::Phy;

pub mod bresser;
pub mod fineoffset;
pub mod lacrosse;

/// What a protocol module makes of a frame.
pub struct Decoded {
    pub protocol: IsmProtocol,
    /// Model or variant, when the frame actually says. `None` rather than a
    /// guess — see the note in [`lacrosse::parse`].
    pub model: Option<String>,
    pub device: String,
    pub readings: Vec<IsmReading>,
    pub extra: Vec<(String, String)>,
    /// The frame validated but its payload is encrypted. Reached by the metering
    /// protocols; no sensor uses it.
    pub encrypted: bool,
    /// The validated frame as hex. Filled in by [`decode`], which is the layer
    /// that still has the bytes — a protocol module leaves it empty.
    pub raw_hex: String,
}

/// One entry in the registry.
pub struct Protocol {
    pub id: IsmProtocol,
    /// The channel this protocol lives on, matched against the burst's.
    pub channel_hz: f64,
    pub phy: Phy,
    /// Bytes after the sync word to readings, or `None` if they are not a frame
    /// of this protocol. Must validate — the slicer's candidates include noise.
    pub parse: fn(&[u8]) -> Option<Decoded>,
}

/// Every protocol, in the order they are tried.
pub const PROTOCOLS: &[Protocol] = &[
    Protocol {
        id: IsmProtocol::LaCrosseIt,
        channel_hz: lacrosse::CHANNEL_HZ,
        phy: lacrosse::PHY,
        parse: lacrosse::parse,
    },
    Protocol {
        id: IsmProtocol::FineOffset,
        channel_hz: fineoffset::CHANNEL_HZ,
        phy: fineoffset::PHY,
        parse: fineoffset::parse,
    },
    Protocol {
        id: IsmProtocol::Bresser,
        channel_hz: bresser::CHANNEL_HZ,
        phy: bresser::PHY,
        parse: bresser::parse,
    },
];

/// What a burst turned out to be, alongside the measurements taken from it.
pub struct Outcome {
    pub decoded: Vec<Decoded>,
    /// Absolute RF frequency the burst was actually on: the channel centre plus
    /// the carrier offset measured from the signal. Reporting the channel centre
    /// instead would put every device in the band on the same four frequencies.
    pub freq_hz: f64,
    pub snr_db: f32,
    /// Absolute peak power, for comparing two receivers' views of one burst.
    pub peak_dbfs: f32,
    /// Measured deviation, symbol rate and envelope swing, for the diagnostic
    /// dump. Not used to decide anything.
    pub dev_hz: f32,
    pub baud: f64,
    pub env_depth: f32,
    /// Candidate frames the slicer produced, whether or not any parsed. A burst
    /// with candidates but no decode is an unsupported device; one with no
    /// candidates at all is not this modulation.
    pub candidates: usize,
    /// Symbol rate measured from the burst itself with no protocol to guess from,
    /// and how well the measurement fitted. The number that says what to write
    /// next when nothing decoded.
    pub measured_baud: Option<(f64, f32)>,
    /// The burst sliced at its own measured rate, both bit senses, as hex.
    ///
    /// Computed always rather than behind a flag: it is one slicing pass against
    /// the thirty-two each protocol already does, and a burst nothing could read is
    /// exactly when somebody wants to look at the bits.
    pub bits_hex: String,
}

/// Furthest a burst's carrier may sit from its channel and still be treated as
/// that channel's traffic, in Hz.
///
/// The channels in the plan are as close as 120 kHz apart while their baseband
/// windows are two to five hundred kilohertz wide, so a transmission on one
/// arrives in its neighbour's downconverter as well. Without this guard the same
/// device would be attributed to two channels — and once a protocol exists on
/// each of them, decoded and reported twice.
///
/// 60 kHz at 868 MHz is 69 ppm, which no crystal-referenced transmitter is out
/// by: these are cheap references, but they are an order of magnitude better than
/// that. So the bound rejects a neighbour without ever rejecting a device that is
/// merely off frequency.
const MAX_CARRIER_OFFSET_HZ: f32 = 60_000.0;

/// Space-separated lowercase hex, which is how every write-up and capture file
/// for these protocols prints a frame — so a report can be pasted straight into a
/// search or a bug report.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

/// Scratch buffers, so a channel's decoder does not allocate per burst.
#[derive(Default)]
pub struct Scratch {
    disc: Vec<f32>,
    pick: Vec<f32>,
}

/// Try every enabled protocol on a burst.
///
/// `enabled` decides which registry entries run; the caller maps the operator's
/// family switches onto it.
///
/// `require_channel` asks whether a protocol may only be tried on bursts from its
/// own channel. True for the engine, where a burst arriving on 868.95 MHz cannot
/// have come from a sensor that only transmits on 868.30 and trying anyway is
/// wasted work. False for a survey, whose receivers sit on a grid of its own
/// choosing rather than on the plan — there, "it decoded as a LaCrosse" is the most
/// useful identification a burst can be given, and refusing to try because the
/// tile centre is 54 kHz off would throw it away.
pub fn decode(
    burst: &Burst,
    scratch: &mut Scratch,
    enabled: &dyn Fn(IsmProtocol) -> bool,
    require_channel: bool,
) -> Outcome {
    crate::demod::discriminate(&burst.iq, burst.rate_hz, &mut scratch.disc);
    let stats = crate::demod::stats(&burst.iq, &scratch.disc, &mut scratch.pick);

    // Only the part of the burst that is signal; see the note at the slicers below.
    let body = &scratch.disc[stats.signal.clone()];
    let measured_baud = crate::demod::estimate_baud_free(body, stats.center_hz, burst.rate_hz);
    let bits_hex = match measured_baud {
        Some((b, _)) => {
            let bits = crate::slice::slice_at(body, stats.center_hz, burst.rate_hz / b);
            // Enough for the longest frame in the band — a Bresser 7-in-1 is
            // twenty-three payload bytes behind its preamble — so a whole frame is
            // visible rather than just its header.
            let take = bits.len().min(40 * 8);
            let bytes = crate::slice::pack_bits(&bits[..take]);
            let inverted: Vec<u8> = bytes.iter().map(|x| !*x).collect();
            format!("bits {}\n      inv  {}", hex(&bytes), hex(&inverted))
        }
        None => String::new(),
    };

    let mut decoded = Vec::new();
    let mut candidates = 0usize;
    let mut baud = 0.0f64;

    // A burst sitting far off the channel centre is a neighbouring channel's
    // transmission leaking through this one's filter, not this one's traffic.
    let on_channel = stats.center_hz.abs() <= MAX_CARRIER_OFFSET_HZ;

    for p in PROTOCOLS {
        if !on_channel {
            break;
        }
        if !enabled(p.id) {
            continue;
        }
        // The channel a burst arrived on decides which protocols can possibly
        // have sent it. Comparing to within a tenth of the channel spacing
        // rather than exactly keeps this robust to a plan edited in Hz.
        if require_channel && (p.channel_hz - burst.center_hz).abs() > 10_000.0 {
            continue;
        }

        // The slicing level is the *measured* carrier, not zero: these
        // transmitters are tens of kilohertz off channel often enough that
        // slicing about zero would read every bit the same.
        // The signal region only: the padding either side is noise, and over noise
        // the discriminator wanders across the whole channel — enough to defeat the
        // symbol-period measurement, which looks at the run-up and would otherwise
        // be reading the pre-trigger ring.
        let frames = crate::slice::frames(body, stats.center_hz, burst.rate_hz, &p.phy);
        candidates += frames.len();

        for f in &frames {
            if let Some(mut d) = (p.parse)(&f.bytes) {
                d.raw_hex = hex(&f.bytes);
                baud = burst.rate_hz / f.sps;
                decoded.push(d);
                // One device per protocol per burst. Later candidates are other
                // slicing phases reading the same transmission.
                break;
            }
        }
    }

    Outcome {
        decoded,
        freq_hz: burst.center_hz + f64::from(stats.center_hz),
        snr_db: burst.snr_db,
        peak_dbfs: burst.peak_dbfs,
        dev_hz: stats.dev_hz,
        baud,
        env_depth: stats.env_depth,
        candidates,
        measured_baud,
        bits_hex,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registry entry has to sit on a channel the plan actually opens, or
    /// its bursts are never delivered to it and it silently never runs.
    #[test]
    fn every_protocol_is_on_a_channel_in_the_plan() {
        for p in PROTOCOLS {
            let ch = crate::plan::CHANNELS
                .iter()
                .find(|c| (c.center_hz - p.channel_hz).abs() < 10_000.0)
                .unwrap_or_else(|| {
                    panic!("{:?} is on {} Hz, which is not in the plan", p.id, p.channel_hz)
                });
            // And that channel has to be wide enough to hold the signal.
            assert!(
                ch.rate_hz >= p.phy.baud * 4.0,
                "{:?} needs more than {} Hz to be demodulated",
                p.id,
                ch.rate_hz
            );
        }
    }

    /// A protocol whose family is switched off must not be tried at all.
    #[test]
    fn a_disabled_protocol_is_not_tried() {
        let burst = Burst {
            iq: vec![sdroxide_dsp::Complex32::new(0.1, 0.0); 4096],
            rate_hz: 250_000.0,
            center_hz: 868_300_000.0,
            snr_db: 20.0,
            peak_dbfs: -20.0,
        };
        let mut scratch = Scratch::default();
        let out = decode(&burst, &mut scratch, &|_| false, true);
        assert!(out.decoded.is_empty());
        assert_eq!(out.candidates, 0, "a disabled protocol still ran its slicer");
    }

    /// A burst from the wrong channel must not be offered to a protocol that
    /// does not live there.
    #[test]
    fn a_burst_from_another_channel_is_not_offered() {
        let burst = Burst {
            iq: vec![sdroxide_dsp::Complex32::new(0.1, 0.0); 4096],
            rate_hz: 500_000.0,
            center_hz: 868_950_000.0,
            snr_db: 20.0,
            peak_dbfs: -20.0,
        };
        let mut scratch = Scratch::default();
        let out = decode(&burst, &mut scratch, &|_| true, true);
        assert_eq!(out.candidates, 0);
    }
}
