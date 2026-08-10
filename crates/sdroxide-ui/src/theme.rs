//! The sdroxide look: dark panels, cut corners, Chakra Petch type (SIL OFL —
//! see assets/fonts/OFL.txt) — in one of a handful of colour themes, the
//! classic navy/cyan/hot-pink one by default.
//!
//! The palette used to be a set of `pub const Color32`s; when the theme became
//! switchable at runtime they turned into same-named accessor *functions*
//! reading the current [`Palette`] — which is why the call sites still say
//! `theme::CYAN()`. The current theme and chrome-style selections live in
//! process-wide atomics rather than the egui context because the native
//! solar-3d viewport renders deferred, possibly on another thread, and must
//! see the same look.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};
use sdroxide_types::{ChromeStyle, FontSize, UiTheme};

/// Every colour role the chrome wears. A theme is one full assignment of
/// these; the field names keep the historic constant names (`cyan` is "the
/// primary accent", whatever hue a theme gives it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub bg_deep: Color32,
    pub panel: Color32,
    pub input_bg: Color32,
    pub fill: Color32,
    pub fill_hover: Color32,
    pub fill_active: Color32,
    pub line: Color32,
    pub line_lit: Color32,
    pub text: Color32,
    pub text_strong: Color32,
    /// Primary accent: selection fills, headings, links.
    pub cyan: Color32,
    /// Secondary accent: captions, dimmed highlights.
    pub cyan_dim: Color32,
    /// Chrome accent: window borders, decorative strokes.
    pub pink: Color32,
    pub yellow: Color32,
    pub green: Color32,
    /// Dark ink used on top of accent fills.
    pub ink_on_cyan: Color32,
    /// Red-accent chrome (cyberpunk box borders / list rows).
    pub red_deep: Color32,
    pub cq_bg: Color32,
    /// Background for a decode addressed to our own station (warm gold,
    /// stands out).
    pub tome_bg: Color32,
    /// Background for the QSO-complete line in a transcript whose received
    /// messages are already green — the band is what makes it read as an
    /// event, not a message.
    pub done_bg: Color32,
    pub row_bg: Color32,
    pub row_hover: Color32,
    /// Scrollbars: an accent handle riding in a recessed gutter. The gutter is
    /// a touch lighter than every panel/list background it sits on, so the
    /// full scroll range stays readable even when the handle is at the far
    /// end.
    pub scroll_track: Color32,
    pub scroll_handle: Color32,
    pub scroll_handle_hover: Color32,
    pub scroll_handle_drag: Color32,
    /// egui's `faint_bg_color` (striped table rows and the like).
    pub faint_bg: Color32,
    /// TX / SWR / error indications. Red in *every* theme — the phosphor
    /// themes are monochrome everywhere else, but whether RF is leaving the
    /// antenna is never left to a shade of green.
    pub alert: Color32,
}

/// `c(0x00d0f4)` — a palette entry from one hex triple, so a theme reads as a
/// column of colour values.
const fn c(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// The classic look: dark navy panels, cyan accents, hot-pink strokes. Must
/// stay bit-exact to the historic constants — it is the default every
/// screenshot in the manual shows.
const DEFAULT: Palette = Palette {
    bg_deep: c(0x050810),
    panel: c(0x0b111e),
    input_bg: c(0x04070e),
    fill: c(0x101a2c),
    fill_hover: c(0x17243c),
    fill_active: c(0x1d2f4d),
    line: c(0x1a2740),
    line_lit: c(0x2a4a66),
    text: c(0xb4c6da),
    text_strong: c(0xe8f4ff),
    cyan: c(0x00d0f4),
    cyan_dim: c(0x1d9cbe),
    pink: c(0xff2a55),
    yellow: c(0xffd23f),
    green: c(0x46e07d),
    ink_on_cyan: c(0x021019),
    red_deep: c(0x6e182c),
    cq_bg: c(0x240c15),
    tome_bg: c(0x2c2406),
    done_bg: c(0x082a17),
    row_bg: c(0x0a101b),
    row_hover: c(0x141e2e),
    scroll_track: c(0x0e1626),
    scroll_handle: c(0x178ead),
    scroll_handle_hover: c(0x00d0f4), // = cyan
    scroll_handle_drag: c(0xff2a55),  // = pink
    faint_bg: c(0x0e1626),
    alert: c(0xff2a55), // = the historic pink, which doubled as the error colour
};

/// Green CRT phosphor: every role brightness-graded within one green family
/// on a near-black ground. `yellow` keeps its warning hue and `tome_bg` its
/// warm attention gold on purpose — they are the second alert tier.
const GREEN_PHOSPHOR: Palette = Palette {
    bg_deep: c(0x020703),
    panel: c(0x04120a),
    input_bg: c(0x010704),
    fill: c(0x062010),
    fill_hover: c(0x0a2c16),
    fill_active: c(0x0e3a1d),
    line: c(0x0d3018),
    line_lit: c(0x1a5c30),
    text: c(0x55c882),
    text_strong: c(0xccffdd),
    cyan: c(0x00ff66),
    cyan_dim: c(0x00b348),
    pink: c(0x00d957),
    yellow: c(0xffd23f),
    green: c(0x46e07d),
    ink_on_cyan: c(0x021203),
    red_deep: c(0x124a26),
    cq_bg: c(0x0c2412),
    tome_bg: c(0x2c2406),
    done_bg: c(0x082a17),
    row_bg: c(0x071408),
    row_hover: c(0x0e2415),
    scroll_track: c(0x08180d),
    scroll_handle: c(0x17ad5c),
    scroll_handle_hover: c(0x00ff66),
    scroll_handle_drag: c(0xb3ffcc),
    faint_bg: c(0x08180d),
    alert: c(0xff2a3c),
};

/// Amber CRT phosphor — the same grading as [`GREEN_PHOSPHOR`] in a warm
/// amber family.
const AMBER_PHOSPHOR: Palette = Palette {
    bg_deep: c(0x070402),
    panel: c(0x120c04),
    input_bg: c(0x070401),
    fill: c(0x201607),
    fill_hover: c(0x2c1f0a),
    fill_active: c(0x3a290e),
    line: c(0x30240d),
    line_lit: c(0x5c451a),
    text: c(0xc89e55),
    text_strong: c(0xffeacc),
    cyan: c(0xffb000),
    cyan_dim: c(0xb37b00),
    pink: c(0xd99500),
    yellow: c(0xffd23f),
    green: c(0xffc966),
    ink_on_cyan: c(0x120902),
    red_deep: c(0x4a3512),
    cq_bg: c(0x241a0c),
    tome_bg: c(0x2c2406),
    done_bg: c(0x2a2008),
    row_bg: c(0x140e05),
    row_hover: c(0x241a0e),
    scroll_track: c(0x181008),
    scroll_handle: c(0xad7c17),
    scroll_handle_hover: c(0xffb000),
    scroll_handle_drag: c(0xffdb99),
    faint_bg: c(0x181008),
    alert: c(0xff2a3c),
};

/// The default's structure with teal accents and orange chrome.
const TEAL_ORANGE: Palette = Palette {
    bg_deep: c(0x040e0d),
    panel: c(0x0b1e1c),
    input_bg: c(0x040b0a),
    fill: c(0x102c29),
    fill_hover: c(0x17403b),
    fill_active: c(0x1d524b),
    line: c(0x1a403c),
    line_lit: c(0x2a6660),
    text: c(0xb4dad2),
    text_strong: c(0xe8fffa),
    cyan: c(0x14e0c4),
    cyan_dim: c(0x1d9c8e),
    pink: c(0xff8a3f),
    yellow: c(0xffd23f),
    green: c(0x46e07d),
    ink_on_cyan: c(0x021917),
    red_deep: c(0x6e3a18),
    cq_bg: c(0x241505),
    tome_bg: c(0x2c2406),
    done_bg: c(0x082a24),
    row_bg: c(0x0a1b18),
    row_hover: c(0x142e2a),
    scroll_track: c(0x0e2622),
    scroll_handle: c(0x17ad9c),
    scroll_handle_hover: c(0x14e0c4),
    scroll_handle_drag: c(0xff8a3f),
    faint_bg: c(0x0e2622),
    alert: c(0xff2a55),
};

/// The default's dark navy grounds with the hues spread across the roles —
/// magenta borders, blue selections, violet captions, orange warnings, green
/// status. Different roles light different features, which is what makes the
/// whole UI come out multi-coloured.
const RAINBOW: Palette = Palette {
    bg_deep: c(0x050810),
    panel: c(0x0b111e),
    input_bg: c(0x04070e),
    fill: c(0x101a2c),
    fill_hover: c(0x17243c),
    fill_active: c(0x1d2f4d),
    line: c(0x1a2740),
    line_lit: c(0x2a4a66),
    text: c(0xb4c6da),
    text_strong: c(0xe8f4ff),
    cyan: c(0x3fa9ff),
    cyan_dim: c(0x8a5cff),
    pink: c(0xff2ad5),
    yellow: c(0xffa53f),
    green: c(0x46e07d),
    ink_on_cyan: c(0x040d19),
    red_deep: c(0x6e182c),
    cq_bg: c(0x240c15),
    tome_bg: c(0x2c2406),
    done_bg: c(0x082a17),
    row_bg: c(0x0a101b),
    row_hover: c(0x141e2e),
    scroll_track: c(0x0e1626),
    scroll_handle: c(0x8a5cff),
    scroll_handle_hover: c(0x3fa9ff),
    scroll_handle_drag: c(0xff2ad5),
    faint_bg: c(0x0e1626),
    alert: c(0xff2a55),
};

/// Indexed by [`theme_index`].
static PALETTES: [Palette; 5] = [DEFAULT, GREEN_PHOSPHOR, AMBER_PHOSPHOR, TEAL_ORANGE, RAINBOW];

/// The S-meter instrument's colours: the face wash, the backlight bloom, the
/// cool-side (below the red-line) inks, the bar's recessed rail, and the cool
/// half of the S/ALC ramps. Kept apart from [`Palette`] because these are one
/// widget's instrument face, not roles the rest of the UI shares.
///
/// Only the cool side lives here on purpose: everything past S9 / past 3:1 —
/// the needle's red-line, the TX backlight, the hot tick/label inks, the
/// amber-to-red ramp tops — stays red in every theme, the same rule that keeps
/// [`ALERT`] red.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterPalette {
    pub face_top: Color32,
    pub face_bot: Color32,
    /// Specular hairline along the top of the glass.
    pub glass: Color32,
    /// Backlight bloom behind the scale on receive (transmit's stays red).
    pub backlight: Color32,
    /// Readout text: the headline value and the sub-reading beside it.
    pub readout: Color32,
    pub subdued: Color32,
    pub tick_minor: Color32,
    pub tick_major: Color32,
    pub label: Color32,
    pub grid_line: Color32,
    pub grid_label: Color32,
    /// The bar face's recessed trough (top/bottom of its wash) and its edge.
    pub rail_top: Color32,
    pub rail_bot: Color32,
    pub rail_edge: Color32,
    /// The cool half of the S and ALC ramps: floor → mid → ice at S9.
    pub ramp_lo: Color32,
    pub ramp_mid: Color32,
    pub ramp_hi: Color32,
}

/// The historic instrument: navy glass, cyan backlight, ice at S9.
const METER_DEFAULT: MeterPalette = MeterPalette {
    face_top: c(0x111b2b),
    face_bot: c(0x03060c),
    glass: c(0x2c4460),
    backlight: c(0x1c7496),
    readout: c(0xe6f6ff),
    subdued: c(0x849eb6),
    tick_minor: c(0x54708a),
    tick_major: c(0xcfe6f5),
    label: c(0xbdd6e8),
    grid_line: c(0x1b2a3e),
    grid_label: c(0x637c92),
    rail_top: c(0x04070d),
    rail_bot: c(0x131f2f),
    rail_edge: c(0x1e2e42),
    ramp_lo: c(0x16627e),
    ramp_mid: c(0x1da8cf),
    ramp_hi: c(0x8eecff),
};

const METER_GREEN: MeterPalette = MeterPalette {
    face_top: c(0x0c2112),
    face_bot: c(0x020803),
    glass: c(0x2c6040),
    backlight: c(0x1c9654),
    readout: c(0xd9ffe6),
    subdued: c(0x74b68b),
    tick_minor: c(0x4f8a63),
    tick_major: c(0xccf5d8),
    label: c(0xb0e8c4),
    grid_line: c(0x14301d),
    grid_label: c(0x5d9270),
    rail_top: c(0x040d07),
    rail_bot: c(0x132f1d),
    rail_edge: c(0x1e422c),
    ramp_lo: c(0x167e46),
    ramp_mid: c(0x1dcf74),
    ramp_hi: c(0x8effc0),
};

const METER_AMBER: MeterPalette = MeterPalette {
    face_top: c(0x211809),
    face_bot: c(0x080502),
    glass: c(0x60482c),
    backlight: c(0x96661c),
    readout: c(0xffefd6),
    subdued: c(0xb69a74),
    tick_minor: c(0x8a734f),
    tick_major: c(0xf5e3c6),
    label: c(0xe8d4b0),
    grid_line: c(0x30240f),
    grid_label: c(0x92795d),
    rail_top: c(0x0d0904),
    rail_bot: c(0x2f2313),
    rail_edge: c(0x42331e),
    ramp_lo: c(0x7e5216),
    ramp_mid: c(0xcf8e1d),
    ramp_hi: c(0xffe28e),
};

const METER_TEAL: MeterPalette = MeterPalette {
    face_top: c(0x0c2320),
    face_bot: c(0x030b0a),
    glass: c(0x2c605a),
    backlight: c(0x1c9688),
    readout: c(0xdcfff8),
    subdued: c(0x84b6ac),
    tick_minor: c(0x548a82),
    tick_major: c(0xccf5ec),
    label: c(0xb0e8de),
    grid_line: c(0x143733),
    grid_label: c(0x5d928a),
    rail_top: c(0x040d0c),
    rail_bot: c(0x132f2b),
    rail_edge: c(0x1e423d),
    ramp_lo: c(0x167e72),
    ramp_mid: c(0x1dcfba),
    ramp_hi: c(0x8effef),
};

/// Indexed by [`theme_index`], like [`PALETTES`]. Rainbow keeps the historic
/// navy instrument: its grounds are the default's, and the meter already
/// reads in the accents the ramps give it.
static METER_PALETTES: [MeterPalette; 5] =
    [METER_DEFAULT, METER_GREEN, METER_AMBER, METER_TEAL, METER_DEFAULT];

/// The current theme's S-meter instrument colours.
#[inline]
pub fn meter_palette() -> &'static MeterPalette {
    &METER_PALETTES[THEME.load(Ordering::Relaxed) as usize]
}

/// Indexed by [`style_index`].
const STYLE_ORDER: [ChromeStyle; 5] = [
    ChromeStyle::Angled,
    ChromeStyle::Rectangular,
    ChromeStyle::Rounded,
    ChromeStyle::Gradient,
    ChromeStyle::Bevel,
];

const fn theme_index(t: UiTheme) -> u8 {
    match t {
        UiTheme::Default => 0,
        UiTheme::GreenPhosphor => 1,
        UiTheme::AmberPhosphor => 2,
        UiTheme::TealOrange => 3,
        UiTheme::Rainbow => 4,
    }
}

const fn style_index(s: ChromeStyle) -> u8 {
    match s {
        ChromeStyle::Angled => 0,
        ChromeStyle::Rectangular => 1,
        ChromeStyle::Rounded => 2,
        ChromeStyle::Gradient => 3,
        ChromeStyle::Bevel => 4,
    }
}

// The zero defaults are UiTheme::Default / ChromeStyle::Angled — the look the
// first frame wears if nothing calls `set_look` first.
static THEME: AtomicU8 = AtomicU8::new(0);
static BUTTON_STYLE: AtomicU8 = AtomicU8::new(0);
static WINDOW_STYLE: AtomicU8 = AtomicU8::new(0);

/// Select the process-wide look. Takes effect on whatever is painted next; the
/// egui `Visuals` derived from it need a separate [`apply_visuals`] call.
pub fn set_look(theme: UiTheme, buttons: ChromeStyle, windows: ChromeStyle) {
    THEME.store(theme_index(theme), Ordering::Relaxed);
    BUTTON_STYLE.store(style_index(buttons), Ordering::Relaxed);
    WINDOW_STYLE.store(style_index(windows), Ordering::Relaxed);
}

/// The current theme's palette.
#[inline]
pub fn palette() -> &'static Palette {
    &PALETTES[THEME.load(Ordering::Relaxed) as usize]
}

/// The shape buttons currently wear.
#[inline]
pub fn button_style() -> ChromeStyle {
    STYLE_ORDER[BUTTON_STYLE.load(Ordering::Relaxed) as usize]
}

/// The shape windows and popups currently wear.
#[inline]
pub fn window_style() -> ChromeStyle {
    STYLE_ORDER[WINDOW_STYLE.load(Ordering::Relaxed) as usize]
}

const fn font_index(f: FontSize) -> u8 {
    match f {
        FontSize::Small => 0,
        FontSize::Medium => 1,
        FontSize::Large => 2,
    }
}

// The defaults match `UiSettings::default()` — the sizes the first frame wears
// if nothing calls `set_font_sizes` first: skimmer and menus Medium (1),
// panadapter labels Small (0).
static SKIMMER_FONT: AtomicU8 = AtomicU8::new(1);
static PANADAPTER_FONT: AtomicU8 = AtomicU8::new(0);
static MENU_FONT: AtomicU8 = AtomicU8::new(1);

/// Select the process-wide font sizes. Like [`set_look`], the values live in
/// atomics rather than the egui context so the solar-3d viewport's menus see
/// the same sizes from their own thread. Takes effect on whatever is painted
/// next — the affected text is all laid out per frame.
pub fn set_font_sizes(skimmer: FontSize, panadapter: FontSize, menu: FontSize) {
    SKIMMER_FONT.store(font_index(skimmer), Ordering::Relaxed);
    PANADAPTER_FONT.store(font_index(panadapter), Ordering::Relaxed);
    MENU_FONT.store(font_index(menu), Ordering::Relaxed);
}

/// Scale for the skimmer / spot boxes on the waterfall. The historic point
/// sizes are the `Medium` step, so that is 1.
#[inline]
pub fn skimmer_font_scale() -> f32 {
    [0.85, 1.0, 1.2][SKIMMER_FONT.load(Ordering::Relaxed) as usize]
}

/// Scale for the labels painted onto the spectrum and waterfall (frequency
/// scale, band plan, measurements, markers). The historic point sizes are the
/// `Small` step, so that is 1.
#[inline]
pub fn panadapter_font_scale() -> f32 {
    [1.0, 1.25, 1.5][PANADAPTER_FONT.load(Ordering::Relaxed) as usize]
}

/// Scale for the popup menus' text. The historic sizes — the egui text styles
/// unscaled — are the `Medium` step, so that is 1.
#[inline]
pub fn menu_font_scale() -> f32 {
    [0.85, 1.0, 1.2][MENU_FONT.load(Ordering::Relaxed) as usize]
}

/// The palette under the historic constant names — these were `pub const`s
/// before the theme became switchable, and the call sites still read them by
/// name.
macro_rules! palette_accessors {
    ($($NAME:ident => $field:ident),* $(,)?) => {
        $(
            #[allow(non_snake_case)]
            #[inline]
            pub fn $NAME() -> Color32 { palette().$field }
        )*
    };
}

palette_accessors! {
    BG_DEEP => bg_deep,
    PANEL => panel,
    INPUT_BG => input_bg,
    FILL => fill,
    FILL_HOVER => fill_hover,
    FILL_ACTIVE => fill_active,
    LINE => line,
    LINE_LIT => line_lit,
    TEXT => text,
    TEXT_STRONG => text_strong,
    CYAN => cyan,
    CYAN_DIM => cyan_dim,
    PINK => pink,
    YELLOW => yellow,
    GREEN => green,
    INK_ON_CYAN => ink_on_cyan,
    RED_DEEP => red_deep,
    CQ_BG => cq_bg,
    TOME_BG => tome_bg,
    DONE_BG => done_bg,
    ROW_BG => row_bg,
    ROW_HOVER => row_hover,
    SCROLL_TRACK => scroll_track,
    SCROLL_HANDLE => scroll_handle,
    SCROLL_HANDLE_HOVER => scroll_handle_hover,
    SCROLL_HANDLE_DRAG => scroll_handle_drag,
    ALERT => alert,
}

/// A colour per continent, so a list of decodes reads as a map at a glance —
/// which way the band is open is visible before a single callsign is read.
/// Anything unrecognised comes back grey.
pub fn continent_color(code: &str) -> Color32 {
    match code {
        "EU" => Color32::from_rgb(0x7c, 0xa8, 0xff),
        "NA" => Color32::from_rgb(0x46, 0xe0, 0x7d),
        "SA" => Color32::from_rgb(0xff, 0xa5, 0x3f),
        "AS" => Color32::from_rgb(0xff, 0x7a, 0xd9),
        "AF" => Color32::from_rgb(0xff, 0xd2, 0x3f),
        "OC" => Color32::from_rgb(0x3f, 0xe0, 0xd8),
        "AN" => Color32::from_rgb(0xd0, 0xe4, 0xf4),
        _ => Color32::from_gray(110),
    }
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    apply_metrics(ctx, crate::layout::Tier::Desktop);
    apply_visuals(ctx);
}

/// Write the current palette and chrome shapes into the context style. Split
/// out of [`apply`] so a theme change at runtime doesn't rebuild the font
/// atlas.
///
/// Call this only from app construction and the top-of-frame look check —
/// never from inside a frame: [`ScrollPalette`] clones the visuals out of the
/// context style and writes them back, and a rewrite between its push and pop
/// would be popped away again.
pub fn apply_visuals(ctx: &egui::Context) {
    let p = palette();
    // Stock egui widgets follow the chosen shapes as far as a rounded rect
    // can: the Rounded styles round them, every other style keeps them sharp
    // (the cut corners of the Angled style are painted by `chrome`, not egui).
    let window_corner = match window_style() {
        ChromeStyle::Rounded => CornerRadius::same(6),
        _ => CornerRadius::ZERO,
    };
    let button_corner = match button_style() {
        ChromeStyle::Rounded => CornerRadius::same(5),
        _ => CornerRadius::ZERO,
    };

    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = p.panel;
        v.window_fill = p.panel;
        v.extreme_bg_color = p.input_bg;
        v.faint_bg_color = p.faint_bg;
        v.code_bg_color = p.input_bg;

        v.window_stroke = Stroke::new(1.0, p.pink);
        v.window_corner_radius = window_corner;
        v.menu_corner_radius = window_corner;

        v.selection.bg_fill = p.cyan;
        v.selection.stroke = Stroke::new(1.0, p.ink_on_cyan);
        v.hyperlink_color = p.cyan;
        v.warn_fg_color = p.yellow;
        v.error_fg_color = p.alert;
        v.slider_trailing_fill = true;

        v.widgets.noninteractive.bg_fill = p.panel;
        v.widgets.noninteractive.weak_bg_fill = p.panel;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.noninteractive.corner_radius = button_corner;

        v.widgets.inactive.bg_fill = p.fill;
        v.widgets.inactive.weak_bg_fill = p.fill;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.line_lit);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.inactive.corner_radius = button_corner;

        v.widgets.hovered.bg_fill = p.fill_hover;
        v.widgets.hovered.weak_bg_fill = p.fill_hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.cyan_dim);
        v.widgets.hovered.fg_stroke = Stroke::new(1.2, p.text_strong);
        v.widgets.hovered.corner_radius = button_corner;

        v.widgets.active.bg_fill = p.fill_active;
        v.widgets.active.weak_bg_fill = p.fill_active;
        v.widgets.active.bg_stroke = Stroke::new(1.0, p.cyan);
        v.widgets.active.fg_stroke = Stroke::new(1.2, p.cyan);
        v.widgets.active.corner_radius = button_corner;

        v.widgets.open.bg_fill = p.fill_active;
        v.widgets.open.weak_bg_fill = p.fill_active;
        v.widgets.open.bg_stroke = Stroke::new(1.0, p.cyan_dim);
        v.widgets.open.fg_stroke = Stroke::new(1.0, p.text_strong);
        v.widgets.open.corner_radius = button_corner;
    });
}

/// Sizes, spacing and hit targets for one layout tier. Split out of [`apply`]
/// so it can be re-run when the window crosses a breakpoint — [`apply`] itself
/// installs the fonts, and rebuilding the atlas every time a window is dragged
/// across 600 pt would be ruinous.
///
/// Deliberately touches no `visuals`: [`ScrollPalette`] swaps those in and out
/// of the same context style, and the two must not fight over it.
///
/// A touched screen gets bigger type and roomier targets. Everything a finger
/// has to hit is sized from `button_padding` and `interact_size` — chips
/// through [`crate::chrome::chip`], fields through egui itself — so those two
/// carry most of the tier here.
pub fn apply_metrics(ctx: &egui::Context, tier: crate::layout::Tier) {
    let touch = tier.touch();
    let body = if touch { 14.5 } else { 13.5 };
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(16.0, FontFamily::Name("chakra-bold".into()))),
            (TextStyle::Body, FontId::new(body, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(body, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();

        style.spacing.item_spacing =
            if touch { egui::vec2(6.0, 7.0) } else { egui::vec2(7.0, 5.0) };
        style.spacing.button_padding =
            if touch { egui::vec2(11.0, 9.0) } else { egui::vec2(7.0, 3.0) };
        // Roughly a fingertip where the screen is touched, egui's own default
        // where it is not. Set in *both* directions on purpose: this runs again
        // whenever the window crosses a breakpoint, and a value left behind by
        // the tier before would keep every field and every control box that
        // sizes from it stretched after the layout had gone back to a mouse.
        style.spacing.interact_size.y = if touch { 34.0 } else { 18.0 };
        // Fixed slider width: otherwise sliders expand to fill the row, so a
        // module with a slider balloons and pushes later modules off-screen
        // instead of letting `horizontal_wrapped` wrap them.
        style.spacing.slider_width = if touch { 110.0 } else { 84.0 };
        style.spacing.combo_width = 84.0;

        // egui's default scrollbars float over the content as a 2 px hairline that
        // only fades in on hover — easy to miss and hard to grab. Use solid bars of
        // a constant width that are always fully opaque.
        style.spacing.scroll = egui::style::ScrollStyle {
            bar_width: if touch { 14.0 } else { 9.0 },
            handle_min_length: 24.0,
            bar_inner_margin: 3.0,
            bar_outer_margin: 0.0,
            // Take the handle colour from `fg_stroke` instead of the (near-black)
            // widget fill, so bars we don't paint by hand — combo popups, menus —
            // still get a handle that stands out from the gutter.
            foreground_color: true,
            ..egui::style::ScrollStyle::solid()
        };
    });
}

/// Paint the scrollbar palette into `v`: cyan handle in a recessed gutter,
/// brightening on hover and going hot pink while dragged.
///
/// egui has no scrollbar colours of its own — it takes the handle colour from
/// the widget visuals of whatever `Ui` owns the bars (with `foreground_color`,
/// the same `fg_stroke` that colours button labels), so this can't simply live
/// in [`apply`]. It goes in around a scroll area and comes back off its
/// contents instead; see [`ThemedScroll`] and [`ScrollPalette`].
fn scroll_palette(v: &mut egui::Visuals) {
    let p = palette();
    v.extreme_bg_color = p.scroll_track; // the gutter
    v.widgets.inactive.fg_stroke.color = p.scroll_handle;
    v.widgets.hovered.fg_stroke.color = p.scroll_handle_hover;
    v.widgets.active.fg_stroke.color = p.scroll_handle_drag;
}

/// `ScrollArea::show` with themed scrollbars.
pub trait ThemedScroll {
    /// Show the area, painting its bars in the [`scroll_palette`].
    fn show_themed<R>(
        self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::containers::scroll_area::ScrollAreaOutput<R>;
}

impl ThemedScroll for egui::ScrollArea {
    fn show_themed<R>(
        self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::containers::scroll_area::ScrollAreaOutput<R> {
        let normal = ui.visuals().clone();
        scroll_palette(ui.visuals_mut());

        let out = self.show(ui, |ui| {
            *ui.visuals_mut() = normal.clone();
            add_contents(ui)
        });

        *ui.visuals_mut() = normal;
        out
    }
}

/// The [`scroll_palette`], lent to the context style for containers that build
/// their own scrollbars — an [`egui::Window`] with `vscroll`, whose bars belong
/// to a `Ui` inside egui that we never get to hold.
///
/// Push it before showing the container, hand the body back the normal palette
/// with [`Self::restore`], then [`Self::pop`] it off the context again.
#[must_use = "the palette stays in the context style until popped"]
pub struct ScrollPalette(egui::Visuals);

impl ScrollPalette {
    pub fn push(ctx: &egui::Context) -> Self {
        let normal = ctx.style_of(ctx.theme()).visuals.clone();
        let mut bars = normal.clone();
        scroll_palette(&mut bars);
        ctx.set_visuals(bars);
        Self(normal)
    }

    /// Give a container body the normal palette back — and the context with it,
    /// so tooltips and dropdowns opened from the body aren't tinted either. The
    /// bars keep the palette: their `Ui` was built (and took its copy of the
    /// style) before this runs.
    pub fn restore(&self, ui: &mut egui::Ui) {
        ui.ctx().set_visuals(self.0.clone());
        *ui.visuals_mut() = self.0.clone();
    }

    /// Take the palette off the context — a no-op once [`Self::restore`] has
    /// run, and the way it comes back off when the container never shows a body
    /// (a closed or collapsed window).
    pub fn pop(self, ctx: &egui::Context) {
        ctx.set_visuals(self.0);
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "chakra".into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/ChakraPetch-Regular.ttf"))),
    );
    fonts.font_data.insert(
        "chakra-bold".into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/ChakraPetch-SemiBold.ttf"))),
    );
    // Angular techno monospace for the FT8 decode list (Share Tech Mono, OFL).
    fonts.font_data.insert(
        "cyber-mono".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/ShareTechMono-Regular.ttf"
        ))),
    );

    if let Some(prop) = fonts.families.get_mut(&FontFamily::Proportional) {
        prop.insert(0, "chakra".into());
    }
    // Make Share Tech Mono the primary monospace (used by the decode list,
    // frequency readout, and meters).
    if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
        mono.insert(0, "cyber-mono".into());
    }
    fonts.families.insert(FontFamily::Name("cyber-mono".into()), vec!["cyber-mono".to_string()]);
    // Bold family for headings, falling back through the proportional stack.
    let mut bold_stack = vec!["chakra-bold".to_string()];
    if let Some(prop) = fonts.families.get(&FontFamily::Proportional) {
        bold_stack.extend(prop.iter().cloned());
    }
    fonts.families.insert(FontFamily::Name("chakra-bold".into()), bold_stack);

    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Tier;

    /// Every metric [`apply_metrics`] sets has to be set in *both* directions.
    ///
    /// It runs again whenever the window crosses a breakpoint, so a value
    /// written only for the touched layouts stays behind when the window goes
    /// back to a desktop one. That is how the control boxes came to stand
    /// taller than the frequency box beside them, and how the RIT field came to
    /// sit a few points low in its row: `interact_size` was still a fingertip
    /// tall long after the layout had gone back to a mouse.
    #[test]
    fn desktop_metrics_survive_a_trip_through_the_touch_layouts() {
        let ctx = egui::Context::default();
        apply_metrics(&ctx, Tier::Desktop);
        let (spacing, text) = {
            let s = ctx.style_of(ctx.theme());
            (s.spacing.clone(), s.text_styles.clone())
        };

        for tier in [Tier::Phone, Tier::Tablet, Tier::Phone] {
            apply_metrics(&ctx, tier);
        }
        apply_metrics(&ctx, Tier::Desktop);

        let s = ctx.style_of(ctx.theme());
        assert_eq!(spacing, s.spacing, "a spacing metric was left behind by a touch layout");
        assert_eq!(text, s.text_styles, "a text size was left behind by a touch layout");
    }

    /// The touched layouts really do differ — otherwise the test above would
    /// pass on a function that had stopped doing anything at all.
    #[test]
    fn a_touched_layout_gets_bigger_targets() {
        let ctx = egui::Context::default();
        apply_metrics(&ctx, Tier::Desktop);
        let desktop = ctx.style_of(ctx.theme()).spacing.clone();
        apply_metrics(&ctx, Tier::Phone);
        let phone = ctx.style_of(ctx.theme()).spacing.clone();

        assert!(phone.interact_size.y > desktop.interact_size.y);
        assert!(phone.button_padding.y > desktop.button_padding.y);
        assert!(phone.scroll.bar_width > desktop.scroll.bar_width);
    }

    /// The default palette is the classic look and must never drift: these are
    /// the exact values of the `pub const`s the theme system replaced, which
    /// every screenshot in the manual shows.
    #[test]
    fn default_palette_is_the_historic_look() {
        let p = &PALETTES[theme_index(UiTheme::Default) as usize];
        for (got, want, name) in [
            (p.bg_deep, 0x050810, "bg_deep"),
            (p.panel, 0x0b111e, "panel"),
            (p.input_bg, 0x04070e, "input_bg"),
            (p.fill, 0x101a2c, "fill"),
            (p.fill_hover, 0x17243c, "fill_hover"),
            (p.fill_active, 0x1d2f4d, "fill_active"),
            (p.line, 0x1a2740, "line"),
            (p.line_lit, 0x2a4a66, "line_lit"),
            (p.text, 0xb4c6da, "text"),
            (p.text_strong, 0xe8f4ff, "text_strong"),
            (p.cyan, 0x00d0f4, "cyan"),
            (p.cyan_dim, 0x1d9cbe, "cyan_dim"),
            (p.pink, 0xff2a55, "pink"),
            (p.yellow, 0xffd23f, "yellow"),
            (p.green, 0x46e07d, "green"),
            (p.ink_on_cyan, 0x021019, "ink_on_cyan"),
            (p.red_deep, 0x6e182c, "red_deep"),
            (p.cq_bg, 0x240c15, "cq_bg"),
            (p.tome_bg, 0x2c2406, "tome_bg"),
            (p.done_bg, 0x082a17, "done_bg"),
            (p.row_bg, 0x0a101b, "row_bg"),
            (p.row_hover, 0x141e2e, "row_hover"),
            (p.scroll_track, 0x0e1626, "scroll_track"),
            (p.scroll_handle, 0x178ead, "scroll_handle"),
            (p.scroll_handle_hover, 0x00d0f4, "scroll_handle_hover"),
            (p.scroll_handle_drag, 0xff2a55, "scroll_handle_drag"),
            (p.faint_bg, 0x0e1626, "faint_bg"),
            (p.alert, 0xff2a55, "alert"),
        ] {
            assert_eq!(got, c(want), "default palette field {name} drifted");
        }
    }

    /// The default S-meter instrument must stay the historic one: these are
    /// the exact values of the constants `widgets::smeter` carried before the
    /// instrument followed the theme. Rainbow shares them by design.
    #[test]
    fn default_meter_palette_is_the_historic_instrument() {
        let m = &METER_PALETTES[theme_index(UiTheme::Default) as usize];
        assert_eq!(METER_PALETTES[theme_index(UiTheme::Rainbow) as usize], *m);
        for (got, want, name) in [
            (m.face_top, 0x111b2b, "face_top"),
            (m.face_bot, 0x03060c, "face_bot"),
            (m.glass, 0x2c4460, "glass"),
            (m.backlight, 0x1c7496, "backlight"),
            (m.readout, 0xe6f6ff, "readout"),
            (m.subdued, 0x849eb6, "subdued"),
            (m.tick_minor, 0x54708a, "tick_minor"),
            (m.tick_major, 0xcfe6f5, "tick_major"),
            (m.label, 0xbdd6e8, "label"),
            (m.grid_line, 0x1b2a3e, "grid_line"),
            (m.grid_label, 0x637c92, "grid_label"),
            (m.rail_top, 0x04070d, "rail_top"),
            (m.rail_bot, 0x131f2f, "rail_bot"),
            (m.rail_edge, 0x1e2e42, "rail_edge"),
            (m.ramp_lo, 0x16627e, "ramp_lo"),
            (m.ramp_mid, 0x1da8cf, "ramp_mid"),
            (m.ramp_hi, 0x8eecff, "ramp_hi"),
        ] {
            assert_eq!(got, c(want), "default meter palette field {name} drifted");
        }
    }

    /// Every theme and style maps to a distinct in-bounds slot — the indices
    /// are what ties the enums to `PALETTES`/`STYLE_ORDER`, and a botched
    /// mapping would silently hand one theme another theme's colours.
    ///
    /// Deliberately does not call [`set_look`]: the atomics are process-wide
    /// and tests run in parallel, so flipping the theme here would repaint
    /// another test's world.
    #[test]
    fn look_indices_are_a_bijection() {
        let mut seen = [false; PALETTES.len()];
        for t in UiTheme::ALL {
            let i = theme_index(t) as usize;
            assert!(!seen[i], "two themes share palette slot {i}");
            seen[i] = true;
        }
        assert_eq!(theme_index(UiTheme::Default), 0, "Default must be the boot-up palette");

        let mut seen = [false; STYLE_ORDER.len()];
        for s in ChromeStyle::ALL {
            let i = style_index(s) as usize;
            assert_eq!(STYLE_ORDER[i], s, "STYLE_ORDER and style_index disagree");
            assert!(!seen[i], "two styles share slot {i}");
            seen[i] = true;
        }
        assert_eq!(style_index(ChromeStyle::Angled), 0, "Angled must be the boot-up style");
    }

    /// The alert role stays red in every theme — the phosphor themes are
    /// monochrome, but a TX or SWR indication must never come out green.
    #[test]
    fn every_theme_keeps_alerts_red() {
        for (i, p) in PALETTES.iter().enumerate() {
            assert!(
                p.alert.r() > 180 && p.alert.g() < 90 && p.alert.b() < 120,
                "palette {i} has a non-red alert colour {:?}",
                p.alert
            );
        }
    }
}
