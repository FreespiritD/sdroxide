//! The two operator overlays opened from the top bar's WIN group: the memory
//! channel list and the voice keyer.
//!
//! Both are thin views over engine state — the memories come from the radio
//! backend, the keyer slots from the audio engine — so all either does is
//! draw the list and push the [`Command`] a click means.

use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use sdroxide_types::Command;

use crate::app::SdroxideApp;

impl SdroxideApp {
    pub(in crate::app) fn memories_window(&mut self, ctx: &egui::Context, cmds: &mut Vec<Command>) {
        let mut open = self.show_memories;
        let resp = egui::Window::new("Memories")
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(true)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.mem_name);
                    let name_ok = !self.mem_name.trim().is_empty();
                    if ui.add_enabled(name_ok, egui::Button::new("Store")).clicked() {
                        cmds.push(Command::StoreMemory { name: self.mem_name.trim().to_string() });
                        self.mem_name.clear();
                    }
                });
                ui.separator();
                if self.memories.is_empty() {
                    ui.label(RichText::new("no memories yet").color(Color32::from_gray(120)));
                }
                for m in &self.memories {
                    ui.horizontal(|ui| {
                        if crate::chrome::chip(ui, false, "RCL").on_hover_text("Recall").clicked() {
                            cmds.push(Command::RecallMemory(m.id));
                        }
                        ui.label(
                            RichText::new(format!(
                                "{:<12} {:>12.6} MHz  {}",
                                m.name,
                                m.freq_hz / 1e6,
                                m.mode.label()
                            ))
                            .monospace(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::chrome::chip_accent(
                                ui,
                                false,
                                RichText::new("DEL").size(11.0),
                                crate::theme::PINK,
                                Color32::WHITE,
                            )
                            .on_hover_text("Delete")
                            .clicked()
                            {
                                cmds.push(Command::DeleteMemory(m.id));
                            }
                        });
                    });
                }
            });
        if let Some(r) = &resp {
            crate::chrome::paint_window_border(ctx, &r.response);
        }
        self.show_memories = open;
    }

    /// The voice keyer: ten recorded messages with record / transmit / erase
    /// per slot.
    ///
    /// Everything the window shows comes from the engine (it owns the
    /// recordings and the transmitter), so the buttons only ever send commands
    /// — there is no local latch that could disagree with what is on the air.
    pub(in crate::app) fn voice_window(&mut self, ctx: &egui::Context, cmds: &mut Vec<Command>) {
        // Entering a digital mode other than RADE takes the feature away; the
        // window goes with it rather than sitting there doing nothing.
        if !self.state.rx[0].mode.allows_voice_keyer() {
            self.show_voice = false;
            return;
        }
        let mut open = self.show_voice;
        let recording = self.voice.recording;
        let playing = self.voice.playing;
        let previewing = self.voice.previewing;
        let pos = self.voice.position_s;
        let max_len = self.voice.max_len_s;
        // TUNE holds the transmitter at the tune level, so a message would go
        // nowhere; the engine refuses, and the buttons say so up front.
        let tuning = self.state.tx.tune;
        let slots: Vec<sdroxide_types::VoiceSlotInfo> = self.voice.slots.clone();

        let resp = egui::Window::new("Voice keyer")
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(false)
            // `min_width` as well as `default_width`: the default only applies
            // the first time the window is ever shown, and egui persists its
            // size — without the minimum, a build that shipped a narrower
            // window would keep squeezing the slot-name fields forever.
            .default_width(600.0)
            .min_width(600.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "REC records from your microphone, PLAY lets you listen to what you \
                         recorded, TX puts it on the air — as does a numpad key, a MIDI pad, \
                         or rigctld's send_voice_mem.",
                    )
                    .weak()
                    .size(11.5),
                );
                ui.add_space(6.0);
                egui::Grid::new("voice-grid")
                    .num_columns(6)
                    .spacing([8.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for (i, slot) in slots.iter().enumerate() {
                            let is_rec = recording == Some(i as u8);
                            let is_play = playing == Some(i as u8);
                            let is_prev = previewing == Some(i as u8);

                            ui.label(
                                RichText::new(format!("{:>2}", i + 1))
                                    .monospace()
                                    .color(crate::theme::CYAN_DIM),
                            );

                            // The slot label. Only the row being typed into is
                            // UI-owned; every other row shows the engine's copy.
                            let mut text = match &self.voice_name_edit {
                                Some((row, s)) if *row == i => s.clone(),
                                _ => slot.name.clone(),
                            };
                            // `add_sized`, not `desired_width`: inside a Grid a
                            // desired width is clamped by the column width egui
                            // measured (and persisted) last frame, so a field
                            // that once came up narrow would stay narrow.
                            let edit = ui.add_sized(
                                [190.0, 20.0],
                                egui::TextEdit::singleline(&mut text)
                                    .hint_text(format!("Slot {}", i + 1)),
                            );
                            if edit.changed() {
                                self.voice_name_edit = Some((i, text.clone()));
                            }
                            if edit.lost_focus()
                                && let Some((row, name)) = self.voice_name_edit.take()
                                && row == i
                            {
                                cmds.push(Command::VoiceRename { slot: i as u8, name });
                            }

                            // REC — starts/stops recording this slot. Refused
                            // while the transmitter is up (same microphone).
                            let busy_elsewhere = (recording.is_some() && !is_rec)
                                || playing.is_some()
                                || previewing.is_some()
                                || self.state.tx.ptt
                                || tuning;
                            let rec = ui
                                .add_enabled_ui(!busy_elsewhere, |ui| {
                                    crate::chrome::chip_accent(
                                        ui,
                                        is_rec,
                                        RichText::new("REC").size(11.5),
                                        crate::theme::PINK,
                                        Color32::WHITE,
                                    )
                                })
                                .inner
                                .on_hover_text(if is_rec {
                                    "Stop and store".to_string()
                                } else {
                                    format!("Record from the microphone (up to {max_len:.0} s)")
                                });
                            if rec.clicked() {
                                cmds.push(Command::VoiceRecord(if is_rec {
                                    None
                                } else {
                                    Some(i as u8)
                                }));
                            }

                            // PLAY — listen to the message locally. Nothing goes
                            // on the air, so this is safe to press any time the
                            // receiver is running.
                            let can_prev = !slot.is_empty()
                                && recording.is_none()
                                && !self.state.tx.ptt
                                && !tuning
                                && (is_prev || previewing.is_none());
                            let prev = ui
                                .add_enabled_ui(can_prev || is_prev, |ui| {
                                    crate::chrome::chip(
                                        ui,
                                        is_prev,
                                        RichText::new(if is_prev { "STOP" } else { "PLAY" })
                                            .size(11.5),
                                    )
                                })
                                .inner
                                .on_hover_text(if is_prev {
                                    "Stop listening"
                                } else if slot.is_empty() {
                                    "Nothing recorded in this slot"
                                } else if self.state.tx.ptt || tuning {
                                    "Not while transmitting"
                                } else {
                                    "Listen to this message — nothing is transmitted"
                                });
                            if prev.clicked() {
                                cmds.push(if is_prev {
                                    Command::VoicePreview(None)
                                } else {
                                    Command::VoicePreview(Some(i as u8))
                                });
                            }

                            // TX — puts the message on the air.
                            let can_play = !slot.is_empty()
                                && recording.is_none()
                                && !tuning
                                && (is_play || playing.is_none());
                            let play = ui
                                .add_enabled_ui(can_play || is_play, |ui| {
                                    crate::chrome::chip_accent(
                                        ui,
                                        is_play,
                                        RichText::new(if is_play { "STOP" } else { "TX" })
                                            .size(11.5),
                                        crate::theme::PINK,
                                        Color32::WHITE,
                                    )
                                })
                                .inner
                                .on_hover_text(if is_play {
                                    "Stop transmitting"
                                } else if slot.is_empty() {
                                    "Nothing recorded in this slot"
                                } else if tuning {
                                    "TUNE is active — switch it off first"
                                } else {
                                    "Transmit this message"
                                });
                            if play.clicked() {
                                cmds.push(if is_play {
                                    Command::VoicePlay(None)
                                } else {
                                    Command::VoicePlay(Some(i as u8))
                                });
                            }

                            // Length, or the running position of whichever of
                            // record / listen / transmit this row owns.
                            ui.horizontal(|ui| {
                                let (text, colour) = if is_rec {
                                    (format!("● {pos:.1} s"), crate::theme::PINK)
                                } else if is_play {
                                    (
                                        format!("▶ {pos:.1} / {:.1} s", slot.len_s),
                                        crate::theme::PINK,
                                    )
                                } else if is_prev {
                                    (
                                        format!("▶ {pos:.1} / {:.1} s", slot.len_s),
                                        crate::theme::CYAN,
                                    )
                                } else if slot.is_empty() {
                                    ("—".to_string(), Color32::from_gray(110))
                                } else {
                                    (format!("{:.1} s", slot.len_s), Color32::from_gray(170))
                                };
                                ui.add_sized(
                                    [88.0, 18.0],
                                    egui::Label::new(
                                        RichText::new(text).monospace().size(11.5).color(colour),
                                    )
                                    .selectable(false),
                                );
                                let erasable = !slot.is_empty() && !is_rec && !is_play && !is_prev;
                                if ui
                                    .add_enabled_ui(erasable, |ui| {
                                        crate::chrome::chip_accent(
                                            ui,
                                            false,
                                            RichText::new("DEL").size(11.0),
                                            crate::theme::PINK,
                                            Color32::WHITE,
                                        )
                                    })
                                    .inner
                                    .on_hover_text("Erase this recording")
                                    .clicked()
                                {
                                    cmds.push(Command::VoiceClear(i as u8));
                                }
                            });
                            ui.end_row();
                        }
                    });

                if self.state.rx[0].mode.is_rade() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "RADE: the message is encoded by the digital-voice codec, \
                             exactly as a live over would be.",
                        )
                        .weak()
                        .size(11.0),
                    );
                }
            });
        if let Some(r) = &resp {
            crate::chrome::paint_window_border(ctx, &r.response);
        }
        // Keep the position readout moving while something is running; the app
        // otherwise idles between spectrum frames.
        if self.voice.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.show_voice = open;
    }
}
