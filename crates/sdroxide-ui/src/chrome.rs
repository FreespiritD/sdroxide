//! Cyberpunk chrome: cut-corner panel frames, angled chip buttons, and
//! corner accents — the shapes egui's rounded-rect widgets can't draw.

use eframe::egui::{
    self, Color32, FontSelection, Painter, Pos2, Rect, Response, RichText, Sense, Shape, Stroke,
    TextStyle, Ui, WidgetText, pos2, vec2,
};

use crate::theme::{self, ThemedScroll};

/// Corner cut size for panel frames.
const FRAME_CUT: f32 = 10.0;
/// Corner cut size for chip buttons.
const CHIP_CUT: f32 = 5.0;
/// Fixed module height for the captioned control boxes. Must exceed the tallest
/// content (caption + a combo or slider row + margins) so every module ends up
/// exactly this tall — then they line up regardless of the row's cross-axis
/// alignment.
pub const MODULE_H: f32 = 50.0;
/// Taller box height for the prominent, caption-less boxes (frequency readout,
/// S-meter, VFO/band-mode stack) — ~25% taller than a control box's original
/// height, so two shortened control rows sit alongside one of these.
pub const MODULE_TALL_H: f32 = 72.0;
/// The hairline border a module box wears. egui draws a [`egui::Frame`]'s
/// stroke outside the content it was given, so a module built to `height`
/// stands `height + 2 * MODULE_BORDER` tall overall — which is the figure a box
/// that paints its own border has to match. See [`module_bare_flush_h`].
pub const MODULE_BORDER: f32 = 1.0;

/// A panel with a pink border and cut corners (top-right + bottom-left),
/// sitting on the darker page background.
pub fn angled_frame<R>(ui: &mut Ui, accent: Color32, add: impl FnOnce(&mut Ui) -> R) -> R {
    // A Frame measures its content with UNBOUNDED width to auto-size, and
    // `horizontal_wrapped` inside that pass never wraps (nothing to wrap
    // against). Capture the panel's real width here, before the frame, and
    // pin the content to it so wrapping happens at the visible edge.
    let avail = {
        let a = ui.available_width();
        if a.is_finite() && a > 50.0 { a } else { ui.ctx().content_rect().width() - 24.0 }
    };
    let margin = 10i8;
    let inner = egui::Frame::new()
        .fill(theme::PANEL)
        .inner_margin(egui::Margin::symmetric(margin, 8))
        .show(ui, |ui| {
            // Pin to the panel width (both min and max) so wrapping happens at
            // the visible edge AND the frame — and its cut-corner border — spans
            // the full width even when the last row of content is short.
            let w = (avail - 2.0 * margin as f32).max(120.0);
            ui.set_min_width(w);
            ui.set_max_width(w);
            add(ui)
        });
    paint_cut_border(ui.painter(), inner.response.rect, accent, theme::BG_DEEP);
    inner.inner
}

/// Frame for a floating window: flat panel fill, square corners (the cut
/// corners are painted on top afterwards by [`paint_window_border`]), with a
/// roomy content margin.
pub fn window_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::PANEL)
        .inner_margin(egui::Margin::same(11))
        .corner_radius(egui::CornerRadius::ZERO)
}

/// Paint the pink cut-corner border around a floating window (top-right +
/// bottom-left bevels), matching the main panadapter chrome. Draws on the
/// window's own layer so it sits over the panel fill.
pub fn paint_window_border(ctx: &egui::Context, resp: &Response) {
    let p = ctx.layer_painter(resp.layer_id);
    paint_cut_border(&p, resp.rect, theme::PINK, theme::PANEL);
}

/// Multiply a colour's alpha by `a` (for fading chrome in/out).
fn fade(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * a.clamp(0.0, 1.0)) as u8)
}

/// A floating-window frame (flat panel, square corners) with its fill faded to
/// `alpha` — pair with [`paint_popup_cut_border`] for a fading popup.
pub fn window_frame_alpha(alpha: f32) -> egui::Frame {
    let mut f = window_frame();
    f.fill = fade(f.fill, alpha);
    f
}

/// Paint the pink top-right/bottom-left cut border of a popup, faded to `alpha`.
pub fn paint_popup_cut_border(ctx: &egui::Context, resp: &Response, alpha: f32) {
    let p = ctx.layer_painter(resp.layer_id);
    paint_cut_border(&p, resp.rect, fade(theme::PINK, alpha), fade(theme::PANEL, alpha));
}

/// Fade timing for an auto-dismissing popup: full opacity for `HOLD` seconds
/// after it opens, then a linear fade over `FADE` seconds, then it closes.
/// `since` (caller-owned, one per popup) remembers when it opened. Returns the
/// current opacity; apply it to the frame ([`window_frame_alpha`]), the content
/// (`ui.set_opacity`), and the border ([`paint_popup_cut_border`]).
pub fn popup_fade_alpha(
    ctx: &egui::Context,
    popup_id: egui::Id,
    now: f64,
    since: &mut Option<f64>,
) -> f32 {
    const HOLD: f64 = 5.0;
    const FADE: f64 = 3.0;
    // A touched screen has no hover to hold a popup open with — the browser
    // reports the pointer gone the instant a finger lifts — so the fade would
    // take a popup away while it was being read. There it closes on a tap
    // outside, and on nothing else.
    if crate::layout::tier(ctx).touch() {
        *since = None;
        return 1.0;
    }
    if !egui::Popup::is_id_open(ctx, popup_id) {
        *since = None;
        return 1.0;
    }
    let t0 = *since.get_or_insert(now);
    let elapsed = now - t0;
    if elapsed >= HOLD + FADE {
        egui::Popup::close_id(ctx, popup_id);
        *since = None;
        0.0
    } else if elapsed > HOLD {
        ctx.request_repaint(); // animate the fade
        (1.0 - (elapsed - HOLD) / FADE) as f32
    } else {
        // Wake up exactly when the fade should begin.
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((HOLD - elapsed).max(0.001)));
        1.0
    }
}

/// Cut-corner border: masks the two corners with `mask` (the surrounding
/// background) and strokes the six-sided outline.
pub fn paint_cut_border(p: &Painter, rect: Rect, color: Color32, mask: Color32) {
    let cut = FRAME_CUT.min(rect.height() * 0.4);
    let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());

    // Mask the square corners so the cut reads as a real bevel.
    p.add(Shape::convex_polygon(
        vec![pos2(r - cut, t), pos2(r, t), pos2(r, t + cut)],
        mask,
        Stroke::NONE,
    ));
    p.add(Shape::convex_polygon(
        vec![pos2(l, b - cut), pos2(l + cut, b), pos2(l, b)],
        mask,
        Stroke::NONE,
    ));

    let outline = vec![
        pos2(l, t),
        pos2(r - cut, t),
        pos2(r, t + cut),
        pos2(r, b),
        pos2(l + cut, b),
        pos2(l, b - cut),
    ];
    p.add(Shape::closed_line(outline, Stroke::new(1.2, color)));
}

/// 45° yellow/black hazard stripes filling `rect` (clipped to it).
///
/// The app's "pay attention to this" mark: it runs down the side of every
/// section header in the manual, and along the CME arrival banner in the solar
/// view. Shared so the two cannot drift apart.
pub fn hazard_stripes(p: &Painter, rect: Rect, stripe_w: f32) {
    let clip = p.with_clip_rect(rect);
    let dark = Color32::from_rgb(0x16, 0x12, 0x04);
    let h = rect.height();
    let mut x = rect.left() - h;
    let mut k = 0i32;
    while x < rect.right() + stripe_w {
        let color = if k % 2 == 0 { theme::YELLOW } else { dark };
        clip.add(Shape::convex_polygon(
            vec![
                pos2(x, rect.bottom()),
                pos2(x + h, rect.top()),
                pos2(x + h + stripe_w, rect.top()),
                pos2(x + stripe_w, rect.bottom()),
            ],
            color,
            Stroke::NONE,
        ));
        x += stripe_w;
        k += 1;
    }
}

/// A captioned control module of fixed `width`: a bordered box with a small
/// cyan uppercase label above a row of controls.
///
/// Uses `allocate_ui_with_layout` so the fixed width is reserved *before*
/// the content is drawn — that lets a `horizontal_wrapped` parent wrap the
/// whole module to the next row cleanly (a plain `Frame` instead shrinks
/// into whatever sliver is left, which is the wrong behavior here).
pub fn module<R>(ui: &mut Ui, caption: &str, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    module_h(ui, caption, width, MODULE_H, add)
}

/// Like [`module`] but with an explicit box `height` (e.g. [`MODULE_TALL_H`]).
pub fn module_h<R>(
    ui: &mut Ui,
    caption: &str,
    width: f32,
    height: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    // Fixed height too: a bare (w, 0) allocation lets the top-down layout
    // over-reserve vertical space, leaving big gaps between wrapped rows.
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(width);
            egui::Frame::new()
                .fill(theme::FILL)
                .stroke(Stroke::new(1.0, theme::LINE_LIT))
                .inner_margin(egui::Margin { left: 8, right: 8, top: 4, bottom: 3 })
                .show(ui, |ui| {
                    ui.set_width(width - 16.0);
                    // Fill the full module height so every box — captioned or
                    // bare — ends up exactly `height` tall.
                    ui.set_min_height(height - 7.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(caption.to_uppercase())
                            .color(theme::CYAN_DIM)
                            .size(9.5)
                            .strong(),
                    );
                    // Top-align the control row. egui's ComboBox positions its
                    // button from `available_rect_before_wrap().top()` and so
                    // ignores vertical centering, unlike chips and drag-values;
                    // top-aligning everything keeps them all on one baseline.
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                        ui.set_min_height(24.0);
                        add(ui)
                    })
                    .inner
                })
                .inner
        },
    )
    .inner
}

/// Like [`module`] but with no caption — the content fills the full box height
/// (vertically centred). Used for the frequency readout and S-meter, where the
/// label would only waste space.
pub fn module_bare<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    module_bare_h(ui, width, MODULE_H, add)
}

/// Like [`module_bare`] but with an explicit box `height`.
pub fn module_bare_h<R>(ui: &mut Ui, width: f32, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(width);
            egui::Frame::new()
                .fill(theme::FILL)
                .stroke(Stroke::new(1.0, theme::LINE_LIT))
                .inner_margin(egui::Margin { left: 8, right: 8, top: 4, bottom: 5 })
                .show(ui, |ui| {
                    ui.set_width(width - 16.0);
                    ui.set_min_height(height - 9.0);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_height(height - 9.0);
                        add(ui)
                    })
                    .inner
                })
                .inner
        },
    )
    .inner
}

/// Like [`module_bare_h`] but with zero inner margin and no border, so the
/// content fills the box edge-to-edge. Used by the S-meter, which paints its
/// own instrument face over the whole rect (an opaque fill would otherwise hide
/// a frame border) and draws the box border itself on top.
///
/// It takes [`MODULE_BORDER`] into its own content on both axes: a bordered
/// module's stroke is drawn *outside* the content it was handed, so one built
/// to `height` stands `height + 2` tall. This box's border is painted inside
/// its content instead, so without the two points back it would come out two
/// short — and the S-meter would sit a hair below the boxes beside it.
pub fn module_bare_flush_h<R>(
    ui: &mut Ui,
    width: f32,
    height: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let width = width + 2.0 * MODULE_BORDER;
    let height = height + 2.0 * MODULE_BORDER;
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(width);
            egui::Frame::new()
                .fill(theme::FILL)
                .inner_margin(egui::Margin::ZERO)
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.set_min_height(height);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                        ui.set_min_height(height);
                        add(ui)
                    })
                    .inner
                })
                .inner
        },
    )
    .inner
}

/// A row of controls inside a module box or inside a menu popup.
///
/// The bodies of the top-bar modules are written as rows sized for a box of a
/// few hundred points. A popup is one narrow column instead, so the same row
/// has to wrap and its sliders have to shrink to what is left. That difference
/// is the whole of `narrow`, which is why it describes the *container* rather
/// than the layout tier: one body, two shapes, and no second copy to drift.
pub fn control_row<R>(ui: &mut Ui, narrow: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    if narrow {
        ui.horizontal_wrapped(|ui| {
            // Leave room for the label and readout that ride beside a slider;
            // the floor keeps a rail draggable even in the narrowest popup.
            ui.spacing_mut().slider_width = (ui.available_width() - 96.0).clamp(80.0, 220.0);
            add(ui)
        })
        .inner
    } else {
        ui.horizontal(add).inner
    }
}

/// A tap-to-open menu popup wearing the app's cut-corner chrome.
///
/// Full opacity and no auto-fade — a menu is read, not glanced at, and on a
/// touch screen there is no hover to hold [`popup_fade_alpha`] open with. It
/// closes on a tap outside or on the chip again.
///
/// Scrolled rather than truncated: egui clamps a popup to the screen but will
/// not scroll it, so a long menu on a phone in landscape would otherwise have
/// its bottom quietly cut off.
pub fn menu_popup<R>(ui: &mut Ui, btn: &Response, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let screen = ui.ctx().content_rect();
    let resp = egui::Popup::from_toggle_button_response(btn)
        .frame(window_frame())
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_max_width(430.0_f32.min(screen.width() - 24.0));
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            egui::ScrollArea::vertical()
                .max_height(screen.height() * 0.6)
                .show_themed(ui, add)
                .inner
        });
    if let Some(r) = &resp {
        paint_window_border(ui.ctx(), &r.response);
    }
    resp.map(|r| r.inner)
}

/// A section caption inside a menu popup — the same small cyan label the
/// module boxes wear, so a menu reads as the box it replaced.
pub fn menu_caption(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).color(theme::CYAN_DIM).size(9.5).strong());
}

/// Small L-shaped corner accents (page decoration, reference-style).
pub fn corner_brackets(p: &Painter, rect: Rect, color: Color32) {
    let len = 16.0;
    let s = Stroke::new(2.0, color);
    let r = rect.shrink(3.0);
    // top-left
    p.line_segment([r.left_top(), r.left_top() + vec2(len, 0.0)], s);
    p.line_segment([r.left_top(), r.left_top() + vec2(0.0, len)], s);
    // bottom-right
    p.line_segment([r.right_bottom(), r.right_bottom() - vec2(len, 0.0)], s);
    p.line_segment([r.right_bottom(), r.right_bottom() - vec2(0.0, len)], s);
}

/// A red-bordered content box (cyberpunk section panel). Fills the available
/// width and draws a red left-accent bar.
pub fn red_panel<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let inner = egui::Frame::new()
        .fill(theme::ROW_BG)
        .stroke(Stroke::new(1.0, theme::RED_DEEP))
        .inner_margin(egui::Margin { left: 9, right: 7, top: 6, bottom: 6 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
    // Red left-accent bar.
    let r = inner.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_max(r.left_top(), pos2(r.left() + 2.5, r.bottom())),
        0.0,
        theme::PINK,
    );
    inner.inner
}

/// A slider with a visible dark track. egui draws the slider rail with
/// `widgets.inactive.bg_fill`, which equals the module background here, so
/// the empty portion of the track would otherwise be invisible.
pub fn slider(ui: &mut Ui, slider: egui::Slider<'_>) -> Response {
    // A fatter rail where the handle is dragged with a finger. Set here rather
    // than in the theme so the handful of raw `Slider`s elsewhere keep theirs.
    let rail = if crate::layout::tier(ui.ctx()).touch() { 12.0 } else { 6.0 };
    ui.scope(|ui| {
        ui.visuals_mut().widgets.inactive.bg_fill = theme::INPUT_BG;
        ui.visuals_mut().widgets.hovered.bg_fill = theme::INPUT_BG;
        ui.spacing_mut().slider_rail_height = rail;
        ui.add(slider)
    })
    .inner
}

/// The draggable divider between two regions of a panel: allocates the strip,
/// shows the resize cursor over it, and paints three grip marks so it reads as
/// something that can be moved. Returns the response for the caller's own drag
/// arithmetic.
///
/// One helper for every splitter in the program because the grip marks are the
/// only thing that says a divider exists at all — a handle that paints nothing
/// until the pointer is already on it is a control nobody finds.
///
/// Orientation comes from the shape: a tall, narrow strip divides two columns
/// (grips stacked down it, horizontal resize cursor); a wide, short one divides
/// two rows. `bg` fills the strip first, for handles that sit on the page
/// background rather than inside a panel.
pub fn split_handle(ui: &mut Ui, size: egui::Vec2, bg: Option<Color32>) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    // A strip taller than it is wide separates left from right.
    let columns = size.y >= size.x;
    let hot = resp.hovered() || resp.dragged();
    if hot {
        ui.ctx().set_cursor_icon(if columns {
            egui::CursorIcon::ResizeHorizontal
        } else {
            egui::CursorIcon::ResizeVertical
        });
    }
    let p = ui.painter_at(rect);
    if let Some(bg) = bg {
        p.rect_filled(rect, 0.0, bg);
    }
    let col = if hot { theme::CYAN } else { Color32::from_gray(70) };
    let (cx, cy) = (rect.center().x, rect.center().y);
    for d in [-16.0f32, 0.0, 16.0] {
        let seg = if columns {
            [pos2(cx, cy + d - 6.0), pos2(cx, cy + d + 6.0)]
        } else {
            [pos2(cx + d - 6.0, cy), pos2(cx + d + 6.0, cy)]
        };
        p.line_segment(seg, Stroke::new(2.0, col));
    }
    resp
}

/// Angled chip: a selectable button with cut top-left and bottom-right corners.
/// Selected chips fill cyan with dark ink, like the reference nav pills.
pub fn chip(ui: &mut Ui, selected: bool, text: impl Into<RichText>) -> Response {
    chip_impl(ui, selected, text.into(), None, Sense::click())
}

/// Chip with an explicit accent fill when selected (e.g. PTT red).
pub fn chip_accent(
    ui: &mut Ui,
    selected: bool,
    text: impl Into<RichText>,
    fill: Color32,
    ink: Color32,
) -> Response {
    chip_impl(ui, selected, text.into(), Some((fill, ink)), Sense::click())
}

/// An accent chip that reports being *held* rather than clicked — for a control
/// that is on only while a finger or a mouse button is on it. Read the result
/// with [`Response::is_pointer_button_down_on`]: it goes false the moment the
/// press ends, including when the pointer is taken away entirely.
pub fn chip_hold(
    ui: &mut Ui,
    selected: bool,
    text: impl Into<RichText>,
    fill: Color32,
    ink: Color32,
) -> Response {
    chip_impl(ui, selected, text.into(), Some((fill, ink)), Sense::click_and_drag())
}

fn chip_impl(
    ui: &mut Ui,
    selected: bool,
    text: RichText,
    accent: Option<(Color32, Color32)>,
    sense: Sense,
) -> Response {
    let galley = WidgetText::from(text).into_galley(
        ui,
        None,
        f32::INFINITY,
        FontSelection::Style(TextStyle::Button),
    );
    // A shade roomier than a plain button, which is what gives a chip its
    // pill-like proportions. Taken from the style rather than fixed so a
    // touched layout's larger `button_padding` grows every chip in the program
    // at once — a chip is the only button this app has.
    let padding = ui.spacing().button_padding + vec2(2.0, 1.0);
    let size = galley.size() + padding * 2.0;
    let (rect, resp) = ui.allocate_exact_size(size, sense);

    if ui.is_rect_visible(rect) {
        let v = ui.style().interact_selectable(&resp, selected);
        let cut = CHIP_CUT.min(size.y * 0.35);
        let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
        // Cut corners on the top-left and bottom-right (matching diagonal).
        let outline = vec![
            pos2(l + cut, t),
            pos2(r, t),
            pos2(r, b - cut),
            pos2(r - cut, b),
            pos2(l, b),
            pos2(l, t + cut),
        ];

        let (fill, stroke, ink) = if selected {
            let (fill, ink) = accent.unwrap_or((theme::CYAN, theme::INK_ON_CYAN));
            (fill, Stroke::new(1.0, fill), ink)
        } else {
            (v.bg_fill, v.bg_stroke, v.fg_stroke.color)
        };
        ui.painter().add(Shape::convex_polygon(outline, fill, stroke));

        let text_pos = Pos2 {
            x: rect.center().x - galley.size().x / 2.0,
            y: rect.center().y - galley.size().y / 2.0,
        };
        ui.painter().galley(text_pos, galley, ink);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay a single module out in a fresh context and return how tall it made
    /// the row, along with whatever height it handed its own content.
    fn module_height(height: f32, flush: bool) -> (f32, f32) {
        let ctx = egui::Context::default();
        let (mut row, mut content) = (0.0, 0.0);
        let _ = ctx.run_ui(Default::default(), |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                let grab = |ui: &mut Ui| content = ui.available_size().y;
                if flush {
                    module_bare_flush_h(ui, 100.0, height, grab);
                } else {
                    module_bare_h(ui, 100.0, height, grab);
                }
                row = ui.min_rect().height();
            });
        });
        (row, content)
    }

    /// The S-meter's box has no frame stroke — it paints its own border inside
    /// its content — so it has to allocate the two points a bordered module
    /// gets for free from the stroke egui draws outside the content it was
    /// handed. Without them the meter stands two points shorter than every box
    /// beside it, which reads as a row out of alignment rather than as rounding.
    #[test]
    fn a_flush_module_is_exactly_as_tall_as_a_bordered_one() {
        for height in [MODULE_H, MODULE_TALL_H, 40.0] {
            let (bordered, _) = module_height(height, false);
            let (flush, content) = module_height(height, true);
            assert_eq!(bordered, flush, "at {height} pt: bordered {bordered}, flush {flush}");
            // And the meter really does get the whole of it to paint in.
            assert_eq!(content, flush, "the flush box handed its content {content} of {flush}");
        }
    }

    /// The figure the box heights are quoted in: a module built to `height`
    /// stands that tall plus its border. If egui ever stops drawing a frame's
    /// stroke outside the content, this is the assumption that breaks.
    #[test]
    fn a_bordered_module_stands_its_height_plus_its_border() {
        let (row, content) = module_height(MODULE_TALL_H, false);
        assert_eq!(row, MODULE_TALL_H + 2.0 * MODULE_BORDER, "outer height moved");
        // Inner margin of 4 above and 5 below, inside the border.
        assert_eq!(content, MODULE_TALL_H - 9.0, "content height moved");
    }
}
