//! Which decoded stations are currently "up", and how brightly.
//!
//! Two views draw this: the flat FT8 panel map and the 3D globe. They have to
//! agree — a station visible on one and absent from the other reads as a bug —
//! so the rule lives here once rather than in each of them.
//!
//! Ages use egui's frame time, which is monotonic and works on both targets;
//! `slot_utc` only decides whether a decode is *newer* than the one already
//! recorded for that grid.

use std::collections::HashMap;

use sdroxide_types::Decode;

use crate::solar3d::DigiTraffic;

/// A decoded station's dot fades over this many seconds since it was last
/// heard, then expires.
pub const STATION_FADE_S: f64 = 120.0;

/// Grid square → (newest slot seen, frame time it was seen at).
#[derive(Default)]
pub struct DigiStations {
    seen: HashMap<String, (i64, f64)>,
}

impl DigiStations {
    /// Fold in this frame's decode list and drop anything that has expired.
    ///
    /// Idempotent within a slot: re-observing the same decodes does not refresh
    /// a dot, because only a *newer* `slot_utc` counts as hearing the station
    /// again. That is what lets both the panel map and the globe call this with
    /// whatever list they happen to be holding.
    pub fn observe(&mut self, decodes: &[Decode], now_t: f64) {
        for d in decodes {
            let Some(grid) = d.grid.as_deref() else { continue };
            let e = self.seen.entry(grid.to_string()).or_insert((i64::MIN, now_t));
            if d.slot_utc > e.0 {
                *e = (d.slot_utc, now_t); // refreshed → dot returns to full brightness
            }
        }
        self.seen.retain(|_, &mut (_, seen)| now_t - seen < STATION_FADE_S);
    }

    /// Located stations with their 1.0 → 0.0 fade.
    pub fn stations(&self, now_t: f64) -> Vec<(f64, f64, f32)> {
        self.seen
            .iter()
            .filter_map(|(grid, &(_, seen))| {
                let (lat, lon) = sdroxide_types::grid_to_latlon(grid)?;
                let alpha = (1.0 - (now_t - seen) / STATION_FADE_S).clamp(0.0, 1.0) as f32;
                (alpha > 0.0).then_some((lat, lon, alpha))
            })
            .collect()
    }

    /// The globe's view of the same set, plus the QSO in progress.
    pub fn traffic(
        &self,
        now_t: f64,
        dx_grid: Option<&str>,
        preview: Option<(f64, f64)>,
        transmitting: bool,
    ) -> DigiTraffic {
        DigiTraffic {
            stations: self.stations(now_t),
            dx: dx_grid.and_then(sdroxide_types::grid_to_latlon),
            preview,
            transmitting,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(grid: &str, slot: i64) -> Decode {
        Decode {
            slot_utc: slot,
            snr_db: -10,
            dt: 0.2,
            audio_hz: 1000.0,
            message: format!("CQ TEST {grid}"),
            to: None,
            from: Some("AB1CD".into()),
            grid: Some(grid.into()),
            is_cq: true,
            cq_to: None,
            free_text: false,
            rr73_to: None,
        }
    }

    #[test]
    fn a_station_fades_out_over_two_minutes_and_then_expires() {
        let mut s = DigiStations::default();
        s.observe(&[decode("FN42", 100)], 0.0);
        assert_eq!(s.stations(0.0)[0].2, 1.0, "a fresh decode is not at full brightness");

        let half = s.stations(STATION_FADE_S / 2.0);
        assert!((half[0].2 - 0.5).abs() < 1e-6, "half-way fade was {}", half[0].2);

        // Past the window it is gone entirely, not merely transparent: it must
        // also drop out of the flat map's zoom fit.
        s.observe(&[], STATION_FADE_S + 1.0);
        assert!(s.stations(STATION_FADE_S + 1.0).is_empty());
    }

    #[test]
    fn hearing_a_station_again_restores_full_brightness() {
        let mut s = DigiStations::default();
        s.observe(&[decode("FN42", 100)], 0.0);
        s.observe(&[decode("FN42", 115)], 60.0);
        assert_eq!(s.stations(60.0)[0].2, 1.0);
    }

    /// The panel map and the globe both call `observe` with whatever decode
    /// list they hold, which is usually the same one twice. Re-observing must
    /// not keep a station alive forever.
    #[test]
    fn re_observing_the_same_slot_does_not_refresh_the_fade() {
        let mut s = DigiStations::default();
        let d = [decode("FN42", 100)];
        s.observe(&d, 0.0);
        s.observe(&d, 60.0);
        let f = s.stations(60.0)[0].2;
        assert!((f - 0.5).abs() < 1e-6, "re-observing reset the fade to {f}");
    }

    #[test]
    fn a_decode_without_a_grid_places_nothing() {
        let mut s = DigiStations::default();
        let mut d = decode("FN42", 100);
        d.grid = None;
        s.observe(&[d], 0.0);
        assert!(s.stations(0.0).is_empty());

        // Nor does a grid that does not decode to a position.
        s.observe(&[decode("ZZ99zz", 100)], 0.0);
        assert!(s.stations(0.0).is_empty());
    }

    #[test]
    fn the_dx_station_comes_from_its_grid_not_from_the_decode_list() {
        let mut s = DigiStations::default();
        s.observe(&[decode("FN42", 100)], 0.0);
        let t = s.traffic(0.0, Some("JN88"), None, true);
        assert_eq!(t.stations.len(), 1);
        assert!(t.transmitting);
        let (lat, lon) = t.dx.expect("JN88 is a valid grid");
        assert!(lat > 40.0 && lat < 50.0 && lon > 10.0 && lon < 20.0, "JN88 at {lat},{lon}");
    }
}
