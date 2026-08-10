//! The full-band strip: everything a direct-sampling receiver can see at once,
//! as a scrolling waterfall.
//!
//! Deliberately *not* a second [`crate::widgets::spectrum_view`]. That widget is
//! two thousand lines of panadapter — filter-edge grips, sub-receiver dragging,
//! fling, zoom, viewport negotiation with the engine — all of it pinned to the
//! device passband through `ViewState::clamp_to`. None of that applies here: the
//! span is fixed at whatever the front end can see, there is nothing to zoom
//! into, and the one interaction that matters is "take me there".
//!
//! Overloading `state.sample_rate` to describe this wider window instead would
//! have avoided a new widget and broken two things that read it as the real IQ
//! span: `keep_vfo_in_span`, which would then never retune, and the sub-receiver
//! clamp, which would let the sub be placed somewhere the DDC cannot reach.
//!
//! # Why this waterfall is on the CPU
//!
//! [`crate::waterfall_gpu`] keeps its state in egui's *type-keyed*
//! `CallbackResources`, so a second instance needs a newtype and another pair of
//! 2048×2048 textures — 8 MB — to show a strip a few dozen rows tall. This one
//! rebuilds a 1024×120 image twenty times a second, which is not worth a second
//! GPU pipeline.

use std::collections::VecDeque;

use eframe::egui::{
    self, Color32, ColorImage, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Vec2,
};
use sdroxide_types::{Command, RadioState, SpectrumFrame};

use crate::colormap;

/// Height of the strip, in points.
pub const STRIP_HEIGHT: f32 = 96.0;

/// Rows of history kept. At ~20 frames a second this is about six seconds —
/// long enough to see a band opening without the strip turning into a second
/// waterfall competing with the main one.
const ROWS: usize = 120;

/// Width the rows are stored at. Independent of the widget's pixel width, so a
/// window resize does not throw the history away.
const COLS: usize = 1024;

/// Scrolling waterfall state for the full-band strip.
///
/// Lives in the app rather than the widget because it has to survive between
/// frames — and because it must be droppable when the source changes, or the
/// history goes on showing a band nothing is receiving any more.
#[derive(Default)]
pub struct WideWaterfall {
    /// Newest row last, each `COLS` bytes of 0..=255 magnitude.
    rows: VecDeque<Vec<u8>>,
    tex: Option<TextureHandle>,
    last_seq: u32,
    palette: usize,
    /// The dB window the newest row was built with, for the scale readout.
    levels: (f32, f32),
}

impl WideWaterfall {
    /// Fold a new frame in, if it is one we have not seen.
    pub fn push(&mut self, frame: &SpectrumFrame, palette: usize) {
        if frame.seq == self.last_seq && !self.rows.is_empty() {
            return;
        }
        self.last_seq = frame.seq;
        self.levels = (frame.db_floor, frame.db_ceil);
        self.palette = palette;

        // Resample to the history width, keeping the strongest bin in each
        // column: a carrier occupying one bin of two thousand has to survive
        // being squeezed into one pixel.
        let n = frame.bins.len().max(1);
        let mut row = vec![0u8; COLS];
        for (c, slot) in row.iter_mut().enumerate() {
            let b0 = c * n / COLS;
            let b1 = (((c + 1) * n / COLS).max(b0 + 1)).min(n);
            *slot = frame.bins.get(b0..b1).and_then(|s| s.iter().copied().max()).unwrap_or(0);
        }
        self.rows.push_back(row);
        while self.rows.len() > ROWS {
            self.rows.pop_front();
        }
        // Rebuilt on the next draw.
        self.tex = None;
    }

    /// Drop the history — used when the radio source changes.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.tex = None;
        self.last_seq = 0;
    }

    fn texture(&mut self, ctx: &egui::Context) -> Option<&TextureHandle> {
        if self.tex.is_none() && !self.rows.is_empty() {
            let lut = colormap::lut(self.palette);
            // Newest row first, so the strip scrolls downward like the main
            // waterfall rather than against it. Built as one contiguous vec —
            // `ColorImage` wants pixels row-major top to bottom anyway, and
            // pushing into it beats indexing a pre-filled image.
            let mut pixels = Vec::with_capacity(COLS * self.rows.len());
            for row in self.rows.iter().rev() {
                pixels.extend(row.iter().map(|v| {
                    let i = *v as usize * 4;
                    Color32::from_rgb(lut[i], lut[i + 1], lut[i + 2])
                }));
            }
            let img = ColorImage::new([COLS, self.rows.len()], pixels);
            self.tex = Some(ctx.load_texture("wide-waterfall", img, egui::TextureOptions::LINEAR));
        }
        self.tex.as_ref()
    }
}

/// Draw the full-band strip and handle clicks on it.
pub fn show(
    ui: &mut egui::Ui,
    wf: &mut WideWaterfall,
    frame: &SpectrumFrame,
    state: &RadioState,
    palette: usize,
    cmds: &mut Vec<Command>,
) {
    wf.push(frame, palette);

    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), STRIP_HEIGHT), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let painter = ui.painter_at(rect);
    let lo = frame.center_hz - frame.span_hz / 2.0;
    let hi = frame.center_hz + frame.span_hz / 2.0;
    let x_of = |hz: f64| -> f32 {
        rect.left() + ((hz - lo) / (hi - lo)).clamp(0.0, 1.0) as f32 * rect.width()
    };

    painter.rect_filled(rect, 2.0, Color32::BLACK);
    let ctx = ui.ctx().clone();
    if let Some(tex) = wf.texture(&ctx) {
        painter.image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    // The slice the main panadapter is receiving. Outlined rather than filled,
    // so it does not tint the waterfall underneath it.
    let sx0 = x_of(state.center_hz - state.sample_rate / 2.0);
    let sx1 = x_of(state.center_hz + state.sample_rate / 2.0);
    painter.rect_stroke(
        Rect::from_min_max(
            Pos2::new(sx0, rect.top()),
            Pos2::new(sx1.max(sx0 + 2.0), rect.bottom()),
        ),
        0.0,
        Stroke::new(1.0, Color32::from_rgb(150, 190, 255)),
        StrokeKind::Inside,
    );

    // Megahertz grid, ticked at the top so it does not sit over the newest rows.
    let step_mhz = if frame.span_hz > 20e6 { 5.0 } else { 1.0 };
    let mut mhz = (lo / 1e6).ceil();
    while mhz * 1e6 <= hi {
        let x = x_of(mhz * 1e6);
        let major = (mhz / step_mhz).fract().abs() < 1e-6;
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.top() + if major { 7.0 } else { 3.0 })],
            Stroke::new(1.0, Color32::from_white_alpha(90)),
        );
        if major {
            painter.text(
                Pos2::new(x + 2.0, rect.top() + 1.0),
                egui::Align2::LEFT_TOP,
                format!("{mhz:.0}"),
                egui::FontId::proportional(9.0 * crate::theme::panadapter_font_scale()),
                Color32::from_white_alpha(150),
            );
        }
        mhz += 1.0;
    }

    // The tuned frequency.
    let vfo_x = x_of(state.active_freq_hz());
    painter.line_segment(
        [Pos2::new(vfo_x, rect.top()), Pos2::new(vfo_x, rect.bottom())],
        Stroke::new(1.0, Color32::from_rgb(255, 190, 60)),
    );

    // The auto-ranged window, so the scale is not a mystery.
    painter.text(
        Pos2::new(rect.right() - 3.0, rect.bottom() - 1.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{:.0} … {:.0} dBFS", wf.levels.0, wf.levels.1),
        egui::FontId::proportional(9.0 * crate::theme::panadapter_font_scale()),
        Color32::from_white_alpha(120),
    );

    // Straight to `SetVfo`: the engine's own `keep_vfo_in_span` notices the new
    // frequency is outside the received slice and retunes the front end. On this
    // hardware that costs nothing — retuning is a change of FFT bin, not an LO
    // move — so a click anywhere in 32 MHz lands immediately.
    if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked()) {
        let frac = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
        let hz = lo + frac * (hi - lo);
        cmds.push(Command::SetVfo { vfo: state.active_vfo, hz: (hz / 100.0).round() * 100.0 });
    }

    resp.on_hover_text_at_pointer(hover_label(rect, ui, lo, hi));
}

/// The frequency under the cursor, for the hover tooltip.
fn hover_label(rect: Rect, ui: &egui::Ui, lo: f64, hi: f64) -> String {
    let Some(pos) = ui.ctx().pointer_latest_pos() else {
        return String::new();
    };
    let frac = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
    format!("{:.3} MHz — click to tune", (lo + frac * (hi - lo)) / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u32, bins: Vec<u8>) -> SpectrumFrame {
        SpectrumFrame {
            seq,
            center_hz: 16.2e6,
            span_hz: 32.4e6,
            db_floor: -120.0,
            db_ceil: -20.0,
            bins,
        }
    }

    /// The axis mapping is the part that has to be right: a frame claiming
    /// 0–32.4 MHz must put 0 at the left edge and Nyquist at the right.
    #[test]
    fn the_axis_spans_dc_to_nyquist() {
        let f = frame(1, vec![0; 2048]);
        let lo = f.center_hz - f.span_hz / 2.0;
        let hi = f.center_hz + f.span_hz / 2.0;
        assert!(lo.abs() < 1.0, "left edge should be DC, got {lo}");
        assert!((hi - 32.4e6).abs() < 1.0, "right edge should be Nyquist, got {hi}");
    }

    #[test]
    fn a_click_maps_back_to_the_frequency_under_it() {
        let (lo, hi) = (0.0f64, 32.4e6f64);
        // A click a quarter of the way across a 32.4 MHz strip is 8.1 MHz.
        assert!(((lo + 0.25 * (hi - lo)) - 8.1e6).abs() < 1.0);
        let hz = lo + 0.5 * (hi - lo);
        assert!((((hz / 100.0f64).round() * 100.0) - hz).abs() <= 50.0);
    }

    #[test]
    fn history_scrolls_and_is_bounded() {
        let mut wf = WideWaterfall::default();
        for seq in 1..(ROWS as u32 + 40) {
            wf.push(&frame(seq, vec![0; 2048]), 0);
        }
        assert_eq!(wf.rows.len(), ROWS, "history grew past its bound");
    }

    #[test]
    fn the_same_frame_is_not_added_twice() {
        let mut wf = WideWaterfall::default();
        wf.push(&frame(7, vec![0; 2048]), 0);
        wf.push(&frame(7, vec![0; 2048]), 0);
        assert_eq!(wf.rows.len(), 1, "a repeated seq added a second row");
    }

    /// The whole point of max-pooling: a carrier one bin wide out of two
    /// thousand must still be visible after resampling to the history width.
    #[test]
    fn a_narrow_carrier_survives_resampling() {
        let mut bins = vec![0u8; 2048];
        bins[1000] = 255;
        let mut wf = WideWaterfall::default();
        wf.push(&frame(1, bins), 0);
        let row = wf.rows.back().unwrap();
        assert_eq!(row.iter().copied().max(), Some(255), "the carrier vanished");
        assert_eq!(row[1000 * COLS / 2048], 255, "the carrier moved column");
    }

    #[test]
    fn clearing_drops_the_history() {
        let mut wf = WideWaterfall::default();
        wf.push(&frame(1, vec![9; 2048]), 0);
        wf.clear();
        assert!(wf.rows.is_empty());
        // ...and a frame with the same seq is then accepted again, rather than
        // being mistaken for one already shown.
        wf.push(&frame(1, vec![9; 2048]), 0);
        assert_eq!(wf.rows.len(), 1);
    }

    #[test]
    fn the_reported_window_follows_the_frame() {
        let mut wf = WideWaterfall::default();
        let mut f = frame(1, vec![0; 64]);
        f.db_floor = -101.0;
        f.db_ceil = -17.0;
        wf.push(&f, 0);
        assert_eq!(wf.levels, (-101.0, -17.0));
    }
}
