use serde::{Deserialize, Serialize};

use crate::Region;

/// Amateur bands plus general coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Band {
    M160,
    M80,
    M60,
    M40,
    M30,
    M20,
    M17,
    M15,
    M12,
    M10,
    M6,
    M2,
    Gen,
    /// 70 cm. Appended rather than placed after [`Band::M2`] because `Band` is
    /// postcard-encoded by declaration index and stored in band stacks and
    /// memories; [`Band::ALL`] puts it where it belongs on screen.
    M70,
}

impl Band {
    pub const ALL: [Band; 14] = [
        Band::M160,
        Band::M80,
        Band::M60,
        Band::M40,
        Band::M30,
        Band::M20,
        Band::M17,
        Band::M15,
        Band::M12,
        Band::M10,
        Band::M6,
        Band::M2,
        Band::M70,
        Band::Gen,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Band::M160 => "160M",
            Band::M80 => "80M",
            Band::M60 => "60M",
            Band::M40 => "40M",
            Band::M30 => "30M",
            Band::M20 => "20M",
            Band::M17 => "17M",
            Band::M15 => "15M",
            Band::M12 => "12M",
            Band::M10 => "10M",
            Band::M6 => "6M",
            Band::M2 => "2M",
            Band::M70 => "70CM",
            Band::Gen => "GEN",
        }
    }

    /// Band edges in Hz for the station's configured region (see
    /// [`crate::region`]). `None` for general coverage.
    pub fn edges(self) -> Option<(f64, f64)> {
        self.edges_in(crate::region())
    }

    /// Band edges in Hz for `region`, or `None` for general coverage — and for
    /// a band the station's [`crate::BandPlan`] does not give that region.
    ///
    /// Read from the installed band plan, which is `bandplan.json` in the
    /// config directory once the operator has one and
    /// [`Band::iaru_default_edges_in`] until then.
    pub fn edges_in(self, region: Region) -> Option<(f64, f64)> {
        crate::band_plan().region(region).edges(self)
    }

    /// The built-in IARU edges for `region` — the seed for a fresh
    /// `bandplan.json`, and what is used until one is loaded.
    ///
    /// The *allocation*, not any one country's licence conditions: a national
    /// administration may grant less (Germany's 10 m stops at 29.700 like
    /// everyone's, but its 2 m ends at 146 while Region 1 as a whole varies) and
    /// occasionally more. These are the widest edges the region's amateurs
    /// share, which is what a shipped default can honestly be — an operator who
    /// needs their own licence's edges puts them in the file.
    ///
    /// Where the bands differ:
    /// - **160 m** starts at 1.810 in Region 1 and at 1.800 in Regions 2 and 3.
    /// - **80 m** ends at 3.800 / 4.000 / 3.900 in Regions 1 / 2 / 3.
    /// - **40 m** ends at 7.200 outside Region 2, which has the whole
    ///   7.000–7.300 to itself.
    /// - **6 m** ends at 52 MHz in Region 1 and 54 MHz elsewhere.
    /// - **2 m** ends at 146 MHz in Region 1 and 148 MHz elsewhere.
    /// - **70 cm** is 430–440 in Region 1, 420–450 in Region 2 and 430–450 in
    ///   Region 3.
    ///
    /// 30 m through 10 m, and the WRC-15 60 m allocation, are the same
    /// everywhere.
    pub fn iaru_default_edges_in(self, region: Region) -> Option<(f64, f64)> {
        let by_region = |r1: (f64, f64), r2: (f64, f64), r3: (f64, f64)| match region {
            Region::R1 => Some(r1),
            Region::R2 => Some(r2),
            Region::R3 => Some(r3),
        };
        match self {
            Band::M160 => by_region(
                (1_810_000.0, 2_000_000.0),
                (1_800_000.0, 2_000_000.0),
                (1_800_000.0, 2_000_000.0),
            ),
            Band::M80 => by_region(
                (3_500_000.0, 3_800_000.0),
                (3_500_000.0, 4_000_000.0),
                (3_500_000.0, 3_900_000.0),
            ),
            // The WRC-15 secondary allocation, identical in all three regions.
            // Region 2's channelised 60 m (five 2.8 kHz channels between 5.332
            // and 5.405) is a US national arrangement inside a different slice
            // of spectrum, not a regional allocation, so it is not modelled
            // here — a licence that grants those channels grants them outside
            // this band whichever region it was issued in.
            Band::M60 => Some((5_351_500.0, 5_366_500.0)),
            Band::M40 => by_region(
                (7_000_000.0, 7_200_000.0),
                (7_000_000.0, 7_300_000.0),
                (7_000_000.0, 7_200_000.0),
            ),
            Band::M30 => Some((10_100_000.0, 10_150_000.0)),
            Band::M20 => Some((14_000_000.0, 14_350_000.0)),
            Band::M17 => Some((18_068_000.0, 18_168_000.0)),
            Band::M15 => Some((21_000_000.0, 21_450_000.0)),
            Band::M12 => Some((24_890_000.0, 24_990_000.0)),
            Band::M10 => Some((28_000_000.0, 29_700_000.0)),
            Band::M6 => by_region(
                (50_000_000.0, 52_000_000.0),
                (50_000_000.0, 54_000_000.0),
                (50_000_000.0, 54_000_000.0),
            ),
            Band::M2 => by_region(
                (144_000_000.0, 146_000_000.0),
                (144_000_000.0, 148_000_000.0),
                (144_000_000.0, 148_000_000.0),
            ),
            Band::M70 => by_region(
                (430_000_000.0, 440_000_000.0),
                (420_000_000.0, 450_000_000.0),
                (430_000_000.0, 450_000_000.0),
            ),
            Band::Gen => None,
        }
    }

    /// The band containing `hz` in the station's configured region, or `Gen` if
    /// none does.
    pub fn containing(hz: f64) -> Band {
        Band::containing_in(hz, crate::region())
    }

    /// The band containing `hz` in `region`, or `Gen` if none does.
    pub fn containing_in(hz: f64, region: Region) -> Band {
        crate::band_plan().region(region).containing(hz)
    }

    /// A reasonable default frequency/mode when jumping to a band with no stack
    /// history.
    ///
    /// One set for every region: each of these sits inside the band in all
    /// three, and inside a part of it the mode belongs in — which is the most
    /// a starting point has to do, since the band stack replaces it the moment
    /// the operator tunes.
    pub fn default_entry(self) -> (f64, crate::Mode) {
        use crate::Mode;
        match self {
            Band::M160 => (1_840_000.0, Mode::Lsb),
            Band::M80 => (3_700_000.0, Mode::Lsb),
            Band::M60 => (5_357_000.0, Mode::Usb),
            Band::M40 => (7_100_000.0, Mode::Lsb),
            Band::M30 => (10_120_000.0, Mode::Cw),
            Band::M20 => (14_200_000.0, Mode::Usb),
            Band::M17 => (18_120_000.0, Mode::Usb),
            Band::M15 => (21_250_000.0, Mode::Usb),
            Band::M12 => (24_940_000.0, Mode::Usb),
            Band::M10 => (28_400_000.0, Mode::Usb),
            Band::M6 => (50_150_000.0, Mode::Usb),
            Band::M2 => (145_500_000.0, Mode::Nfm),
            // 70 cm opens on the RIFP calling frequency: it is the band this
            // mode is meant for, and the band stack overrides this the moment
            // the operator tunes anywhere else.
            Band::M70 => (crate::RIFP_CALLING_HZ, Mode::Rifp),
            Band::Gen => (7_200_000.0, Mode::Am),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Region 1 is the default, and it must still be exactly the band table
    /// sdroxide shipped before regions existed — an installation that never
    /// opens the setting may not find its bands moved underneath it.
    #[test]
    fn region_1_is_unchanged() {
        for (band, edges) in [
            (Band::M160, (1_810_000.0, 2_000_000.0)),
            (Band::M80, (3_500_000.0, 3_800_000.0)),
            (Band::M60, (5_351_500.0, 5_366_500.0)),
            (Band::M40, (7_000_000.0, 7_200_000.0)),
            (Band::M30, (10_100_000.0, 10_150_000.0)),
            (Band::M20, (14_000_000.0, 14_350_000.0)),
            (Band::M17, (18_068_000.0, 18_168_000.0)),
            (Band::M15, (21_000_000.0, 21_450_000.0)),
            (Band::M12, (24_890_000.0, 24_990_000.0)),
            (Band::M10, (28_000_000.0, 29_700_000.0)),
            (Band::M6, (50_000_000.0, 52_000_000.0)),
            (Band::M2, (144_000_000.0, 146_000_000.0)),
            (Band::M70, (430_000_000.0, 440_000_000.0)),
        ] {
            assert_eq!(band.edges_in(Region::R1), Some(edges), "{band:?}");
        }
        assert_eq!(Band::Gen.edges_in(Region::R1), None);
    }

    /// The frequencies that are in one region's band and outside another's.
    /// This is what the setting is *for*, so it is asserted rather than left to
    /// the edge table to imply.
    #[test]
    fn the_regional_differences_are_the_ones_that_matter() {
        // The operator's own example: 446 MHz is 70 cm in the Americas and out
        // of band across most of Europe.
        assert_eq!(Band::containing_in(446_000_000.0, Region::R1), Band::Gen);
        assert_eq!(Band::containing_in(446_000_000.0, Region::R2), Band::M70);
        assert_eq!(Band::containing_in(446_000_000.0, Region::R3), Band::M70);
        // 70 cm's lower edge: Region 2 alone starts at 420.
        assert_eq!(Band::containing_in(425_000_000.0, Region::R1), Band::Gen);
        assert_eq!(Band::containing_in(425_000_000.0, Region::R2), Band::M70);
        assert_eq!(Band::containing_in(425_000_000.0, Region::R3), Band::Gen);
        // 40 m above 7.200 is Region 2's alone.
        assert_eq!(Band::containing_in(7_250_000.0, Region::R1), Band::Gen);
        assert_eq!(Band::containing_in(7_250_000.0, Region::R2), Band::M40);
        assert_eq!(Band::containing_in(7_250_000.0, Region::R3), Band::Gen);
        // 80 m: Region 2 to 4.000, Region 3 to 3.900, Region 1 to 3.800.
        assert_eq!(Band::containing_in(3_850_000.0, Region::R1), Band::Gen);
        assert_eq!(Band::containing_in(3_850_000.0, Region::R2), Band::M80);
        assert_eq!(Band::containing_in(3_850_000.0, Region::R3), Band::M80);
        assert_eq!(Band::containing_in(3_950_000.0, Region::R3), Band::Gen);
        assert_eq!(Band::containing_in(3_950_000.0, Region::R2), Band::M80);
        // 160 m's lower edge.
        assert_eq!(Band::containing_in(1_805_000.0, Region::R1), Band::Gen);
        assert_eq!(Band::containing_in(1_805_000.0, Region::R2), Band::M160);
        // 6 m and 2 m are 2 MHz wider outside Region 1.
        assert_eq!(Band::containing_in(53_000_000.0, Region::R1), Band::Gen);
        assert_eq!(Band::containing_in(53_000_000.0, Region::R2), Band::M6);
        assert_eq!(Band::containing_in(147_000_000.0, Region::R1), Band::Gen);
        assert_eq!(Band::containing_in(147_000_000.0, Region::R3), Band::M2);
    }

    /// Every band's edges have to be the right way round and disjoint from
    /// every other band's, in every region — `containing` returns the first
    /// match, so an overlap would silently hide a band.
    #[test]
    fn edges_are_ordered_and_disjoint_in_every_region() {
        for region in Region::ALL {
            let mut spans: Vec<(Band, (f64, f64))> =
                Band::ALL.iter().filter_map(|&b| b.edges_in(region).map(|e| (b, e))).collect();
            for (b, (lo, hi)) in &spans {
                assert!(lo < hi, "{b:?} in {region:?}: {lo} >= {hi}");
            }
            spans.sort_by(|a, b| a.1.0.total_cmp(&b.1.0));
            for w in spans.windows(2) {
                assert!(w[0].1.1 < w[1].1.0, "{:?} and {:?} overlap in {region:?}", w[0].0, w[1].0);
            }
        }
    }

    /// A starting frequency outside its own band would drop the operator into
    /// general coverage — and, with `tx_ham_only` set, into a transmit lockout
    /// — the moment they pressed a band button.
    #[test]
    fn every_default_entry_is_inside_its_band_in_every_region() {
        for region in Region::ALL {
            for band in Band::ALL {
                let Some((lo, hi)) = band.edges_in(region) else { continue };
                let (hz, _) = band.default_entry();
                assert!(
                    (lo..=hi).contains(&hz),
                    "{band:?} opens on {hz} Hz, outside {lo}..{hi} in {region:?}"
                );
            }
        }
    }
}
