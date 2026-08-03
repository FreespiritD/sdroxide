//! The FT8/FT4/JS8 decode list, and the QSO sequencer area beside it.
//!
//! Rows are grouped into transmit slots and, within a slot, ordered by the
//! operator's choice of signal or distance. Each row resolves up front what it
//! needs to colour itself — distance, whether it is a CQ we may answer, what
//! working the station would be worth against the log — because the list is
//! rebuilt from scratch every frame.

use eframe::egui::{self, Color32, RichText};
use sdroxide_types::{Command, Decode, Mode};

use crate::theme::ThemedScroll;
use crate::time::now_unix;

use crate::app::SdroxideApp;
use crate::app::panels::widgets::{row_cell, snr_color, station_card};

/// How the FT8/FT4 decode list orders the stations within each turn.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::app) enum DecodeSort {
    /// As received (no reordering).
    #[default]
    None,
    /// Strongest signal first.
    Signal,
    /// Farthest (DX) first.
    Distance,
}

/// One decode as the list draws it: the entry itself, plus everything the row
/// needs resolved once up front — how far away they are, whether this is a CQ
/// *we* may answer, what working them would be worth against the log, and
/// whether the message names our own station.
#[derive(Clone, Copy)]
struct DecodeRow<'a> {
    /// Index into `digi_decodes`, so rows keep their identity after sorting.
    idx: usize,
    d: &'a Decode,
    dist_km: Option<f64>,
    cq: bool,
    novelty: sdroxide_types::Novelty,
    to_me: bool,
}

/// Whether a spot's announced mode is the one this panel works.
///
/// Feeds are inconsistent about case and about the FT4 spelling — the clusters
/// carry both `FT4` and the older `FT8-4` — so this compares loosely rather than
/// going through [`sdroxide_types::Mode`], which would also fold every other
/// data mode into `USB`.
fn is_ft8_or_ft4(mode: &str) -> bool {
    let m = mode.trim();
    m.eq_ignore_ascii_case("FT8")
        || m.eq_ignore_ascii_case("FT4")
        || m.eq_ignore_ascii_case("FT8-4")
}

impl SdroxideApp {
    /// Touch-friendly decode list with a per-row REPLY button. Clicking a
    /// row moves the TX audio frequency to that signal; REPLY starts a QSO.
    pub(in crate::app) fn decode_list(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let phone = crate::layout::tier(ui.ctx()) == crate::layout::Tier::Phone;
        // The tab row above already says which view this is and how many
        // stations came in; a second header would be a row of a phone's screen
        // spent repeating it.
        if !phone {
            ui.horizontal(|ui| {
                ui.label(RichText::new("DECODES").size(9.5).strong().color(crate::theme::CYAN_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} rx", self.digi_decodes.len()))
                            .size(10.0)
                            .color(Color32::from_gray(120)),
                    );
                });
            });
        }
        // Per-turn ordering + a CQ-only filter for the decode list.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new("Sort").size(9.5).color(crate::theme::CYAN_DIM));
            for (m, base) in [
                (DecodeSort::None, "None"),
                (DecodeSort::Signal, "SNR"),
                (DecodeSort::Distance, "Dist"),
            ] {
                let active = self.digi_sort == m;
                // Active mode shows its direction; re-pressing it flips direction.
                let lbl = if active && m != DecodeSort::None {
                    format!("{base} {}", if self.digi_sort_desc { "↓" } else { "↑" })
                } else {
                    base.to_string()
                };
                if crate::chrome::chip(ui, active, lbl).clicked() {
                    if active && m != DecodeSort::None {
                        self.digi_sort_desc = !self.digi_sort_desc;
                    } else {
                        self.digi_sort = m;
                        self.digi_sort_desc = true; // default: strongest / farthest first
                    }
                }
            }
            ui.add_space(8.0);
            if crate::chrome::chip(ui, self.digi_cq_only, "CQ only")
                .on_hover_text(
                    "Only stations calling CQ — and only the calls you may answer: a directed \
                     CQ (DX, EU, JA, POTA, TEST …) is listed when it names you and hidden when \
                     it names someone else.",
                )
                .clicked()
            {
                self.digi_cq_only = !self.digi_cq_only;
            }
            if crate::chrome::chip(ui, self.digi_new_only, "New only")
                .on_hover_text("Only stations that would be new: entity, band-slot, grid, or call")
                .clicked()
            {
                self.digi_new_only = !self.digi_new_only;
            }
            // Whether the engine chooses our transmit frequency. Here rather
            // than in the setup window because it decides what clicking a
            // decode in this list does.
            if self.digi_cfg_seeded {
                let auto = self.digi_cfg_edit.auto_tx_freq;
                if crate::chrome::chip(ui, auto, "Auto TX FRQ")
                    .on_hover_text(
                        "Pick our transmit frequency automatically: the quietest spot in the \
                         period we transmit in, rather than the frequency of whoever we are \
                         answering — they transmit in the other period, so theirs says nothing \
                         about who is there when we key. Off holds the frequency you set.",
                    )
                    .clicked()
                {
                    self.digi_cfg_edit.auto_tx_freq = !auto;
                    cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
                }
            }
        });
        ui.add_space(2.0);
        // Call of the currently previewed decode (cloned so the scroll closure
        // doesn't hold a borrow of `self` we need to write back afterwards).
        let preview_call = self.digi_preview.as_ref().map(|(c, _)| c.clone());
        // Own grid, for the per-decode great-circle distance column.
        let my_grid =
            self.digi_status.as_ref().map(|s| s.config.my_grid.clone()).unwrap_or_default();
        // Own callsign, to spotlight decodes addressed to us.
        let my_call =
            self.digi_status.as_ref().map(|s| s.config.my_call.clone()).unwrap_or_default();
        // Stations already marked to work, so their queue button reads as set.
        let queued_calls: Vec<String> = self
            .digi_status
            .as_ref()
            .map(|s| s.call_queue.iter().map(|q| q.call.clone()).collect())
            .unwrap_or_default();
        // Staged preview change: `None` = no click this frame; `Some(v)` =
        // replace the preview with `v` (`Some(None)` clears it).
        let mut new_preview: Option<Option<(String, (f64, f64))>> = None;
        // Location of the row hovered this frame → yellow dot on the map.
        let mut hover_ll: Option<(f64, f64)> = None;
        let cq_only = self.digi_cq_only;
        let new_only = self.digi_new_only;
        let auto_tx_freq = self.digi_status.as_ref().map(|s| s.config.auto_tx_freq).unwrap_or(true);
        let sort = self.digi_sort;
        let desc = self.digi_sort_desc;
        // Turn parity needs the mode's slot length (FT8 15 s, FT4 7.5 s). JS8's
        // is an operator setting rather than implied by the mode, so it comes
        // from the status — otherwise Turbo draws one turn header per 2.5 turns.
        let period = self.slot_period_s();
        // The band we'd log a contact on, for the "is this one new?" judgement.
        let dial_hz = self.state.active_freq_hz();
        let band = if dial_hz > 0.0 { sdroxide_types::adif_band(dial_hz) } else { "" };
        // Refresh the log index (needs &mut self), then borrow it beside the
        // decodes for the per-row novelty lookups.
        self.log_index();
        let log_ix = &self.log_index_cache.as_ref().expect("just refreshed").1;
        // Filter (CQ-only / new-only) and precompute distance for sorting and
        // display. Entries stay newest-turn-first; same-slot decodes are
        // contiguous in the list. A "CQ DX" from a station we're local to is not
        // a CQ *we* can answer: it neither passes the filter nor gets the CQ
        // highlight.
        let mut items: Vec<DecodeRow> = self
            .digi_decodes
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                // A message addressed to our own station survives every filter.
                // It is the one decode in the list we owe an answer to, and
                // hiding it behind "CQ only" — a station calling us back is not
                // calling CQ — leaves no REPLY button anywhere to answer from.
                let to_me = !my_call.is_empty() && d.to.as_deref() == Some(my_call.as_str());
                let cq = sdroxide_types::cq_is_for_us(d, &my_call, &my_grid);
                if cq_only && !cq && !to_me {
                    return None;
                }
                let novelty =
                    log_ix.novelty(d.from.as_deref().unwrap_or(""), d.grid.as_deref(), band);
                if new_only && !novelty.is_new() && !to_me {
                    return None;
                }
                let dist_km = (!my_grid.is_empty())
                    .then(|| {
                        d.grid
                            .as_deref()
                            .and_then(|g| sdroxide_types::grid_distance_km(&my_grid, g))
                    })
                    .flatten();
                Some(DecodeRow { idx: i, d, dist_km, cq, novelty, to_me })
            })
            .collect();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_themed(ui, |ui| {
            let mut gi = 0;
            while gi < items.len() {
                // A turn is one slot: group the contiguous same-slot decodes.
                let slot = items[gi].d.slot_utc;
                let mut end = gi;
                while end < items.len() && items[end].d.slot_utc == slot {
                    end += 1;
                }
                match sort {
                    DecodeSort::None => {}
                    DecodeSort::Signal => items[gi..end].sort_by(|a, b| {
                        let o = a.d.snr_db.cmp(&b.d.snr_db);
                        if desc { o.reverse() } else { o }
                    }),
                    DecodeSort::Distance => items[gi..end].sort_by(|a, b| {
                        // Decodes without a grid always sort last (push them to the
                        // far end of whichever direction is active).
                        let sentinel = if desc { f64::NEG_INFINITY } else { f64::INFINITY };
                        let ka = a.dist_km.unwrap_or(sentinel);
                        let kb = b.dist_km.unwrap_or(sentinel);
                        let o = ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal);
                        if desc { o.reverse() } else { o }
                    }),
                }
                // Turn separator: even/odd parity + UTC timestamp.
                let even = ((slot as f64 / period).round() as i64).rem_euclid(2) == 0;
                let s = slot.rem_euclid(86_400);
                let tstr = format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60);
                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    let (ptxt, pcol) = if even {
                        ("EVEN", crate::theme::CYAN)
                    } else {
                        ("ODD", crate::theme::YELLOW)
                    };
                    ui.label(RichText::new(ptxt).size(9.0).strong().color(pcol));
                    ui.label(
                        RichText::new(format!("{tstr} UTC"))
                            .size(9.5)
                            .monospace()
                            .color(Color32::from_gray(130)),
                    );
                });
                ui.separator();
                for k in gi..end {
                    let DecodeRow { idx: i, d, dist_km, cq, novelty, to_me } = items[k];
                    // Free text names no sender, and a hashed callsign nobody
                    // has heard yet resolves to none either — say which it is
                    // rather than showing a bare "?".
                    let who = d
                        .from
                        .clone()
                        .unwrap_or_else(|| if d.free_text { "TEXT".into() } else { "?".into() });
                    // What this station would be worth working: one badge, and
                    // a dupe fades the row back so the new ones carry the eye.
                    let (badge, badge_col) = match novelty.highlight() {
                        Some(sdroxide_types::Highlight::NewDxcc) => ("DXCC", crate::theme::PINK),
                        Some(sdroxide_types::Highlight::NewDxccBand) => {
                            ("BAND", crate::theme::YELLOW)
                        }
                        Some(sdroxide_types::Highlight::NewGrid) => ("GRID", crate::theme::CYAN),
                        Some(sdroxide_types::Highlight::NewCall) => ("NEW", crate::theme::CYAN_DIM),
                        Some(sdroxide_types::Highlight::Dupe) => ("DUPE", Color32::from_gray(85)),
                        None => ("", Color32::TRANSPARENT),
                    };
                    let dupe = novelty.dupe;
                    let grid = d.grid.clone().unwrap_or_default();
                    // Where in the world they are, from the callsign alone —
                    // most decodes carry no grid, and the entity always knows.
                    let entity = d.from.as_deref().and_then(sdroxide_types::resolve_callsign);
                    let continent = entity.map(|e| e.continent).unwrap_or("");
                    let is_preview =
                        d.from.is_some() && preview_call.as_deref() == d.from.as_deref();
                    let queued = d
                        .from
                        .as_deref()
                        .is_some_and(|f| queued_calls.iter().any(|q| q.eq_ignore_ascii_case(f)));
                    let mut reply = false;
                    let mut queue = false;
                    // Left edge of the REPLY button, so the row-body click area can
                    // exclude it (otherwise the full-row interaction below sits on
                    // top of the button and swallows its clicks).
                    let mut reply_left: Option<f32> = None;

                    let inner = egui::Frame::new()
                        .fill(if to_me {
                            crate::theme::TOME_BG
                        } else if cq {
                            crate::theme::CQ_BG
                        } else {
                            crate::theme::ROW_BG
                        })
                        .inner_margin(egui::Margin { left: 11, right: 6, top: 6, bottom: 6 })
                        .show(ui, |ui| {
                            // Fixed-width columns so every field lines up down the
                            // list. Right-aligned numbers, then callsign (wide
                            // proportional font), grid, and the message filling the
                            // rest with a right-pinned REPLY button.
                            let ch = 22.0;
                            ui.horizontal(|ui| {
                                ui.set_min_height(ch);
                                ui.spacing_mut().item_spacing.x = 7.0;
                                let cell =
                                    |ui: &mut egui::Ui,
                                     w: f32,
                                     align_right: bool,
                                     lbl: egui::Label| {
                                        row_cell(ui, w, ch, align_right, lbl)
                                    };
                                // SNR.
                                cell(
                                    ui,
                                    28.0,
                                    true,
                                    egui::Label::new(
                                        RichText::new(format!("{:+}", d.snr_db))
                                            .monospace()
                                            .size(13.0)
                                            .color(snr_color(d.snr_db)),
                                    ),
                                );
                                // Audio frequency.
                                cell(
                                    ui,
                                    40.0,
                                    true,
                                    egui::Label::new(
                                        RichText::new(format!("{:.0}", d.audio_hz))
                                            .monospace()
                                            .size(12.0)
                                            .color(Color32::from_gray(120)),
                                    ),
                                );
                                // Callsign — wider proportional (button) font.
                                cell(
                                    ui,
                                    98.0,
                                    false,
                                    egui::Label::new(
                                        RichText::new(&who).size(15.0).strong().color(if to_me {
                                            crate::theme::YELLOW
                                        } else if d.from.is_none() || dupe {
                                            Color32::from_gray(105)
                                        } else if cq {
                                            crate::theme::GREEN
                                        } else {
                                            crate::theme::TEXT_STRONG
                                        }),
                                    )
                                    .truncate(),
                                );
                                // What they'd be worth: new entity / band / grid /
                                // call, or a dupe already in the log for this band.
                                cell(
                                    ui,
                                    34.0,
                                    false,
                                    egui::Label::new(
                                        RichText::new(badge).size(9.5).strong().color(badge_col),
                                    ),
                                );
                                // Continent — the band's opening, readable down
                                // the column without reading a single callsign.
                                cell(
                                    ui,
                                    24.0,
                                    false,
                                    egui::Label::new(
                                        RichText::new(continent)
                                            .monospace()
                                            .size(11.0)
                                            .strong()
                                            .color(if dupe {
                                                Color32::from_gray(85)
                                            } else {
                                                crate::theme::continent_color(continent)
                                            }),
                                    ),
                                );
                                // Grid.
                                cell(
                                    ui,
                                    44.0,
                                    false,
                                    egui::Label::new(
                                        RichText::new(&grid)
                                            .monospace()
                                            .size(12.0)
                                            .color(crate::theme::CYAN_DIM),
                                    ),
                                );
                                // Distance (km, great-circle from my grid).
                                cell(
                                    ui,
                                    58.0,
                                    true,
                                    egui::Label::new(
                                        RichText::new(
                                            dist_km
                                                .map(|km| format!("{km:.0} km"))
                                                .unwrap_or_default(),
                                        )
                                        .monospace()
                                        .size(11.0)
                                        .color(crate::theme::YELLOW),
                                    ),
                                );
                                // Message fills the remaining width; REPLY and the
                                // queue button pinned right.
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let resp = crate::chrome::chip_accent(
                                            ui,
                                            false,
                                            RichText::new("REPLY").size(12.0).strong(),
                                            if to_me {
                                                crate::theme::YELLOW
                                            } else if cq {
                                                crate::theme::GREEN
                                            } else {
                                                crate::theme::CYAN
                                            },
                                            crate::theme::INK_ON_CYAN,
                                        );
                                        reply = resp.clicked();
                                        // Mark for later. Pressing it again drops
                                        // the station, so one button both queues
                                        // and un-queues.
                                        let qresp = crate::chrome::chip(
                                            ui,
                                            queued,
                                            RichText::new(if queued { "＋" } else { "+" })
                                                .size(12.0)
                                                .strong(),
                                        )
                                        .on_hover_text(if queued {
                                            "Queued — click to remove"
                                        } else {
                                            "Work this station after the current one"
                                        });
                                        queue = qresp.clicked();
                                        reply_left = Some(resp.rect.left().min(qresp.rect.left()));
                                        ui.with_layout(
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&d.message)
                                                            .monospace()
                                                            .size(12.5)
                                                            .color(if dupe {
                                                                Color32::from_gray(95)
                                                            } else {
                                                                crate::theme::TEXT
                                                            }),
                                                    )
                                                    .truncate(),
                                                );
                                            },
                                        );
                                    },
                                );
                            });
                        });

                    let r = inner.response.rect;
                    // Left-accent bar: gold (to us) / red (CQ) / cyan (other). Wider
                    // for a to-us decode so it really pops — and for a directed CQ
                    // (DX, EU, JA …) that names us, which is a better prospect than
                    // a plain CQ anyone in the world is free to answer.
                    let (accent, aw) = if to_me {
                        (crate::theme::YELLOW, 4.0)
                    } else if cq {
                        (crate::theme::PINK, if d.cq_to.is_some() { 4.0 } else { 2.5 })
                    } else {
                        (crate::theme::CYAN_DIM, 2.5)
                    };
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(
                            r.left_top(),
                            egui::pos2(r.left() + aw, r.bottom()),
                        ),
                        0.0,
                        accent,
                    );
                    // Row-body click (everything left of the REPLY button) tunes
                    // the audio freq. Excluding the button's rect keeps this
                    // interaction from covering — and stealing clicks from — REPLY.
                    let body_right = reply_left.map(|x| x - 2.0).unwrap_or(r.right());
                    let body_rect =
                        egui::Rect::from_min_max(r.left_top(), egui::pos2(body_right, r.bottom()));
                    let row =
                        ui.interact(body_rect, ui.id().with(("dec", i)), egui::Sense::click());
                    // Everything already resolved about this station, gathered
                    // where there is room to say it: the row itself has to fit
                    // twenty of these on screen.
                    let row = row.on_hover_ui(|ui| {
                        station_card(ui, d, entity, dist_km, &my_grid, novelty, band, queued, cq);
                    });
                    if is_preview {
                        // Amber outline ties this row to its faint map marker.
                        ui.painter().rect_stroke(
                            r,
                            0.0,
                            egui::Stroke::new(1.0, crate::theme::YELLOW),
                            egui::StrokeKind::Inside,
                        );
                    } else if to_me {
                        // A message to our own station: a gold box so it can't be missed.
                        ui.painter().rect_stroke(
                            r,
                            0.0,
                            egui::Stroke::new(1.4, crate::theme::YELLOW),
                            egui::StrokeKind::Inside,
                        );
                    } else if row.hovered() {
                        ui.painter().rect_stroke(
                            r,
                            0.0,
                            egui::Stroke::new(1.0, crate::theme::CYAN_DIM),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if row.hovered() {
                        hover_ll = d.grid.as_deref().and_then(sdroxide_types::grid_to_latlon);
                    }
                    if reply {
                        if let Some(from) = &d.from {
                            // If they're neither calling CQ nor calling us, hold until
                            // they call CQ rather than barging into their exchange.
                            // Any CQ counts here, including a "CQ DX" we're local to:
                            // the operator asked to call them, and they are free now.
                            cmds.push(Command::DigiStartQso {
                                from: from.clone(),
                                grid: d.grid.clone(),
                                snr: d.snr_db,
                                audio_hz: d.audio_hz,
                                wait_for_cq: !d.is_cq && !to_me,
                            });
                        }
                        // Starting a QSO promotes the station to the active DX
                        // marker; drop the faint preview so they don't overlap.
                        new_preview = Some(None);
                        // On a phone the exchange happens in the other view, so
                        // answering somebody takes you there. Leaving the
                        // operator on the decode list would have them watching
                        // the sequencer they just started from behind a chip.
                        if phone {
                            self.view.digi_pane =
                                crate::app::panels::pane_index(self.state.rx[0].mode, "QSO");
                        }
                    } else if queue {
                        if let Some(from) = &d.from {
                            if queued {
                                cmds.push(Command::DigiQueueRemove(from.clone()));
                            } else {
                                cmds.push(Command::DigiQueueAdd {
                                    from: from.clone(),
                                    grid: d.grid.clone(),
                                    snr: d.snr_db,
                                    audio_hz: d.audio_hz,
                                    // Same judgement the reply button makes: a
                                    // station mid-exchange is not free yet.
                                    wait_for_cq: !d.is_cq && !to_me,
                                });
                            }
                        }
                    } else if row.clicked() {
                        // Moving our transmit onto theirs is exactly what Auto
                        // TX FRQ exists to avoid, so with it on a click only
                        // previews the station.
                        if !auto_tx_freq {
                            cmds.push(Command::SetDigiAudioFreq(d.audio_hz));
                        }
                        // Preview this station's location (if it sent a grid).
                        let ll = d.grid.as_deref().and_then(sdroxide_types::grid_to_latlon);
                        new_preview = Some(ll.map(|ll| (who.clone(), ll)));
                    }
                    ui.add_space(3.0);
                }
                gi = end;
            }
        });
        self.digi_hover_ll = hover_ll;
        if let Some(sel) = new_preview {
            self.digi_preview = sel;
        }
    }

    /// The QSO operating area to the right of the decode list: header row,
    /// world map, station card, transcript, and action buttons.
    pub(in crate::app) fn qso_area(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let status = self.digi_status.clone();

        let tier = crate::layout::tier(ui.ctx());
        let compact = tier.compact();
        let phone = tier == crate::layout::Tier::Phone;

        // Header: QSO left, session log + downloads centered, SETUP right.
        // The count is this run's; the export buttons still save the whole
        // logbook, which is what their hover text says.
        //
        // A phone keeps only the two that steer the mode — the frequency chip
        // and SETUP. The session count and the ADIF/TXT exports are a row of
        // screen for something the logbook window (SYS → LOG) already offers,
        // exports included.
        let session = self.session_qsos;
        let logged = self.qso_log.len();
        // Measured, not assumed: this row holds `Button`s and chips, and both
        // grow with the touch style. Allocating the desktop's 26 points for a
        // 34-point row is what pushed the buttons at the bottom of this column
        // off the end of it.
        let row_h = 26.0_f32.max(ui.spacing().interact_size.y);
        let (row, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
        let third = row.width() / 3.0;
        let zone = |i: f32| {
            egui::Rect::from_min_size(
                egui::pos2(row.left() + i * third, row.top()),
                egui::vec2(third, row_h),
            )
        };
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(zone(0.0))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.label(RichText::new("QSO").size(9.5).strong().color(crate::theme::CYAN_DIM));
                self.digi_freq_chip(ui, cmds);
            },
        );
        if !phone {
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(zone(1.0))
                    .layout(egui::Layout::top_down(egui::Align::Center)),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("Session: {session} QSO"))
                                .size(11.0)
                                .color(Color32::from_gray(150)),
                        )
                        .on_hover_text("QSOs worked since sdroxide was started");
                        if ui
                            .add_enabled(logged > 0, egui::Button::new("ADIF"))
                            .on_hover_text(format!("Save the whole logbook ({logged} QSO) as ADIF"))
                            .clicked()
                        {
                            let adif = sdroxide_types::qso_log_to_adif(&self.qso_log);
                            crate::download::save("sdroxide-log.adi", adif.as_bytes());
                        }
                        if ui
                            .add_enabled(logged > 0, egui::Button::new("TXT"))
                            .on_hover_text(format!("Save the whole logbook ({logged} QSO) as text"))
                            .clicked()
                        {
                            let txt = sdroxide_types::qso_log_to_text(&self.qso_log);
                            crate::download::save("sdroxide-log.txt", txt.as_bytes());
                        }
                    });
                },
            );
        }
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(zone(2.0))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
            |ui| {
                if crate::chrome::chip(ui, self.show_digi_settings, "⚙ SETUP").clicked() {
                    self.show_digi_settings = !self.show_digi_settings;
                }
            },
        );

        ui.add_space(5.0);
        // World map — its height is a user-draggable fraction of the QSO area,
        // clamped so the station card + a usable transcript + the action buttons
        // stay visible. On very short windows it shrinks and then disappears.
        //
        // A compact layout has no map at all. It is the largest thing in this
        // column and the only one that is purely nice to have: everything else
        // here is either the state of the QSO or a control that changes it, and
        // on a tablet the map was taking the room they needed. The same
        // stations are still on the panadapter and in the 3D view.
        let show_map = !compact;
        let gap = 8.0;
        // Only the map budget needs this: the rows below the transcript place
        // themselves (see the bottom-up layout further down). Measured rather
        // than fixed at 44 so it still means "a row of buttons" on a layout
        // whose chips are half again as tall.
        let btn_h = crate::chrome::chip_height(ui, Some(15.0));
        let map_handle_h = 7.0;
        const CARD_RESERVE: f32 = 60.0;
        let full_h = ui.available_height();
        let avail_w = ui.available_width();
        // The map height is a user-draggable fraction of a range: from MIN_HEIGHT
        // up to whatever still leaves the station card + action buttons room below
        // (the transcript can shrink to nothing). `digi_map_fraction` slides
        // linearly across this range, so the divider tracks the cursor 1:1 and the
        // map genuinely grows/shrinks. Height is capped at the width (aspect ≤ 1).
        let map_lo = crate::widgets::worldmap::MIN_HEIGHT;
        let map_hi =
            (full_h - (map_handle_h + CARD_RESERVE + 5.0 + gap + btn_h)).min(avail_w).max(map_lo);
        let map_budget = map_lo + (map_hi - map_lo) * self.view.digi_map_fraction;
        let my_grid = status.as_ref().map(|s| s.config.my_grid.clone()).unwrap_or_default();
        // Everything from here to the map itself is only ever read by the map,
        // so a layout without one does none of it — including walking every
        // spot the client holds, once a frame.
        if show_map && map_budget >= crate::widgets::worldmap::MIN_HEIGHT {
            let hover_ll = self.digi_hover_ll;
            let home_ll = sdroxide_types::grid_to_latlon(&my_grid);
            let dx_grid = status.as_ref().and_then(|s| s.dx_grid.clone());
            let dx_ll = dx_grid.as_deref().and_then(sdroxide_types::grid_to_latlon);
            // A clicked (but not yet answered) decode shows as a faint preview.
            let preview_ll = self.digi_preview.as_ref().map(|(_, ll)| *ll);
            let tx_active = status.as_ref().map(|s| s.transmitting).unwrap_or(false);
            // White station dots fade over 2 minutes since a station was last
            // decoded, then expire (dropped from the map and from the zoom fit).
            // Ages use egui frame time (monotonic, works native + wasm); each grid
            // remembers the frame it was last freshly decoded in.
            let now_t = ui.input(|i| i.time);
            self.digi_stations.observe(&self.digi_decodes, now_t, now_unix());
            let stations = self.digi_stations.stations(now_t);
            // Located network spots (filtered by the shown-kind toggles), as
            // kind-coloured dots on the map.
            //
            // Only spots announced as FT8 or FT4, whatever fed them: this is the map
            // beside the FT8/FT4 decode list, and it exists to show where the mode is
            // being worked right now. Broadcast stations, FreeDV Reporter's hundreds
            // of connected stations, and the SSB/CW half of the cluster all buried
            // that with traffic from bands the panel cannot answer. They are still on
            // the panadapter overlay and in the SPOTS window.
            let spot_dots: Vec<(f64, f64, (u8, u8, u8))> = self
                .all_spots()
                .filter(|s| is_ft8_or_ft4(&s.mode))
                .filter(|s| self.spot_visible(s))
                .filter_map(|s| s.loc.map(|(lat, lon)| (lat, lon, s.kind.color())))
                .collect();
            let heat = self.prop_texture(ui.ctx(), self.state.rx_freq_hz());
            self.prop_map_controls(ui);
            crate::widgets::worldmap::show(
                ui,
                &mut self.map_view,
                home_ll,
                dx_ll,
                preview_ll,
                hover_ll,
                &stations,
                &spot_dots,
                heat,
                tx_active,
                map_budget,
            );
            // Draggable border between the map and the QSO form below it.
            let hresp = crate::chrome::split_handle(
                ui,
                egui::vec2(ui.available_width(), map_handle_h),
                None,
            );
            if hresp.dragged() {
                // 1:1 with the cursor: a drag of `dy` px moves the map edge `dy` px.
                let df = hresp.drag_delta().y / (map_hi - map_lo).max(1.0);
                self.view.digi_map_fraction = (self.view.digi_map_fraction + df).clamp(0.0, 1.0);
            }
        }
        // Station card.
        crate::chrome::red_panel(ui, |ui| {
            match status.as_ref() {
                Some(s) => {
                    ui.horizontal(|ui| {
                        // The contact is over and logged once we are confirming;
                        // green says so, since "Confirming" on its own reads like
                        // an exchange still in progress.
                        let done = s.step == sdroxide_types::QsoStep::Confirming;
                        let step_col = if done { crate::theme::GREEN } else { crate::theme::CYAN };
                        ui.label(RichText::new(s.step.label()).size(13.0).strong().color(step_col));
                        if done {
                            ui.label(
                                RichText::new("✓ QSO COMPLETE")
                                    .size(11.0)
                                    .strong()
                                    .color(crate::theme::GREEN),
                            )
                            .on_hover_text(
                                "The contact is complete and in the log. It is held here for a \
                                 few minutes so the final message can be re-sent if the other \
                                 station repeats theirs — nothing more is owed.",
                            );
                        }
                        if s.transmitting {
                            ui.label(
                                RichText::new("● TX").size(13.0).strong().color(crate::theme::PINK),
                            );
                        }
                        if s.config.dxped_mode != sdroxide_types::DxpedMode::Normal
                            && s.mode == Mode::Ft8
                        {
                            ui.label(
                                RichText::new(s.config.dxped_mode.label().to_uppercase())
                                    .size(11.0)
                                    .strong()
                                    .color(crate::theme::PINK),
                            )
                            .on_hover_text(
                                "DXpedition mode. The transmit frequency is held out of the \
                                 Fox's half of the passband (below 1000 Hz) until the Fox \
                                 answers.",
                            );
                        }
                        if s.tx_watchdog {
                            // The sequencer stood down on its own; say so, since
                            // an idle step alone looks like nothing happened.
                            ui.label(
                                RichText::new("WATCHDOG")
                                    .size(11.0)
                                    .strong()
                                    .color(crate::theme::YELLOW),
                            )
                            .on_hover_text(
                                "Transmitting stopped: no reply and no action for the watchdog \
                                 period. Call CQ or pick a message to resume.",
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{:.0} Hz · {} slots",
                                    s.audio_hz,
                                    if s.tx_even { "even" } else { "odd" }
                                ))
                                .size(11.0)
                                .color(Color32::from_gray(140)),
                            );
                            // What everyone else's timing says about ours. A
                            // clock far enough out that nobody can decode us
                            // looks exactly like a dead band from this side, so
                            // it is worth a permanent readout.
                            if let Some(off) = s.clock_offset_s {
                                use sdroxide_types::ClockHealth::*;
                                let health = sdroxide_types::clock_health(off);
                                let col = match health {
                                    Good => Color32::from_gray(140),
                                    Marginal => crate::theme::YELLOW,
                                    Bad => crate::theme::PINK,
                                };
                                let txt = RichText::new(format!("DT {off:+.1} s")).size(11.0);
                                ui.label(if health == Good {
                                    txt.color(col)
                                } else {
                                    txt.strong().color(col)
                                })
                                .on_hover_text(format!(
                                    "Your slot timing against the stations you are hearing.\n\
                                     {}\n\nIt covers the whole receive path, so a slow audio or \
                                     network chain counts the same as a wrong clock. Under 0.5 s \
                                     is comfortable.",
                                    match health {
                                        Good => "Well inside tolerance.".to_string(),
                                        _ => format!(
                                            "You transmit {:.1} s {} everyone else — the usual \
                                             reason calls go unanswered. Sync your computer clock.",
                                            off.abs(),
                                            if off > 0.0 { "before" } else { "after" },
                                        ),
                                    }
                                ));
                            }
                        });
                    });
                    match &s.dx_call {
                        Some(dx) => {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(dx)
                                        .size(17.0)
                                        .strong()
                                        .color(crate::theme::TEXT_STRONG),
                                );
                                if let Some(g) = &s.dx_grid {
                                    ui.label(
                                        RichText::new(g).size(13.0).color(crate::theme::CYAN_DIM),
                                    );
                                }
                                if let (Some(hg), Some(dg)) = (
                                    (!my_grid.is_empty()).then_some(my_grid.as_str()),
                                    s.dx_grid.as_deref(),
                                ) {
                                    if let (Some(km), Some(brg)) = (
                                        sdroxide_types::grid_distance_km(hg, dg),
                                        sdroxide_types::grid_bearing(hg, dg),
                                    ) {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{:.0} km · {:.0}°",
                                                        km, brg
                                                    ))
                                                    .size(12.0)
                                                    .color(crate::theme::YELLOW),
                                                );
                                            },
                                        );
                                    }
                                }
                            });
                        }
                        None => {
                            ui.label(
                                RichText::new("no active QSO — pick a decode to reply, or Call CQ")
                                    .size(11.0)
                                    .color(Color32::from_gray(120)),
                            );
                        }
                    }
                }
                None => {
                    ui.label(
                        RichText::new("FT8 engine idle").size(12.0).color(Color32::from_gray(130)),
                    );
                }
            }
        });

        // Fox pile-up: who is being worked and who is waiting. Only a Fox has
        // one, so its presence is the mode indicator.
        let fox_queue = status.as_ref().map(|s| s.fox_queue.clone()).unwrap_or_default();
        if !fox_queue.is_empty() {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(
                    RichText::new(format!("PILE-UP {}", fox_queue.len()))
                        .size(9.5)
                        .strong()
                        .color(crate::theme::CYAN_DIM),
                );
                for c in &fox_queue {
                    let col = if c.working { crate::theme::GREEN } else { Color32::from_gray(150) };
                    ui.label(RichText::new(&c.call).size(11.5).strong().color(col)).on_hover_text(
                        format!(
                            "{} · {:+} dB{}",
                            if c.working { "being worked" } else { "waiting" },
                            c.snr_db,
                            c.grid.as_deref().map(|g| format!(" · {g}")).unwrap_or_default(),
                        ),
                    );
                }
            });
        }

        // The call queue: stations marked to be worked, in the order they will
        // be taken. Clicking one drops it; CLEAR empties the lot.
        let call_queue = status.as_ref().map(|s| s.call_queue.clone()).unwrap_or_default();
        if !call_queue.is_empty() {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(
                    RichText::new(format!("QUEUE {}", call_queue.len()))
                        .size(9.5)
                        .strong()
                        .color(crate::theme::CYAN_DIM),
                );
                for (i, q) in call_queue.iter().enumerate() {
                    // The one going next is the one worth reading first.
                    let col = if i == 0 { crate::theme::GREEN } else { Color32::from_gray(150) };
                    if crate::chrome::chip(ui, false, RichText::new(&q.call).size(11.5).color(col))
                        .on_hover_text(format!(
                            "{} · {:+} dB · {:.0} Hz{}\nClick to remove",
                            if i == 0 { "next" } else { "waiting" },
                            q.snr_db,
                            q.audio_hz,
                            q.grid.as_deref().map(|g| format!(" · {g}")).unwrap_or_default(),
                        ))
                        .clicked()
                    {
                        cmds.push(Command::DigiQueueRemove(q.call.clone()));
                    }
                }
                if crate::chrome::chip(ui, false, RichText::new("CLEAR").size(10.0)).clicked() {
                    cmds.push(Command::DigiQueueRemove(String::new()));
                }
            });
        }

        // The rest of the column: the conversation, the message picker and the
        // action buttons.
        //
        // Laid out bottom-up, so the two rows that have to stay reachable are
        // placed first — from the bottom edge, where they belong — and the
        // conversation takes whatever is left above them. The alternative is to
        // predict their height and give the transcript the remainder, and every
        // prediction of it has been wrong on some screen: the rows carry chips
        // whose padding comes from the style, a text field that sizes from
        // `interact_size`, and a button row that wraps when three no longer fit
        // across. Asking the layout instead of guessing is what keeps STOP TX
        // on the screen.
        ui.add_space(5.0);
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            self.qso_controls(ui, cmds, gap);
            // Whatever is left above the controls is the conversation's.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                self.transcript(ui, status.as_ref());
            });
        });
    }

    /// The conversation box: what has been sent and heard this QSO.
    fn transcript(&mut self, ui: &mut egui::Ui, status: Option<&sdroxide_types::DigiStatus>) {
        ui.allocate_ui(egui::vec2(ui.available_width(), ui.available_height()), |ui| {
            let inner = egui::Frame::new()
                .fill(crate::theme::ROW_BG)
                .stroke(egui::Stroke::new(1.0, crate::theme::RED_DEEP))
                .inner_margin(egui::Margin { left: 9, right: 7, top: 6, bottom: 6 })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(ui.available_height());
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show_themed(ui, |ui| {
                            let mut any = false;
                            if let Some(s) = status.as_ref() {
                                for line in &s.transcript {
                                    any = true;
                                    // Pink marks traffic that isn't ours: the
                                    // station we called is working someone else.
                                    let (tag, col) = if line.done {
                                        ("✓", crate::theme::GREEN)
                                    } else if line.overheard {
                                        ("·", crate::theme::PINK)
                                    } else if line.tx {
                                        ("»", crate::theme::YELLOW)
                                    } else {
                                        ("«", crate::theme::GREEN)
                                    };
                                    let txt = RichText::new(format!("{tag} {}", line.text))
                                        .monospace()
                                        .size(12.5)
                                        .color(col);
                                    // Received messages are green too, so the
                                    // completion line needs more than colour to
                                    // stand out at the end of a run of them.
                                    ui.label(if line.done {
                                        txt.strong().background_color(crate::theme::DONE_BG)
                                    } else {
                                        txt
                                    });
                                }
                                if let Some(msg) = &s.tx_pending_msg {
                                    any = true;
                                    ui.label(
                                        RichText::new(format!("→ {msg}"))
                                            .monospace()
                                            .size(11.5)
                                            .color(Color32::from_gray(150)),
                                    );
                                }
                            }
                            if !any {
                                ui.label(
                                    RichText::new("— no messages —")
                                        .monospace()
                                        .size(11.5)
                                        .color(Color32::from_gray(90)),
                                );
                            }
                        });
                });
            // Red left-accent bar (matching chrome::red_panel).
            let r = inner.response.rect;
            ui.painter().rect_filled(
                egui::Rect::from_min_max(r.left_top(), egui::pos2(r.left() + 2.5, r.bottom())),
                0.0,
                crate::theme::PINK,
            );
        });
    }

    /// The two rows under the conversation: the message picker and the action
    /// buttons. Drawn into a bottom-up layout, so they are added in the order
    /// they should sit from the bottom edge — buttons first.
    fn qso_controls(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>, gap: f32) {
        let status = self.digi_status.clone();
        let in_qso = status
            .as_ref()
            .map(|s| {
                !matches!(
                    s.step,
                    sdroxide_types::QsoStep::Idle | sdroxide_types::QsoStep::Confirming
                )
            })
            .unwrap_or(false);
        // Action buttons (larger for touch). Wrapped, so a column too narrow
        // for all three takes a second row rather than clipping the last one —
        // and STOP TX is the one that would have been clipped.
        ui.horizontal_wrapped(|ui| {
            let cq = ui.add_enabled_ui(!in_qso, |ui| {
                crate::chrome::chip_accent(
                    ui,
                    false,
                    RichText::new("  CALL CQ  ").size(15.0).strong(),
                    crate::theme::GREEN,
                    crate::theme::INK_ON_CYAN,
                )
            });
            if cq.inner.clicked() {
                cmds.push(Command::DigiCallCq);
            }
            if crate::chrome::chip(ui, false, RichText::new(" STOP QSO ").size(14.0)).clicked() {
                cmds.push(Command::DigiStopQso);
            }
            if crate::chrome::chip_accent(
                ui,
                false,
                RichText::new(" STOP TX ").size(15.0).strong(),
                crate::theme::PINK,
                Color32::WHITE,
            )
            .clicked()
            {
                cmds.push(Command::DigiAbortTx);
            }
        });
        ui.add_space(gap);
        // Message picker: choose by hand which message goes next (WSJT-X's
        // Tx1–Tx6), or send a line of free text in the next slot.
        let has_dx = status.as_ref().and_then(|s| s.dx_call.as_ref()).is_some();
        let step_now = status.as_ref().map(|s| s.step);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (step, label) in [
                (sdroxide_types::QsoStep::TxGrid, "GRID"),
                (sdroxide_types::QsoStep::TxReport, "RPT"),
                (sdroxide_types::QsoStep::TxRReport, "R+RPT"),
                (sdroxide_types::QsoStep::TxRr73, "RR73"),
                (sdroxide_types::QsoStep::Tx73, "73"),
            ] {
                let resp = ui.add_enabled_ui(has_dx, |ui| {
                    crate::chrome::chip(ui, step_now == Some(step), RichText::new(label).size(11.0))
                });
                if resp.inner.clicked() {
                    cmds.push(Command::DigiSetStep(step));
                }
            }
            ui.add_space(4.0);
            // Free text: 13 characters is all FT8 carries, so cap the entry
            // there rather than letting the operator type a message that would
            // be silently cut on the air.
            let entry = ui.add(
                egui::TextEdit::singleline(&mut self.digi_free_text)
                    .desired_width(ui.available_width() - 52.0)
                    .char_limit(13)
                    .hint_text("free text (13 chars)"),
            );
            let send = crate::chrome::chip_accent(
                ui,
                false,
                RichText::new("SEND").size(11.0).strong(),
                crate::theme::CYAN,
                crate::theme::INK_ON_CYAN,
            );
            let entered = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (send.clicked() || entered) && !self.digi_free_text.trim().is_empty() {
                cmds.push(Command::DigiSendText(self.digi_free_text.clone()));
                self.digi_free_text.clear();
            }
        });
    }
}
