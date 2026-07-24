//! The egui chrome drawn inside the solar-system window: a wrapping top bar of
//! captioned modules over the 3D scene, styled with the app's own
//! [`crate::chrome`] widgets so the second window reads as part of sdroxide.

use eframe::egui::{self, RichText};
use sdroxide_solar::{SdoChannel, SolarData, Source, timefmt};

use super::state::{Focus, SolarUi};
use crate::chrome;
use crate::theme;
use crate::view::solar_layer as layer;

/// Layer chips, in bar order.
const LAYERS: [(u32, &str, &str); 7] = [
    (layer::ORBITS, "ORBITS", "Earth and Moon orbital paths"),
    (layer::CME, "CME", "Coronal mass ejection trajectory cones"),
    (layer::SPOTS, "SPOTS", "Sunspot active regions"),
    (layer::FLARES, "FLARES", "Solar flare source locations"),
    (layer::GRID, "GRID", "Ecliptic plane and heliographic graticule"),
    (layer::LABELS, "LABELS", "Body and region labels"),
    (layer::STARS, "STARS", "Background star field"),
];

pub fn ui(ui: &mut egui::Ui, st: &mut SolarUi) {
    // Take a snapshot of the feed's data for this frame. Cloning the `Arc`
    // first means the guard's lifetime is not tied to `st`, which is borrowed
    // mutably by every module below.
    let handle = st.data.clone();
    let guard = handle.as_ref().map(|d| d.lock().unwrap_or_else(|e| e.into_inner()));
    let data = guard.as_deref();
    let now = super::wall_clock_unix() as i64;

    egui::Panel::top(egui::Id::new("solar-top"))
        .frame(
            egui::Frame::new()
                .fill(theme::BG_DEEP)
                .inner_margin(egui::Margin::symmetric(8, 6)),
        )
        .show(ui, |ui| {
            chrome::angled_frame(ui, theme::PINK, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                ui.with_layout(
                    egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true),
                    |ui| {
                        view_module(ui, st);
                        layers_module(ui, st);
                        sun_module(ui, st, data, now);
                        scale_module(ui, st);
                        time_module(ui, st);
                    },
                );
            });
        });

    scene(ui, st, data);
}

/// Camera focus + the animated tour toggle.
fn view_module(ui: &mut egui::Ui, st: &mut SolarUi) {
    chrome::module(ui, "View", 336.0, |ui| {
        for f in Focus::ALL {
            if chrome::chip(ui, st.focus() == f, f.label()).clicked() {
                st.view.focus = f.to_u8();
                // The tour drives the focus itself, so let the user win.
                st.view.auto = false;
            }
        }
        if chrome::chip_accent(ui, st.view.auto, "▶ AUTO", theme::CYAN, theme::INK_ON_CYAN)
            .on_hover_text(
                "Fly a spline through a set of framed viewpoints. Any mouse input cancels it.",
            )
            .clicked()
        {
            st.view.auto = !st.view.auto;
            if st.view.auto {
                st.tour.request_resume();
            }
        }
    });
}

fn layers_module(ui: &mut egui::Ui, st: &mut SolarUi) {
    chrome::module(ui, "Layers", 476.0, |ui| {
        for (bit, label, hint) in LAYERS {
            if chrome::chip(ui, st.layer(bit), label).on_hover_text(hint).clicked() {
                st.toggle_layer(bit);
            }
        }
    });
}

/// Which SDO product wraps the Sun, plus the honest freshness readout.
fn sun_module(ui: &mut egui::Ui, st: &mut SolarUi, data: Option<&SolarData>, now: i64) {
    chrome::module(ui, "Sun", 372.0, |ui| {
        let current = SdoChannel::from_u8(st.view.channel);
        for c in SdoChannel::ALL {
            if chrome::chip(ui, current == c, c.label()).on_hover_text(c.description()).clicked() {
                st.view.channel = c.to_u8();
            }
        }
        if chrome::chip(ui, false, "↻").on_hover_text("Fetch everything again now").clicked() {
            st.refresh_requested = true;
        }

        // Say what is actually being shown. Presenting hours-old cached data as
        // if it were current is the one thing this readout must not do.
        let (text, color) = match data {
            None => ("starting…".to_string(), theme::CYAN_DIM),
            Some(d) => {
                let s = d.status(Source::Sun);
                match (s.age_secs(now), &s.last_error) {
                    (Some(age), None) => (timefmt::age(age), theme::GREEN),
                    (Some(age), Some(_)) => (format!("{} · offline", timefmt::age(age)), theme::YELLOW),
                    (None, Some(_)) => ("offline".to_string(), theme::PINK),
                    (None, None) => ("…".to_string(), theme::CYAN_DIM),
                }
            }
        };
        ui.label(RichText::new(text).color(color).size(10.5));
    });
}

/// Size exaggeration. Positions are always real; only radii (and optionally the
/// Moon's orbit) are scaled, or nothing at this distance would be visible.
fn scale_module(ui: &mut egui::Ui, st: &mut SolarUi) {
    chrome::module(ui, "Scale", 400.0, |ui| {
        // The Moon renders *inside* the Earth once the exaggerated radii exceed
        // the (unexaggerated) Earth–Moon distance, so cap body scale against it.
        let max_body = super::max_body_scale(st.view.moon_orbit_scale);
        ui.label(RichText::new("body").color(theme::CYAN_DIM).size(10.0));
        ui.add(
            egui::DragValue::new(&mut st.view.body_scale)
                .speed(0.25)
                .range(1.0..=max_body as f64)
                .suffix("×"),
        )
        .on_hover_text(format!(
            "Earth/Moon radius exaggeration (max {max_body:.0}× at this moon-orbit scale)"
        ));
        ui.label(RichText::new("moon orbit").color(theme::CYAN_DIM).size(10.0));
        ui.add(
            egui::DragValue::new(&mut st.view.moon_orbit_scale)
                .speed(0.1)
                .range(1.0..=30.0)
                .suffix("×"),
        )
        .on_hover_text("Stretch the Earth→Moon distance so the pair can be seen apart");
        ui.label(RichText::new("sun").color(theme::CYAN_DIM).size(10.0));
        ui.add(
            egui::DragValue::new(&mut st.view.sun_scale).speed(0.1).range(1.0..=20.0).suffix("×"),
        )
        .on_hover_text(
            "Sun radius exaggeration. Leave at 1× to keep the CME geometry readable — \
             a swollen Sun swallows the base of every cone.",
        );
        st.view.body_scale = st.view.body_scale.clamp(1.0, max_body);
    });
}

/// Scrub the whole scene forward and back in time.
fn time_module(ui: &mut egui::Ui, st: &mut SolarUi) {
    chrome::module(ui, "Time", 300.0, |ui| {
        if chrome::chip(ui, st.sim_offset_s == 0.0, "NOW").clicked() {
            st.sim_offset_s = 0.0;
        }
        for (label, dt) in [("−24h", -86400.0), ("−6h", -21600.0), ("+6h", 21600.0), ("+24h", 86400.0)]
        {
            if chrome::chip(ui, false, label).clicked() {
                st.sim_offset_s += dt;
            }
        }
    });
}

/// The 3D scene: mouse interaction, the wgpu paint callback, then the readouts
/// painted over it.
fn scene(ui: &mut egui::Ui, st: &mut SolarUi, data: Option<&SolarData>) {
    let rect = ui.available_rect_before_wrap();
    if rect.width() < 4.0 || rect.height() < 4.0 {
        return;
    }
    let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
    interact(ui, st, &resp);

    let ppp = ui.ctx().pixels_per_point();
    let px = [
        (rect.width() * ppp).round().max(1.0),
        (rect.height() * ppp).round().max(1.0),
    ];
    let sim_now = super::wall_clock_unix() + st.sim_offset_s;
    let anim = ui.input(|i| i.time);
    advance_tour(ui, st, sim_now, anim);
    let scene = super::scene::build(st, data, sim_now, px, anim as f32);

    let (sun_img, sun_gen) = match data {
        Some(d) => (d.sun.clone(), d.sun_gen),
        None => (None, 0),
    };
    ui.painter().add(crate::egui_wgpu::Callback::new_paint_callback(
        rect,
        super::gpu::SolarCallback {
            scene: std::sync::Arc::new(scene),
            px_size: [px[0] as u32, px[1] as u32],
            sun_img,
            sun_gen,
        },
    ));

    info_card(ui, st, data, rect, sim_now);
    impact_banner(ui, data, rect, sim_now as i64);

    if st.qth.is_none() {
        ui.painter().text(
            rect.right_top() + egui::vec2(-12.0, 12.0),
            egui::Align2::RIGHT_TOP,
            "QTH not set — enter your grid square in Settings",
            egui::FontId::proportional(12.5),
            theme::YELLOW,
        );
    }
}

/// Bottom-left readout: where the Sun is, where it is over the operator, and
/// what the feed knows.
fn info_card(
    ui: &egui::Ui,
    st: &SolarUi,
    data: Option<&SolarData>,
    rect: egui::Rect,
    sim_now: f64,
) {
    use sdroxide_solar::ephem;
    let jd = ephem::julian_day(sim_now);
    let (_, b0, l0) = ephem::solar_p_b0_l0(jd);
    let (slat, slon) = ephem::subsolar_point(jd);

    let mut lines = vec![
        format!("{}  UTC", timefmt::ymd_hm(sim_now as i64)),
        format!("sub-solar  {slat:+.1}°  {slon:+.1}°"),
        format!("B0 {b0:+.2}°   L0 {l0:.1}°"),
    ];
    if let Some((lat, lon)) = st.qth {
        let (el, az) = sun_elevation_azimuth(lat, lon, slat, slon);
        let state = if el > 0.0 { "day" } else { "night" };
        lines.push(format!("{}  sun {el:+.0}° el {az:.0}° az ({state})", st.qth_grid));
    }
    if let Some(d) = data {
        let visible = d
            .cmes
            .iter()
            .filter(|e| {
                e.analysis.as_ref().is_some_and(|a| {
                    let age = sim_now as i64 - a.t21_5_unix;
                    (0..(st.view.cme_window_h as i64 * 3600)).contains(&age)
                })
            })
            .count();
        lines.push(format!("{visible} CME · {} spots", d.regions.len()));
    }
    if st.view.auto {
        let s = st.tour.station();
        let phase = if st.tour.in_transit() { "→ " } else { "" };
        lines.push(format!("AUTO  {phase}{}", s.name));
    }

    let font = egui::FontId::proportional(11.5);
    let galleys: Vec<_> = lines
        .iter()
        .map(|l| ui.painter().layout_no_wrap(l.clone(), font.clone(), theme::TEXT))
        .collect();
    let w = galleys.iter().map(|g| g.size().x).fold(0.0f32, f32::max) + 20.0;
    let h = galleys.iter().map(|g| g.size().y + 2.0).sum::<f32>() + 16.0;
    let card = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 12.0, rect.bottom() - h - 12.0),
        egui::vec2(w, h),
    );
    if !rect.contains_rect(card) {
        return; // too small a window to be worth crowding
    }
    ui.painter().rect_filled(card, 0, theme::FILL.gamma_multiply(0.82));
    chrome::paint_cut_border(ui.painter(), card, theme::LINE_LIT, egui::Color32::TRANSPARENT);
    let mut y = card.top() + 8.0;
    for g in galleys {
        let dy = g.size().y + 2.0;
        ui.painter().galley(egui::pos2(card.left() + 10.0, y), g, theme::TEXT);
        y += dy;
    }
}

/// The banner that justifies the whole window: a CME whose cone contains the
/// Earth, with an arrival estimate.
fn impact_banner(ui: &egui::Ui, data: Option<&SolarData>, rect: egui::Rect, now: i64) {
    let Some(d) = data else { return };
    // The soonest arrival that has not already happened.
    let mut best: Option<(&sdroxide_solar::CmeEvent, sdroxide_solar::Impact)> = None;
    for e in &d.cmes {
        let Some(a) = &e.analysis else { continue };
        let Some(hit) = sdroxide_solar::earth_impact(a) else { continue };
        // Keep it on screen for a few hours past the estimate, since arrival
        // estimates are routinely off by that much.
        if hit.eta_unix < now - 6 * 3600 {
            continue;
        }
        if best.as_ref().is_none_or(|(_, b)| hit.eta_unix < b.eta_unix) {
            best = Some((e, hit));
        }
    }
    let Some((event, hit)) = best else { return };
    let a = event.analysis.as_ref().expect("filtered above");

    let hours = (hit.eta_unix - now) as f64 / 3600.0;
    let when = if hours >= 0.0 {
        format!("ETA {} (+{hours:.0} h)", timefmt::ymd_hm(hit.eta_unix))
    } else {
        format!("arrival was {} ({:.0} h ago)", timefmt::ymd_hm(hit.eta_unix), -hours)
    };
    let glancing = if hit.directness(a.half_angle_deg) < 0.35 { " · glancing" } else { "" };
    let estimated = if a.estimated { " · direction estimated" } else { "" };
    let text = format!(
        "⚡ EARTH-DIRECTED CME  {}  ·  {:.0} km/s  ·  {when}{glancing}{estimated}",
        timefmt::ymd_hm(a.t21_5_unix),
        a.speed_km_s,
    );

    let font = egui::FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(text, font, theme::TEXT_STRONG);
    let size = galley.size() + egui::vec2(28.0, 14.0);
    if size.x > rect.width() - 24.0 {
        return;
    }
    let banner = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.bottom() - size.y * 0.5 - 14.0),
        size,
    );
    ui.painter().rect_filled(banner, 0, theme::CQ_BG.gamma_multiply(0.92));
    chrome::paint_cut_border(ui.painter(), banner, theme::PINK, egui::Color32::TRANSPARENT);
    ui.painter().galley(
        banner.min + egui::vec2(14.0, 7.0),
        galley,
        theme::TEXT_STRONG,
    );
}

/// Solar elevation and azimuth at a location, from the sub-solar point.
///
/// Both points are on the same sphere, so this is the great-circle geometry the
/// FT8 map already uses for bearings — elevation is 90° minus the angular
/// distance to the sub-solar point.
fn sun_elevation_azimuth(lat: f64, lon: f64, slat: f64, slon: f64) -> (f64, f64) {
    let (p1, p2) = (lat.to_radians(), slat.to_radians());
    let dl = (slon - lon).to_radians();
    let cos_c = p1.sin() * p2.sin() + p1.cos() * p2.cos() * dl.cos();
    let elevation = cos_c.clamp(-1.0, 1.0).asin().to_degrees();
    let az = (dl.sin() * p2.cos())
        .atan2(p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos())
        .to_degrees();
    (elevation, (az + 360.0) % 360.0)
}

/// Fly the AUTO tour, using real elapsed time so the pacing is frame-rate
/// independent.
fn advance_tour(ui: &egui::Ui, st: &mut SolarUi, sim_now: f64, frame_time: f64) {
    let dt = (frame_time - st.last_frame_time) as f32;
    st.last_frame_time = frame_time;
    if !st.view.auto {
        return;
    }
    let b = super::scene::bodies(st, sim_now);
    // `Tour` is `Copy`, so step a local and write it back rather than fighting
    // the borrow of `st.view` inside it.
    let mut tour = st.tour;
    tour.step(&mut st.view, &b, if st.last_frame_time > 0.0 { dt } else { 1.0 / 60.0 });
    st.tour = tour;
    ui.ctx().request_repaint();
}

/// Drag to rotate, scroll to zoom, double-click to reframe. Any of them cancels
/// the animated tour — the user taking the controls is the signal to stop.
fn interact(ui: &egui::Ui, st: &mut SolarUi, resp: &egui::Response) {
    let mut touched = false;

    if resp.dragged_by(egui::PointerButton::Primary) {
        let d = resp.drag_delta();
        st.view.yaw -= d.x * 0.006;
        st.view.pitch = (st.view.pitch + d.y * 0.006)
            .clamp(-super::camera::PITCH_LIMIT, super::camera::PITCH_LIMIT);
        touched |= d != egui::Vec2::ZERO;
    }

    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            // Multiplicative, so a wheel click covers the same visual fraction
            // whether you are 3 Gm or 3 AU out.
            st.view.dist *= (1.0 - scroll * 0.0022).clamp(0.4, 2.5);
            touched = true;
        }
    }

    // Clamping needs the focus radius, which only the scene knows; `Camera`
    // re-clamps anyway, so here just keep the stored value sane.
    st.view.dist = st.view.dist.clamp(1e-5, super::camera::MAX_DIST);

    if touched {
        st.view.auto = false;
    }
    // Continuous repaint only while something is actually moving; otherwise the
    // window idles and is woken by input or by the data feed.
    if touched || st.view.auto || resp.is_pointer_button_down_on() {
        ui.ctx().request_repaint();
    }
}
