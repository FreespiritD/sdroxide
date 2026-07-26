//! Everything the solar-system window's deferred viewport callback touches.
//!
//! `show_viewport_deferred` requires an `Fn + Send + Sync + 'static` closure, so
//! the window cannot borrow `SdroxideApp`. All of its mutable state lives here
//! behind an `Arc<Mutex<_>>` that both the root pass and the child pass hold.

use std::sync::{Arc, Mutex};

use sdroxide_solar::SolarData;

use crate::view::Solar3dView;

/// Which body the orbit camera pivots around.
///
/// Persisted as a `u8` in [`Solar3dView::focus`], so the encoding in
/// [`Focus::to_u8`] is a stable format: the four original values keep indices
/// 0–3 and everything new is appended after them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sun,
    Earth,
    Moon,
    /// Midpoint of the Earth–Moon line, for framing the pair.
    EarthMoon,
    Planet(sdroxide_solar::Planet),
    /// A moon of another planet, by index into [`sdroxide_solar::planets::MOONS`].
    Satellite(usize),
}

impl Focus {
    /// The four targets that are not a table lookup.
    pub const NEAR: [Focus; 4] = [Focus::Sun, Focus::Earth, Focus::Moon, Focus::EarthMoon];

    /// Every target, grouped the way the picker lays them out: the Sun and the
    /// Earth–Moon system first, then a row per planet with its own moons.
    pub fn groups() -> Vec<(&'static str, Vec<Focus>)> {
        let mut v = vec![("HOME", Focus::NEAR.to_vec())];
        for p in sdroxide_solar::Planet::ALL {
            let mut row = vec![Focus::Planet(p)];
            row.extend(
                sdroxide_solar::planets::MOONS
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.parent == p)
                    .map(|(i, _)| Focus::Satellite(i)),
            );
            v.push((p.name(), row));
        }
        v
    }

    /// Every target, flattened. Used by the tests that guard the persisted
    /// encoding; the picker itself wants them grouped.
    #[cfg(test)]
    pub fn all() -> Vec<Focus> {
        Focus::groups().into_iter().flat_map(|(_, row)| row).collect()
    }

    pub fn from_u8(v: u8) -> Focus {
        let v = v as usize;
        if let Some(f) = Focus::NEAR.get(v) {
            return *f;
        }
        let k = v - Focus::NEAR.len();
        match sdroxide_solar::Planet::ALL.get(k) {
            Some(p) => Focus::Planet(*p),
            None => {
                let m = k - sdroxide_solar::Planet::ALL.len();
                // An index from a future version with more moons: fall back to
                // the Sun rather than to a body that is not the one meant.
                if m < sdroxide_solar::planets::MOONS.len() {
                    Focus::Satellite(m)
                } else {
                    Focus::Sun
                }
            }
        }
    }

    pub fn to_u8(self) -> u8 {
        let base = Focus::NEAR.len();
        let planets = sdroxide_solar::Planet::ALL.len();
        (match self {
            Focus::Planet(p) => base + p.index(),
            Focus::Satellite(i) => base + planets + i,
            f => Focus::NEAR.iter().position(|x| *x == f).unwrap_or(0),
        }) as u8
    }

    /// Full name, as the picker shows it.
    pub fn label(self) -> &'static str {
        match self {
            Focus::Sun => "Sun",
            Focus::Earth => "Earth",
            Focus::Moon => "Moon",
            Focus::EarthMoon => "Earth + Moon",
            Focus::Planet(p) => p.name(),
            Focus::Satellite(i) => {
                sdroxide_solar::planets::MOONS.get(i).map_or("Sun", |m| m.name)
            }
        }
    }

    /// Short upper-case form for the button face.
    pub fn short(self) -> String {
        match self {
            Focus::EarthMoon => "E+M".to_string(),
            f => f.label().to_uppercase(),
        }
    }

    /// True for a body that orbits another body this view also draws — the
    /// picker indents those under their planet.
    pub fn is_satellite(self) -> bool {
        matches!(self, Focus::Moon | Focus::Satellite(_))
    }
}

/// Shared window state. Never hold the lock across I/O or across a call into
/// egui that could re-enter the viewport callback.
pub struct SolarUi {
    /// Persisted camera / layer / scale settings, mirrored back into
    /// `ViewState` by the root pass each frame.
    pub view: Solar3dView,
    /// Set by the child pass when the OS window's close button is hit; drained
    /// by the root pass, which then stops emitting the viewport.
    pub close_requested: bool,
    /// Set by the overlay's ↻ button; drained by the root pass, which owns the
    /// feed handle the child pass cannot reach.
    pub refresh_requested: bool,
    /// Operator QTH as configured (Maidenhead) and its decoded (lat, lon).
    pub qth_grid: String,
    pub qth: Option<(f64, f64)>,
    /// Simulated-time offset from now, in seconds — driven by the time chips so
    /// the whole scene can be scrubbed forward and back.
    pub sim_offset_s: f64,
    /// The background feed's snapshot, once the feed has been started. A second
    /// handle on the feed's own mutex, because this closure outlives any borrow
    /// of the feed itself.
    ///
    /// Lock ordering is always `SolarUi` then `SolarData`; the worker thread
    /// only ever takes `SolarData`, so there is no cycle.
    pub data: Option<Arc<Mutex<SolarData>>>,
    /// FT8/FT4 traffic to plot on the globe.
    pub digi: DigiTraffic,
    /// Satellite whose pass table is open, by catalogue number.
    pub selected_sat: Option<u64>,
    /// Cached pass prediction: which satellite, from what QTH, computed when,
    /// and the result. Stepping a whole orbit at second resolution is far too
    /// expensive to redo every frame.
    pub sat_passes: Option<SatPasses>,
    /// Pivot supplied by the AUTO tour while it is flying between stations:
    /// position and the radius the distance clamp uses. Frame-scoped — cleared
    /// whenever the tour is not driving.
    pub focus_override: Option<(super::math::V3, f32)>,
    /// Animated camera tour state, and the frame time it last advanced at.
    pub tour: super::camera::Tour,
    pub last_frame_time: f64,
    /// Set when the target changes, so the next frame — which has the bodies
    /// placed already — can pull the camera in to frame whatever was picked.
    pub retarget: bool,
}

/// A cached pass prediction for one satellite.
pub struct SatPasses {
    pub norad_id: u64,
    pub name: String,
    /// The QTH it was computed for, so moving the QTH invalidates it.
    pub qth: (f64, f64),
    /// Wall clock when it was computed, so it can be refreshed as it ages.
    pub computed_unix: f64,
    pub result: sdroxide_solar::PassSearch,
}

/// FT8/FT4 activity, republished into the window each frame by the root pass.
///
/// The decode list lives in `SdroxideApp`, which this window cannot borrow, so
/// the positions are copied across rather than shared.
#[derive(Clone, Default)]
pub struct DigiTraffic {
    /// Decoded stations: latitude, longitude, and 1.0→0.0 as the decode ages out.
    pub stations: Vec<(f64, f64, f32)>,
    /// The station currently being worked.
    pub dx: Option<(f64, f64)>,
    /// A decode the operator has clicked but not yet answered.
    pub preview: Option<(f64, f64)>,
    /// True while transmitting, which animates the arc.
    pub transmitting: bool,
}

impl SolarUi {
    pub fn new(mut view: Solar3dView) -> Self {
        // A layer mask persisted before a layer existed would leave that layer
        // off for anyone upgrading, which reads as the feature being broken. Any
        // mask that was "everything" at the time becomes "everything" now.
        if crate::view::solar_layer::PREVIOUS_ALL.contains(&view.layers) {
            view.layers = crate::view::solar_layer::ALL;
        }
        SolarUi {
            view,
            close_requested: false,
            refresh_requested: false,
            qth_grid: String::new(),
            qth: None,
            sim_offset_s: 0.0,
            data: None,
            digi: DigiTraffic::default(),
            selected_sat: None,
            sat_passes: None,
            focus_override: None,
            tour: super::camera::Tour::default(),
            last_frame_time: 0.0,
            retarget: false,
        }
    }

    /// Adopt the operator's grid square, re-decoding only when it changes.
    pub fn set_qth(&mut self, grid: &str) {
        if self.qth_grid == grid {
            return;
        }
        self.qth_grid = grid.to_string();
        self.qth = sdroxide_types::grid_to_latlon(grid);
    }

    pub fn focus(&self) -> Focus {
        Focus::from_u8(self.view.focus)
    }

    /// Point the camera at a body, from the picker or from a click in the view.
    ///
    /// Cancels the tour — the tour drives the target itself, so a user choosing
    /// one has to win — and asks the next frame to close the distance.
    pub fn set_focus(&mut self, f: Focus) {
        if self.focus() != f {
            self.retarget = true;
        }
        self.view.focus = f.to_u8();
        self.view.auto = false;
    }

    pub fn layer(&self, bit: u32) -> bool {
        self.view.layers & bit != 0
    }

    pub fn toggle_layer(&mut self, bit: u32) {
        self.view.layers ^= bit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The camera target is persisted as a single byte, so its encoding is a
    /// file format: every target has to survive the round trip, and the four
    /// that existed before the planets were added have to keep their original
    /// values or an upgrade would silently move everyone's camera.
    #[test]
    fn every_target_round_trips_through_its_persisted_byte() {
        for f in Focus::all() {
            assert_eq!(Focus::from_u8(f.to_u8()), f, "{f:?} did not survive");
        }
        for (i, f) in Focus::NEAR.iter().enumerate() {
            assert_eq!(f.to_u8() as usize, i, "{f:?} moved off its historical index");
        }
        // Distinct bytes, or two bodies would share a slot.
        let mut seen: Vec<u8> = Focus::all().iter().map(|f| f.to_u8()).collect();
        let count = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), count, "two targets encode to the same byte");
    }

    /// A stored byte from a *newer* build, or from a corrupt file, must land on
    /// something rather than on a body that is not the one meant.
    #[test]
    fn an_unknown_target_byte_falls_back_to_the_sun() {
        assert_eq!(Focus::from_u8(u8::MAX), Focus::Sun);
        assert_eq!(Focus::from_u8(Focus::all().len() as u8), Focus::Sun);
    }

    #[test]
    fn the_picker_lists_every_body_with_its_planet() {
        let all = Focus::all();
        // The four near targets, seven planets, and every moon in the table.
        assert_eq!(all.len(), 4 + 7 + sdroxide_solar::planets::MOONS.len());
        // Each planet is immediately followed by its own moons.
        let jupiter = all.iter().position(|f| *f == Focus::Planet(sdroxide_solar::Planet::Jupiter));
        let after = &all[jupiter.expect("Jupiter is in the list") + 1..][..4];
        for f in after {
            let Focus::Satellite(i) = f else { panic!("{f:?} is not a moon") };
            assert_eq!(sdroxide_solar::planets::MOONS[*i].parent, sdroxide_solar::Planet::Jupiter);
        }
        assert_eq!(after[0].label(), "Io");
    }

    #[test]
    fn choosing_a_target_stops_the_tour_and_asks_for_a_reframe() {
        let mut st = SolarUi::new(Solar3dView::default());
        st.view.auto = true;
        st.set_focus(Focus::Planet(sdroxide_solar::Planet::Saturn));
        assert!(!st.view.auto, "the tour kept driving after the user picked a target");
        assert!(st.retarget);
        assert_eq!(st.focus(), Focus::Planet(sdroxide_solar::Planet::Saturn));

        // Picking the body that is already the target is not a reframe: it
        // would yank the camera back out of a close-up the user zoomed into.
        st.retarget = false;
        st.set_focus(Focus::Planet(sdroxide_solar::Planet::Saturn));
        assert!(!st.retarget);
    }
}
