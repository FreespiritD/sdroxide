//! The SSTV and RIFP image panel.
//!
//! [`SstvUi`] holds everything the panel draws that is not engine state: the
//! gallery of received images, the four transmit slots with their overlay
//! messages, and the textures for both. Received scanlines arrive one at a
//! time and are painted into a texture as they come, so a picture builds up on
//! screen exactly as it does on the air.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use sdroxide_types::{
    Command, Mode, RifpEncoding, RifpMeta, RifpProfile, RifpSize, RifpStatus, SstvMode, SstvStatus,
};

use crate::theme::ThemedScroll;

use crate::app::SdroxideApp;
use crate::app::panels::widgets::{pick_image, sstv_section};
use crate::app::util::shorten;

// ───────────────────────────── SSTV panel ──────────────────────────────

/// A transmit-image slot: the (bounded) source picture plus its thumbnail.
pub(in crate::app) struct SstvSlot {
    src_rgb: Vec<u8>,
    sw: u16,
    sh: u16,
    tex: egui::TextureHandle,
}

/// A received-image gallery entry.
#[allow(dead_code)] // not used on wasm
pub(in crate::app) struct SstvRecv {
    mode: Option<SstvMode>,
    /// Where a RIFP picture came from and how it was carried, for the caption
    /// under the enlarged view. `None` for SSTV, which carries no metadata.
    rifp: Option<RifpMeta>,
    tex: egui::TextureHandle,
}

/// Image-panel state, shared by SSTV and RIFP: received gallery, in-progress
/// incoming picture, transmit slots, the overlay message, the current mode, and
/// cached textures.
///
/// One workspace for both modes on purpose. The pictures an operator wants to
/// send, the captions on them, and the pictures that came back are the same
/// things whichever protocol carried them; only the control strip and the
/// transmit sizing differ.
pub(in crate::app) struct SstvUi {
    pub(in crate::app) tx_mode: SstvMode,
    /// Latest RIFP engine status (transfer progress, sessions, counters).
    pub(in crate::app) rifp: RifpStatus,
    /// Size of the picture currently arriving, so the live canvas can be built
    /// before the whole object is in. `(0, 0)` when nothing is arriving.
    pub(in crate::app) rx_dims: (u16, u16),
    /// Size the cached preview was composed at, so a change of transmit size
    /// rebuilds it.
    pub(in crate::app) preview_dims: (u16, u16),
    /// Operator callsign for the transmit-image header (mirrors the digi config).
    pub(in crate::app) callsign: String,
    /// Auto mode: RX auto-detects the mode; TX defaults to Martin 1 until a mode
    /// is heard or the operator picks one.
    pub(in crate::app) auto: bool,
    /// Overlay message per image slot (index-aligned with `slots`). The message
    /// box edits the entry for `selected_slot`, so switching slots swaps the
    /// text — and each is persisted alongside its picture.
    pub(in crate::app) slot_messages: Vec<String>,
    pub(in crate::app) slots: Vec<Option<SstvSlot>>,
    pub(in crate::app) selected_slot: usize,
    pub(in crate::app) received: Vec<SstvRecv>,
    /// In-progress incoming image (painted line-by-line).
    pub(in crate::app) rx_color: Option<egui::ColorImage>,
    pub(in crate::app) rx_tex: Option<egui::TextureHandle>,
    pub(in crate::app) rx_id: u32,
    pub(in crate::app) status: SstvStatus,
    /// Received-gallery index currently shown enlarged in an overlay window.
    pub(in crate::app) enlarged: Option<usize>,
    /// Last VIS/free-run-detected mode we auto-applied to `tx_mode`, so a steady
    /// detection doesn't keep overriding the operator's manual mode choice.
    pub(in crate::app) last_detected: Option<SstvMode>,
    pub(in crate::app) preview_tex: Option<egui::TextureHandle>,
    pub(in crate::app) preview_dirty: bool,
    pub(in crate::app) loaded_disk: bool,
    /// File-picker result inbox (raw image bytes), filled by the picker task.
    pub(in crate::app) inbox: Arc<Mutex<Option<Vec<u8>>>>,
    pub(in crate::app) pick_target: Option<usize>,
}

impl Default for SstvUi {
    fn default() -> Self {
        SstvUi {
            tx_mode: SstvMode::Martin1,
            rifp: RifpStatus::default(),
            rx_dims: (0, 0),
            preview_dims: (0, 0),
            callsign: String::new(),
            auto: true,
            slot_messages: vec![String::new(); 5],
            slots: (0..5).map(|_| None).collect(),
            selected_slot: 0,
            received: Vec::new(),
            rx_color: None,
            rx_tex: None,
            rx_id: 0,
            status: SstvStatus::default(),
            enlarged: None,
            last_detected: None,
            preview_tex: None,
            preview_dirty: true,
            loaded_disk: false,
            inbox: Arc::new(Mutex::new(None)),
            pick_target: None,
        }
    }
}

impl SstvUi {
    /// A decoded scanline arrived: paint it into the in-progress image.
    pub(in crate::app) fn on_line(&mut self, id: u32, y: u16, rgb: &[u8], ctx: &egui::Context) {
        let Some(mode) = self.status.detected else { return };
        let (w, h) = mode.dimensions();
        if self.rx_id != id || self.rx_color.is_none() {
            self.rx_id = id;
            self.rx_color =
                Some(crate::sstv::color_image(&vec![0u8; w as usize * h as usize * 3], w, h));
        }
        let Some(ci) = self.rx_color.as_mut() else { return };
        let (w, h) = (w as usize, h as usize);
        if (y as usize) < h && rgb.len() >= w * 3 {
            let row = y as usize * w;
            for x in 0..w {
                ci.pixels[row + x] = Color32::from_rgb(rgb[x * 3], rgb[x * 3 + 1], rgb[x * 3 + 2]);
            }
        }
        self.rx_tex = Some(ctx.load_texture("sstv_rx", ci.clone(), egui::TextureOptions::NEAREST));
    }

    /// A completed image arrived: decode and add it to the gallery.
    pub(in crate::app) fn on_image(
        &mut self,
        _id: u32,
        mode: SstvMode,
        _w: u16,
        _h: u16,
        png: &[u8],
        ctx: &egui::Context,
    ) {
        if let Some((rgb, w, h)) = crate::sstv::decode_image(png) {
            let ci = crate::sstv::color_image(&rgb, w, h);
            let tex = ctx.load_texture("sstv_recv", ci, egui::TextureOptions::NEAREST);
            self.received.insert(0, SstvRecv { mode: Some(mode), rifp: None, tex });
            self.received.truncate(60);
        }
        self.rx_color = None;
        self.rx_tex = None;
    }

    /// RIFP: reassembled raster rows arrived — paint them into the live
    /// picture. Only the unencoded raster gets here; everything else appears
    /// whole in [`SstvUi::on_rifp_image`].
    pub(in crate::app) fn on_rifp_rows(
        &mut self,
        id: u32,
        y: u16,
        w: u16,
        h: u16,
        gray: &[u8],
        ctx: &egui::Context,
    ) {
        if self.rx_id != id || self.rx_color.is_none() || self.rx_dims != (w, h) {
            self.rx_id = id;
            self.rx_dims = (w, h);
            self.rx_color =
                Some(crate::sstv::color_image(&vec![0u8; w as usize * h as usize * 3], w, h));
        }
        let Some(ci) = self.rx_color.as_mut() else { return };
        let (wu, hu) = (w as usize, h as usize);
        for (row, pixels) in gray.chunks_exact(wu).enumerate() {
            let y = y as usize + row;
            if y >= hu {
                break;
            }
            for (x, &g) in pixels.iter().enumerate() {
                ci.pixels[y * wu + x] = Color32::from_gray(g);
            }
        }
        self.rx_tex = Some(ctx.load_texture("rifp_rx", ci.clone(), egui::TextureOptions::NEAREST));
    }

    /// RIFP: a complete, digest-verified picture arrived.
    pub(in crate::app) fn on_rifp_image(
        &mut self,
        _id: u32,
        meta: RifpMeta,
        png: &[u8],
        ctx: &egui::Context,
    ) {
        if let Some((rgb, w, h)) = crate::sstv::decode_image(png) {
            let ci = crate::sstv::color_image(&rgb, w, h);
            let tex = ctx.load_texture("rifp_recv", ci, egui::TextureOptions::NEAREST);
            self.received.insert(0, SstvRecv { mode: None, rifp: Some(meta), tex });
            self.received.truncate(60);
        }
        self.rx_color = None;
        self.rx_tex = None;
        self.rx_dims = (0, 0);
    }

    /// The overlay message for the slot currently being edited.
    pub(in crate::app) fn current_message(&self) -> &str {
        self.slot_messages.get(self.selected_slot).map(String::as_str).unwrap_or("")
    }

    /// Persist the per-slot overlay messages to the config file (native only).
    pub(in crate::app) fn save_messages(&self) {
        sstv_save_messages(&self.slot_messages);
    }

    /// Rebuild the transmit preview when the size, slot, or message changed.
    /// `dims` is the transmitted picture size — the SSTV line format's, or the
    /// operator's chosen RIFP size.
    pub(in crate::app) fn ensure_preview(&mut self, dims: (u16, u16), ctx: &egui::Context) {
        if !self.preview_dirty {
            return;
        }
        self.preview_dirty = false;
        let message = self.current_message().to_string();
        match self.slots.get(self.selected_slot).and_then(|s| s.as_ref()) {
            Some(slot) => {
                let (rgb, w, h) = crate::sstv::compose(
                    dims.0,
                    dims.1,
                    &slot.src_rgb,
                    slot.sw,
                    slot.sh,
                    &message,
                    &self.callsign,
                );
                let ci = crate::sstv::color_image(&rgb, w, h);
                self.preview_tex =
                    Some(ctx.load_texture("sstv_preview", ci, egui::TextureOptions::NEAREST));
            }
            None => self.preview_tex = None,
        }
    }

    /// The composed PNG for the current selection, for transmit.
    pub(in crate::app) fn compose_png(&self, dims: (u16, u16)) -> Option<Vec<u8>> {
        let slot = self.slots.get(self.selected_slot).and_then(|s| s.as_ref())?;
        let (rgb, w, h) = crate::sstv::compose(
            dims.0,
            dims.1,
            &slot.src_rgb,
            slot.sw,
            slot.sh,
            self.current_message(),
            &self.callsign,
        );
        crate::sstv::encode_png(&rgb, w, h)
    }

    /// Accept a picked image file into `slot`, building a thumbnail texture.
    pub(in crate::app) fn set_slot(&mut self, slot: usize, bytes: &[u8], ctx: &egui::Context) {
        let Some((rgb, w, h)) = crate::sstv::load_source_bounded(bytes, 1024) else { return };
        let ci = crate::sstv::color_image(&rgb, w, h);
        let tex = ctx.load_texture("sstv_slot", ci, egui::TextureOptions::LINEAR);
        if let Some(cell) = self.slots.get_mut(slot) {
            *cell = Some(SstvSlot { src_rgb: rgb, sw: w, sh: h, tex });
        }
        self.selected_slot = slot;
        self.preview_dirty = true;
        sstv_save_slot(slot, bytes);
    }
}

// ── Disk persistence (native only) ──

#[cfg(not(target_arch = "wasm32"))]
fn sstv_save_slot(i: usize, png_bytes: &[u8]) {
    if let Ok(dir) = sdroxide_config::sstv_tx_dir() {
        let _ = std::fs::write(dir.join(format!("slot{i}.png")), png_bytes);
    }
}

#[cfg(target_arch = "wasm32")]
fn sstv_save_slot(_i: usize, _png_bytes: &[u8]) {}

#[cfg(not(target_arch = "wasm32"))]
fn sstv_save_messages(messages: &[String]) {
    let _ = sdroxide_config::save_sstv_messages(messages);
}

#[cfg(target_arch = "wasm32")]
fn sstv_save_messages(_messages: &[String]) {}

#[cfg(not(target_arch = "wasm32"))]
fn sstv_load_messages() -> Vec<String> {
    sdroxide_config::load_sstv_messages()
}

#[cfg(target_arch = "wasm32")]
fn sstv_load_messages() -> Vec<String> {
    Vec::new()
}

#[cfg(not(target_arch = "wasm32"))]
fn sstv_load_slots() -> Vec<Option<(Vec<u8>, u16, u16)>> {
    let mut out = Vec::new();
    let dir = match sdroxide_config::sstv_tx_dir() {
        Ok(d) => d,
        Err(_) => return (0..5).map(|_| None).collect(),
    };
    for i in 0..5 {
        let entry = std::fs::read(dir.join(format!("slot{i}.png")))
            .ok()
            .and_then(|b| crate::sstv::load_source_bounded(&b, 1024));
        out.push(entry);
    }
    out
}

#[cfg(target_arch = "wasm32")]
fn sstv_load_slots() -> Vec<Option<(Vec<u8>, u16, u16)>> {
    (0..5).map(|_| None).collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn sstv_load_gallery() -> Vec<(Vec<u8>, u16, u16)> {
    let mut entries: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(dir) = sdroxide_config::sstv_rx_dir() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("png") {
                    entries.push(p);
                }
            }
        }
    }
    // Newest first by filename (timestamps), cap the count.
    entries.sort();
    entries.reverse();
    entries.truncate(40);
    entries
        .into_iter()
        .filter_map(|p| std::fs::read(&p).ok().and_then(|b| crate::sstv::decode_image(&b)))
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn sstv_load_gallery() -> Vec<(Vec<u8>, u16, u16)> {
    Vec::new()
}

/// One incoming RIFP transfer's chunk map: a lit cell per chunk received, dark
/// per chunk still missing. Beyond what fits, it degrades to a plain bar — the
/// point is to see *where* the holes are, and a thousand one-pixel cells show
/// nothing.
fn rifp_chunk_map(ui: &mut egui::Ui, session: &sdroxide_types::RifpSession) {
    let cells = session.total.max(session.have) as usize;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(120.0, 10.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, Color32::from_gray(20));
    let have = |i: usize| session.map.get(i / 8).is_some_and(|b| b >> (i % 8) & 1 != 0);
    if cells > 0 && cells <= rect.width() as usize {
        let cw = rect.width() / cells as f32;
        for i in 0..cells {
            if !have(i) {
                continue;
            }
            let x = rect.left() + i as f32 * cw;
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(cw.max(1.0), 10.0)),
                0.0,
                crate::theme::GREEN,
            );
        }
    } else if session.total > 0 {
        let mut fill = rect;
        fill.set_width(rect.width() * (session.have as f32 / session.total as f32).clamp(0.0, 1.0));
        p.rect_filled(fill, 2.0, crate::theme::GREEN);
    }
    p.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, Color32::from_gray(60)),
        egui::StrokeKind::Inside,
    );
    resp.on_hover_text("Chunks received (lit) and still missing (dark)");
}

fn sstv_level_bar(ui: &mut egui::Ui, level: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(90.0, 10.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, Color32::from_gray(20));
    // Log scale (~ -60..0 dBFS mean-abs) so weak-but-decodable signals still show.
    let db = 20.0 * level.max(1e-6).log10();
    let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let mut fill = rect;
    fill.set_width(rect.width() * frac);
    let col = if frac > 0.06 { crate::theme::GREEN } else { Color32::from_gray(45) };
    p.rect_filled(fill, 2.0, col);
    p.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, Color32::from_gray(60)),
        egui::StrokeKind::Inside,
    );
}

impl SdroxideApp {
    /// The image panel, shared by SSTV and RIFP: a live picture and a gallery
    /// on the left, a transmit compositor on the right, and a control strip
    /// that is the only part either mode owns alone.
    pub(in crate::app) fn image_panel(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        mode: Mode,
    ) {
        let ctx = ui.ctx().clone();
        let rifp = mode.is_rifp();
        self.sstv_load_disk_once(&ctx);
        // Drain a completed file-pick (only consume the target once bytes arrive).
        let picked = self.sstv.inbox.lock().ok().and_then(|mut g| g.take());
        if let Some(bytes) = picked {
            if let Some(target) = self.sstv.pick_target.take() {
                self.sstv.set_slot(target, &bytes, &ctx);
            }
        }
        // Keep the header callsign in sync with the operator config.
        if self.sstv.callsign != self.digi_cfg_edit.my_call {
            self.sstv.callsign = self.digi_cfg_edit.my_call.clone();
            self.sstv.preview_dirty = true;
        }
        // The transmitted size: SSTV's line format fixes it, RIFP leaves it to
        // the operator. Changing it invalidates the composed preview.
        let dims = if rifp {
            self.digi_cfg_edit.rifp_size.dimensions()
        } else {
            self.sstv.tx_mode.dimensions()
        };
        if self.sstv.preview_dims != dims {
            self.sstv.preview_dims = dims;
            self.sstv.preview_dirty = true;
        }
        self.sstv.ensure_preview(dims, &ctx);
        ctx.request_repaint_after(Duration::from_millis(120));

        let st = self.sstv.status;
        let (signal, tx_active, progress) = if rifp {
            (self.sstv.rifp.signal, self.sstv.rifp.tx_active, self.sstv.rifp.tx_progress)
        } else {
            (st.signal, st.tx_active, st.progress)
        };

        // Whole-panel size. The mode/signal/slant controls sit in a boxed strip
        // on the left above LIVE + RECEIVED; the transmit compositor spans the
        // full height on the right, reclaiming the space the old full-width
        // control rows used to leave empty at the top.
        let avail = ui.available_size();
        let full_h = avail.y;
        let handle_w = 7.0;
        // TRANSMIT (send) column takes a user-draggable fraction of the width; the
        // receive side (LIVE + RECEIVED) gets the rest. Each keeps a usable minimum.
        let tx_w = (avail.x * self.view.sstv_tx_fraction)
            .clamp(300.0, (avail.x - handle_w - 300.0).max(300.0));
        let left_w = (avail.x - tx_w - handle_w).max(300.0);
        // LIVE takes the rest of the receive side; the RECEIVED gallery width is a
        // user-draggable fraction of it (min one thumbnail column).
        let gallery_w = (left_w * self.view.sstv_gallery_fraction)
            .clamp(150.0, (left_w - handle_w - 160.0).max(150.0));
        let live_w = (left_w - gallery_w - handle_w).max(160.0);

        ui.horizontal_top(|ui| {
            // A received thumbnail was clicked → enlarge it (applied after the row).
            let mut enlarge: Option<usize> = None;

            // ── LEFT: boxed controls, then LIVE + RECEIVED ──
            ui.allocate_ui_with_layout(
                egui::vec2(left_w, full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::Frame::new()
                        .fill(crate::theme::ROW_BG)
                        .stroke(egui::Stroke::new(1.0, crate::theme::LINE_LIT))
                        .inner_margin(egui::Margin { left: 8, right: 8, top: 6, bottom: 7 })
                        .show(ui, |ui| {
                            ui.set_min_width(left_w - 16.0);
                            ui.set_max_width(left_w - 16.0);
                            if rifp {
                                self.rifp_controls(ui, cmds);
                                return;
                            }

                            // Mode selection: Auto + the per-mode chips.
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new("SSTV")
                                        .size(12.0)
                                        .strong()
                                        .color(crate::theme::CYAN),
                                );
                                self.digi_freq_chip(ui, cmds);
                                let auto_label = if self.sstv.auto {
                                    format!("Auto ({})", self.sstv.tx_mode.label())
                                } else {
                                    "Auto".to_string()
                                };
                                if crate::chrome::chip(ui, self.sstv.auto, &auto_label).clicked() {
                                    self.sstv.auto = true;
                                    self.sstv.tx_mode = SstvMode::Martin1;
                                    self.sstv.preview_dirty = true;
                                    cmds.push(Command::SstvSetMode(None));
                                }
                                for m in SstvMode::ALL {
                                    let active = !self.sstv.auto && self.sstv.tx_mode == m;
                                    if crate::chrome::chip(ui, active, m.label()).clicked() {
                                        self.sstv.auto = false;
                                        self.sstv.tx_mode = m;
                                        self.sstv.preview_dirty = true;
                                        cmds.push(Command::SstvSetMode(Some(m)));
                                    }
                                }
                            });
                            ui.add_space(5.0);

                            // Signal meter + activity, and the TX-slant trim.
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("Signal").size(10.0).weak());
                                sstv_level_bar(ui, signal);
                                if tx_active {
                                    ui.label(
                                        RichText::new(format!("● TX {:.0}%", progress * 100.0))
                                            .size(11.0)
                                            .strong()
                                            .color(crate::theme::PINK),
                                    );
                                } else if st.rx_active {
                                    ui.label(
                                        RichText::new(format!("● RX {:.0}%", st.progress * 100.0))
                                            .size(11.0)
                                            .strong()
                                            .color(crate::theme::GREEN),
                                    );
                                } else if let Some(m) = st.detected {
                                    ui.label(
                                        RichText::new(format!("last: {}", m.label()))
                                            .size(10.0)
                                            .weak(),
                                    );
                                } else {
                                    ui.label(RichText::new("listening…").size(10.0).weak());
                                }

                                ui.add_space(12.0);
                                ui.separator();
                                ui.label(RichText::new("TX slant").size(10.0).weak()).on_hover_text(
                                    "Transmit clock trim (ppm) to remove slant on the far-end decoder",
                                );
                                ui.add_enabled_ui(self.digi_cfg_seeded, |ui| {
                                    ui.spacing_mut().slider_width = 130.0;
                                    let resp = ui.add(
                                        egui::Slider::new(
                                            &mut self.digi_cfg_edit.sstv_tx_ppm,
                                            -5000.0..=5000.0,
                                        )
                                        .suffix(" ppm")
                                        .fixed_decimals(0),
                                    );
                                    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                                        cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
                                    }
                                    if ui
                                        .small_button("0")
                                        .on_hover_text("Reset to 0 ppm")
                                        .clicked()
                                    {
                                        self.digi_cfg_edit.sstv_tx_ppm = 0.0;
                                        cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
                                    }
                                });
                            });
                        });
                    ui.add_space(6.0);

                    // LIVE + RECEIVED fill the remaining height of the left column.
                    let row_h = ui.available_height().max(160.0);
                    ui.horizontal_top(|ui| {
                        // LIVE: the picture currently decoding, shown large.
                        sstv_section(ui, "LIVE", egui::vec2(live_w, row_h), |ui| {
                            ui.centered_and_justified(|ui| {
                                if let Some(tex) = &self.sstv.rx_tex {
                                    ui.add(
                                        egui::Image::new(tex)
                                            .max_height(row_h - 34.0)
                                            .max_width(live_w - 16.0),
                                    );
                                } else {
                                    let msg = if rifp {
                                        // RIFP only paints live from the raw
                                        // raster; anything else appears whole.
                                        "waiting for a picture…"
                                    } else if signal > 0.0008 {
                                        "waiting for a signal…"
                                    } else {
                                        "no / low audio"
                                    };
                                    ui.label(RichText::new(msg).size(11.0).weak());
                                }
                            });
                        });
                        // Draggable vertical divider between LIVE and RECEIVED.
                        let hresp =
                            crate::chrome::split_handle(ui, egui::vec2(handle_w, row_h), None);
                        if hresp.dragged() {
                            // Dragging right shrinks the gallery (grows LIVE).
                            let d = hresp.drag_delta().x / left_w.max(1.0);
                            self.view.sstv_gallery_fraction =
                                (self.view.sstv_gallery_fraction - d).clamp(0.2, 0.6);
                        }

                        // RECEIVED: narrow multi-column gallery of decoded pictures.
                        sstv_section(ui, "RECEIVED", egui::vec2(gallery_w, row_h), |ui| {
                            if self.sstv.received.is_empty() {
                                ui.label(
                                    RichText::new("Decoded pictures collect here.")
                                        .size(11.0)
                                        .weak(),
                                );
                                return;
                            }
                            let thumb = egui::vec2(112.0, 90.0);
                            egui::ScrollArea::vertical()
                                .id_salt("sstv-gallery")
                                .max_height(row_h - 24.0)
                                .auto_shrink([false, false])
                                .show_themed(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                                        for (i, r) in self.sstv.received.iter().enumerate() {
                                            let resp = ui
                                                .add(
                                                    egui::Image::new(&r.tex)
                                                        .fit_to_exact_size(thumb)
                                                        .corner_radius(2.0)
                                                        .sense(egui::Sense::click()),
                                                )
                                                .on_hover_text("Click to enlarge");
                                            if resp.clicked() {
                                                enlarge = Some(i);
                                            }
                                        }
                                    });
                                });
                        });
                    });
                },
            );

            // Draggable vertical divider between the receive side and the
            // TRANSMIT (send) column — mirrors the FT8 decode/QSO splitter.
            let hresp = crate::chrome::split_handle(ui, egui::vec2(handle_w, full_h), None);
            if hresp.dragged() {
                // Dragging right shrinks the TX column (grows the receive side).
                let d = hresp.drag_delta().x / avail.x.max(1.0);
                self.view.sstv_tx_fraction = (self.view.sstv_tx_fraction - d).clamp(0.22, 0.6);
            }

            // ── RIGHT: transmit compositor, full height ──
            ui.allocate_ui(egui::vec2(tx_w, full_h), |ui| {
                sstv_section(ui, "TRANSMIT", egui::vec2(tx_w, full_h), |ui| {
                    let inner_w = tx_w - 16.0;

                    // Five source slots — the highlighted one acts as the active
                    // "tab" whose message the box below edits.
                    ui.label(
                        RichText::new("Image slots — click one to edit its message")
                            .size(9.5)
                            .weak(),
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        for i in 0..self.sstv.slots.len() {
                            let sel = self.sstv.selected_slot == i;
                            let size = egui::vec2(70.0, 54.0);
                            let resp = if let Some(slot) = &self.sstv.slots[i] {
                                ui.add(
                                    egui::Image::new(&slot.tex)
                                        .fit_to_exact_size(size)
                                        .corner_radius(2.0)
                                        .sense(egui::Sense::click()),
                                )
                            } else {
                                let (rect, resp) =
                                    ui.allocate_exact_size(size, egui::Sense::click());
                                ui.painter().rect_stroke(
                                    rect,
                                    2.0,
                                    egui::Stroke::new(1.0, Color32::from_gray(70)),
                                    egui::StrokeKind::Inside,
                                );
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "+",
                                    egui::FontId::proportional(22.0),
                                    Color32::from_gray(110),
                                );
                                resp
                            };
                            // Active-tab highlight: a cyan wash + heavier border so
                            // it is obvious which slot the message box targets.
                            if sel {
                                ui.painter().rect_filled(
                                    resp.rect,
                                    2.0,
                                    Color32::from_rgba_unmultiplied(0x00, 0xd0, 0xf4, 34),
                                );
                                ui.painter().rect_stroke(
                                    resp.rect,
                                    2.0,
                                    egui::Stroke::new(2.5, crate::theme::CYAN),
                                    egui::StrokeKind::Outside,
                                );
                            }
                            // Slot number badge (1..5), like a tab label.
                            let badge = egui::Rect::from_min_size(
                                resp.rect.left_top() + egui::vec2(2.0, 2.0),
                                egui::vec2(15.0, 13.0),
                            );
                            ui.painter().rect_filled(badge, 2.0, Color32::from_black_alpha(150));
                            ui.painter().text(
                                badge.center(),
                                egui::Align2::CENTER_CENTER,
                                format!("{}", i + 1),
                                egui::FontId::proportional(10.0),
                                if sel { crate::theme::CYAN } else { Color32::from_gray(170) },
                            );
                            let resp = resp.on_hover_text(
                                "Click to edit this slot's message · double-click to load an image",
                            );
                            if resp.double_clicked() {
                                self.sstv.pick_target = Some(i);
                                pick_image(self.sstv.inbox.clone());
                            } else if resp.clicked() && !sel {
                                self.sstv.save_messages(); // flush the slot we leave
                                self.sstv.selected_slot = i;
                                self.sstv.preview_dirty = true;
                            }
                        }
                    });
                    ui.add_space(5.0);

                    // Explicit image load button for the active slot.
                    ui.horizontal(|ui| {
                        let sel = self.sstv.selected_slot;
                        let has_img =
                            self.sstv.slots.get(sel).map(|s| s.is_some()) == Some(true);
                        let label = if has_img { "Change image…" } else { "Load image…" };
                        if crate::chrome::chip(ui, false, label).clicked() {
                            self.sstv.pick_target = Some(sel);
                            pick_image(self.sstv.inbox.clone());
                        }
                    });
                    ui.add_space(6.0);

                    // Preview gets a capped share of the height; the message box
                    // grows to fill whatever's left above the buttons.
                    let btn_h = 42.0;
                    let gap = 6.0;
                    ui.label(RichText::new("Preview (what is transmitted)").size(9.5).weak());
                    let preview_h = (ui.available_height() * 0.45).clamp(80.0, 260.0);
                    egui::Frame::new()
                        .fill(Color32::from_gray(6))
                        .stroke(egui::Stroke::new(1.0, crate::theme::LINE_LIT))
                        .inner_margin(2.0)
                        .show(ui, |ui| {
                            ui.set_min_size(egui::vec2(inner_w, preview_h));
                            ui.set_max_size(egui::vec2(inner_w, preview_h));
                            ui.centered_and_justified(|ui| {
                                if let Some(tex) = &self.sstv.preview_tex {
                                    ui.add(
                                        egui::Image::new(tex)
                                            .max_height(preview_h - 4.0)
                                            .max_width(inner_w - 4.0),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new("Load an image into this slot →")
                                            .size(11.0)
                                            .weak(),
                                    );
                                }
                            });
                        });
                    ui.add_space(gap);

                    // Overlay message for the active slot — fills the height above
                    // the buttons; persisted when focus leaves the box or the slot
                    // changes. A per-slot id keeps each tab's cursor independent.
                    let sel = self.sstv.selected_slot;
                    let msg_h = (ui.available_height() - btn_h - gap).max(48.0);
                    let resp = ui
                        .push_id(sel, |ui| {
                            ui.add_sized(
                                egui::vec2(inner_w, msg_h),
                                egui::TextEdit::multiline(&mut self.sstv.slot_messages[sel])
                                    .hint_text("Drawn on this slot's image"),
                            )
                        })
                        .inner;
                    if resp.changed() {
                        self.sstv.preview_dirty = true;
                    }
                    if resp.lost_focus() {
                        self.sstv.save_messages();
                    }
                    ui.add_space(gap);

                    // Large cut-corner TX / ABORT buttons.
                    ui.horizontal(|ui| {
                        let can_tx = self.sstv.slots.get(self.sstv.selected_slot).map(|s| s.is_some())
                            == Some(true)
                            && !tx_active;
                        let tx = ui
                            .add_enabled_ui(can_tx, |ui| {
                                crate::chrome::chip_accent(
                                    ui,
                                    can_tx,
                                    RichText::new("   TX   ").size(16.0).strong(),
                                    crate::theme::PINK,
                                    Color32::WHITE,
                                )
                            })
                            .inner;
                        if tx.clicked() {
                            self.sstv.save_messages(); // capture any unfocused edit
                            if let Some(png) = self.sstv.compose_png(dims) {
                                cmds.push(if rifp {
                                    Command::RifpTx { png }
                                } else {
                                    Command::SstvTx { mode: self.sstv.tx_mode, png }
                                });
                            }
                        }
                        ui.add_space(8.0);
                        let abort = ui
                            .add_enabled_ui(tx_active, |ui| {
                                crate::chrome::chip(
                                    ui,
                                    false,
                                    RichText::new(" ABORT TX ").size(15.0).strong(),
                                )
                            })
                            .inner;
                        if abort.clicked() {
                            cmds.push(Command::DigiAbortTx);
                        }
                    });
                });
            });

            if let Some(i) = enlarge {
                self.sstv.enlarged = Some(i);
            }
        });

        // Enlarged view of a clicked received image (overlay window).
        if let Some(idx) = self.sstv.enlarged {
            let mut open = true;
            if let Some(r) = self.sstv.received.get(idx) {
                egui::Window::new("Received image")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .default_size([660.0, 528.0])
                    .frame(crate::chrome::window_frame())
                    .show(&ctx, |ui| {
                        // Scale up to fill the window width (preserving aspect).
                        let native = r.tex.size_vec2();
                        let avail_w = ui.available_width().min(1000.0);
                        let scale = (avail_w / native.x.max(1.0)).clamp(1.0, 4.0);
                        ui.add(egui::Image::new(&r.tex).fit_to_exact_size(native * scale));
                        // RIFP knows where a picture came from and how it was
                        // carried; SSTV knows none of that, and says nothing.
                        if let Some(m) = &r.rifp {
                            ui.add_space(4.0);
                            let from = m.sender.as_deref().unwrap_or("unidentified");
                            ui.label(
                                RichText::new(format!(
                                    "{from} · {} · {}×{} {}-bit · {} / {} · {} octets in {} chunks \
                                     ({} first pass) · session {}",
                                    m.filename,
                                    m.width,
                                    m.height,
                                    m.bits_per_pixel,
                                    m.media_type,
                                    m.content_encoding,
                                    m.encoded_size,
                                    m.chunk_count,
                                    m.chunks_first_pass,
                                    m.session,
                                ))
                                .size(10.5)
                                .weak(),
                            );
                            if let Some(hint) = &m.hint {
                                ui.label(RichText::new(hint).size(11.0).italics());
                            }
                        }
                    });
            } else {
                open = false;
            }
            if !open {
                self.sstv.enlarged = None;
            }
        }
    }

    /// The RIFP half of the image panel's control strip: profile, picture size
    /// and encoding, robustness, the transfer readout, and the sessions being
    /// reassembled.
    fn rifp_controls(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let st = self.sstv.rifp.clone();
        let seeded = self.digi_cfg_seeded;
        let mut changed = false;

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("RIFP").size(12.0).strong().color(crate::theme::CYAN));
            // Outside the enabled scope: which frequency to sit on has nothing
            // to do with whether the operator's digi config has loaded yet.
            self.digi_freq_chip(ui, cmds);
            ui.add_enabled_ui(seeded, |ui| {
                for p in RifpProfile::ALL {
                    let active = self.digi_cfg_edit.rifp_profile == p;
                    if crate::chrome::chip(ui, active, p.label())
                        .on_hover_text(format!(
                            "{} — {:.0} baud CPFSK, ±{:.0} Hz, {:.0} kHz occupied bandwidth",
                            p.name(),
                            p.symbol_rate(),
                            p.deviation_hz(),
                            p.bandwidth_hz() / 1000.0,
                        ))
                        .clicked()
                        && !active
                    {
                        self.digi_cfg_edit.rifp_profile = p;
                        changed = true;
                    }
                }
                ui.separator();
                ui.label(RichText::new("Size").size(10.0).weak());
                for s in RifpSize::ALL {
                    let active = self.digi_cfg_edit.rifp_size == s;
                    if crate::chrome::chip(ui, active, s.label()).clicked() && !active {
                        self.digi_cfg_edit.rifp_size = s;
                        self.sstv.preview_dirty = true;
                        changed = true;
                    }
                }
            });
        });
        ui.add_space(4.0);

        // The bandwidth warning, and a jump to the calling frequency. RIFP
        // itself is band-agnostic; what is legal is not.
        let dial = self.state.rx_freq_hz();
        ui.horizontal_wrapped(|ui| {
            let profile = self.digi_cfg_edit.rifp_profile;
            if profile.fits_at(dial) {
                ui.label(
                    RichText::new(format!(
                        "{} · ~{:.0} kHz occupied · dial is the channel centre",
                        profile.name(),
                        profile.bandwidth_hz() / 1000.0,
                    ))
                    .size(10.5)
                    .weak(),
                );
            } else {
                ui.label(
                    RichText::new(format!(
                        "⚠ {} occupies ~{:.0} kHz — too wide for a narrow-band segment",
                        profile.name(),
                        profile.bandwidth_hz() / 1000.0,
                    ))
                    .size(10.5)
                    .strong()
                    .color(crate::theme::PINK),
                )
                .on_hover_text(format!(
                    "RIFP assigns no frequency, and sdroxide will transmit it wherever you tune. \
                     A {:.0} kHz channel only fits where wideband or FM operation is allowed — \
                     {} — and not in a narrow-band segment, least of all on HF. Even inside those \
                     your own licence conditions may be narrower. You are the operator; check \
                     your own rules.",
                    profile.bandwidth_hz() / 1000.0,
                    profile.wide_segments_text(),
                ));
            }
            if (dial - sdroxide_types::RIFP_CALLING_HZ).abs() > 1.0
                && crate::chrome::chip(ui, false, "433.920")
                    .on_hover_text("The calling frequency the draft names")
                    .clicked()
            {
                cmds.push(Command::SetVfo {
                    vfo: self.state.active_vfo,
                    hz: sdroxide_types::RIFP_CALLING_HZ,
                });
            }
        });
        ui.add_space(5.0);

        // Encoding and depth: what the picture is turned into before framing.
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(seeded, |ui| {
                ui.label(RichText::new("Encode").size(10.0).weak()).on_hover_text(
                    "How the picture is encoded into the object RIFP carries. Auto tries each and \
                 sends the smallest.",
                );
                for e in RifpEncoding::TX_MENU {
                    let active = self.digi_cfg_edit.rifp_encoding == e;
                    let hover = match e.manifest_pair() {
                        Some((mt, ce)) => format!("{mt} / {ce}"),
                        None => "Try every encoding, send the smallest (never lossy)".into(),
                    };
                    if crate::chrome::chip(ui, active, e.label()).on_hover_text(hover).clicked()
                        && !active
                    {
                        self.digi_cfg_edit.rifp_encoding = e;
                        changed = true;
                    }
                }
                ui.separator();
                ui.label(RichText::new("Gray").size(10.0).weak()).on_hover_text(
                "Grayscale depth. RIFP's raster is grayscale by definition — colour has no place \
                 in its manifest.",
            );
                for bits in [1u8, 2, 4, 8] {
                    let active = self.digi_cfg_edit.rifp_bits_per_pixel == bits;
                    if crate::chrome::chip(ui, active, &format!("{bits}b")).clicked() && !active {
                        self.digi_cfg_edit.rifp_bits_per_pixel = bits;
                        changed = true;
                    }
                }
                let mut dither = self.digi_cfg_edit.rifp_dither;
                if crate::chrome::chip(ui, dither, "Dither")
                    .on_hover_text("Diffuse quantisation error — worth it below 8 bits")
                    .clicked()
                {
                    dither = !dither;
                    self.digi_cfg_edit.rifp_dither = dither;
                    changed = true;
                }
            });
        });
        ui.add_space(5.0);

        // Robustness: RIFP has no repair requests, so repetition is the only
        // recovery there is.
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(seeded, |ui| {
                ui.label(RichText::new("Repeat data").size(10.0).weak()).on_hover_text(
                    "Send every data frame this many times. RIFP is one-way with no repair \
                     requests, so this is the only recovery a receiver gets.",
                );
                ui.spacing_mut().slider_width = 90.0;
                changed |= ui
                    .add(egui::Slider::new(&mut self.digi_cfg_edit.rifp_data_repeats, 1..=4))
                    .drag_stopped();
                ui.label(RichText::new("Chunk").size(10.0).weak())
                    .on_hover_text("Payload octets per data frame (the profile recommends 192)");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.digi_cfg_edit.rifp_chunk_size, 32..=1024)
                            .step_by(16.0),
                    )
                    .drag_stopped();
            });
            ui.separator();
            if st.tx_active {
                ui.label(
                    RichText::new(format!(
                        "● TX frame {}/{} · {} s left",
                        st.tx_frame, st.tx_frames, st.tx_remaining_s
                    ))
                    .size(11.0)
                    .strong()
                    .color(crate::theme::PINK),
                );
            }
            if let Some(enc) = st.tx_encoding {
                ui.label(
                    RichText::new(format!("sent as {} · {} octets", enc.label(), st.tx_bytes))
                        .size(10.0)
                        .weak(),
                );
            }
        });
        ui.add_space(5.0);

        // Counters and the sessions being reassembled.
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "frames {} · bad {} · pictures {}",
                    st.rx_frames, st.rx_bad_frames, st.rx_objects
                ))
                .size(10.0)
                .weak(),
            )
            .on_hover_text("Valid frames, frames that failed CRC, and complete verified pictures");
            if st.sessions.is_empty() {
                ui.label(RichText::new("no transfer in progress").size(10.0).weak());
            }
            for s in &st.sessions {
                ui.separator();
                let from = s.sender.as_deref().unwrap_or_else(|| shorten(&s.session, 8));
                let label = if s.total > 0 {
                    format!("{from} {}/{}", s.have, s.total)
                } else {
                    format!("{from} {}", s.have)
                };
                let colour =
                    if s.have_manifest { crate::theme::GREEN } else { crate::theme::YELLOW };
                ui.label(RichText::new(label).size(10.5).strong().color(colour)).on_hover_text(
                    if s.have_manifest {
                        format!("session {} · idle {} s", s.session, s.idle_s)
                    } else {
                        format!(
                            "session {} · chunks held, still waiting for the manifest · idle {} s",
                            s.session, s.idle_s
                        )
                    },
                );
                rifp_chunk_map(ui, s);
                if crate::chrome::chip(ui, false, "✕")
                    .on_hover_text("Forget this incomplete transfer")
                    .clicked()
                {
                    cmds.push(Command::RifpDropSession(s.session.clone()));
                }
            }
        });
        if let Some(err) = &st.last_error {
            ui.label(RichText::new(err).size(10.0).color(crate::theme::YELLOW));
        }
        if changed && seeded {
            cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
        }
    }

    /// On first entry, load any persisted transmit slots and received gallery
    /// from disk (native only).
    fn sstv_load_disk_once(&mut self, ctx: &egui::Context) {
        if self.sstv.loaded_disk {
            return;
        }
        self.sstv.loaded_disk = true;
        for (i, entry) in sstv_load_slots().into_iter().enumerate() {
            if let Some((rgb, w, h)) = entry {
                let ci = crate::sstv::color_image(&rgb, w, h);
                let tex = ctx.load_texture("sstv_slot", ci, egui::TextureOptions::LINEAR);
                if let Some(cell) = self.sstv.slots.get_mut(i) {
                    *cell = Some(SstvSlot { src_rgb: rgb, sw: w, sh: h, tex });
                }
            }
        }
        // Restore the per-slot overlay messages (padded to the slot count).
        for (i, msg) in sstv_load_messages().into_iter().enumerate() {
            if let Some(cell) = self.sstv.slot_messages.get_mut(i) {
                *cell = msg;
            }
        }
        for (rgb, w, h) in sstv_load_gallery() {
            let ci = crate::sstv::color_image(&rgb, w, h);
            let tex = ctx.load_texture("sstv_recv", ci, egui::TextureOptions::NEAREST);
            self.sstv.received.push(SstvRecv { mode: None, rifp: None, tex });
        }
    }
}
