//! The ISM window: what the 868 MHz band around you is saying.
//!
//! One row per device rather than per transmission — see
//! [`sdroxide_types::IsmReport`] for why — with the channels the decoder is
//! listening on above them, because an empty list has two very different causes
//! and only one of them is "there is nothing here".

use eframe::egui::{self, RichText};
use sdroxide_types::{Command, IsmFamily, IsmReport, IsmSettings, Mode, Vfo};

use crate::app::SdroxideApp;
use crate::app::panels::widgets::row_cell;
use crate::app::util::fmt_age;
use crate::theme::ThemedScroll;

/// How the device list is ordered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::app) enum IsmSort {
    /// Most recently heard first. What you want while watching a band.
    #[default]
    Heard,
    /// Most frequently heard first. What you want when identifying what is
    /// permanently nearby as against what walked past once.
    Count,
    Signal,
    Frequency,
}

impl IsmSort {
    const ALL: [IsmSort; 4] = [IsmSort::Heard, IsmSort::Count, IsmSort::Signal, IsmSort::Frequency];

    fn label(self) -> &'static str {
        match self {
            IsmSort::Heard => "HEARD",
            IsmSort::Count => "SEEN",
            IsmSort::Signal => "SIG",
            IsmSort::Frequency => "FREQ",
        }
    }
}

const ROW_H: f32 = 19.0;

impl SdroxideApp {
    pub(in crate::app) fn ism_window(&mut self, ctx: &egui::Context, cmds: &mut Vec<Command>) {
        if !self.show_ism {
            return;
        }
        let mut open = self.show_ism;
        // Edited in place and sent whole on any change, as the skimmer's and the
        // scanner's are; the engine persists it and echoes it back, so there is
        // no apply step to get wrong.
        let mut cfg = self.state.ism;
        let resp = egui::Window::new("ISM 868 MHz")
            .id(crate::layout::salted_id(ctx, "Ism"))
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(true)
            .default_width(crate::layout::window_w(ctx, 620.0))
            .show(ctx, |ui| {
                crate::chrome::window_body_bg(ui);
                self.ism_body(ui, &mut cfg, cmds)
            });
        if let Some(r) = &resp {
            crate::chrome::paint_window_border(ctx, &r.response);
        }
        if cfg != self.state.ism {
            cmds.push(Command::SetIsmConfig(cfg));
        }
        self.show_ism = open;
    }

    fn ism_body(&mut self, ui: &mut egui::Ui, cfg: &mut IsmSettings, cmds: &mut Vec<Command>) {
        // Matching the skimmer popup: a radio whose capabilities have not
        // arrived yet is assumed wideband, so the controls are live from the
        // first frame rather than greyed until the first state message.
        let wideband = self.caps.as_ref().is_none_or(|c| !c.audio_mode);

        ui.horizontal(|ui| {
            let run = crate::chrome::chip_enabled(
                ui,
                wideband,
                cfg.enabled,
                if cfg.enabled { "DECODING" } else { "OFF" },
            );
            if run.clicked() {
                cfg.enabled = !cfg.enabled;
            }
            if !wideband {
                run.on_hover_text("Needs a wideband IQ source; a CAT rig sends only audio");
            }

            ui.add_space(8.0);
            ui.label(RichText::new("sql").size(10.0).color(crate::theme::CYAN_DIM()));
            ui.add_enabled(
                wideband,
                egui::DragValue::new(&mut cfg.threshold_db).range(6..=30).suffix(" dB").speed(0.2),
            )
            .on_hover_text(
                "How far a burst must stand above the channel's own noise floor \
                 before it is decoded",
            );
        });

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for f in IsmFamily::ALL {
                let usable = wideband && f.implemented();
                let chip =
                    crate::chrome::chip_enabled(ui, usable, cfg.family_enabled(f), f.label());
                if chip.clicked() {
                    let on = cfg.family_enabled(f);
                    cfg.set_family(f, !on);
                }
                // A family with nothing behind it yet says so rather than
                // looking like a switch that does not work. The devices are on
                // the air either way, and an operator who knows meters are "not
                // yet" is better informed than one who sees no mention of them.
                chip.on_hover_text(if f.implemented() {
                    f.hint().to_string()
                } else {
                    format!("{} — not decoded yet", f.hint())
                });
            }
        });

        ui.add_space(6.0);
        self.ism_channels(ui, cmds);

        ui.add_space(6.0);
        ui.separator();
        self.ism_sort_chips(ui);
        ui.add_space(2.0);
        self.ism_list(ui, cmds);
    }

    /// Where the decoder is listening, and why it is not where it is not.
    fn ism_channels(&self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let Some(st) = &self.ism_status else {
            ui.label(
                RichText::new("waiting for the decoder…")
                    .size(11.0)
                    .color(crate::theme::CYAN_DIM()),
            );
            return;
        };

        if let Some(why) = &st.unavailable {
            ui.label(RichText::new(why).size(11.0).color(crate::theme::YELLOW()));
            // The one problem the operator can fix with a click.
            if let Some(hz) = st.suggest_center_hz {
                ui.horizontal(|ui| {
                    if crate::chrome::chip(ui, false, &format!("TUNE {:.3} MHz", hz / 1e6))
                        .on_hover_text(
                            "Put the whole 868 MHz channel plan inside the receiver's span",
                        )
                        .clicked()
                    {
                        cmds.push(Command::SetVfo { vfo: Vfo::A, hz });
                    }
                });
            }
            ui.add_space(4.0);
        }

        for c in &st.channels {
            ui.horizontal(|ui| {
                // Painted rather than written. A filled-circle glyph is not in
                // the bundled font's fallback chain, so `●` comes out as an
                // empty box — and next to the hollow `○` of an idle channel
                // that reads as the wrong state rather than as a missing glyph.
                let colour = if c.live { crate::theme::GREEN() } else { crate::theme::CYAN_DIM() };
                let (lamp, _) =
                    ui.allocate_exact_size(egui::vec2(11.0, 11.0), egui::Sense::hover());
                if c.live {
                    ui.painter().circle_filled(lamp.center(), 3.5, colour);
                } else {
                    ui.painter().circle_stroke(lamp.center(), 3.5, egui::Stroke::new(1.0, colour));
                }
                ui.label(
                    RichText::new(format!("{:.3}", c.freq_hz / 1e6)).size(11.0).monospace().color(
                        if c.live { crate::theme::TEXT() } else { crate::theme::CYAN_DIM() },
                    ),
                );
                let tail = match &c.reason {
                    Some(r) => format!("{}  —  {r}", c.label),
                    None => c.label.clone(),
                };
                ui.label(RichText::new(tail).size(10.5).color(crate::theme::CYAN_DIM()));
            });
        }

        // What the gate is seeing, which is the difference between "quiet band"
        // and "busy band full of things we cannot read yet" — and, when it is
        // zero, between those and "not actually listening". Shown whenever a
        // channel is live, including at zero: hiding it then makes a decoder that
        // has heard nothing look the same as one that is not running.
        if st.channels.iter().any(|c| c.live) {
            ui.add_space(3.0);
            ui.label(
                RichText::new(format!("{} bursts, {} decoded", st.bursts, st.decodes))
                    .size(10.0)
                    .color(crate::theme::CYAN_DIM()),
            )
            .on_hover_text(
                "Transmissions the gate opened on, and how many produced a valid frame. \
                 Many bursts and no decodes means the band is busy with devices \
                 sdroxide cannot read yet. None at all means nothing is reaching the \
                 threshold — lower it, or check that the dial is on the band.",
            );
        }
    }

    fn ism_sort_chips(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("sort").size(10.0).color(crate::theme::CYAN_DIM()));
            for s in IsmSort::ALL {
                let on = self.ism_sort == s;
                if crate::chrome::chip(ui, on, s.label()).clicked() {
                    // Pressing the active one flips the direction, as the decode
                    // list's sort chips do.
                    if on {
                        self.ism_sort_desc = !self.ism_sort_desc;
                    } else {
                        self.ism_sort = s;
                        self.ism_sort_desc = true;
                    }
                }
            }
            crate::chrome::row_tail(ui, |ui| {
                ui.label(
                    RichText::new(format!("{} devices", self.ism_reports.len()))
                        .size(10.0)
                        .color(crate::theme::CYAN_DIM()),
                );
            });
        });
    }

    fn ism_list(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        if self.ism_reports.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("nothing heard yet").size(11.0).color(crate::theme::CYAN_DIM()));
            return;
        }

        let mut order: Vec<&IsmReport> = self.ism_reports.iter().collect();
        let desc = self.ism_sort_desc;
        match self.ism_sort {
            IsmSort::Heard => order.sort_by_key(|r| r.last_at),
            IsmSort::Count => order.sort_by_key(|r| r.count),
            IsmSort::Signal => order.sort_by_key(|r| r.snr_db),
            IsmSort::Frequency => order.sort_by(|a, b| a.freq_hz.total_cmp(&b.freq_hz)),
        }
        if desc {
            order.reverse();
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Staged out of the closure: it borrows `self` and cannot also push a
        // tune command through `cmds`.
        let mut tune_to: Option<f64> = None;
        let narrow = ui.available_width() < 560.0;
        egui::ScrollArea::vertical()
            .id_salt("ism-devices")
            .max_height(ui.available_height())
            .auto_shrink([false, false])
            .show_themed(ui, |ui| {
                for r in order {
                    if ism_row(ui, r, now, narrow).clicked() {
                        tune_to = Some(r.freq_hz);
                    }
                }
            });

        if let Some(hz) = tune_to {
            // These are wideband FSK bursts, not something to listen to: tuning
            // to one puts it under the cursor so the operator can watch it on
            // the waterfall. NFM is the widest ordinary mode, which is as close
            // as the receiver gets to the right shape.
            cmds.push(Command::SetVfo { vfo: Vfo::A, hz });
            cmds.push(Command::SetMode { rx: sdroxide_types::RxId::Main, mode: Mode::Nfm });
        }
    }
}

/// One device. Reads the way the question is asked: when, what, which one, and
/// what it said.
fn ism_row(ui: &mut egui::Ui, r: &IsmReport, now: i64, narrow: bool) -> egui::Response {
    let inner = egui::Frame::new()
        .fill(crate::theme::ROW_BG())
        .inner_margin(egui::Margin { left: 6, right: 6, top: 3, bottom: 3 })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(ROW_H);
                ui.spacing_mut().item_spacing.x = 5.0;

                // Age, not a clock: "how long since this spoke" is the question,
                // and a device that has gone quiet is the thing worth noticing.
                row_cell(
                    ui,
                    30.0,
                    ROW_H,
                    true,
                    egui::Label::new(
                        RichText::new(fmt_age(now - r.last_at))
                            .size(10.5)
                            .color(crate::theme::CYAN_DIM()),
                    ),
                );
                row_cell(
                    ui,
                    62.0,
                    ROW_H,
                    true,
                    egui::Label::new(
                        RichText::new(format!("{:.4}", r.freq_hz / 1e6))
                            .size(11.0)
                            .monospace()
                            .color(crate::theme::TEXT()),
                    ),
                );
                row_cell(
                    ui,
                    96.0,
                    ROW_H,
                    false,
                    egui::Label::new(
                        RichText::new(r.fmt_kind()).size(11.0).color(crate::theme::CYAN()),
                    ),
                );
                row_cell(
                    ui,
                    46.0,
                    ROW_H,
                    false,
                    egui::Label::new(RichText::new(&r.device).size(11.0).monospace().strong()),
                );
                if !narrow {
                    row_cell(
                        ui,
                        34.0,
                        ROW_H,
                        true,
                        egui::Label::new(
                            RichText::new(format!("{} dB", r.snr_db))
                                .size(10.5)
                                .color(crate::theme::CYAN_DIM()),
                        ),
                    );
                    // How many times it has been heard: one frame is a CRC that
                    // happened to pass, fifty is a device that is really there.
                    row_cell(
                        ui,
                        30.0,
                        ROW_H,
                        true,
                        egui::Label::new(
                            RichText::new(format!("×{}", r.count))
                                .size(10.5)
                                .color(crate::theme::CYAN_DIM()),
                        ),
                    );
                }
                // The readings take whatever is left, so a narrow pane drops the
                // signal columns rather than squeezing the values that matter.
                let colour =
                    if r.encrypted { crate::theme::YELLOW() } else { crate::theme::GREEN() };
                ui.add(egui::Label::new(RichText::new(r.fmt_readings()).size(11.0).color(colour)));
            });
        });

    let resp = inner.response.interact(egui::Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let mut hover = format!(
        "{}  id {}\n{:.4} MHz   {} dB   heard {}×\nfirst heard {} ago",
        r.protocol.label(),
        r.device,
        r.freq_hz / 1e6,
        r.snr_db,
        r.count,
        fmt_age(now - r.first_at),
    );
    if !r.raw_hex.is_empty() {
        hover.push_str(&format!("\n\n{}", r.raw_hex));
    }
    hover.push_str("\n\nClick to tune the receiver to it");
    resp.on_hover_text(hover)
}
