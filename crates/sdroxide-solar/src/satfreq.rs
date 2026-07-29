//! Published transponder, beacon and telemetry frequencies for the satellites
//! the tracker knows about.
//!
//! This is reference data, not something derived from the element sets: a TLE
//! says where a satellite is, never what it listens on. The table below is
//! transcribed from the AMSAT frequency list and the operators' own status
//! pages, keyed by NORAD catalogue number so it survives a satellite being
//! renamed in the CelesTrak listing.
//!
//! Frequencies move. Transponders get switched to a different mode, FM birds
//! are put into schedule-only operation, and a bird that has been quiet for a
//! year comes back on a different passband. Nothing here is authoritative
//! against the satellite itself, which is why [`crate::satfreq`] is only half
//! the story — the operator can add and correct entries of their own, and those
//! win over anything in this file.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// A passband, or a single frequency when `lo == hi`.
///
/// Megahertz rather than hertz because that is how every published satellite
/// frequency is written, and rounding a 10.489 GHz downlink into an integer
/// number of hertz would invent precision the source never had.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Passband {
    pub lo_mhz: f64,
    pub hi_mhz: f64,
}

impl Passband {
    pub const fn at(mhz: f64) -> Passband {
        Passband { lo_mhz: mhz, hi_mhz: mhz }
    }

    pub const fn range(lo_mhz: f64, hi_mhz: f64) -> Passband {
        Passband { lo_mhz, hi_mhz }
    }

    /// True for a single frequency rather than a transponder passband.
    pub fn is_point(&self) -> bool {
        (self.hi_mhz - self.lo_mhz).abs() < 1e-9
    }

    /// Passband width in kilohertz — zero for a single frequency.
    pub fn width_khz(&self) -> f64 {
        (self.hi_mhz - self.lo_mhz).abs() * 1000.0
    }

    /// Middle of the passband, which is where a transponder is normally netted.
    pub fn centre_mhz(&self) -> f64 {
        0.5 * (self.lo_mhz + self.hi_mhz)
    }
}

impl std::fmt::Display for Passband {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_point() {
            write!(f, "{}", fmt_mhz(self.lo_mhz))
        } else {
            write!(f, "{}–{}", fmt_mhz(self.lo_mhz), fmt_mhz(self.hi_mhz))
        }
    }
}

/// Format a frequency in MHz at the precision it was published to.
///
/// Most satellite frequencies land on a kilohertz; the NOAA APT downlinks are
/// on a quarter of one. Printing everything to four decimals would put a
/// meaningless trailing zero on almost every row.
pub fn fmt_mhz(mhz: f64) -> String {
    if (mhz * 1000.0 - (mhz * 1000.0).round()).abs() < 1e-6 {
        format!("{mhz:.3}")
    } else {
        format!("{mhz:.4}")
    }
}

/// One radio link on a satellite: a transponder, a repeater, a beacon or a
/// telemetry downlink.
///
/// Both `uplink` and `downlink` are optional because plenty of links have only
/// one: a CW beacon transmits and never listens, and there is no such thing as
/// a satellite that listens and never transmits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SatLink {
    /// What the link is for — "V/U linear transponder", "FM voice", "APRS
    /// digipeater", "CW beacon".
    pub label: String,
    /// Emission the link carries — "SSB/CW", "FM", "BPSK 1k2", "DVB-S2".
    pub mode: String,
    pub uplink: Option<Passband>,
    pub downlink: Option<Passband>,
    /// Anything an operator has to know before keying up: a CTCSS tone, an
    /// inverting transponder, a duty schedule.
    pub note: String,
}

impl Default for SatLink {
    fn default() -> Self {
        SatLink {
            label: String::new(),
            mode: String::new(),
            uplink: None,
            downlink: None,
            note: String::new(),
        }
    }
}

impl SatLink {
    /// A downlink-only link (beacon, telemetry, APT).
    pub fn down(label: &str, mode: &str, down: Passband) -> SatLink {
        SatLink {
            label: label.into(),
            mode: mode.into(),
            downlink: Some(down),
            ..Default::default()
        }
    }

    /// A two-way link.
    pub fn duplex(label: &str, mode: &str, up: Passband, down: Passband) -> SatLink {
        SatLink {
            label: label.into(),
            mode: mode.into(),
            uplink: Some(up),
            downlink: Some(down),
            ..Default::default()
        }
    }

    pub fn note(mut self, note: &str) -> SatLink {
        self.note = note.into();
        self
    }

    /// True when neither direction has a frequency — an entry the operator
    /// started and never filled in, which is not worth a row in the table.
    pub fn is_empty(&self) -> bool {
        self.uplink.is_none() && self.downlink.is_none()
    }
}

/// Every published link for one satellite.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SatFreqs {
    pub norad_id: u64,
    /// The designator the frequencies were published under. Shown when it
    /// differs from the name the element set carries, so a table entry that has
    /// drifted onto the wrong catalogue number is visible rather than silent.
    pub name: String,
    pub links: Vec<SatLink>,
}

impl Default for SatFreqs {
    fn default() -> Self {
        SatFreqs { norad_id: 0, name: String::new(), links: Vec::new() }
    }
}

impl SatFreqs {
    fn new(norad_id: u64, name: &str, links: Vec<SatLink>) -> SatFreqs {
        SatFreqs { norad_id, name: name.into(), links }
    }
}

/// The built-in table, built once and shared.
pub fn builtin() -> &'static [SatFreqs] {
    static TABLE: OnceLock<Vec<SatFreqs>> = OnceLock::new();
    TABLE.get_or_init(build)
}

/// Look a satellite up in the built-in table.
pub fn builtin_for(norad_id: u64) -> Option<&'static SatFreqs> {
    builtin().iter().find(|s| s.norad_id == norad_id)
}

fn build() -> Vec<SatFreqs> {
    use Passband as P;
    let (down, duplex) = (SatLink::down, SatLink::duplex);

    vec![
        // The geostationary one. Its narrowband transponder is the only
        // amateur satellite passband that can be left tuned and worked all day.
        SatFreqs::new(
            crate::satellites::QO100_NORAD,
            "QO-100 / Es'hail-2",
            vec![
                duplex(
                    "NB transponder",
                    "SSB/CW/digital",
                    P::range(2400.050, 2400.300),
                    P::range(10489.550, 10489.800),
                )
                .note("non-inverting; 2.4 GHz up, 10 GHz down"),
                down("NB lower beacon", "CW", P::at(10489.500)),
                down("NB upper beacon", "BPSK 400bd", P::at(10489.800)),
                duplex(
                    "WB transponder",
                    "DVB-S2",
                    P::range(2401.500, 2409.500),
                    P::range(10491.000, 10499.000),
                )
                .note("amateur television"),
            ],
        ),
        SatFreqs::new(
            25544,
            "ISS",
            vec![
                duplex("FM voice", "FM", P::at(145.200), P::at(145.800))
                    .note("uplink 144.490 in ITU regions 2 and 3"),
                down("SSTV", "FM / PD120", P::at(145.800)).note("event-driven; check ARISS"),
                duplex("APRS digipeater", "AX.25 1k2 AFSK", P::at(145.825), P::at(145.825))
                    .note("simplex; path ARISS"),
                duplex("Cross-band repeater", "FM", P::at(145.990), P::at(437.800))
                    .note("CTCSS 67.0 Hz on the uplink"),
            ],
        ),
        SatFreqs::new(
            7530,
            "AO-7",
            vec![
                duplex(
                    "Mode A transponder",
                    "SSB/CW",
                    P::range(145.850, 145.950),
                    P::range(29.400, 29.500),
                )
                .note("non-inverting; alternates with mode B roughly daily"),
                duplex(
                    "Mode B transponder",
                    "SSB/CW",
                    P::range(432.125, 432.175),
                    P::range(145.925, 145.975),
                )
                .note("inverting"),
                down("Mode A beacon", "CW", P::at(29.502)),
                down("Mode B beacon", "CW", P::at(145.972)),
            ],
        ),
        SatFreqs::new(
            24278,
            "FO-29",
            vec![
                duplex(
                    "V/U transponder",
                    "SSB/CW",
                    P::range(145.900, 146.000),
                    P::range(435.800, 435.900),
                )
                .note("inverting; LSB up, USB down"),
                down("Beacon", "CW", P::at(435.795)),
            ],
        ),
        SatFreqs::new(
            27607,
            "SO-50",
            vec![
                duplex("FM repeater", "FM", P::at(145.850), P::at(436.795))
                    .note("CTCSS 67.0 Hz; arm the 10-minute timer with a 74.4 Hz burst"),
            ],
        ),
        SatFreqs::new(
            39444,
            "AO-73 / FUNcube-1",
            vec![
                duplex(
                    "U/V transponder",
                    "SSB/CW",
                    P::range(435.130, 435.150),
                    P::range(145.950, 145.970),
                )
                .note("inverting; transponder runs in eclipse, telemetry in sunlight"),
                down("Telemetry", "BPSK 1k2", P::at(145.935)),
            ],
        ),
        SatFreqs::new(
            43803,
            "JO-97 / JY1SAT",
            vec![
                duplex(
                    "U/V transponder",
                    "SSB/CW",
                    P::range(435.100, 435.140),
                    P::range(145.855, 145.895),
                )
                .note("inverting"),
                down("Telemetry / SSDV", "BPSK 1k2", P::at(145.840)),
            ],
        ),
        SatFreqs::new(
            44909,
            "RS-44",
            vec![
                duplex(
                    "U/V transponder",
                    "SSB/CW",
                    P::range(145.935, 145.995),
                    P::range(435.610, 435.670),
                )
                .note("inverting; a high, slow orbit, so passes run long"),
                down("Beacon", "CW", P::at(435.605)),
            ],
        ),
        SatFreqs::new(
            50466,
            "XW-3 / CAS-9",
            vec![
                duplex(
                    "U/V transponder",
                    "SSB/CW",
                    P::range(435.170, 435.190),
                    P::range(145.860, 145.880),
                )
                .note("inverting"),
                down("CW beacon", "CW", P::at(145.845)),
            ],
        ),
        SatFreqs::new(
            53109,
            "IO-117 / GreenCube",
            vec![
                duplex("Digipeater", "GMSK 1k2", P::at(435.310), P::at(435.310))
                    .note("simplex store-and-forward; a MEO orbit, so it is workable for hours"),
            ],
        ),
        SatFreqs::new(
            43017,
            "AO-91 / RadFxSat",
            vec![
                duplex("FM repeater", "FM", P::at(435.250), P::at(145.960))
                    .note("CTCSS 67.0 Hz; sunlit passes only"),
            ],
        ),
        SatFreqs::new(
            43678,
            "PO-101 / Diwata-2",
            vec![
                duplex("FM repeater", "FM", P::at(437.500), P::at(145.900))
                    .note("CTCSS 141.3 Hz; scheduled operation only"),
            ],
        ),
        SatFreqs::new(
            44881,
            "TO-108 / CAS-6",
            vec![
                duplex(
                    "U/V transponder",
                    "SSB/CW",
                    P::range(435.270, 435.290),
                    P::range(145.915, 145.935),
                )
                .note("inverting"),
                down("CW beacon", "CW", P::at(145.910)),
            ],
        ),
        // Not amateur, but they are the reason half of us own a 137 MHz
        // antenna, and their downlinks feed the same weather-fax habit as the
        // HF charts. Their element sets are not in the amateur group, so they
        // only appear once the operator adds them in the TLE manager.
        SatFreqs::new(
            25338,
            "NOAA-15",
            vec![down("APT", "FM / APT 2400 Hz", P::at(137.620)).note("polar, ~102 min")],
        ),
        SatFreqs::new(
            28654,
            "NOAA-18",
            vec![down("APT", "FM / APT 2400 Hz", P::at(137.9125)).note("polar, ~102 min")],
        ),
        SatFreqs::new(
            33591,
            "NOAA-19",
            vec![down("APT", "FM / APT 2400 Hz", P::at(137.100)).note("polar, ~102 min")],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_keyed_by_a_unique_catalogue_number() {
        let mut ids: Vec<u64> = builtin().iter().map(|s| s.norad_id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "two entries share a NORAD id");
        assert!(n >= 15, "only {n} satellites in the table");
    }

    /// Every row has to be usable: a name, at least one link, and every link
    /// with a direction and a plausible amateur/weather-satellite frequency.
    #[test]
    fn every_entry_is_complete_and_in_a_real_band() {
        for s in builtin() {
            assert!(!s.name.is_empty(), "{} has no name", s.norad_id);
            assert!(!s.links.is_empty(), "{} has no links", s.name);
            for l in &s.links {
                assert!(!l.label.is_empty(), "{}: a link has no label", s.name);
                assert!(!l.mode.is_empty(), "{}: {} has no mode", s.name, l.label);
                assert!(!l.is_empty(), "{}: {} has neither direction", s.name, l.label);
                for band in [l.uplink, l.downlink].into_iter().flatten() {
                    assert!(band.lo_mhz <= band.hi_mhz, "{}: {} inverted", s.name, l.label);
                    // 29 MHz mode A through the 10 GHz QO-100 downlink.
                    assert!(
                        (28.0..=11_000.0).contains(&band.lo_mhz),
                        "{}: {} at {} MHz",
                        s.name,
                        l.label,
                        band.lo_mhz
                    );
                    // No transponder is a megahertz wide; a typo in the high
                    // end is the failure this catches.
                    assert!(
                        band.width_khz() <= 10_000.0,
                        "{}: {} is {} kHz wide",
                        s.name,
                        l.label,
                        band.width_khz()
                    );
                }
            }
        }
    }

    /// The reference cases, spot-checked against the published lists.
    #[test]
    fn the_well_known_satellites_are_on_their_published_frequencies() {
        let qo = builtin_for(crate::satellites::QO100_NORAD).expect("QO-100 in the table");
        let nb = &qo.links[0];
        assert_eq!(nb.downlink.unwrap().lo_mhz, 10489.550);
        assert_eq!(nb.uplink.unwrap().lo_mhz, 2400.050);
        assert_eq!(nb.downlink.unwrap().width_khz(), 250.0);

        let iss = builtin_for(25544).expect("ISS in the table");
        assert!(iss.links.iter().any(|l| l.downlink == Some(Passband::at(145.800))));
        assert!(iss.links.iter().any(|l| l.label.contains("APRS")));

        let so50 = builtin_for(27607).expect("SO-50 in the table");
        assert_eq!(so50.links[0].downlink, Some(Passband::at(436.795)));
        assert!(so50.links[0].note.contains("67.0"), "SO-50 needs its CTCSS tone");

        // The three APT birds, which is what the 137 MHz antenna is for.
        assert_eq!(builtin_for(25338).unwrap().links[0].downlink, Some(Passband::at(137.620)));
        assert_eq!(builtin_for(33591).unwrap().links[0].downlink, Some(Passband::at(137.100)));
    }

    /// Every satellite in [`crate::satellites::POPULAR`] — the ones drawn by
    /// default — has to have frequencies, or clicking the most prominent
    /// markers in the view would open a pass table with nothing to tune to.
    #[test]
    fn every_default_satellite_has_frequencies() {
        for (id, name) in crate::satellites::POPULAR {
            assert!(builtin_for(*id).is_some(), "{name} ({id}) has no frequencies");
        }
    }

    #[test]
    fn frequencies_print_at_the_precision_they_were_published_to() {
        assert_eq!(fmt_mhz(145.8), "145.800");
        assert_eq!(fmt_mhz(137.9125), "137.9125");
        assert_eq!(fmt_mhz(10489.55), "10489.550");
        assert_eq!(Passband::at(436.795).to_string(), "436.795");
        assert_eq!(Passband::range(145.95, 145.97).to_string(), "145.950–145.970");
        assert!((Passband::range(145.95, 145.97).centre_mhz() - 145.96).abs() < 1e-9);
        assert!(Passband::at(1.0).is_point());
        assert!(!Passband::range(1.0, 2.0).is_point());
    }

    #[test]
    fn an_unfilled_link_is_empty() {
        assert!(SatLink::default().is_empty());
        assert!(!SatLink::down("b", "CW", Passband::at(29.5)).is_empty());
    }
}
