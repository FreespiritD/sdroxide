//! The digimode setup window.
//!
//! One dialog for the operator identity and the per-mode settings that all the
//! panels share, reachable from any of them. The identity half is the same
//! store the Settings dialog's General tab edits.

use eframe::egui::{self, Color32, RichText};
use sdroxide_types::Command;

use crate::app::SdroxideApp;

impl SdroxideApp {
    /// Own-call / grid / message-template editor (and RTTY parameters).
    pub(in crate::app) fn digi_settings_window(
        &mut self,
        ctx: &egui::Context,
        cmds: &mut Vec<Command>,
    ) {
        let mut open = self.show_digi_settings;
        let mode = self.state.rx[0].mode;
        // Per-mode parameters (RTTY/Olivia/THOR/FSQ) now live in each panel's
        // header, so this dialog only carries the shared identity + FT8/FT4
        // message templates.
        let title = if mode.is_text_modem() || mode.is_hell() || mode.is_js8() {
            format!("{} Setup", mode.label())
        } else {
            "FT8 / FT4 Setup".to_string()
        };
        let resp = egui::Window::new(title)
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(false)
            .default_width(crate::layout::window_w(ctx, 420.0))
            .show(ctx, |ui| {
                crate::chrome::window_body_bg(ui);
                // Edit the UI-owned copy so keystrokes aren't clobbered by the
                // engine's status echo; persist on any change.
                let cfg = &mut self.digi_cfg_edit;
                let mut changed = false;
                egui::Grid::new("digi-cfg").num_columns(2).show(ui, |ui| {
                    ui.label("My callsign");
                    if ui.text_edit_singleline(&mut cfg.my_call).changed() {
                        cfg.my_call = cfg.my_call.to_uppercase();
                        changed = true;
                    }
                    ui.end_row();
                    ui.label("My grid");
                    if ui.text_edit_singleline(&mut cfg.my_grid).changed() {
                        changed = true;
                    }
                    ui.end_row();
                    if mode.is_js8() {
                        let turbo = cfg.js8_speed == sdroxide_types::Js8Speed::Turbo;
                        ui.label("Auto-reply");
                        changed |= ui
                            .checkbox(&mut cfg.js8_auto_reply, "Answer SNR? / GRID? / STATUS?")
                            .on_hover_text(
                                "Answer a direct question addressed to you or to @ALLCALL, with \
                                 the answer rather than an acknowledgement. Never answers another \
                                 station's traffic, and never answers itself.",
                            )
                            .changed();
                        ui.end_row();
                        ui.label("Heartbeat");
                        ui.horizontal(|ui| {
                            // The intervals JS8Call offers, plus off. A beacon
                            // is a commitment of air time, so the choice is a
                            // few sensible ones rather than a free number.
                            for (mins, label) in [
                                (0u32, "Off"),
                                (10, "10 min"),
                                (15, "15 min"),
                                (30, "30 min"),
                                (60, "60 min"),
                            ] {
                                if crate::chrome::chip(ui, cfg.js8_heartbeat_min == mins, label)
                                    .clicked()
                                    && cfg.js8_heartbeat_min != mins
                                {
                                    cfg.js8_heartbeat_min = mins;
                                    changed = true;
                                }
                            }
                        });
                        ui.end_row();
                        ui.label("");
                        // Off by default and worth saying why: a beacon that
                        // switches itself on is an on-air behaviour the
                        // operator never chose.
                        ui.label(
                            RichText::new(if turbo {
                                "Turbo does not beacon — it is the local and VHF speed."
                            } else {
                                "Sends your callsign and grid so others know you are receivable. \
                                 The first goes out one interval from now, not immediately."
                            })
                            .size(10.5)
                            .weak(),
                        );
                        ui.end_row();
                        ui.label("Beacon frequency");
                        ui.horizontal(|ui| {
                            let sub_band = !cfg.js8_hb_anywhere;
                            if crate::chrome::chip(ui, sub_band, "500–1000 Hz")
                                .on_hover_text(
                                    "Move each beacon to a free slot in the heartbeat sub-band, \
                                     the way JS8Call does: it is where stations watching for \
                                     beacons look, and it keeps an unattended transmitter off \
                                     somebody else's QSO. The slot is chosen when the beacon \
                                     actually goes out, clear of everything being decoded.",
                                )
                                .clicked()
                                && !sub_band
                            {
                                cfg.js8_hb_anywhere = false;
                                changed = true;
                            }
                            if crate::chrome::chip(ui, !sub_band, "Working freq")
                                .on_hover_text(
                                    "Beacon where you are working instead. Against the band \
                                     convention, but it keeps everything you transmit in one \
                                     place.",
                                )
                                .clicked()
                                && sub_band
                            {
                                cfg.js8_hb_anywhere = true;
                                changed = true;
                            }
                        });
                        ui.end_row();
                        ui.label("Heartbeat reply");
                        ui.add_enabled_ui(cfg.js8_auto_reply && !turbo, |ui| {
                            changed |= ui
                                .checkbox(
                                    &mut cfg.js8_hb_ack,
                                    "Answer heartbeats with a signal report",
                                )
                                .on_hover_text(
                                    "Tell a station that beaconed how well you copied them. Off \
                                     by default: a busy band carries a heartbeat every slot, and \
                                     answering all of them would flood exactly the band \
                                     heartbeats exist to keep quiet. Rate-limited to one answer \
                                     per station every 15 minutes, and never while a message is \
                                     still arriving or while you have something queued to send.",
                                )
                                .changed();
                        });
                        ui.end_row();
                        ui.label("Status message");
                        changed |= ui.text_edit_singleline(&mut cfg.js8_status).changed();
                        ui.end_row();
                    }
                    ui.label("TX period");
                    ui.horizontal(|ui| {
                        changed |= ui.selectable_value(&mut cfg.tx_even, true, "Even").changed();
                        changed |= ui.selectable_value(&mut cfg.tx_even, false, "Odd").changed();
                    });
                    ui.end_row();
                    ui.label("Auto-sequence");
                    changed |= ui.checkbox(&mut cfg.auto_seq, "").changed();
                    ui.end_row();
                    ui.label("Auto TX frequency");
                    changed |= ui
                        .checkbox(&mut cfg.auto_tx_freq, "")
                        .on_hover_text(
                            "Choose the transmit frequency automatically: the quietest spot in \
                             the period you transmit in, rather than the frequency of the \
                             station you are answering. Off holds whatever you set by hand.",
                        )
                        .changed();
                    ui.end_row();
                    ui.label("TX watchdog");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut cfg.tx_watchdog_min)
                                .range(0..=60)
                                .suffix(" min"),
                        )
                        .on_hover_text(
                            "Stop transmitting after this long with no reply and no action \
                             from you. 0 disables it.",
                        )
                        .changed();
                    ui.end_row();
                    ui.label("Give up after");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut cfg.max_tx_repeats)
                                .range(0..=30)
                                .suffix(" calls"),
                        )
                        .on_hover_text(
                            "Unanswered calls to one station before moving on. Calling CQ is \
                             exempt. 0 disables it.",
                        )
                        .changed();
                    ui.end_row();
                    ui.label("DXpedition");
                    ui.horizontal(|ui| {
                        for m in sdroxide_types::DxpedMode::ALL {
                            changed |= ui
                                .selectable_value(&mut cfg.dxped_mode, m, m.label())
                                .on_hover_text(match m {
                                    sdroxide_types::DxpedMode::Normal => "Ordinary FT8 operation.",
                                    sdroxide_types::DxpedMode::Hound => {
                                        "Calling a DXpedition running Fox mode: call from above \
                                         1000 Hz, move down onto the Fox when it answers, and \
                                         log on its RR73 without sending 73."
                                    }
                                    sdroxide_types::DxpedMode::Fox => {
                                        "Run the pile-up: several signals at once, a queue of \
                                         callers, worked strongest and rarest first. CALL CQ \
                                         starts it, STOP QSO stands it down."
                                    }
                                })
                                .changed();
                        }
                    });
                    ui.end_row();
                    if cfg.dxped_mode == sdroxide_types::DxpedMode::Fox {
                        ui.label("Fox signals");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut cfg.fox_slots)
                                    .range(1..=sdroxide_types::FOX_MAX_SLOTS)
                                    .suffix(" at once"),
                            )
                            .on_hover_text(
                                "Simultaneous transmissions, spaced 60 Hz apart. They share the \
                                 transmitter's power, so more signals means each is weaker.",
                            )
                            .changed();
                        ui.end_row();
                    }
                });
                ui.separator();
                ui.label(
                    RichText::new("Message templates  {MYCALL} {MYGRID} {DX} {REPORT}")
                        .size(10.5)
                        .color(Color32::from_gray(150)),
                );
                egui::Grid::new("digi-msgs").num_columns(2).show(ui, |ui| {
                    for (label, field) in [
                        ("CQ", &mut cfg.msg_cq),
                        ("Grid", &mut cfg.msg_grid),
                        ("Report", &mut cfg.msg_report),
                        ("R+Report", &mut cfg.msg_rreport),
                        ("RR73", &mut cfg.msg_rr73),
                        ("73", &mut cfg.msg_73),
                    ] {
                        ui.label(label);
                        changed |= ui.text_edit_singleline(field).changed();
                        ui.end_row();
                    }
                });
                if changed {
                    cmds.push(Command::SetDigiConfig(cfg.clone()));
                }
            });
        if let Some(r) = &resp {
            crate::chrome::paint_window_border(ctx, &r.response);
        }
        self.show_digi_settings = open;
    }
}
