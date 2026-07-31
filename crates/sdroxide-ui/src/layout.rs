//! Which layout the window wears, and the handful of metrics that follow from
//! it: how big a chip has to be to hit with a finger, how far from a filter
//! edge counts as grabbing it, how large the frequency digits may grow.
//!
//! The UI is the same immediate-mode code natively and in the browser, and
//! eframe pins the web zoom factor to 1.0 — so one egui point is one CSS pixel
//! and every hardcoded size in this crate is literally a pixel on a phone. The
//! control strip is eight boxes that reserve a fixed width before they draw
//! (see [`crate::chrome::module`]), so on a narrow screen they wrap to a row
//! each and the widest still overflow. That is what the tiers exist to fix.

use eframe::egui;
use sdroxide_types::LayoutMode;

/// The layout in force: how much room there is to lay controls out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// The full eight-module control strip.
    Desktop,
    /// Frequency readout and S-meter at full size, everything else in menus.
    Tablet,
    /// Compact readout and meter, menus, and the waterfall alone below.
    Phone,
}

impl Tier {
    /// Menus instead of the full module strip.
    pub fn compact(self) -> bool {
        self != Tier::Desktop
    }

    /// Finger-sized targets, no hover to rely on, no popups that fade by
    /// themselves. True for tablets as well as phones: both are touched.
    pub fn touch(self) -> bool {
        self != Tier::Desktop
    }

    /// The waterfall and nothing else — no spectrum line, no full-band strip.
    /// A spectrum trace in a 360 pt-wide window is a strip too thin to read
    /// that costs the waterfall a third of its height.
    pub fn waterfall_only(self) -> bool {
        self == Tier::Phone
    }

    /// How far from a filter edge or a marker still counts as grabbing it.
    /// A mouse lands where it is pointed; a fingertip covers about 9 mm.
    pub fn grab_px(self) -> f32 {
        match self {
            Tier::Desktop => 6.0,
            Tier::Tablet => 11.0,
            Tier::Phone => 13.0,
        }
    }

    /// Largest frequency-readout digit this tier will use. The readout is sized
    /// to the space it is given (see `freq_module`); this only caps it, so a
    /// phone in landscape doesn't spend its width on 40 pt digits.
    pub fn digit_cap(self) -> f32 {
        match self {
            Tier::Desktop | Tier::Tablet => crate::widgets::freq_display::DIGIT_SIZE,
            Tier::Phone => 30.0,
        }
    }
}

/// The tier a viewport of `size` earns, with the operator's override folded in.
///
/// Phone below 600 wide because the widest single control box is TX at 520 and
/// the frequency readout alone measures ~512: under 600 something is clipped
/// however the strip wraps. Phone below 440 *tall* as well, for a phone in
/// landscape — 852×393 or 932×430 is wide enough for a desktop row, but a
/// 150 pt stack of control strip would take 40% of the height. No tablet lands
/// there: every one of them is 768 tall or more in landscape.
///
/// Tablet up to 1100 wide because 768 portrait leaves 732 pt of content width
/// (the top panel's 8+8 margin plus `angled_frame`'s 10+10), while the desktop
/// readout and the S-meter together want 770.
pub fn tier_for(size: egui::Vec2, mode: LayoutMode) -> Tier {
    match mode {
        LayoutMode::Desktop => Tier::Desktop,
        LayoutMode::Tablet => Tier::Tablet,
        LayoutMode::Phone => Tier::Phone,
        LayoutMode::Auto => {
            let (w, h) = (size.x, size.y);
            if w < 600.0 || h < 440.0 {
                Tier::Phone
            } else if w < 1400.0 || h < 620.0 {
                Tier::Tablet
            } else {
                Tier::Desktop
            }
        }
    }
}

/// Where the tier lives between [`set_tier`] and [`tier`].
fn tier_id() -> egui::Id {
    egui::Id::new("sdroxide-tier")
}

/// Publish the tier for this frame. Called once, at the top of the app's
/// `update`, because the operator's override lives in `UiSettings` — which the
/// context cannot see and the widgets that need the tier cannot reach.
pub fn set_tier(ctx: &egui::Context, tier: Tier) {
    ctx.data_mut(|d| d.insert_temp(tier_id(), tier));
}

/// The tier in force this frame.
///
/// Read from context memory rather than passed as an argument: the widgets that
/// need it are as deep as [`crate::widgets::spectrum_view::show_ext`], which
/// already takes nineteen parameters, and every one of them already holds a
/// `Context`. The fallback covers the frame before the first [`set_tier`] and
/// the solar view, which has no settings of its own.
pub fn tier(ctx: &egui::Context) -> Tier {
    ctx.data(|d| d.get_temp(tier_id()))
        .unwrap_or_else(|| tier_for(ctx.content_rect().size(), LayoutMode::Auto))
}

/// A window width that fits the viewport, with a margin so the cut-corner
/// border stays visible. egui persists window sizes, so without this a keyer
/// opened at 600 pt on a desktop stays 600 pt wide on the phone that later
/// loads the same storage.
pub fn window_w(ctx: &egui::Context, want: f32) -> f32 {
    want.min(ctx.content_rect().width() - 16.0).max(160.0)
}

/// A window height that fits the viewport. See [`window_w`].
pub fn window_h(ctx: &egui::Context, want: f32) -> f32 {
    want.min(ctx.content_rect().height() - 16.0).max(120.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;

    #[test]
    fn viewports_land_in_the_tier_they_were_measured_for() {
        let auto = LayoutMode::Auto;
        // Phones, portrait and landscape.
        assert_eq!(tier_for(vec2(360.0, 800.0), auto), Tier::Phone);
        assert_eq!(tier_for(vec2(393.0, 852.0), auto), Tier::Phone);
        assert_eq!(tier_for(vec2(412.0, 915.0), auto), Tier::Phone);
        assert_eq!(tier_for(vec2(852.0, 393.0), auto), Tier::Phone, "landscape phone is short");
        assert_eq!(tier_for(vec2(932.0, 430.0), auto), Tier::Phone);
        // Tablets, both ways up.
        assert_eq!(tier_for(vec2(768.0, 1024.0), auto), Tier::Tablet);
        assert_eq!(tier_for(vec2(1024.0, 768.0), auto), Tier::Tablet);
        assert_eq!(tier_for(vec2(820.0, 1180.0), auto), Tier::Tablet);
        // Desktops.
        assert_eq!(tier_for(vec2(1280.0, 800.0), auto), Tier::Desktop);
        assert_eq!(tier_for(vec2(1920.0, 1080.0), auto), Tier::Desktop);
    }

    #[test]
    fn the_override_wins_over_the_viewport() {
        let big = vec2(1920.0, 1080.0);
        assert_eq!(tier_for(big, LayoutMode::Phone), Tier::Phone);
        assert_eq!(tier_for(big, LayoutMode::Tablet), Tier::Tablet);
        let small = vec2(360.0, 800.0);
        assert_eq!(tier_for(small, LayoutMode::Desktop), Tier::Desktop);
    }
}
