//! Widgets shared by more than one panel.
//!
//! The decode list and the JS8 heard list draw the same station card and the
//! same fixed-width row cells, and every panel that transmits an image opens
//! the same file picker, so those live here rather than in whichever panel
//! happened to grow them first.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};
use sdroxide_types::Decode;

/// The hover card behind a decode row: everything the entity file, the log and
/// the operator's own grid already know about this station, said in full.
///
/// The row can only afford a callsign, a grid and two numbers; all of this is
/// resolved for it anyway, so the card costs nothing but the space to show it.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn station_card(
    ui: &mut egui::Ui,
    d: &Decode,
    entity: Option<sdroxide_types::EntityInfo>,
    dist_km: Option<f64>,
    my_grid: &str,
    novelty: sdroxide_types::Novelty,
    band: &str,
    queued: bool,
    cq_for_us: bool,
) {
    ui.set_max_width(300.0);
    let dim = Color32::from_gray(140);
    match d.from.as_deref() {
        Some(call) => {
            ui.label(RichText::new(call).size(16.0).strong().color(crate::theme::TEXT_STRONG));
        }
        None if d.free_text => {
            ui.label(RichText::new("free text").size(13.0).italics().color(dim));
        }
        // A hashed callsign nobody on this receiver has heard in full yet.
        None => {
            ui.label(RichText::new("hashed callsign, not yet heard in full").size(13.0).color(dim));
        }
    }

    match entity {
        Some(e) => {
            ui.label(
                RichText::new(e.name)
                    .size(13.0)
                    .strong()
                    .color(crate::theme::continent_color(e.continent)),
            );
            ui.label(
                RichText::new(format!(
                    "{} · CQ zone {} · ITU zone {}",
                    e.continent, e.cq_zone, e.itu_zone
                ))
                .size(11.5)
                .color(dim),
            );
        }
        None if d.from.is_some() => {
            ui.label(RichText::new("entity unknown").size(11.5).color(dim));
        }
        None => {}
    }

    // Where they are, from their grid: distance and the beam heading to point.
    if let Some(g) = d.grid.as_deref() {
        let bearing =
            (!my_grid.is_empty()).then(|| sdroxide_types::grid_bearing(my_grid, g)).flatten();
        let mut line = g.to_string();
        if let Some(km) = dist_km {
            line.push_str(&format!(" · {km:.0} km"));
        }
        if let Some(b) = bearing {
            line.push_str(&format!(" · {b:.0}°"));
        }
        ui.label(RichText::new(line).size(12.0).color(crate::theme::YELLOW));
    }

    ui.separator();
    // Worked before? The one thing that decides whether this decode is worth
    // acting on, spelled out rather than compressed into a four-letter badge.
    let band_label = if band.is_empty() { "this band".to_string() } else { band.to_string() };
    let (worked, col) = if novelty.new_dxcc {
        ("New entity — never worked, on any band".to_string(), crate::theme::PINK)
    } else if novelty.new_dxcc_band {
        (format!("New entity on {band_label}"), crate::theme::YELLOW)
    } else if novelty.new_grid {
        ("New grid square".to_string(), crate::theme::CYAN)
    } else if novelty.new_call {
        ("Not worked before".to_string(), crate::theme::CYAN_DIM)
    } else if novelty.dupe {
        (format!("Worked before on {band_label}"), Color32::from_gray(130))
    } else {
        ("Worked before, but not on this band".to_string(), Color32::from_gray(150))
    };
    ui.label(RichText::new(worked).size(12.0).color(col));

    if let Some(target) = d.cq_to.as_deref() {
        ui.label(
            RichText::new(if cq_for_us {
                format!("Calling CQ {target} — that includes you")
            } else {
                format!("Calling CQ {target} — not aimed at you")
            })
            .size(11.5)
            .color(if cq_for_us { crate::theme::GREEN } else { dim }),
        );
    }
    if queued {
        ui.label(RichText::new("In the call queue").size(11.5).color(crate::theme::GREEN));
    }
    ui.label(
        RichText::new(format!("{:+} dB · {:.0} Hz · DT {:+.1} s", d.snr_db, d.audio_hz, d.dt))
            .size(11.0)
            .monospace()
            .color(dim),
    );
}

/// One fixed-width column of a station row, shared by the FT8 decode list and
/// the JS8 heard list so the two line up field for field.
///
/// The width is *reserved*, not requested: a plain `allocate_ui` shrinks to its
/// content, so a short callsign would collapse the column and shift everything
/// after it out of alignment down the list.
pub(in crate::app) fn row_cell(
    ui: &mut egui::Ui,
    w: f32,
    h: f32,
    align_right: bool,
    lbl: egui::Label,
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let layout = if align_right {
        egui::Layout::right_to_left(egui::Align::Center)
    } else {
        egui::Layout::left_to_right(egui::Align::Center)
    };
    ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout)).add(lbl);
}

/// Colour a decode's SNR: green for strong, cyan mid, dimmed for weak.
pub(in crate::app) fn snr_color(snr_db: i16) -> Color32 {
    if snr_db >= 0 {
        crate::theme::GREEN
    } else if snr_db >= -12 {
        crate::theme::CYAN
    } else {
        crate::theme::CYAN_DIM
    }
}

// ── File picker (native thread / wasm async) ──

#[cfg(not(target_arch = "wasm32"))]
pub(in crate::app) fn pick_image(inbox: Arc<Mutex<Option<Vec<u8>>>>) {
    std::thread::spawn(move || {
        if let Some(path) =
            rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg"]).pick_file()
        {
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(mut g) = inbox.lock() {
                    *g = Some(bytes);
                }
            }
        }
    });
}

#[cfg(target_arch = "wasm32")]
pub(in crate::app) fn pick_image(inbox: Arc<Mutex<Option<Vec<u8>>>>) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Some(file) = rfd::AsyncFileDialog::new()
            .add_filter("Image", &["png", "jpg", "jpeg"])
            .pick_file()
            .await
        {
            let bytes = file.read().await;
            if let Ok(mut g) = inbox.lock() {
                *g = Some(bytes);
            }
        }
    });
}

/// A titled, bordered section box of a fixed size, for the SSTV panel's LIVE /
/// RECEIVED / TRANSMIT areas.
pub(in crate::app) fn sstv_section<R>(
    ui: &mut egui::Ui,
    title: &str,
    size: egui::Vec2,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    // Force a top-down layout: `allocate_ui` would otherwise inherit the parent's
    // horizontal layout (we're inside a `horizontal_top`), laying the section's
    // contents out side by side instead of stacked.
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
        egui::Frame::new()
            .fill(crate::theme::ROW_BG)
            .stroke(egui::Stroke::new(1.0, crate::theme::LINE_LIT))
            .inner_margin(egui::Margin { left: 8, right: 8, top: 5, bottom: 7 })
            .show(ui, |ui| {
                ui.set_min_size(egui::vec2(size.x - 16.0, size.y - 12.0));
                ui.set_max_width(size.x - 16.0);
                ui.label(RichText::new(title).size(9.5).strong().color(crate::theme::CYAN_DIM));
                ui.add_space(3.0);
                add(ui)
            })
            .inner
    })
    .inner
}
