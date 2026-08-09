//! The SAT window: pick a satellite, lock on, and operate through it.
//!
//! The lock itself lives in the engine — this window only *asks* for one
//! ([`Command::SetSatLock`]) and displays the [`sdroxide_types::SatTrackStatus`]
//! stream that comes back: look angles, range, and the Doppler corrections as
//! actually applied. That split is why a lock keeps correcting with this
//! window closed, and why a remote client sees the same numbers.
//!
//! The picker is built from the same element sets the 3D view tracks: the
//! operator's pasted TLEs and the cached subscription listings (native; the
//! browser gets the pasted sets and the curated names, and the engine — which
//! has the caches — resolves the elements when it locks). Pass prediction for
//! the list is memoized one satellite per frame: a search over 48 hours is
//! cheap enough to do once, and far too dear to do ninety times per repaint.

use eframe::egui::{self, RichText};
use sdroxide_types::{Command, Mode, SatLockConfig, SatUplink, Vfo};

use crate::app::SdroxideApp;
use crate::theme;

/// Everything the window remembers between frames.
#[derive(Default)]
pub(in crate::app) struct SatWinState {
    pub search: String,
    /// Satellite whose detail/link pane is open, by catalogue number.
    pub selected: Option<u64>,
    /// Which of the selected satellite's published links is chosen.
    pub link_idx: usize,
    /// The lock we last asked the engine for — what the DOPPLER/ROTATOR
    /// toggles re-send modified. `None` when the running lock came from
    /// another client, in which case the toggles are not shown.
    pub sent: Option<SatLockConfig>,
    list: Option<SatList>,
}

/// The parsed satellite list, rebuilt when the station's satellite config
/// changes or ages out (a TLE refresh rewrites the caches underneath it).
struct SatList {
    cfg: std::sync::Arc<sdroxide_types::SatConfig>,
    built_unix: i64,
    entries: Vec<SatEntry>,
}

struct SatEntry {
    norad_id: u64,
    name: String,
    /// The element lines, handed to the engine with the lock so it needs no
    /// lookup of its own. `None` for a curated name whose elements this
    /// client does not hold — the engine still resolves those from its side.
    tle: Option<(String, String)>,
    sat: Option<sdroxide_solar::Satellite>,
    /// Next-pass memo: when it was computed and what it said.
    pass: Option<(i64, PassMemo)>,
}

enum PassMemo {
    Pass(sdroxide_types::SatPass),
    Always,
    Never,
}

/// How long a pass memo (and the list itself) is trusted before recomputing.
const MEMO_FRESH_S: i64 = 600;

impl SatEntry {
    fn matches(&self, q: &str) -> bool {
        q.is_empty()
            || self.name.to_ascii_lowercase().contains(&q.to_ascii_lowercase())
            || self.norad_id.to_string().contains(q)
    }
}

fn build_list(cfg: &std::sync::Arc<sdroxide_types::SatConfig>, now: i64) -> SatList {
    let mut entries: Vec<SatEntry> = Vec::new();
    let add_text = |text: &str, entries: &mut Vec<SatEntry>| {
        for t in sdroxide_types::parse_tle_block(text) {
            let Some(id) = t.norad_id() else { continue };
            if entries.iter().any(|e| e.norad_id == id) {
                continue;
            }
            let Some(sat) = sdroxide_solar::satellites::parse_pasted_tles(&format!(
                "{}\n{}\n{}",
                t.name, t.line1, t.line2
            ))
            .pop() else {
                continue;
            };
            entries.push(SatEntry {
                norad_id: id,
                name: sat.name.clone(),
                tle: Some((t.line1.clone(), t.line2.clone())),
                sat: Some(sat),
                pass: None,
            });
        }
    };
    add_text(&cfg.tle_text(), &mut entries);
    // The subscription caches live on disk beside the engine — the browser
    // client has neither, and leans on the engine to resolve elements instead.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let cache = sdroxide_solar::cache::Cache::open();
        for sub in cfg.live_subs() {
            if let Some(text) = sdroxide_solar::tlesub::cached_text(&cache, sub) {
                add_text(&text, &mut entries);
            }
        }
    }
    // The curated names always appear, elements or not: a row that says "no
    // elements" tells the operator what to fix, an absent row says nothing.
    for (id, name) in sdroxide_solar::satellites::POPULAR {
        if !entries.iter().any(|e| e.norad_id == *id) {
            entries.push(SatEntry {
                norad_id: *id,
                name: (*name).to_string(),
                tle: None,
                sat: None,
                pass: None,
            });
        }
    }
    SatList { cfg: std::sync::Arc::clone(cfg), built_unix: now, entries }
}

/// The mode a link's published emission maps onto. Satellite SSB convention
/// is USB on the downlink, whatever the band.
fn mode_for_link(l: &sdroxide_types::SatLink) -> Mode {
    let m = l.mode.to_ascii_uppercase();
    if m.contains("FM") || m.contains("APT") {
        Mode::Nfm
    } else if m.contains("SSB") || m.contains("BPSK") || m.contains("GMSK") || m.contains("AX.25") {
        Mode::Usb
    } else if m.contains("CW") {
        Mode::Cw
    } else {
        Mode::Usb
    }
}

/// The downlink dial a link starts on: the current dial if it already sits
/// inside the passband (re-locking must not yank a tuned-in operator to the
/// centre), else the passband centre.
fn lock_downlink_hz(l: &sdroxide_types::SatLink, dial_hz: f64) -> f64 {
    match l.downlink {
        Some(pb) => {
            let (lo, hi) = (pb.lo_mhz * 1e6, pb.hi_mhz * 1e6);
            if (lo..=hi).contains(&dial_hz) { dial_hz } else { pb.centre_mhz() * 1e6 }
        }
        None => dial_hz,
    }
}

fn fmt_hz_signed(hz: f64) -> String {
    if hz.abs() >= 1000.0 { format!("{:+.2} kHz", hz / 1000.0) } else { format!("{hz:+.0} Hz") }
}

fn fmt_mhz(hz: f64) -> String {
    sdroxide_types::fmt_sat_mhz(hz / 1e6)
}

/// "in 12 min" / "in 3 h 05 min" — the countdown side of `timefmt::age`.
fn in_words(s: i64) -> String {
    let s = s.max(0);
    if s < 60 {
        format!("in {s} s")
    } else if s < 3600 {
        format!("in {} min", s / 60)
    } else {
        format!("in {} h {:02} min", s / 3600, (s % 3600) / 60)
    }
}

impl SdroxideApp {
    pub(in crate::app) fn sat_window(&mut self, ctx: &egui::Context, cmds: &mut Vec<Command>) {
        if !self.show_sat {
            return;
        }
        let mut win = std::mem::take(&mut self.sat_win);
        let mut open = self.show_sat;
        let resp = egui::Window::new("SATELLITE")
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(true)
            .default_width(crate::layout::window_w(ctx, 480.0))
            .default_height(crate::layout::window_h(ctx, 520.0))
            .show(ctx, |ui| {
                crate::chrome::window_body_bg(ui);
                self.sat_body(ui, &mut win, cmds)
            });
        if let Some(r) = &resp {
            crate::chrome::paint_window_border(ctx, &r.response);
        }
        self.show_sat = open;
        self.sat_win = win;
    }

    fn sat_body(&mut self, ui: &mut egui::Ui, win: &mut SatWinState, cmds: &mut Vec<Command>) {
        if let Some(track) = self.sat_track.clone() {
            self.sat_locked_pane(ui, win, cmds, &track);
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
        }
        self.sat_picker(ui, win, cmds);
    }

    /// The live pane: what the engine's lock is doing right now.
    fn sat_locked_pane(
        &mut self,
        ui: &mut egui::Ui,
        win: &mut SatWinState,
        cmds: &mut Vec<Command>,
        t: &sdroxide_types::SatTrackStatus,
    ) {
        use sdroxide_solar::satellites::compass;
        let dim = |s: &str| RichText::new(s).size(9.5).color(theme::CYAN_DIM());

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("● LOCKED — {}", t.name))
                    .size(13.0)
                    .strong()
                    .color(if t.visible { theme::GREEN() } else { theme::YELLOW() }),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::chrome::chip(ui, false, "UNLOCK")
                    .on_hover_text("Release the lock; the dial stays where it is")
                    .clicked()
                {
                    cmds.push(Command::SetSatLock(None));
                    win.sent = None;
                }
            });
        });
        ui.add_space(4.0);

        egui::Grid::new("sat-lock-grid").num_columns(4).spacing([16.0, 3.0]).show(ui, |ui| {
            ui.label(dim("AZIMUTH"));
            ui.label(
                RichText::new(format!("{:.1}°  {}", t.az_deg, compass(t.az_deg)))
                    .size(12.5)
                    .strong(),
            );
            ui.label(dim("ELEVATION"));
            ui.label(
                RichText::new(format!("{:+.1}°", t.el_deg))
                    .size(12.5)
                    .strong()
                    .color(if t.el_deg > 0.0 { theme::GREEN() } else { theme::TEXT() }),
            );
            ui.end_row();

            ui.label(dim("RANGE"));
            ui.label(RichText::new(format!("{:.0} km", t.range_km)).size(11.5));
            ui.label(dim("RATE"));
            let dir = if t.range_rate_km_s < -0.01 {
                "approaching"
            } else if t.range_rate_km_s > 0.01 {
                "receding"
            } else {
                "abeam"
            };
            ui.label(RichText::new(format!("{:+.2} km/s {dir}", t.range_rate_km_s)).size(11.5));
            ui.end_row();

            ui.label(dim("DOWNLINK"));
            ui.label(
                RichText::new(format!("{} MHz", fmt_mhz(t.downlink_hz)))
                    .size(11.5)
                    .color(theme::GREEN()),
            );
            ui.label(dim("DOPPLER RX"));
            ui.label(RichText::new(fmt_hz_signed(t.doppler_rx_hz)).size(11.5));
            ui.end_row();

            if let Some(up) = t.uplink_hz {
                ui.label(dim("UPLINK"));
                ui.label(
                    RichText::new(format!("{} MHz", fmt_mhz(up))).size(11.5).color(theme::YELLOW()),
                );
                ui.label(dim("DOPPLER TX"));
                ui.label(RichText::new(fmt_hz_signed(t.doppler_tx_hz)).size(11.5));
                ui.end_row();
            }
        });
        ui.add_space(4.0);

        // Where the pass stands. `visible` already tells the near half of the
        // story; the memoized prediction tells the rest.
        let now = crate::time::now_unix();
        match (&t.next_pass, t.visible) {
            (_, true) => {
                if let Some(p) = &t.next_pass {
                    if (p.rise_unix..=p.set_unix).contains(&now) {
                        ui.label(dim(&format!(
                            "Pass until {} UTC · max {:.0}°",
                            sdroxide_solar::timefmt::ymd_hm(p.set_unix)
                                .split(' ')
                                .nth(1)
                                .unwrap_or(""),
                            p.max_el
                        )));
                    }
                }
            }
            (Some(p), false) => {
                ui.label(dim(&format!(
                    "Next pass {} UTC ({}) · rises {:.0}° {} · max {:.0}°",
                    sdroxide_solar::timefmt::ymd_hm(p.rise_unix),
                    in_words(p.rise_unix - now),
                    p.rise_az,
                    compass(p.rise_az),
                    p.max_el
                )));
            }
            (None, false) => {
                ui.label(dim("No pass inside the next 48 hours from your QTH."));
            }
        }

        // The warnings, in the order an operator has to act on them.
        if t.stale_elements {
            ui.label(
                RichText::new("⚠ Elements stale — corrections suspended until a TLE refresh")
                    .size(10.5)
                    .color(theme::YELLOW()),
            );
        } else if !t.corrections_active && win.sent.as_ref().is_none_or(|c| c.doppler) {
            ui.label(
                RichText::new("Doppler correction needs an IQ front end — a CAT rig tracks only")
                    .size(10.5)
                    .color(theme::CYAN_DIM()),
            );
        }
        if let Some(s) = &self.rigctld_status {
            if s.running && s.clients > 0 {
                ui.label(
                    RichText::new(format!(
                        "⚠ {} rigctld client{} connected — satellite software steering the dial \
                         there would correct Doppler twice",
                        s.clients,
                        if s.clients == 1 { "" } else { "s" }
                    ))
                    .size(10.5)
                    .color(theme::YELLOW()),
                );
            }
        }
        if let Some((connected, az, el, err)) = &self.rotator_status {
            if win.sent.as_ref().is_some_and(|c| c.rotator) {
                let line = if *connected {
                    RichText::new(format!("Rotator: antenna at {az:.0}° az {el:.0}° el"))
                        .color(theme::CYAN_DIM())
                } else {
                    match err {
                        Some(e) => RichText::new(format!("Rotator: {e}")).color(theme::YELLOW()),
                        None => RichText::new("Rotator: not connected").color(theme::CYAN_DIM()),
                    }
                };
                ui.label(line.size(10.5));
            }
        }

        // The two switches ride the lock config, so flipping one is just the
        // same command again with one field changed.
        if let Some(sent) = win.sent.as_mut() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if crate::chrome::chip(ui, sent.doppler, "DOPPLER")
                    .on_hover_text("Apply the computed correction to RX and TX")
                    .clicked()
                {
                    sent.doppler = !sent.doppler;
                    cmds.push(Command::SetSatLock(Some(Box::new(sent.clone()))));
                }
                if crate::chrome::chip(ui, sent.rotator, "ROTATOR")
                    .on_hover_text(
                        "Steer the antenna through the rotctld client (Settings ▸ Servers)",
                    )
                    .clicked()
                {
                    sent.rotator = !sent.rotator;
                    cmds.push(Command::SetSatLock(Some(Box::new(sent.clone()))));
                }
            });
        }
    }

    /// The picker: every satellite the station tracks, searchable, with the
    /// published links of the chosen one and the TUNE / LOCK actions.
    fn sat_picker(&mut self, ui: &mut egui::Ui, win: &mut SatWinState, cmds: &mut Vec<Command>) {
        let dim = |s: &str| RichText::new(s).size(9.5).color(theme::CYAN_DIM());
        let now = crate::time::now_unix();
        let qth = sdroxide_types::grid_to_latlon(&self.my_grid());

        // (Re)build the list when the config changed or the memos aged out.
        let stale = win.list.as_ref().is_none_or(|l| {
            !std::sync::Arc::ptr_eq(&l.cfg, &self.sat_cfg) || now - l.built_unix > MEMO_FRESH_S
        });
        if stale {
            win.list = Some(build_list(&self.sat_cfg, now));
        }
        let list = win.list.as_mut().expect("built above");

        ui.horizontal(|ui| {
            ui.label(RichText::new("🔍").size(11.0));
            ui.add(
                egui::TextEdit::singleline(&mut win.search)
                    .desired_width(160.0)
                    .hint_text("name or NORAD"),
            );
            if !win.search.is_empty() && ui.small_button("×").clicked() {
                win.search.clear();
            }
            if qth.is_none() {
                ui.label(
                    RichText::new("set your grid for passes (Settings ▸ General)")
                        .size(9.5)
                        .color(theme::YELLOW()),
                );
            }
        });
        ui.add_space(4.0);

        // Fill at most one pass memo per frame — see the module note.
        if let Some((lat, lon)) = qth {
            if let Some(e) = list
                .entries
                .iter_mut()
                .filter(|e| e.matches(&win.search) && e.sat.is_some())
                .find(|e| e.pass.as_ref().is_none_or(|(at, _)| now - at > MEMO_FRESH_S))
            {
                let sat = e.sat.as_ref().expect("filtered above");
                let memo = match sat.next_passes(lat, lon, now as f64, 48.0 * 3600.0, 1) {
                    sdroxide_solar::PassSearch::Passes(p) => match p.first() {
                        Some(p) => PassMemo::Pass(sdroxide_types::SatPass {
                            rise_unix: p.rise_unix,
                            set_unix: p.set_unix,
                            rise_az: p.rise_az,
                            set_az: p.set_az,
                            max_el: p.max_el,
                            max_el_unix: p.max_el_unix,
                        }),
                        None => PassMemo::Never,
                    },
                    sdroxide_solar::PassSearch::AlwaysVisible { .. } => PassMemo::Always,
                    sdroxide_solar::PassSearch::NeverVisible => PassMemo::Never,
                };
                e.pass = Some((now, memo));
                ui.ctx().request_repaint(); // keep filling the memos
            }
        }

        // Displayed rows: what matches, sorted into operating order — up now
        // first (highest first), then whoever rises next.
        let mut rows: Vec<(usize, Option<f64>, i64)> = list
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.matches(&win.search))
            .map(|(i, e)| {
                let el = match (&e.sat, qth) {
                    (Some(s), Some((lat, lon))) => {
                        s.at(now as f64).map(|st| st.elevation_from(lat, lon))
                    }
                    _ => None,
                };
                let rise = match &e.pass {
                    Some((_, PassMemo::Pass(p))) => p.rise_unix,
                    Some((_, PassMemo::Always)) => 0,
                    _ => i64::MAX,
                };
                (i, el, rise)
            })
            .collect();
        rows.sort_by(|a, b| {
            let vis_a = a.1.unwrap_or(-90.0) > 0.0;
            let vis_b = b.1.unwrap_or(-90.0) > 0.0;
            vis_b
                .cmp(&vis_a)
                .then_with(|| {
                    if vis_a && vis_b {
                        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        a.2.cmp(&b.2)
                    }
                })
                .then_with(|| list.entries[a.0].name.cmp(&list.entries[b.0].name))
        });

        // Full width, not content width: the grid hugs its text otherwise and
        // leaves half the window empty with the scrollbar in the middle.
        egui::ScrollArea::vertical().max_height(190.0).auto_shrink([false, true]).show(ui, |ui| {
            let col_w = ((ui.available_width() - 2.0 * 14.0) / 3.0).max(80.0);
            egui::Grid::new("sat-list")
                .num_columns(3)
                .min_col_width(col_w)
                .spacing([14.0, 2.0])
                .striped(true)
                .show(ui, |ui| {
                    ui.label(dim("SATELLITE"));
                    ui.label(dim("NORAD"));
                    ui.label(dim("PASS"));
                    ui.end_row();
                    for (i, el, _) in &rows {
                        let e = &list.entries[*i];
                        let selected = win.selected == Some(e.norad_id);
                        let name = RichText::new(&e.name).size(11.0);
                        // A selected row sits on the cyan selection fill, and
                        // egui keeps the label's own color there — so it has
                        // to be the dark on-cyan ink explicitly, the same ink
                        // the accent chips use. The accents are for
                        // unselected rows alone.
                        let name = match el {
                            _ if selected => name.color(theme::INK_ON_CYAN()).strong(),
                            Some(el) if *el > 0.0 => name.color(theme::GREEN()).strong(),
                            _ => name,
                        };
                        if ui.selectable_label(selected, name).clicked() {
                            win.selected = if selected { None } else { Some(e.norad_id) };
                            win.link_idx = 0;
                        }
                        ui.label(RichText::new(e.norad_id.to_string()).size(10.0));
                        let pass = match (el, &e.pass, &e.sat) {
                            (Some(el), _, _) if *el > 0.0 => {
                                RichText::new(format!("● up now, {el:.0}°")).color(theme::GREEN())
                            }
                            (_, Some((_, PassMemo::Pass(p))), _) => {
                                RichText::new(in_words(p.rise_unix - now))
                            }
                            (_, Some((_, PassMemo::Always)), _) => {
                                RichText::new("always visible").color(theme::GREEN())
                            }
                            (_, Some((_, PassMemo::Never)), _) => {
                                RichText::new("not from here").color(theme::CYAN_DIM())
                            }
                            (_, None, None) => RichText::new("no elements").color(theme::YELLOW()),
                            (_, None, Some(_)) => RichText::new("…"),
                        };
                        ui.label(pass.size(10.0));
                        ui.end_row();
                    }
                });
        });

        let Some(id) = win.selected else {
            ui.add_space(4.0);
            ui.label(dim("Pick a satellite to see its links and lock on."));
            return;
        };
        let Some(entry_idx) = list.entries.iter().position(|e| e.norad_id == id) else {
            win.selected = None;
            return;
        };

        // The operator's own frequency table wins over the built-in one, the
        // same rule the 3D view's pass window applies.
        let freqs = match self.sat_cfg.freqs_for(id) {
            Some(f) => Some(f.clone()),
            None => sdroxide_solar::satfreq::builtin_for(id).cloned(),
        };
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);

        let links: Vec<sdroxide_types::SatLink> =
            freqs.as_ref().map(|f| f.usable_links().cloned().collect()).unwrap_or_default();
        if links.is_empty() {
            ui.label(dim("No frequencies on file for this one — add them in Settings ▸ TLE."));
            return;
        }
        win.link_idx = win.link_idx.min(links.len() - 1);

        // Spread across the window like the list above, for the same reason.
        let link_col_w = ((ui.available_width() - 3.0 * 14.0) / 4.0).max(70.0);
        egui::Grid::new("sat-links")
            .num_columns(4)
            .min_col_width(link_col_w)
            .spacing([14.0, 2.0])
            .show(ui, |ui| {
                ui.label(dim("LINK"));
                ui.label(dim("DOWNLINK (MHz)"));
                ui.label(dim("UPLINK (MHz)"));
                ui.label(dim("MODE"));
                ui.end_row();
                for (i, l) in links.iter().enumerate() {
                    let sel = i == win.link_idx;
                    // The selected row sits on the cyan selection fill and
                    // keeps the label's own color, so that color has to be
                    // the dark on-cyan ink or it vanishes into the fill.
                    let mut label = RichText::new(&l.label).size(10.5);
                    if sel {
                        label = label.color(theme::INK_ON_CYAN()).strong();
                    }
                    let resp = ui.selectable_label(sel, label);
                    if !l.note.is_empty() {
                        resp.clone().on_hover_text(&l.note);
                    }
                    if resp.clicked() {
                        win.link_idx = i;
                    }
                    ui.label(
                        RichText::new(l.downlink.map_or_else(|| "—".into(), |b| b.to_string()))
                            .size(10.5)
                            .color(theme::GREEN()),
                    );
                    ui.label(
                        RichText::new(l.uplink.map_or_else(|| "—".into(), |b| b.to_string()))
                            .size(10.5)
                            .color(theme::YELLOW()),
                    );
                    let mode =
                        if l.inverting { format!("{} · inv", l.mode) } else { l.mode.clone() };
                    ui.label(RichText::new(mode).size(10.5));
                    ui.end_row();
                }
            });
        let link = &links[win.link_idx];
        if !link.note.is_empty() {
            ui.label(dim(&format!("{} — {}", link.label, link.note)));
        }
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            if crate::chrome::chip(ui, false, "TUNE")
                .on_hover_text("Set the dial and mode to this link — no lock, no correction")
                .clicked()
            {
                if link.downlink.is_some() {
                    cmds.push(Command::SetVfo {
                        vfo: Vfo::A,
                        hz: lock_downlink_hz(link, self.state.active_freq_hz()),
                    });
                }
                cmds.push(Command::SetMode {
                    rx: sdroxide_types::RxId::Main,
                    mode: mode_for_link(link),
                });
            }
            let locked_here = self.sat_track.as_ref().is_some_and(|t| t.norad_id == id);
            if crate::chrome::chip_accent(
                ui,
                locked_here,
                RichText::new(if locked_here { " LOCKED " } else { " LOCK ON " }).strong(),
                theme::GREEN(),
                theme::INK_ON_CYAN(),
            )
            .on_hover_text(
                "Track this satellite: Doppler correction on RX and TX, transponder-mapped \
                 uplink, and the 3D view follows it",
            )
            .clicked()
                && !locked_here
            {
                let e = &list.entries[entry_idx];
                let cfg = SatLockConfig {
                    norad_id: id,
                    name: e.name.clone(),
                    tle: e.tle.clone(),
                    observer: qth,
                    downlink_hz: lock_downlink_hz(link, self.state.active_freq_hz()),
                    uplink: SatUplink::from_link(link),
                    doppler: true,
                    rotator: win.sent.as_ref().map(|c| c.rotator).unwrap_or(true),
                };
                cmds.push(Command::SetMode {
                    rx: sdroxide_types::RxId::Main,
                    mode: mode_for_link(link),
                });
                cmds.push(Command::SetSatLock(Some(Box::new(cfg.clone()))));
                win.sent = Some(cfg);
            }
        });
    }

    /// Lock on from outside the window — the 3D view's LOCK button lands
    /// here. Picks the satellite's first two-way link (or first link at all),
    /// and opens the window on it either way so the operator can refine.
    /// Native-only caller (the 3D viewport), hence the wasm allowance.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(in crate::app) fn sat_lock_request(&mut self, norad_id: u64, cmds: &mut Vec<Command>) {
        self.show_sat = true;
        self.sat_win.selected = Some(norad_id);
        self.sat_win.link_idx = 0;
        let freqs = match self.sat_cfg.freqs_for(norad_id) {
            Some(f) => Some(f.clone()),
            None => sdroxide_solar::satfreq::builtin_for(norad_id).cloned(),
        };
        let links: Vec<sdroxide_types::SatLink> =
            freqs.as_ref().map(|f| f.usable_links().cloned().collect()).unwrap_or_default();
        // Prefer the first link that can be worked both ways; a beacon-only
        // satellite still locks for RX.
        let Some((idx, link)) = links
            .iter()
            .enumerate()
            .find(|(_, l)| l.uplink.is_some() && l.downlink.is_some())
            .or_else(|| links.first().map(|l| (0, l)))
        else {
            // No frequencies at all: open the window; the picker explains.
            return;
        };
        self.sat_win.link_idx = idx;
        let entry = self
            .sat_win
            .list
            .as_ref()
            .and_then(|l| l.entries.iter().find(|e| e.norad_id == norad_id));
        let name = entry
            .map(|e| e.name.clone())
            .or_else(|| freqs.as_ref().map(|f| f.name.clone()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| format!("NORAD {norad_id}"));
        let tle = entry.and_then(|e| e.tle.clone());
        let cfg = SatLockConfig {
            norad_id,
            name,
            tle,
            observer: sdroxide_types::grid_to_latlon(&self.my_grid()),
            downlink_hz: lock_downlink_hz(link, self.state.active_freq_hz()),
            uplink: SatUplink::from_link(link),
            doppler: true,
            rotator: true,
        };
        cmds.push(Command::SetMode { rx: sdroxide_types::RxId::Main, mode: mode_for_link(link) });
        cmds.push(Command::SetSatLock(Some(Box::new(cfg.clone()))));
        self.sat_win.sent = Some(cfg);
    }
}
