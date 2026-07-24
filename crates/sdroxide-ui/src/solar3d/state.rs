//! Everything the solar-system window's deferred viewport callback touches.
//!
//! `show_viewport_deferred` requires an `Fn + Send + Sync + 'static` closure, so
//! the window cannot borrow `SdroxideApp`. All of its mutable state lives here
//! behind an `Arc<Mutex<_>>` that both the root pass and the child pass hold.

use std::sync::{Arc, Mutex};

use sdroxide_solar::SolarData;

use crate::view::Solar3dView;

/// Which body the orbit camera pivots around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sun,
    Earth,
    Moon,
    /// Midpoint of the Earth–Moon line, for framing the pair.
    EarthMoon,
}

impl Focus {
    pub const ALL: [Focus; 4] = [Focus::Sun, Focus::Earth, Focus::Moon, Focus::EarthMoon];

    pub fn from_u8(v: u8) -> Focus {
        *Focus::ALL.get(v as usize).unwrap_or(&Focus::Sun)
    }

    pub fn to_u8(self) -> u8 {
        Focus::ALL.iter().position(|f| *f == self).unwrap_or(0) as u8
    }

    pub fn label(self) -> &'static str {
        match self {
            Focus::Sun => "SUN",
            Focus::Earth => "EARTH",
            Focus::Moon => "MOON",
            Focus::EarthMoon => "E+M",
        }
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
    /// Animated camera tour state, and the frame time it last advanced at.
    pub tour: super::camera::Tour,
    pub last_frame_time: f64,
}

impl SolarUi {
    pub fn new(view: Solar3dView) -> Self {
        SolarUi {
            view,
            close_requested: false,
            refresh_requested: false,
            qth_grid: String::new(),
            qth: None,
            sim_offset_s: 0.0,
            data: None,
            tour: super::camera::Tour::default(),
            last_frame_time: 0.0,
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

    pub fn layer(&self, bit: u32) -> bool {
        self.view.layers & bit != 0
    }

    pub fn toggle_layer(&mut self, bit: u32) {
        self.view.layers ^= bit;
    }
}
