//! HF weather-facsimile settings and status.
//!
//! The mode carries no metadata of its own — a radiofax transmission announces
//! its index of cooperation in the start tone and nothing else — so everything
//! that decides how the picture is built is an operator setting, and the panel
//! needs a live view of what the receiver is making of the signal.

use serde::{Deserialize, Serialize};

/// Index of cooperation: what fixes the number of pixels in a line.
///
/// Mirrors `sdroxide_dsp::wefax::Ioc`, which is where the arithmetic lives.
/// Duplicated rather than shared because this crate is the wasm-safe one and
/// must not depend on the DSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WefaxIoc {
    /// 576 — every weather chart. 1809 pixels per line.
    #[default]
    I576,
    /// 288 — half resolution, and the older satellite schedules.
    I288,
}

impl WefaxIoc {
    pub const ALL: [WefaxIoc; 2] = [WefaxIoc::I576, WefaxIoc::I288];

    pub fn value(self) -> u32 {
        match self {
            WefaxIoc::I576 => 576,
            WefaxIoc::I288 => 288,
        }
    }

    /// Pixels per line, `IOC × π` truncated — 1809 and 904.
    pub fn width(self) -> u16 {
        (self.value() as f64 * std::f64::consts::PI) as u16
    }

    pub fn label(self) -> &'static str {
        match self {
            WefaxIoc::I576 => "IOC 576",
            WefaxIoc::I288 => "IOC 288",
        }
    }
}

/// Scan rate in lines per minute. 120 is what the weather services use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WefaxLpm {
    L60,
    L90,
    #[default]
    L120,
    L180,
    L240,
}

impl WefaxLpm {
    pub const ALL: [WefaxLpm; 5] =
        [WefaxLpm::L60, WefaxLpm::L90, WefaxLpm::L120, WefaxLpm::L180, WefaxLpm::L240];

    pub fn value(self) -> f64 {
        match self {
            WefaxLpm::L60 => 60.0,
            WefaxLpm::L90 => 90.0,
            WefaxLpm::L120 => 120.0,
            WefaxLpm::L180 => 180.0,
            WefaxLpm::L240 => 240.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WefaxLpm::L60 => "60",
            WefaxLpm::L90 => "90",
            WefaxLpm::L120 => "120",
            WefaxLpm::L180 => "180",
            WefaxLpm::L240 => "240",
        }
    }
}

/// A broadcast radiofax schedule: what the station is called and the
/// frequencies it transmits on.
///
/// # The 1.9 kHz that catches everyone
///
/// Radiofax frequencies are published as the **assigned carrier**, which is
/// where the 1900 Hz subcarrier's centre would sit. Receiving in USB means
/// tuning 1.9 kHz *below* the published number, and getting this wrong is the
/// single most common reason a chart comes out as a blank page. [`WefaxStation::dial_hz`]
/// does the subtraction; the table stores what the schedules print.
pub struct WefaxStation {
    /// Callsign and location, as the schedules list it.
    pub name: &'static str,
    /// Assigned carrier frequencies in kHz, as published.
    pub carriers_khz: &'static [f64],
    /// Where the transmitter is, degrees north and east.
    ///
    /// Good to a few tens of kilometres, which is all a path drawn across a
    /// globe can show. Several of these are transmitter parks some way from the
    /// place the station is named after — Northwood's aerials are not in
    /// Northwood — and none of that is visible at this scale.
    pub lat: f64,
    pub lon: f64,
}

impl WefaxStation {
    /// The USB dial for a published carrier frequency, in Hz.
    pub fn dial_hz(carrier_khz: f64) -> f64 {
        carrier_khz * 1000.0 - 1900.0
    }

    /// How far a dial may be from a published one and still count as being on
    /// that station, in Hz.
    ///
    /// Generous, because a fax operator tunes by ear and by the subcarrier
    /// readout rather than to the hertz, and being a hundred hertz off is the
    /// normal state of a chart that is decoding perfectly well.
    pub const NEAR_HZ: f64 = 200.0;

    /// The station a dial frequency is sitting on, and the published carrier it
    /// matched.
    pub fn at_dial(dial_hz: f64) -> Option<(&'static WefaxStation, f64)> {
        WEFAX_STATIONS.iter().find_map(|s| {
            s.carriers_khz
                .iter()
                .find(|&&f| (WefaxStation::dial_hz(f) - dial_hz).abs() < WefaxStation::NEAR_HZ)
                .map(|&f| (s, f))
        })
    }
}

/// The stations most likely to be worth a listen, by region.
///
/// Broadcast schedules change and stations close — Northwood and several of the
/// US Coast Guard transmitters have had their hours cut repeatedly — so this is
/// a starting point for finding a signal, not a timetable. Every one of these
/// sends charts at 120 LPM and IOC 576, which is why the panel does not carry a
/// per-station geometry.
pub const WEFAX_STATIONS: &[WefaxStation] = &[
    WefaxStation {
        name: "DWD Pinneberg (Germany)",
        carriers_khz: &[3855.0, 7880.0, 13882.5],
        lat: 53.66,
        lon: 9.80,
    },
    WefaxStation {
        name: "GYA Northwood (UK)",
        carriers_khz: &[2618.5, 4610.0, 8040.0, 11086.5],
        lat: 51.60,
        lon: -0.42,
    },
    WefaxStation {
        name: "NMF Boston (USCG)",
        carriers_khz: &[4235.0, 6340.5, 9110.0, 12750.0],
        lat: 41.72,
        lon: -70.50,
    },
    WefaxStation {
        name: "NMG New Orleans (USCG)",
        carriers_khz: &[4317.9, 8503.9, 12789.9],
        lat: 29.88,
        lon: -89.94,
    },
    WefaxStation {
        name: "NMC Point Reyes (USCG)",
        carriers_khz: &[4346.0, 8682.0, 12786.0, 17151.2],
        lat: 38.10,
        lon: -122.87,
    },
    WefaxStation {
        name: "NOJ Kodiak (USCG)",
        carriers_khz: &[2054.0, 4298.0, 8459.0, 12412.5],
        lat: 57.75,
        lon: -152.48,
    },
    WefaxStation {
        name: "CFH Halifax (Canada)",
        carriers_khz: &[4271.0, 6496.4, 10536.0, 13510.0],
        lat: 44.97,
        lon: -63.35,
    },
    WefaxStation {
        name: "JMH Tokyo (Japan)",
        carriers_khz: &[3622.5, 7795.0, 13988.5],
        lat: 35.68,
        lon: 139.76,
    },
    WefaxStation {
        name: "VMC Charleville (Australia)",
        carriers_khz: &[2628.0, 5100.0, 11030.0, 13920.0, 20469.0],
        lat: -26.40,
        lon: 146.25,
    },
    WefaxStation {
        name: "VMW Wiluna (Australia)",
        carriers_khz: &[5755.0, 7535.0, 10555.0, 15615.0, 18060.0],
        lat: -26.63,
        lon: 120.22,
    },
];

/// What the fax receiver is doing, for the panel.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WefaxStatus {
    /// True once a transmission has started, by tone or by hand.
    pub receiving: bool,
    /// True while still hunting for the phasing pulse that aligns the picture.
    pub phasing: bool,
    /// Lines built so far in the picture in progress.
    pub lines: u16,
    pub ioc: WefaxIoc,
    pub lpm: WefaxLpm,
    /// Smoothed in-band level, for the tuning meter.
    pub signal: f32,
    /// Instantaneous subcarrier frequency, Hz. A correctly tuned receiver puts
    /// a chart's black and white excursions around 1900 Hz; a receiver 200 Hz
    /// off puts them around 1700 or 2100, which is what makes this the fastest
    /// way to tune a fax by eye.
    pub subcarrier_hz: f32,
    /// The sample-clock trim in effect, parts per million.
    pub slant_ppm: f32,
    /// How many pictures have been saved this session.
    pub saved: u32,
}

impl Default for WefaxStatus {
    fn default() -> Self {
        WefaxStatus {
            receiving: false,
            phasing: false,
            lines: 0,
            ioc: WefaxIoc::default(),
            lpm: WefaxLpm::default(),
            signal: 0.0,
            subcarrier_hz: 1900.0,
            slant_ppm: 0.0,
            saved: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These have to agree with `sdroxide_dsp::wefax`, which owns the real
    /// arithmetic; the DSP crate has the same assertion against its own copy.
    #[test]
    fn the_line_geometry_matches_the_standard() {
        assert_eq!(WefaxIoc::I576.width(), 1809);
        assert_eq!(WefaxIoc::I288.width(), 904);
        assert_eq!(WefaxLpm::L120.value(), 120.0);
        // Every variant has a distinct label, or the picker would show two of
        // the same chip.
        let labels: std::collections::HashSet<_> =
            WefaxLpm::ALL.iter().map(|l| l.label()).collect();
        assert_eq!(labels.len(), WefaxLpm::ALL.len());
    }

    /// The dial is the published carrier less 1.9 kHz. Getting this backwards
    /// is what produces a blank chart, so it is worth a test of its own.
    #[test]
    fn the_dial_sits_below_the_published_carrier() {
        // DWD Pinneberg's 7880 kHz is tuned at 7878.1 kHz USB.
        assert_eq!(WefaxStation::dial_hz(7880.0), 7_878_100.0);
        assert_eq!(WefaxStation::dial_hz(13882.5), 13_880_600.0);
    }

    /// A dial lands on the station whose carrier it is under, and nowhere else.
    #[test]
    fn a_dial_frequency_finds_the_station_it_is_tuned_to() {
        // DWD's 7880 kHz carrier is tuned at 7878.1 kHz.
        let (s, carrier) = WefaxStation::at_dial(7_878_100.0).expect("on DWD");
        assert!(s.name.starts_with("DWD"));
        assert_eq!(carrier, 7880.0);
        // A little off still counts — nobody tunes a fax to the hertz.
        assert!(WefaxStation::at_dial(7_878_000.0).is_some());
        // A long way off does not.
        assert!(WefaxStation::at_dial(7_878_100.0 + 1000.0).is_none());
        assert!(WefaxStation::at_dial(14_074_000.0).is_none());
        // Every published carrier of every station is findable from its dial,
        // which is what the station picker relies on to light the right chip.
        for st in WEFAX_STATIONS {
            for &f in st.carriers_khz {
                let (found, c) = WefaxStation::at_dial(WefaxStation::dial_hz(f))
                    .unwrap_or_else(|| panic!("{} {f} kHz is not findable", st.name));
                assert_eq!(found.name, st.name);
                assert_eq!(c, f);
            }
        }
    }

    /// Every station has to be usable: a name, at least one frequency, a
    /// plausible place on the Earth, and all of them on short wave where a fax
    /// transmitter can actually be.
    #[test]
    fn every_station_is_complete_and_on_short_wave() {
        assert!(WEFAX_STATIONS.len() >= 8);
        let mut all: Vec<f64> = Vec::new();
        for s in WEFAX_STATIONS {
            assert!(!s.name.is_empty());
            assert!(!s.carriers_khz.is_empty(), "{} has no frequencies", s.name);
            assert!((-90.0..=90.0).contains(&s.lat), "{}: latitude {}", s.name, s.lat);
            assert!((-180.0..=180.0).contains(&s.lon), "{}: longitude {}", s.name, s.lon);
            // Nothing at (0, 0), which is what an unfilled coordinate looks
            // like and is in the Gulf of Guinea.
            assert!(s.lat.abs() + s.lon.abs() > 1.0, "{} has no location", s.name);
            for &f in s.carriers_khz {
                assert!((2000.0..=25_000.0).contains(&f), "{}: {f} kHz", s.name);
                // The dial has to stay positive and below the carrier.
                let d = WefaxStation::dial_hz(f);
                assert!(d > 0.0 && d < f * 1000.0);
                all.push(f);
            }
        }
        // No frequency listed twice against different stations, which would be
        // a transcription slip.
        let n = all.len();
        all.sort_by(f64::total_cmp);
        all.dedup();
        assert_eq!(all.len(), n, "two stations share a frequency");
    }

    #[test]
    fn the_default_status_is_an_idle_receiver() {
        let s = WefaxStatus::default();
        assert!(!s.receiving && !s.phasing);
        assert_eq!(s.lines, 0);
        assert_eq!(s.ioc, WefaxIoc::I576);
        assert_eq!(s.lpm, WefaxLpm::L120);
        assert_eq!(s.subcarrier_hz, 1900.0);
    }
}
