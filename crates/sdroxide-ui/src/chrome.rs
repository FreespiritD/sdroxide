//! Cyberpunk chrome: cut-corner panel frames, angled chip buttons, and
//! corner accents — the shapes egui's rounded-rect widgets can't draw.

use eframe::egui::{
    self, Color32, CornerRadius, FontSelection, Mesh, Painter, Pos2, Rect, Response, RichText,
    Sense, Shape, Stroke, StrokeKind, TextStyle, Ui, WidgetText, pos2, vec2,
};
use sdroxide_types::ChromeStyle;

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
        // Never wider than the window itself, whatever the parent reports. A
        // `Frame`'s outer rect is its content plus its margins, so a child that
        // took all the width there was leaves the parent expanded past the
        // screen edge — and every row measured against *that* wraps too late
        // and has whatever crossed the edge clipped away.
        let a = ui.available_width().min(ui.ctx().content_rect().width());
        if a.is_finite() && a > 50.0 { a } else { ui.ctx().content_rect().width() - 24.0 }
    };
    let margin = 10i8;
    // The Gradient style needs a fill sized to the frame's final rect, which
    // is only known after the content ran — so reserve a slot in the paint
    // list now, leave the frame's own fill transparent, and set the gradient
    // into the slot afterwards, where it renders *under* the content.
    let grad_slot =
        (theme::window_style() == ChromeStyle::Gradient).then(|| ui.painter().add(Shape::Noop));
    let inner = egui::Frame::new()
        .fill(if grad_slot.is_some() { Color32::TRANSPARENT } else { theme::PANEL() })
        .corner_radius(window_corner_radius())
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
    if let Some(slot) = grad_slot {
        ui.painter().set(slot, panel_gradient(inner.response.rect));
    }
    paint_cut_border(ui.painter(), inner.response.rect, accent, theme::BG_DEEP());
    inner.inner
}

/// The Gradient window style's fill for `rect`: the panel colour, lit a touch
/// at the top and falling off dark at the bottom.
fn panel_gradient(rect: Rect) -> Shape {
    let panel = theme::PANEL();
    grad_rect(rect, lerp(panel, Color32::WHITE, 0.06), lerp(panel, Color32::BLACK, 0.45))
}

/// Restyle a floating window's body for the current window style — call it
/// first thing inside the body closure of every `egui::Window` framed with
/// [`window_frame`].
///
/// A no-op for most styles. For Gradient it paints the body gradient (an
/// `egui::Frame` cannot fill with one, and a window's frame belongs to egui,
/// so the body's own first shape is the only slot under the widgets we can
/// reach); Bevel gets a subtle sheen along the top edge. Sized from the
/// body's `max_rect` grown back over [`window_frame`]'s 11 pt margin — on the
/// frame a window is resized its rect lags one frame behind, which egui's own
/// window-size memory makes invisible in practice.
pub fn window_body_bg(ui: &mut Ui) {
    let r = ui.max_rect().expand(11.0);
    match theme::window_style() {
        ChromeStyle::Gradient => {
            ui.painter().add(panel_gradient(r));
        }
        ChromeStyle::Bevel => {
            let mut sheen = r;
            sheen.set_height((r.height() * 0.2).min(26.0));
            ui.painter().add(grad_rect(sheen, Color32::from_white_alpha(14), Color32::TRANSPARENT));
        }
        _ => {}
    }
}

/// Frame for a floating window: flat panel fill with a roomy content margin,
/// shaped by the current window style — square for most styles (the Angled
/// cuts are painted on top afterwards by [`paint_window_border`]), rounded
/// under the Rounded style.
pub fn window_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::PANEL())
        .inner_margin(egui::Margin::same(11))
        .corner_radius(window_corner_radius())
}

/// Paint the pink cut-corner border around a floating window (top-right +
/// bottom-left bevels), matching the main panadapter chrome. Draws on the
/// window's own layer so it sits over the panel fill.
pub fn paint_window_border(ctx: &egui::Context, resp: &Response) {
    let p = ctx.layer_painter(resp.layer_id);
    paint_cut_border(&p, resp.rect, theme::PINK(), theme::PANEL());
}

/// Multiply a colour's alpha by `a` (for fading chrome in/out).
fn fade(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * a.clamp(0.0, 1.0)) as u8)
}

/// Linear blend from `a` to `b`, all four channels. Colours are premultiplied,
/// where a straight per-channel lerp is the correct blend.
fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgba_premultiplied(
        ch(a.r(), b.r()),
        ch(a.g(), b.g()),
        ch(a.b(), b.b()),
        ch(a.a(), b.a()),
    )
}

/// A rect filled with a vertical `top` → `bottom` gradient — the fill the
/// Gradient chrome style uses. egui has no gradient shape of its own, so this
/// is a two-triangle mesh with the colours on the vertices.
fn grad_rect(rect: Rect, top: Color32, bottom: Color32) -> Shape {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    Shape::mesh(mesh)
}

/// The corner radius a Rounded-style window wears. Matches what
/// [`crate::theme::apply_visuals`] gives egui's own windows and menus.
const ROUND_WINDOW_R: u8 = 6;
/// The corner radius a Rounded-style chip wears.
const ROUND_CHIP_R: f32 = 5.0;

/// The corner radius for the current window style — rounded only under
/// `Rounded`; every other style keeps square fills (Angled paints its cuts on
/// top).
fn window_corner_radius() -> CornerRadius {
    match theme::window_style() {
        ChromeStyle::Rounded => CornerRadius::same(ROUND_WINDOW_R),
        _ => CornerRadius::ZERO,
    }
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
    paint_cut_border(&p, resp.rect, fade(theme::PINK(), alpha), fade(theme::PANEL(), alpha));
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

/// The border every panel and floating window wears, in the current window
/// style. Kept under its historic name — it *was* only the cut-corner border,
/// and its ~30 callers all still mean "give this rect the app's frame chrome".
///
/// `mask` is the surrounding background, used only by the Angled style to
/// cover the square fill corners so the cut reads as a real bevel; the other
/// styles get their shape from the frame fill itself.
pub fn paint_cut_border(p: &Painter, rect: Rect, color: Color32, mask: Color32) {
    match theme::window_style() {
        ChromeStyle::Angled => paint_angled_border(p, rect, color, mask),
        ChromeStyle::Rectangular | ChromeStyle::Gradient => {
            p.rect_stroke(rect, CornerRadius::ZERO, Stroke::new(1.2, color), StrokeKind::Inside);
        }
        ChromeStyle::Rounded => {
            p.rect_stroke(
                rect,
                CornerRadius::same(ROUND_WINDOW_R),
                Stroke::new(1.2, color),
                StrokeKind::Inside,
            );
        }
        ChromeStyle::Bevel => {
            // Raised 3D edge: lit where the light falls (top + left), shaded
            // opposite. The hairline accent outline stays so the frame keeps
            // its colour identity from across the room.
            let rr = rect.shrink(1.0);
            let light = Stroke::new(2.0, lerp(color, Color32::WHITE, 0.4));
            let dark = Stroke::new(2.0, lerp(color, Color32::BLACK, 0.55));
            p.line_segment([rr.left_bottom(), rr.left_top()], light);
            p.line_segment([rr.left_top(), rr.right_top()], light);
            p.line_segment([rr.right_top(), rr.right_bottom()], dark);
            p.line_segment([rr.right_bottom(), rr.left_bottom()], dark);
        }
    }
}

/// The classic look: masks the two corners with `mask` and strokes the
/// six-sided outline.
fn paint_angled_border(p: &Painter, rect: Rect, color: Color32, mask: Color32) {
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
        let color = if k % 2 == 0 { theme::YELLOW() } else { dark };
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

/// A module's inner margin on each side. What a caller measuring its own
/// contents has to add on top of them to arrive at the `width` to reserve.
pub const MODULE_MARGIN_X: f32 = 8.0;

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
                .fill(theme::FILL())
                .stroke(Stroke::new(1.0, theme::LINE_LIT()))
                .inner_margin(egui::Margin {
                    left: MODULE_MARGIN_X as i8,
                    right: MODULE_MARGIN_X as i8,
                    top: 4,
                    bottom: 3,
                })
                .show(ui, |ui| {
                    ui.set_width(width - 2.0 * MODULE_MARGIN_X);
                    // Fill the full module height so every box — captioned or
                    // bare — ends up exactly `height` tall.
                    ui.set_min_height(height - 7.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(caption.to_uppercase())
                            .color(theme::CYAN_DIM())
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
                .fill(theme::FILL())
                .stroke(Stroke::new(1.0, theme::LINE_LIT()))
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
                .fill(theme::FILL())
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

/// The trailing items of a header row: pinned to the right where there is room
/// to spare, and simply next in line where there is not.
///
/// A right-to-left child claims what is left of the row and right-aligns inside
/// it. That reads well on a row with slack, and badly on one that has already
/// wrapped — "what is left of the row" is then the sliver beside the items just
/// placed, and the pinned ones get drawn over them. A compact layout wraps
/// nearly every header, so there it stays in flow.
pub fn row_tail<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    if crate::layout::tier(ui.ctx()).compact() {
        add(ui)
    } else {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add).inner
    }
}

/// A tap-to-open menu popup wearing the app's cut-corner chrome.
///
/// Full opacity and no auto-fade — a menu is read, not glanced at, and on a
/// touch screen there is no hover to hold [`popup_fade_alpha`] open with. It
/// closes on a tap outside or on the chip again.
pub fn menu_popup<R>(ui: &mut Ui, btn: &Response, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    popup_body(ui, btn, None, add)
}

/// [`menu_popup`], but it dismisses itself after a while on a pointer layout —
/// for a popup that also opens from a control box on the desktop strip, where
/// nothing else would take it away. `since` is the caller-owned open time
/// [`popup_fade_alpha`] keeps; the fade is off on a touched layout, so there the
/// two helpers behave identically.
pub fn fading_menu_popup<R>(
    ui: &mut Ui,
    btn: &Response,
    since: &mut Option<f64>,
    add: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    popup_body(ui, btn, Some(since), add)
}

/// The body both menu popups share: cut-corner chrome, a width the viewport can
/// hold, and a scrolled content column.
///
/// Sized against the screen rather than the wish, in both directions, because a
/// popup is the one thing here that is not laid out inside a panel:
///
/// - Width comes out of the viewport with the frame's own margins already
///   subtracted. egui constrains a popup's *position* to the screen but cannot
///   shrink one that is too wide, so a content column sized to the full viewport
///   is a popup hanging off the edge of the phone by its margins.
/// - Height is scrolled rather than truncated, for the same reason: a menu
///   taller than a phone in landscape would otherwise have its bottom quietly
///   cut off, with no way to reach it.
///
/// Both sizes are handed to a child `Ui` rather than taken from the popup's
/// own: egui gives an `Area`'s `Ui` the size that area came out as on the
/// *previous* frame, and a `ScrollArea` fills whatever it is offered and
/// scrolls the rest. A menu whose content grew — a layer switched back on, a
/// band appearing — would then be capped at the room it had while it was
/// smaller, and every frame after that re-measures the same cap, so it never
/// grows back. Sizing the body against the screen breaks that ratchet.
fn popup_body<R>(
    ui: &mut Ui,
    btn: &Response,
    mut fade: Option<&mut Option<f64>>,
    add: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let screen = ui.ctx().content_rect();
    let now = ui.input(|i| i.time);
    let alpha = match fade.as_deref_mut() {
        Some(since) => {
            popup_fade_alpha(ui.ctx(), egui::Popup::default_response_id(btn), now, since)
        }
        None => 1.0,
    };
    // 24 = the frame's 11 pt inner margin either side, plus a couple of points
    // so the cut-corner border is not flush against the screen edge.
    let max_w = (screen.width() - 24.0).clamp(160.0, 430.0);
    let max_h = screen.height() * 0.6;
    let resp = egui::Popup::from_toggle_button_response(btn)
        .frame(window_frame_alpha(alpha))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_opacity(alpha);
            window_body_bg(ui);
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            let body = Rect::from_min_size(ui.max_rect().min, vec2(max_w, max_h));
            ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                egui::ScrollArea::vertical().max_height(max_h).show_themed(ui, add).inner
            })
            .inner
        });
    if let Some(r) = &resp {
        paint_popup_cut_border(ui.ctx(), &r.response, alpha);
        // Hovering the popup keeps it up: the fade is for a menu left open and
        // forgotten, not one being read.
        if r.response.contains_pointer()
            && let Some(since) = fade
        {
            *since = Some(now);
        }
    }
    resp.map(|r| r.inner)
}

/// A section caption inside a menu popup — the same small cyan label the
/// module boxes wear, so a menu reads as the box it replaced.
pub fn menu_caption(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).color(theme::CYAN_DIM()).size(9.5).strong());
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
        .fill(theme::ROW_BG())
        .stroke(Stroke::new(1.0, theme::RED_DEEP()))
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
        theme::PINK(),
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
        ui.visuals_mut().widgets.inactive.bg_fill = theme::INPUT_BG();
        ui.visuals_mut().widgets.hovered.bg_fill = theme::INPUT_BG();
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
    let col = if hot { theme::CYAN() } else { Color32::from_gray(70) };
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

/// How wide `text` lays out in `font`.
pub fn text_width(ui: &Ui, text: &str, font: egui::FontId) -> f32 {
    ui.painter().layout_no_wrap(text.to_owned(), font, Color32::WHITE).size().x
}

/// The font a chip uses for `size` points of text, or for its default.
fn chip_font(ui: &Ui, size: Option<f32>) -> egui::FontId {
    match size {
        Some(pt) => egui::FontId::proportional(pt),
        None => TextStyle::Button.resolve(ui.style()),
    }
}

/// What a chip carrying `label` will measure, padding included — the same
/// arithmetic [`chip`] does, for a caller that has to budget a row before
/// drawing it. `size` is the text size where the chip sets one.
pub fn chip_width(ui: &Ui, label: &str, size: Option<f32>) -> f32 {
    text_width(ui, label, chip_font(ui, size)) + 2.0 * chip_padding(ui).x
}

/// How tall a chip stands, padding included. A literal would be wrong the
/// moment the layout changes: chip padding comes from the style, and a touched
/// one is roomier.
pub fn chip_height(ui: &Ui, size: Option<f32>) -> f32 {
    ui.painter().layout_no_wrap("0".to_owned(), chip_font(ui, size), Color32::WHITE).size().y
        + 2.0 * chip_padding(ui).y
}

/// A chip's padding. A shade roomier than a plain button, which is what gives a
/// chip its pill-like proportions. Taken from the style rather than fixed so a
/// touched layout's larger `button_padding` grows every chip in the program at
/// once — a chip is the only button this app has.
fn chip_padding(ui: &Ui) -> egui::Vec2 {
    ui.spacing().button_padding + vec2(2.0, 1.0)
}

/// Angled chip: a selectable button with cut top-left and bottom-right corners.
/// Selected chips fill cyan with dark ink, like the reference nav pills.
pub fn chip(ui: &mut Ui, selected: bool, text: impl Into<RichText>) -> Response {
    chip_impl(ui, selected, text.into(), None, Sense::click(), None)
}

/// A chip stretched to an exact `size` rather than hugging its label — for the
/// compact strip's button grid, whose rows divide the width they were given
/// between them instead of clustering at one end of it. The label stays
/// centred; everything else matches [`chip`].
pub fn chip_sized(
    ui: &mut Ui,
    selected: bool,
    text: impl Into<RichText>,
    size: egui::Vec2,
) -> Response {
    chip_impl(ui, selected, text.into(), None, Sense::click(), Some(size))
}

/// [`chip_hold`] at an exact size — the compact strip's PTT, which is drawn
/// bigger than its label needs because it is the one control worth a whole
/// thumb.
pub fn chip_hold_sized(
    ui: &mut Ui,
    selected: bool,
    text: impl Into<RichText>,
    fill: Color32,
    ink: Color32,
    size: egui::Vec2,
) -> Response {
    chip_impl(ui, selected, text.into(), Some((fill, ink)), Sense::click_and_drag(), Some(size))
}

/// A chip that may be greyed out, in a row that is allowed to wrap.
///
/// `Ui::add_enabled_ui` builds a child `Ui`, and a child `Ui` inside a
/// `horizontal_wrapped` row does not wrap: it is measured first and placed
/// afterwards with `allocate_rect`, which never consults the wrapping placer.
/// The row then simply grows past the edge it should have broken at — which is
/// how the band row of the band/mode menu came to be one 870 pt line, wider
/// than any phone it opened on. Reserving the chip's own size up front puts the
/// wrap decision back where the layout can make it, and the child then fits
/// exactly inside what was reserved.
pub fn chip_enabled(ui: &mut Ui, enabled: bool, selected: bool, label: &str) -> Response {
    let size = vec2(chip_width(ui, label, None), chip_height(ui, None));
    ui.allocate_ui(size, |ui| ui.add_enabled_ui(enabled, |ui| chip(ui, selected, label)).inner)
        .inner
}

/// [`chip_enabled`], with the label carrying a colour while it is unselected,
/// and an optional accent line along the chip's bottom edge.
///
/// For a row where each chip has a status of its own — the band menu, where
/// every band has published conditions — and the reader is choosing between
/// them. Only while unselected: a selected chip is already filled with the
/// accent, and a second colour inside it would be read as a different state
/// rather than the same one. `None` leaves the chip exactly as it was, which
/// is what a band with nothing published must look like.
///
/// The underline marks a chip with something to offer beyond its neighbours —
/// the band menu draws it under the bands where the current mode has a
/// standard frequency defined, against the ones a click merely takes to the
/// band. Drawn inside the chip rather than under it, so a wrapped row's
/// spacing does not have to make room for it, and stopping short of the cut
/// bottom-right corner so it follows the chip's own outline. On a selected
/// chip it vanishes into the accent fill, which reads correctly: selection
/// already says the chip has been taken up on the offer.
pub fn chip_enabled_tinted(
    ui: &mut Ui,
    enabled: bool,
    selected: bool,
    label: &str,
    tint: Option<Color32>,
    underline: bool,
) -> Response {
    let size = vec2(chip_width(ui, label, None), chip_height(ui, None));
    let text = match tint.filter(|_| !selected) {
        Some(c) => RichText::new(label).color(c),
        None => RichText::new(label),
    };
    let resp = ui
        .allocate_ui(size, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                chip_impl(ui, selected, text, None, Sense::click(), None)
            })
            .inner
        })
        .inner;
    if underline {
        let r = resp.rect;
        // Stop short of the cut corner so the line follows the chip's own
        // outline — only the Angled style has one to dodge.
        let cut = match theme::button_style() {
            ChromeStyle::Angled => CHIP_CUT.min(r.height() * 0.35),
            _ => 0.0,
        };
        let y = r.bottom() - 2.0;
        ui.painter().line_segment(
            [pos2(r.left() + 2.0, y), pos2(r.right() - cut, y)],
            Stroke::new(2.0, theme::CYAN()),
        );
    }
    resp
}

/// Chip with an explicit accent fill when selected (e.g. PTT red).
pub fn chip_accent(
    ui: &mut Ui,
    selected: bool,
    text: impl Into<RichText>,
    fill: Color32,
    ink: Color32,
) -> Response {
    chip_impl(ui, selected, text.into(), Some((fill, ink)), Sense::click(), None)
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
    chip_impl(ui, selected, text.into(), Some((fill, ink)), Sense::click_and_drag(), None)
}

/// The classic chip shape: corners cut on the top-left and bottom-right (the
/// matching diagonal to the frames' top-right/bottom-left).
fn chip_outline(rect: Rect, cut: f32) -> Vec<Pos2> {
    let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
    vec![
        pos2(l + cut, t),
        pos2(r, t),
        pos2(r, b - cut),
        pos2(r - cut, b),
        pos2(l, b),
        pos2(l, t + cut),
    ]
}

fn chip_impl(
    ui: &mut Ui,
    selected: bool,
    text: RichText,
    accent: Option<(Color32, Color32)>,
    sense: Sense,
    exact: Option<egui::Vec2>,
) -> Response {
    let galley = WidgetText::from(text).into_galley(
        ui,
        None,
        f32::INFINITY,
        FontSelection::Style(TextStyle::Button),
    );
    let padding = chip_padding(ui);
    let size = exact.unwrap_or(galley.size() + padding * 2.0);
    let (rect, resp) = ui.allocate_exact_size(size, sense);

    if ui.is_rect_visible(rect) {
        let v = ui.style().interact_selectable(&resp, selected);

        let (fill, stroke, ink) = if selected {
            let (fill, ink) = accent.unwrap_or((theme::CYAN(), theme::INK_ON_CYAN()));
            (fill, Stroke::new(1.0, fill), ink)
        } else {
            (v.bg_fill, v.bg_stroke, v.fg_stroke.color)
        };

        let p = ui.painter();
        match theme::button_style() {
            ChromeStyle::Angled => {
                let cut = CHIP_CUT.min(size.y * 0.35);
                p.add(Shape::convex_polygon(chip_outline(rect, cut), fill, stroke));
            }
            ChromeStyle::Rectangular => {
                p.rect(rect, CornerRadius::ZERO, fill, stroke, StrokeKind::Inside);
            }
            ChromeStyle::Rounded => {
                let radius = ROUND_CHIP_R.min(size.y * 0.35) as u8;
                p.rect(rect, CornerRadius::same(radius), fill, stroke, StrokeKind::Inside);
            }
            ChromeStyle::Gradient => {
                p.add(grad_rect(
                    rect,
                    lerp(fill, Color32::WHITE, 0.22),
                    lerp(fill, Color32::BLACK, 0.30),
                ));
                p.rect_stroke(rect, CornerRadius::ZERO, stroke, StrokeKind::Inside);
            }
            ChromeStyle::Bevel => {
                p.rect(rect, CornerRadius::ZERO, fill, stroke, StrokeKind::Inside);
                let rr = rect.shrink(0.75);
                let light = Stroke::new(1.5, lerp(fill, Color32::WHITE, 0.35));
                let dark = Stroke::new(1.5, lerp(fill, Color32::BLACK, 0.5));
                p.line_segment([rr.left_bottom(), rr.left_top()], light);
                p.line_segment([rr.left_top(), rr.right_top()], light);
                p.line_segment([rr.right_top(), rr.right_bottom()], dark);
                p.line_segment([rr.right_bottom(), rr.left_bottom()], dark);
            }
        }

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

    /// Stand-in for a menu with more in it than any phone can show at once.
    fn a_long_menu(ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            for i in 0..60 {
                chip(ui, false, format!("CHIP{i}"));
            }
        });
    }

    /// Open a menu popup from a chip on a `screen`-sized viewport and return the
    /// rect it took.
    fn menu_popup_rect(screen: egui::Vec2) -> Rect {
        let ctx = egui::Context::default();
        let tier = crate::layout::tier_for(screen, sdroxide_types::LayoutMode::Auto);
        crate::layout::set_tier(&ctx, tier);
        theme::apply_metrics(&ctx, tier);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)),
            ..Default::default()
        };
        // First pass to give the chip an id, then open its popup and lay it out.
        let mut id = None;
        let _ = ctx.run_ui(input(), |ui| {
            let btn = chip(ui, false, "MENU");
            id = Some(egui::Popup::default_response_id(&btn));
            menu_popup(ui, &btn, a_long_menu);
        });
        let id = id.expect("the chip was drawn");
        egui::Popup::open_id(&ctx, id);
        let _ = ctx.run_ui(input(), |ui| {
            let btn = chip(ui, false, "MENU");
            menu_popup(ui, &btn, a_long_menu);
        });
        ctx.memory(|m| m.area_rect(id)).expect("the popup was shown")
    }

    /// A popup is the one thing here not laid out inside a panel: egui will move
    /// one that lands off the edge, but it cannot shrink one that is too big for
    /// the screen — it simply hangs off it, and the part that hangs off cannot
    /// be reached at all. Every menu therefore has to come out no larger than
    /// the phone it opened on, however much content it was handed.
    #[test]
    fn a_menu_popup_fits_the_screen_it_opens_on() {
        // Phones, portrait and landscape, plus a tablet for company.
        for screen in
            [vec2(360.0, 800.0), vec2(393.0, 852.0), vec2(852.0, 393.0), vec2(768.0, 1024.0)]
        {
            let r = menu_popup_rect(screen);
            assert!(r.width() <= screen.x, "{screen:?}: popup {} pt wide", r.width());
            assert!(r.height() <= screen.y, "{screen:?}: popup {} pt tall", r.height());
            assert!(
                r.left() >= 0.0 && r.right() <= screen.x,
                "{screen:?}: popup spans {}..{}",
                r.left(),
                r.right()
            );
        }
    }

    /// The Angled chip outline is the app's signature shape and must never
    /// drift: these are the six points [`chip_impl`] has always painted.
    #[test]
    fn angled_chip_outline_is_the_historic_shape() {
        let rect = Rect::from_min_max(pos2(10.0, 20.0), pos2(80.0, 44.0));
        let cut = CHIP_CUT; // 24 pt tall, so the 0.35 height cap does not bite
        assert_eq!(
            chip_outline(rect, cut),
            vec![
                pos2(15.0, 20.0),
                pos2(80.0, 20.0),
                pos2(80.0, 39.0),
                pos2(75.0, 44.0),
                pos2(10.0, 44.0),
                pos2(10.0, 25.0),
            ]
        );
    }
}
