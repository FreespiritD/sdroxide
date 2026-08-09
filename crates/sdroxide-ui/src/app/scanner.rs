//! The Scanner window: what to scan, how loud a signal has to be to stop it,
//! and what to do once it has stopped.

use eframe::egui::{self, RichText};
use sdroxide_types::{
    Command, Mode, SCAN_STEPS_HZ, SQUELCH_OPEN_DB, ScanKind, ScanResume, ScannerConfig,
};

use crate::app::SdroxideApp;

impl SdroxideApp {
    pub(in crate::app) fn scanner_window(&mut self, ctx: &egui::Context, cmds: &mut Vec<Command>) {
        if !self.show_scanner {
            return;
        }
        let mut open = self.show_scanner;
        // Edited in place and sent whole on any change, the way the skimmer's
        // settings are; the engine persists it and echoes it back.
        let mut cfg = self.scanner.clone();
        let resp = egui::Window::new("Scanner")
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(true)
            .default_width(crate::layout::window_w(ctx, 420.0))
            .show(ctx, |ui| {
                crate::chrome::window_body_bg(ui);
                self.scanner_body(ui, &mut cfg, cmds)
            });
        if let Some(r) = &resp {
            crate::chrome::paint_window_border(ctx, &r.response);
        }
        if cfg != self.scanner {
            self.scanner = cfg.clone();
            cmds.push(Command::SetScannerConfig(cfg));
        }
        self.show_scanner = open;
    }

    fn scanner_body(&self, ui: &mut egui::Ui, cfg: &mut ScannerConfig, cmds: &mut Vec<Command>) {
        let scan = self.state.scan;

        // What to scan.
        ui.horizontal(|ui| {
            for kind in ScanKind::ALL {
                if crate::chrome::chip(ui, cfg.kind == kind, kind.label())
                    .on_hover_text(match kind {
                        ScanKind::Memories => "Work through the stored memory channels",
                        ScanKind::Range => "Work through a slice of a band on a channel grid",
                    })
                    .clicked()
                {
                    cfg.kind = kind;
                }
            }
            crate::chrome::row_tail(ui, |ui| {
                let label = if scan.running { "STOP" } else { "START" };
                if crate::chrome::chip_accent(
                    ui,
                    scan.running,
                    label,
                    crate::theme::GREEN(),
                    egui::Color32::BLACK,
                )
                .clicked()
                {
                    cmds.push(Command::SetScanning(!scan.running));
                }
            });
        });
        ui.separator();

        match cfg.kind {
            ScanKind::Range => self.scanner_range(ui, cfg),
            ScanKind::Memories => self.scanner_memories(ui, cfg),
        }

        ui.separator();
        self.scanner_thresholds(ui, cfg);
        ui.separator();
        self.scanner_status(ui, cmds);
    }

    fn scanner_range(&self, ui: &mut egui::Ui, cfg: &mut ScannerConfig) {
        egui::Grid::new("scan-range").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("From");
            ui.horizontal(|ui| {
                mhz_edit(ui, &mut cfg.range_lo_hz);
                ui.label("to");
                mhz_edit(ui, &mut cfg.range_hi_hz);
                ui.label(RichText::new("MHz").weak());
            });
            ui.end_row();

            ui.label("Step");
            ui.horizontal(|ui| {
                for step in SCAN_STEPS_HZ {
                    let label = if step >= 10_000.0 {
                        format!("{:.0}k", step / 1000.0)
                    } else {
                        format!("{:.2}k", step / 1000.0)
                    };
                    if crate::chrome::chip(ui, (cfg.step_hz - step).abs() < 1.0, label).clicked() {
                        cfg.step_hz = step;
                    }
                }
            });
            ui.end_row();

            ui.label("Mode");
            egui::ComboBox::from_id_salt("scan-mode").selected_text(cfg.mode.label()).show_ui(
                ui,
                |ui| {
                    // The modes anyone scans in. A range scan sets one mode for
                    // the whole range; a memory scan takes each channel's own.
                    for m in [Mode::Nfm, Mode::Am, Mode::Wfm, Mode::Usb, Mode::Lsb] {
                        ui.selectable_value(&mut cfg.mode, m, m.label());
                    }
                },
            );
            ui.end_row();
        });
        if !cfg.range_is_usable() {
            ui.label(
                RichText::new(
                    "That range is empty — the high edge has to be at least one \
                               channel above the low one.",
                )
                .color(crate::theme::ALERT()),
            );
        }
    }

    fn scanner_memories(&self, ui: &mut egui::Ui, cfg: &mut ScannerConfig) {
        if self.memories.is_empty() {
            ui.label(
                RichText::new("No memory channels stored yet — store some in the MEM window.")
                    .weak(),
            );
            return;
        }
        ui.label(RichText::new("SKIP a channel to pass over it").size(10.0).weak());
        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
            egui::Grid::new("scan-mems").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
                for m in &self.memories {
                    let skipped = cfg.skip.contains(&m.id);
                    if crate::chrome::chip(ui, skipped, "SKIP").clicked() {
                        if skipped {
                            cfg.skip.retain(|&id| id != m.id);
                        } else {
                            cfg.skip.push(m.id);
                        }
                    }
                    let name =
                        if m.name.is_empty() { format!("#{}", m.id) } else { m.name.clone() };
                    let text = RichText::new(name);
                    ui.label(if skipped { text.weak() } else { text });
                    ui.label(
                        RichText::new(format!("{:.6} MHz  {}", m.freq_hz / 1e6, m.mode.label()))
                            .weak(),
                    );
                    ui.end_row();
                }
            });
        });
    }

    fn scanner_thresholds(&self, ui: &mut egui::Ui, cfg: &mut ScannerConfig) {
        egui::Grid::new("scan-levels").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Stops at");
            ui.horizontal(|ui| {
                if crate::chrome::chip(ui, cfg.follow_squelch, "SQL")
                    .on_hover_text(
                        "Use the receiver's own squelch, so the scan stops exactly where the \
                         audio would open",
                    )
                    .clicked()
                {
                    cfg.follow_squelch = !cfg.follow_squelch;
                }
                if cfg.follow_squelch {
                    let sql = self.state.rx[0].squelch_db;
                    ui.label(
                        RichText::new(if sql <= SQUELCH_OPEN_DB + 1.0 {
                            "squelch is off — the scan will stop on the first thing it looks at"
                                .to_string()
                        } else {
                            format!("{sql:.0} dBFS")
                        })
                        .weak(),
                    );
                } else {
                    ui.add(
                        egui::DragValue::new(&mut cfg.threshold_db)
                            .speed(0.5)
                            .range(-140.0..=-10.0)
                            .suffix(" dBFS"),
                    )
                    .on_hover_text("Channel power a signal has to reach to stop the scan");
                }
            });
            ui.end_row();

            ui.label("Listens for");
            ui.add(
                egui::DragValue::new(&mut cfg.dwell_ms).speed(5.0).range(40..=2000).suffix(" ms"),
            )
            .on_hover_text(
                "How long to stay on a candidate before judging it. Below about a tenth of a \
                 second the level meter has not settled and weak signals get missed",
            );
            ui.end_row();

            ui.label("Resumes");
            ui.horizontal(|ui| {
                for r in ScanResume::ALL {
                    if crate::chrome::chip(ui, cfg.resume == r, r.label())
                        .on_hover_text(match r {
                            ScanResume::Carrier => "Carry on once the signal drops",
                            ScanResume::Timed => "Carry on after a fixed time, regardless",
                            ScanResume::Manual => "Stay until you press NEXT",
                        })
                        .clicked()
                    {
                        cfg.resume = r;
                    }
                }
                if cfg.resume != ScanResume::Manual {
                    ui.add(
                        egui::DragValue::new(&mut cfg.resume_ms)
                            .speed(50.0)
                            .range(200..=60_000)
                            .suffix(" ms"),
                    )
                    .on_hover_text(match cfg.resume {
                        ScanResume::Timed => "How long to stay",
                        // Long enough to ride out the gap between overs, or the
                        // scan leaves in the middle of a conversation.
                        _ => "How long to wait after the signal drops",
                    });
                }
            });
            ui.end_row();
        });
    }

    fn scanner_status(&self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let scan = self.state.scan;
        ui.horizontal(|ui| {
            if !scan.running {
                ui.label(RichText::new("stopped").weak());
                return;
            }
            let here = self.state.active_freq_hz() / 1e6;
            let (text, colour) = if scan.holding {
                (format!("holding {here:.6} MHz"), crate::theme::GREEN())
            } else {
                (format!("scanning · {here:.6} MHz"), crate::theme::CYAN())
            };
            ui.label(RichText::new(text).color(colour).strong());
            crate::chrome::row_tail(ui, |ui| {
                if crate::chrome::chip(ui, false, "SKIP")
                    .on_hover_text("Move on, and don't stop on this channel again")
                    .clicked()
                {
                    cmds.push(Command::ScanSkip);
                }
                if crate::chrome::chip(ui, false, "NEXT").on_hover_text("Move on now").clicked() {
                    cmds.push(Command::ScanNext);
                }
            });
        });
    }
}

/// A frequency box in MHz, which is how an operator says one.
fn mhz_edit(ui: &mut egui::Ui, hz: &mut f64) {
    let mut mhz = *hz / 1e6;
    if ui
        .add(
            egui::DragValue::new(&mut mhz)
                .speed(0.01)
                .range(0.0..=6000.0)
                .max_decimals(6)
                .fixed_decimals(4),
        )
        .changed()
    {
        *hz = mhz * 1e6;
    }
}
