//! The keyboard-mode panel: PSK, RTTY, Olivia, THOR and Contestia, plus
//! Hellschreiber's raster.
//!
//! All of these are a received-text pane over a transmit buffer, so they share
//! one panel; only the parameter row above it differs by mode. Hell is the
//! exception that gets its own panel — it paints a scrolling image rather than
//! producing characters.

use eframe::egui::{self, Color32, RichText};
use sdroxide_types::{Command, Mode};

use crate::theme::ThemedScroll;

use crate::app::SdroxideApp;

impl SdroxideApp {
    /// PSK/RTTY keyboard-mode panel: the decoded RX stream on top, then a
    /// streaming TX input (already-sent characters shown green) and controls.
    /// `panel_h` is the real bounded height (the surrounding frame reports an
    /// unbounded `available_height`, so we can't use it for the split).
    pub(in crate::app) fn text_modem_panel(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        panel_h: f32,
    ) {
        // Panel content bottom, from the entry cursor (frame margins accounted);
        // the larger margin leaves clear padding below the button row.
        let content_bottom = ui.cursor().top() + panel_h - 40.0;
        let status = self.digi_status.clone();
        let mode = self.state.rx[0].mode;
        let audio_hz = status.as_ref().map(|s| s.audio_hz).unwrap_or(1500.0);
        let sent = status.as_ref().map(|s| s.tx_sent).unwrap_or(0);
        let tx_on = status.as_ref().map(|s| s.tx_next).unwrap_or(false);
        let transmitting = status.as_ref().map(|s| s.transmitting).unwrap_or(false);
        let rx_text = status.as_ref().map(|s| s.text_rx.clone()).unwrap_or_default();
        let my_call = status.as_ref().map(|s| s.config.my_call.clone()).unwrap_or_default();

        // Header: mode + tuning readout / nudges, SETUP + TX indicator.
        ui.horizontal(|ui| {
            ui.label(RichText::new(mode.label()).size(11.0).strong().color(crate::theme::CYAN));
            ui.label(
                RichText::new(format!("{audio_hz:.0} Hz"))
                    .size(11.0)
                    .color(Color32::from_gray(150)),
            );
            if crate::chrome::chip(ui, false, "−").on_hover_text("Tune down 10 Hz").clicked() {
                cmds.push(Command::SetDigiAudioFreq((audio_hz - 10.0).clamp(200.0, 3500.0)));
            }
            if crate::chrome::chip(ui, false, "+").on_hover_text("Tune up 10 Hz").clicked() {
                cmds.push(Command::SetDigiAudioFreq((audio_hz + 10.0).clamp(200.0, 3500.0)));
            }
            self.digi_freq_chip(ui, cmds);
            // Mode parameters inline next to the tune buttons (RTTY shift/baud,
            // Olivia tones/bandwidth, THOR submode) — no separate setup dialog.
            self.text_modem_params_row(ui, cmds);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if transmitting {
                    ui.label(RichText::new("● TX").size(11.0).strong().color(crate::theme::PINK));
                }
                self.digi_squelch_slider(ui, cmds);
            });
        });
        ui.add_space(4.0);

        // Reserve the input + button rows at the bottom; RX stream gets the rest.
        // Sized against the real panel bottom (not the unbounded available_height)
        // so the fixed controls are never pushed off a short panel.
        let btn_h = 32.0;
        let input_h = 56.0; // fixed-height, internally-scrolling TX box
        let gap = 5.0;
        let bottom_pad = 12.0; // clear space below the button row
        let rx_h = (content_bottom - ui.cursor().top() - btn_h - input_h - 2.0 * gap - bottom_pad)
            .max(24.0);

        ui.allocate_ui(egui::vec2(ui.available_width(), rx_h), |ui| {
            egui::Frame::new()
                .fill(crate::theme::ROW_BG)
                .stroke(egui::Stroke::new(1.0, crate::theme::RED_DEEP))
                .inner_margin(egui::Margin { left: 8, right: 7, top: 6, bottom: 6 })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(ui.available_height());
                    // Cap the scroll height explicitly (bounded `available_height`
                    // isn't reliable inside the auto-sizing frame) so long RX text
                    // scrolls instead of growing the panel.
                    egui::ScrollArea::vertical()
                        .max_height((rx_h - 12.0).max(20.0))
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show_themed(ui, |ui| {
                            if rx_text.is_empty() {
                                ui.label(
                                    RichText::new("— listening —")
                                        .monospace()
                                        .size(12.0)
                                        .color(Color32::from_gray(90)),
                                );
                            } else {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&rx_text)
                                            .monospace()
                                            .size(12.5)
                                            .color(crate::theme::GREEN),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                });
        });
        ui.add_space(gap);

        // TX input: already-sent characters are coloured green via a layouter.
        let prev = self.text_tx.clone();
        let sent = sent.min(prev.chars().count());
        let prefix: String = prev.chars().take(sent).collect();
        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            let text = buf.as_str();
            let sent_byte = text.char_indices().nth(sent).map(|(i, _)| i).unwrap_or(text.len());
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = wrap;
            let mono = egui::FontId::monospace(13.0);
            if sent_byte > 0 {
                job.append(
                    &text[..sent_byte],
                    0.0,
                    egui::TextFormat {
                        font_id: mono.clone(),
                        color: crate::theme::GREEN,
                        ..Default::default()
                    },
                );
            }
            if sent_byte < text.len() {
                job.append(
                    &text[sent_byte..],
                    0.0,
                    egui::TextFormat {
                        font_id: mono.clone(),
                        color: crate::theme::TEXT_STRONG,
                        ..Default::default()
                    },
                );
            }
            ui.fonts_mut(|f| f.layout_job(job))
        };
        // Fixed-height box: the multiline TextEdit grows with content, so wrap it
        // in a bounded ScrollArea (stick-to-bottom) instead of letting it push the
        // buttons off the panel.
        let resp = ui
            .allocate_ui(egui::vec2(ui.available_width(), input_h), |ui| {
                egui::Frame::new()
                    .fill(crate::theme::ROW_BG)
                    .stroke(egui::Stroke::new(1.0, crate::theme::RED_DEEP))
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_min_height(ui.available_height());
                        egui::ScrollArea::vertical()
                            .id_salt("text-tx")
                            // Cap the height so the multiline scrolls internally
                            // instead of growing and pushing the buttons off-panel.
                            .max_height((input_h - 8.0).max(20.0))
                            .auto_shrink([false, false])
                            .stick_to_bottom(true)
                            .show_themed(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.text_tx)
                                        .layouter(&mut layouter)
                                        .frame(egui::Frame::NONE)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("Type here to transmit…"),
                                )
                            })
                            .inner
                    })
                    .inner
            })
            .inner;
        if resp.changed() {
            // Protect the already-transmitted prefix from edits.
            if !self.text_tx.starts_with(&prefix) {
                self.text_tx = prev;
            }
            cmds.push(Command::DigiTxText(self.text_tx.clone()));
        }
        ui.add_space(gap);

        // Controls.
        ui.horizontal(|ui| {
            let label = if tx_on { "  TX ON  " } else { "   TX   " };
            if crate::chrome::chip_accent(
                ui,
                tx_on,
                RichText::new(label).size(14.0).strong(),
                crate::theme::PINK,
                Color32::WHITE,
            )
            .clicked()
            {
                cmds.push(Command::DigiTxActive(!tx_on));
            }
            if crate::chrome::chip_accent(
                ui,
                false,
                RichText::new(" CALL CQ ").size(13.0).strong(),
                crate::theme::GREEN,
                crate::theme::INK_ON_CYAN,
            )
            .clicked()
            {
                // Own the CQ text so the green sent-progress shows locally.
                let call = if my_call.is_empty() { "NOCALL".to_string() } else { my_call.clone() };
                let cq = format!("CQ CQ CQ DE {call} {call} {call} PSE K\n");
                cmds.push(Command::DigiAbortTx);
                self.text_tx = cq.clone();
                cmds.push(Command::DigiTxText(cq));
                cmds.push(Command::DigiTxActive(true));
            }
            if crate::chrome::chip(ui, false, " CLEAR ").clicked() {
                self.text_tx.clear();
                cmds.push(Command::DigiAbortTx);
                cmds.push(Command::DigiTxText(String::new()));
            }
        });
        // Visible padding below the buttons so they aren't flush with the edge.
        ui.add_space(bottom_pad);
    }

    /// Hellschreiber panel: the scrolling receive raster on top, then a
    /// streaming TX input (already-sent characters green) and controls.
    ///
    /// Laid out like [`Self::text_modem_panel`] — Hell types the same way — but
    /// where that shows decoded text this shows the raster, because Hell carries
    /// pictures of letters rather than letters.
    pub(in crate::app) fn hell_panel(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        panel_h: f32,
    ) {
        let content_bottom = ui.cursor().top() + panel_h - 40.0;
        let status = self.digi_status.clone();
        let audio_hz = status.as_ref().map(|s| s.audio_hz).unwrap_or(1500.0);
        let sent = status.as_ref().map(|s| s.tx_sent).unwrap_or(0);
        let tx_on = status.as_ref().map(|s| s.tx_next).unwrap_or(false);
        let transmitting = status.as_ref().map(|s| s.transmitting).unwrap_or(false);
        let my_call = status.as_ref().map(|s| s.config.my_call.clone()).unwrap_or_default();
        let variant = status.as_ref().map(|s| s.config.hell_variant).unwrap_or_default();

        // Header: mode + tuning readout / nudges, variant chips, TX indicator.
        ui.horizontal(|ui| {
            ui.label(RichText::new("HELL").size(11.0).strong().color(crate::theme::CYAN));
            ui.label(
                RichText::new(format!("{audio_hz:.0} Hz"))
                    .size(11.0)
                    .color(Color32::from_gray(150)),
            );
            if crate::chrome::chip(ui, false, "−").on_hover_text("Tune down 10 Hz").clicked() {
                cmds.push(Command::SetDigiAudioFreq(audio_hz - 10.0));
            }
            if crate::chrome::chip(ui, false, "+").on_hover_text("Tune up 10 Hz").clicked() {
                cmds.push(Command::SetDigiAudioFreq(audio_hz + 10.0));
            }
            self.digi_freq_chip(ui, cmds);
            self.hell_params_row(ui, cmds);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if transmitting {
                    ui.label(RichText::new("● TX").size(11.0).strong().color(crate::theme::PINK));
                }
                self.digi_squelch_slider(ui, cmds);
            });
        });
        ui.add_space(4.0);

        // Raster appearance + scale. All client-side, so none of it round-trips
        // through the engine.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let v = &mut self.view.hell;
            ui.label(RichText::new("Contrast").size(10.5).color(crate::theme::CYAN_DIM));
            ui.spacing_mut().slider_width = 70.0;
            ui.add(egui::Slider::new(&mut v.contrast, 0.4..=3.0).show_value(false))
                .on_hover_text("Harder or softer dots — redraws the whole strip");
            ui.add_space(6.0);
            ui.label(RichText::new("Width").size(10.5).color(crate::theme::CYAN_DIM));
            for px in [1.0f32, 2.0, 3.0, 4.0] {
                let sel = (v.col_px - px).abs() < 0.01;
                if ui.selectable_label(sel, format!("{px:.0}×")).clicked() {
                    v.col_px = px;
                }
            }
            ui.add_space(6.0);
            if crate::chrome::chip(ui, v.doubled, " 2ROW ")
                .on_hover_text(
                    "Draw every column twice, stacked. Hell has no vertical sync, so this \
                     keeps one complete copy of the text readable whatever the phase.",
                )
                .clicked()
            {
                v.doubled = !v.doubled;
            }
            if crate::chrome::chip(ui, v.reverse, " REV ")
                .on_hover_text("Reverse video — light dots on dark paper")
                .clicked()
            {
                v.reverse = !v.reverse;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::chrome::chip(ui, false, " CLEAR RX ").clicked() {
                    self.hell.clear();
                }
                ui.label(
                    RichText::new(format!("{:.1} char/s", variant.chars_per_sec()))
                        .size(10.5)
                        .color(Color32::from_gray(120)),
                );
            });
        });
        ui.add_space(4.0);

        // Reserve the input + button rows at the bottom; the raster gets the rest.
        let btn_h = 32.0;
        let input_h = 56.0;
        let gap = 5.0;
        let bottom_pad = 12.0;
        let rx_h = (content_bottom - ui.cursor().top() - btn_h - input_h - 2.0 * gap - bottom_pad)
            .max(28.0);

        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), rx_h),
            egui::Sense::click_and_drag(),
        );
        // Dragging lines the text up by hand when the doubled view is off.
        if resp.dragged() && !self.view.hell.doubled {
            let d = resp.drag_delta().y / rect.height().max(1.0);
            self.view.hell.valign = (self.view.hell.valign - d).rem_euclid(1.0);
        }
        if resp.hovered() && !self.view.hell.doubled {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        self.hell.draw(ui, rect, &self.view.hell);
        ui.add_space(gap);

        // TX input: already-sent characters coloured green, exactly as the
        // keyboard modes do it.
        let prev = self.text_tx.clone();
        let sent = sent.min(prev.chars().count());
        let prefix: String = prev.chars().take(sent).collect();
        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            let text = buf.as_str();
            let sent_byte = text.char_indices().nth(sent).map(|(i, _)| i).unwrap_or(text.len());
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = wrap;
            let mono = egui::FontId::monospace(13.0);
            if sent_byte > 0 {
                job.append(
                    &text[..sent_byte],
                    0.0,
                    egui::TextFormat {
                        font_id: mono.clone(),
                        color: crate::theme::GREEN,
                        ..Default::default()
                    },
                );
            }
            if sent_byte < text.len() {
                job.append(
                    &text[sent_byte..],
                    0.0,
                    egui::TextFormat {
                        font_id: mono.clone(),
                        color: crate::theme::TEXT_STRONG,
                        ..Default::default()
                    },
                );
            }
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let resp = ui
            .allocate_ui(egui::vec2(ui.available_width(), input_h), |ui| {
                egui::Frame::new()
                    .fill(crate::theme::ROW_BG)
                    .stroke(egui::Stroke::new(1.0, crate::theme::RED_DEEP))
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_min_height(ui.available_height());
                        egui::ScrollArea::vertical()
                            .id_salt("hell-tx")
                            .max_height((input_h - 8.0).max(20.0))
                            .auto_shrink([false, false])
                            .stick_to_bottom(true)
                            .show_themed(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.text_tx)
                                        .layouter(&mut layouter)
                                        .frame(egui::Frame::NONE)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("Type here to transmit…"),
                                )
                            })
                            .inner
                    })
                    .inner
            })
            .inner;
        if resp.changed() {
            if !self.text_tx.starts_with(&prefix) {
                self.text_tx = prev;
            }
            cmds.push(Command::DigiTxText(self.text_tx.clone()));
        }
        ui.add_space(gap);

        ui.horizontal(|ui| {
            let label = if tx_on { "  TX ON  " } else { "   TX   " };
            if crate::chrome::chip_accent(
                ui,
                tx_on,
                RichText::new(label).size(14.0).strong(),
                crate::theme::PINK,
                Color32::WHITE,
            )
            .on_hover_text("Hold the channel: idle sends blank paper, so the strip keeps scrolling")
            .clicked()
            {
                cmds.push(Command::DigiTxActive(!tx_on));
            }
            if crate::chrome::chip_accent(
                ui,
                false,
                RichText::new(" CALL CQ ").size(13.0).strong(),
                crate::theme::GREEN,
                crate::theme::INK_ON_CYAN,
            )
            .clicked()
            {
                let call = if my_call.is_empty() { "NOCALL".to_string() } else { my_call.clone() };
                let cq = format!("CQ CQ CQ DE {call} {call} {call} PSE K ");
                cmds.push(Command::DigiAbortTx);
                self.text_tx = cq.clone();
                cmds.push(Command::DigiTxText(cq));
                cmds.push(Command::DigiTxActive(true));
            }
            if crate::chrome::chip(ui, false, " CLEAR ").clicked() {
                self.text_tx.clear();
                cmds.push(Command::DigiAbortTx);
                cmds.push(Command::DigiTxText(String::new()));
            }
        });
        ui.add_space(bottom_pad);
    }

    /// Mode-specific parameter buttons for the continuous keyboard modes, shown
    /// inline in the panel header next to the tune buttons (moved here from the
    /// setup dialog). Edits the UI-owned config copy and pushes it on change.
    /// PSK has no parameters, so nothing is drawn.
    fn text_modem_params_row(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let mode = self.state.rx[0].mode;
        if !matches!(mode, Mode::Rtty | Mode::Olivia | Mode::Thor) {
            return; // PSK (and anything else) has no per-mode settings
        }
        fn cap(ui: &mut egui::Ui, text: &str) {
            ui.label(RichText::new(text).size(10.5).strong().color(crate::theme::CYAN_DIM));
        }
        let cfg = &mut self.digi_cfg_edit;
        let mut changed = false;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            match mode {
                Mode::Rtty => {
                    cap(ui, "Shift");
                    for s in [170.0f32, 425.0, 850.0] {
                        let sel = (cfg.rtty_shift_hz - s).abs() < 0.5;
                        if ui.selectable_label(sel, format!("{s:.0}")).clicked() {
                            cfg.rtty_shift_hz = s;
                            changed = true;
                        }
                    }
                    ui.add_space(8.0);
                    cap(ui, "Baud");
                    for b in [45.45f32, 50.0, 75.0] {
                        let sel = (cfg.rtty_baud - b).abs() < 0.5;
                        let lbl = if (b - 45.45).abs() < 0.5 {
                            "45".to_string()
                        } else {
                            format!("{b:.0}")
                        };
                        if ui.selectable_label(sel, lbl).clicked() {
                            cfg.rtty_baud = b;
                            changed = true;
                        }
                    }
                }
                Mode::Olivia => {
                    cap(ui, "Tones");
                    for t in [2u8, 4, 8, 16, 32, 64] {
                        if ui.selectable_label(cfg.olivia_tones == t, t.to_string()).clicked() {
                            cfg.olivia_tones = t;
                            changed = true;
                        }
                    }
                    ui.add_space(8.0);
                    cap(ui, "BW");
                    for bw in [125.0f32, 250.0, 500.0, 1000.0, 2000.0] {
                        let sel = (cfg.olivia_bw_hz - bw).abs() < 0.5;
                        if ui.selectable_label(sel, format!("{bw:.0}")).clicked() {
                            cfg.olivia_bw_hz = bw;
                            changed = true;
                        }
                    }
                }
                Mode::Thor => {
                    cap(ui, "Mode");
                    for m in sdroxide_types::ThorMode::ALL {
                        if ui.selectable_label(cfg.thor_mode == m, m.label()).clicked() {
                            cfg.thor_mode = m;
                            changed = true;
                        }
                    }
                }
                _ => {}
            }
        });
        if changed {
            cmds.push(Command::SetDigiConfig(cfg.clone()));
        }
    }

    /// Hellschreiber variant chips, shown inline in the Hell panel header next
    /// to the tune buttons.
    ///
    /// The variant lives in `DigiConfig` (the engine has to know the dot rate);
    /// contrast and reverse video live in `ViewState`, so moving them repaints
    /// the whole scrollback rather than just what arrives next.
    fn hell_params_row(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(RichText::new("Mode").size(10.5).strong().color(crate::theme::CYAN_DIM));
            let cfg = &mut self.digi_cfg_edit;
            let mut changed = false;
            for v in sdroxide_types::HellVariant::ALL {
                let sel = cfg.hell_variant == v;
                let hint = format!(
                    "{} — {:.1} char/s, {:.0} Hz wide, {}",
                    v.label(),
                    v.chars_per_sec(),
                    v.bandwidth_hz(),
                    if v.is_fsk() { "frequency-shifted" } else { "on/off keyed" }
                );
                if ui.selectable_label(sel, v.label()).on_hover_text(hint).clicked() && !sel {
                    cfg.hell_variant = v;
                    changed = true;
                }
            }
            if changed {
                cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
            }
        });
    }
}
