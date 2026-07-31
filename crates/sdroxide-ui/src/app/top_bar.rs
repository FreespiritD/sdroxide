//! The control strip along the top of the window.
//!
//! One method per module in the strip, all called from [`SdroxideApp::top_bar`]
//! in the order they appear: frequency, S-meter, VFO/RIT, band and mode,
//! RX filter, sub-RX, TX, the skimmer and display popups, and the window
//! buttons. Each pushes [`Command`]s rather than touching the controller, so
//! the whole strip is a pure function of the state it draws from.

use eframe::egui::{self, Color32, ComboBox, DragValue, RichText, Slider};
use sdroxide_types::{
    AgcMode, Band, Command, Direction, GainElement, Mode, RxId, SkimmerKind, Vfo,
};

use crate::widgets::{freq_display, smeter};

use crate::app::SdroxideApp;
use crate::app::panels::digi_freq_for_band;

impl SdroxideApp {
    pub(in crate::app) fn top_bar(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        // All controls are captioned (or bare) modules that reflow when the
        // window is narrow. The frequency box is always first, the S-meter
        // second; the rest follow and wrap to further rows.
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true), |ui| {
            self.freq_module(ui, cmds);
            self.smeter_module(ui);
            self.vfo_rit_module(ui, cmds);
            self.rx_filter_module(ui, cmds);
            // Only while the sub is running: the module appearing is itself the
            // confirmation that SUB took effect, and it costs a wrapped row of
            // top bar that operators who never use it should not have to pay.
            if self.state.sub_rx_enabled {
                self.sub_rx_module(ui, cmds);
            }
            if self.caps.as_ref().is_some_and(|c| c.is_transmit_capable()) {
                self.tx_module(ui, cmds);
            }
            self.display_module(ui, cmds);
            self.windows_module(ui);
        });
    }

    /// The VFO frequency controls (A/B select + big readout + the inactive
    /// VFO's frequency) in a label-less box, always the first module.
    fn freq_module(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        // The 10-digit readout is fixed width, so measure it (via the same fonts
        // freq_display uses) and size the box to hug its contents — that keeps the
        // right column against the box edge (no empty space) and lets the readout
        // be centred vertically by exact geometry rather than a fragile layout hint.
        let font40 = egui::FontId::monospace(40.0);
        let digit =
            ui.painter().layout_no_wrap("0".to_owned(), font40.clone(), Color32::WHITE).size();
        let dot_w = ui.painter().layout_no_wrap(".".to_owned(), font40, Color32::WHITE).size().x;
        let hz_w = ui
            .painter()
            .layout_no_wrap(" Hz".to_owned(), egui::FontId::proportional(12.0), Color32::WHITE)
            .size()
            .x;
        // 10 digits + 3 group separators + " Hz", with freq_display's 1px spacing.
        let readout_w = 10.0 * digit.x + 3.0 * dot_w + hz_w + 13.0;
        let readout_h = digit.y;

        let ab_w = 68.0;
        let right_w = 96.0;
        let box_w = 8.0 + ab_w + 10.0 + readout_w + 12.0 + right_w + 8.0;

        crate::chrome::module_bare_h(ui, box_w, crate::chrome::MODULE_TALL_H, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0; // control every gap explicitly
            let active = self.state.active_vfo;
            let full_h = ui.available_height();

            // VFO A/B selector, vertically centred in the full box height.
            let mut sel = None;
            ui.allocate_ui_with_layout(
                egui::vec2(ab_w, full_h),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (v, label) in [(Vfo::A, "A"), (Vfo::B, "B")] {
                        if crate::chrome::chip(ui, active == v, RichText::new(label).size(15.0))
                            .clicked()
                        {
                            sel = Some(v);
                        }
                    }
                },
            );
            if let Some(v) = sel {
                cmds.push(Command::SelectVfo(v));
            }
            ui.add_space(10.0);

            // Big frequency readout, centred vertically by measured height.
            let mut new_hz = None;
            ui.allocate_ui_with_layout(
                egui::vec2(readout_w, full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.add_space(((full_h - readout_h) / 2.0).max(0.0));
                    new_hz = freq_display::show(
                        ui,
                        egui::Id::new("main-freq"),
                        self.state.active_freq_hz(),
                        self.input.cfg.wheel,
                    );
                },
            );
            if let Some(hz) = new_hz {
                cmds.push(Command::SetVfo { vfo: active, hz });
            }
            ui.add_space(12.0);

            // Right column: inactive VFO frequency anchored top-right, band/mode
            // selector anchored bottom-right, hard against the box edge.
            let inactive_hz = match active {
                Vfo::A => self.state.vfo_b_hz,
                Vfo::B => self.state.vfo_a_hz,
            };
            ui.allocate_ui_with_layout(
                egui::vec2(right_w, full_h),
                egui::Layout::top_down(egui::Align::Max),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(
                        RichText::new(format!("{:.6} MHz", inactive_hz / 1e6))
                            .monospace()
                            .size(12.0)
                            .color(Color32::from_gray(120)),
                    );
                    let pad = (ui.available_height() - 24.0).max(0.0);
                    ui.add_space(pad);
                    self.band_mode_button(ui, cmds);
                },
            );
        });
    }

    /// The S-meter in a label-less box, always pinned top-right. Clicking it
    /// cycles the needle / bar / trace faces.
    fn smeter_module(&mut self, ui: &mut egui::Ui) {
        crate::chrome::module_bare_flush_h(ui, 250.0, crate::chrome::MODULE_TALL_H, |ui| {
            let resp = smeter::show(ui, self.meters.as_ref(), self.view.smeter_style)
                .on_hover_text("Click to cycle meter face: needle / bar / trace");
            if resp.clicked() {
                self.view.smeter_style = self.view.smeter_style.next();
            }
        });
    }

    /// Combined VFO + RIT/XIT box: the VFO A/B utility chips on top, with the
    /// RIT/XIT tuning-offset controls stacked underneath. Bare and tall — this
    /// replaces the separate VFO and RIT/XIT boxes.
    fn vfo_rit_module(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let tx_capable = self.caps.as_ref().is_some_and(|c| c.is_transmit_capable());
        // Fixed field width, wide enough for a signed 4-digit offset plus " Hz".
        let hz_field = egui::vec2(74.0, 22.0);
        crate::chrome::module_bare_h(ui, 270.0, crate::chrome::MODULE_TALL_H, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                // VFO utility chips.
                ui.horizontal(|ui| {
                    if crate::chrome::chip(ui, false, "A↔B").on_hover_text("Swap VFOs").clicked()
                    {
                        cmds.push(Command::SwapVfos);
                    }
                    if crate::chrome::chip(ui, false, "A→B").on_hover_text("Copy A to B").clicked()
                    {
                        cmds.push(Command::CopyAtoB);
                    }
                    if crate::chrome::chip(ui, self.state.split, "SPLIT").clicked() {
                        cmds.push(Command::SetSplit(!self.state.split));
                    }
                    if crate::chrome::chip(ui, self.state.sub_rx_enabled, "SUB")
                        .on_hover_text(
                            "Second receiver, in the right ear. It tunes independently of \
                             A/B — its controls appear in the SUB module, and its passband \
                             on the waterfall.",
                        )
                        .clicked()
                    {
                        cmds.push(Command::SetSubRx(!self.state.sub_rx_enabled));
                    }
                });
                // RIT / XIT tuning offsets.
                ui.horizontal(|ui| {
                    let rit = self.state.rit;
                    if crate::chrome::chip(ui, rit.enabled, "RIT").clicked() {
                        cmds.push(Command::SetRit { enabled: !rit.enabled, hz: rit.hz });
                    }
                    let mut rit_hz = rit.hz;
                    if ui
                        .add_sized(
                            hz_field,
                            DragValue::new(&mut rit_hz).speed(5).range(-9999..=9999).suffix(" Hz"),
                        )
                        .changed()
                    {
                        cmds.push(Command::SetRit { enabled: rit.enabled, hz: rit_hz });
                    }
                    if tx_capable {
                        let xit = self.state.xit;
                        if crate::chrome::chip(ui, xit.enabled, "XIT").clicked() {
                            cmds.push(Command::SetXit { enabled: !xit.enabled, hz: xit.hz });
                        }
                        let mut xit_hz = xit.hz;
                        if ui
                            .add_sized(
                                hz_field,
                                DragValue::new(&mut xit_hz)
                                    .speed(5)
                                    .range(-9999..=9999)
                                    .suffix(" Hz"),
                            )
                            .changed()
                        {
                            cmds.push(Command::SetXit { enabled: xit.enabled, hz: xit_hz });
                        }
                    }
                });
            });
        });
    }

    /// The band/mode selector button plus the floating popup with the band +
    /// mode + digital button rows.
    fn band_mode_button(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let mode = self.state.rx[0].mode;
        let summary = format!("{} · {}", self.state.band.label(), mode.label());
        let btn = crate::chrome::chip(ui, false, RichText::new(summary).size(14.0));

        let popup_id = egui::Popup::default_response_id(&btn);
        let now = ui.input(|i| i.time);
        let alpha =
            crate::chrome::popup_fade_alpha(ui.ctx(), popup_id, now, &mut self.mode_popup_since);
        let resp = egui::Popup::from_toggle_button_response(&btn)
            .frame(crate::chrome::window_frame_alpha(alpha))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_opacity(alpha);
                ui.set_max_width(430.0);
                ui.label(RichText::new("BAND").color(crate::theme::CYAN_DIM).size(9.5).strong());
                let digital = mode.is_digital();
                ui.horizontal_wrapped(|ui| {
                    for b in Band::ALL {
                        // In a digital mode, a band button tunes to that
                        // band's FT8/FT4 dial frequency (SetVfo keeps the
                        // mode); otherwise it's a normal band change. Bands
                        // with no standard digital frequency are disabled.
                        // RF Paint has no calling frequency, so its band
                        // buttons jump to the band's default frequency while
                        // staying in RF Paint — every band the radio can
                        // reach is available.
                        let digi_hz = if mode.is_rf_paint() {
                            Some(b.default_entry().0)
                        } else if digital {
                            digi_freq_for_band(mode, b)
                        } else {
                            None
                        };
                        let cap_ok = self.caps.as_ref().is_none_or(|c| {
                            b.edges().is_none_or(|(lo, hi)| c.can_rx_hz(lo) || c.can_rx_hz(hi))
                        });
                        let enabled = cap_ok && (!digital || digi_hz.is_some());
                        let active = if mode.is_rf_paint() {
                            self.state.band == b
                        } else {
                            match digi_hz {
                                Some(hz) => (self.state.active_freq_hz() - hz).abs() < 500.0,
                                None => !digital && self.state.band == b,
                            }
                        };
                        let clicked = ui
                            .add_enabled_ui(enabled, |ui| {
                                crate::chrome::chip(ui, active, b.label())
                            })
                            .inner
                            .clicked();
                        if clicked {
                            match digi_hz {
                                Some(hz) => {
                                    cmds.push(Command::SetVfo { vfo: self.state.active_vfo, hz })
                                }
                                None => cmds.push(Command::SetBand(b)),
                            }
                        }
                    }
                });
                ui.add_space(6.0);
                ui.label(RichText::new("MODE").color(crate::theme::CYAN_DIM).size(9.5).strong());
                ui.horizontal_wrapped(|ui| {
                    for m in [
                        Mode::Lsb,
                        Mode::Usb,
                        Mode::Cw,
                        Mode::Am,
                        Mode::Sam,
                        Mode::Nfm,
                        Mode::Wfm,
                        Mode::Digu,
                        Mode::Digl,
                        Mode::Dsb,
                        Mode::Spec,
                    ] {
                        if crate::chrome::chip(ui, mode == m, m.label()).clicked() {
                            cmds.push(Command::SetMode { rx: RxId::Main, mode: m });
                        }
                    }
                });
                ui.add_space(6.0);
                ui.label(RichText::new("DIGITAL").color(crate::theme::CYAN_DIM).size(9.5).strong());
                ui.horizontal_wrapped(|ui| {
                    for m in Mode::DIGITAL {
                        if crate::chrome::chip(ui, mode == m, m.label()).clicked() {
                            cmds.push(Command::SetMode { rx: RxId::Main, mode: m });
                        }
                    }
                });
            });
        if let Some(r) = &resp {
            crate::chrome::paint_popup_cut_border(ui.ctx(), &r.response, alpha);
            if r.response.contains_pointer() {
                self.mode_popup_since = Some(now); // keep it up while the pointer is on it
            }
        }
    }

    /// Combined Receiver + Filter/Noise box: AGC / volume / mute on top, with the
    /// squelch + noise-blanker + auto-notch + noise-reduction controls stacked
    /// underneath. Bare and tall, like the VFO/RIT box — replaces the separate
    /// Receiver and Filter boxes.
    fn rx_filter_module(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        // The device's front-end RX gain, if it has one the software can set —
        // the Hermes-Lite 2's LNA, a SoapySDR device's first RX stage. A rig
        // with none (a CAT radio on a sound card) gets no slider and no extra
        // module width, so nothing moves for the people who can't use it.
        let rx_gains: Vec<GainElement> = self
            .caps
            .as_ref()
            .map(|c| c.gains.iter().filter(|g| g.direction == Direction::Rx).cloned().collect())
            .unwrap_or_default();
        let rx_gain = rx_gains.first().cloned();
        let width = if rx_gain.is_some() { 506.0 } else { 356.0 };
        crate::chrome::module_bare_h(ui, width, crate::chrome::MODULE_TALL_H, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                // Receiver: volume, RF gain, AGC, mute.
                ui.horizontal(|ui| {
                    let mut vol = self.state.rx[0].volume;
                    ui.label("Vol");
                    if crate::chrome::slider(ui, Slider::new(&mut vol, 0.0..=1.0).show_value(false))
                        .changed()
                    {
                        self.state.rx[0].volume = vol; // optimistic echo
                        cmds.push(Command::SetVolume { rx: RxId::Main, v: vol });
                    }
                    if let Some(g) = &rx_gain {
                        let mut hint = format!(
                            "Front-end RX gain ({}). Too much clips the receiver's ADC and \
                             smears spurious signals across the band; too little and it goes deaf.",
                            g.name
                        );
                        if rx_gains.len() > 1 {
                            hint.push_str(&format!(
                                "\n\nThis rig has {} RX gain stages — the rest are in \
                                 Settings → Device.",
                                rx_gains.len()
                            ));
                        }
                        ui.label("Gain").on_hover_text(&hint);
                        let mut db = self
                            .state
                            .gains
                            .iter()
                            .find(|(n, _)| *n == g.name)
                            .map(|(_, d)| *d)
                            .unwrap_or(g.min_db);
                        let step = if g.step_db > 0.0 { g.step_db } else { 1.0 };
                        // Narrower rail than Vol: this one carries a dB readout,
                        // and the module has to stay inside one wrapped row.
                        let resp = ui
                            .scope(|ui| {
                                ui.spacing_mut().slider_width = 76.0;
                                crate::chrome::slider(
                                    ui,
                                    Slider::new(&mut db, g.min_db..=g.max_db)
                                        .step_by(step)
                                        .suffix(" dB"),
                                )
                            })
                            .inner
                            .on_hover_text(&hint);
                        if resp.changed() {
                            // Optimistic echo so the knob tracks the drag instead
                            // of snapping back until the engine answers.
                            match self.state.gains.iter_mut().find(|(n, _)| *n == g.name) {
                                Some((_, d)) => *d = db,
                                None => self.state.gains.push((g.name.clone(), db)),
                            }
                            cmds.push(Command::SetGain {
                                dir: Direction::Rx,
                                element: g.name.clone(),
                                db,
                            });
                        }
                    }
                    let agc = self.state.rx[0].agc;
                    ComboBox::from_id_salt("agc")
                        .selected_text(format!("AGC {}", agc.label()))
                        .width(88.0)
                        .show_ui(ui, |ui| {
                            for a in AgcMode::ALL {
                                if ui.selectable_label(agc == a, a.label()).clicked() {
                                    cmds.push(Command::SetAgc { rx: RxId::Main, agc: a });
                                }
                            }
                        });
                    let muted = self.state.rx[0].muted;
                    if crate::chrome::chip_accent(ui, muted, "MUTE", crate::theme::PINK, Color32::WHITE)
                        .clicked()
                    {
                        cmds.push(Command::SetMute { rx: RxId::Main, muted: !muted });
                    }
                    // Record receiver audio to an MP3 file (toggling).
                    let recording = self.state.recording;
                    let rec = crate::chrome::chip_accent(
                        ui,
                        recording,
                        "REC",
                        crate::theme::PINK,
                        Color32::WHITE,
                    )
                    .on_hover_text(match &self.state.recording_file {
                        Some(f) => format!("Recording to {f} — click to stop"),
                        None => "Record receiver audio to MP3".to_string(),
                    });
                    if rec.clicked() {
                        cmds.push(Command::SetRecording(!recording));
                    }
                });
                // Filter / Noise: squelch, noise blanker.
                ui.horizontal(|ui| {
                    let mut sql = self.state.rx[0].squelch_db;
                    ui.label("SQL");
                    if crate::chrome::slider(
                        ui,
                        Slider::new(&mut sql, sdroxide_types::SQUELCH_OPEN_DB..=-30.0)
                            .show_value(true)
                            .custom_formatter(|v, _| {
                                if v <= (sdroxide_types::SQUELCH_OPEN_DB + 1.0) as f64 {
                                    "off".into()
                                } else {
                                    format!("{v:.0}")
                                }
                            }),
                    )
                    .changed()
                    {
                        self.state.rx[0].squelch_db = sql; // optimistic echo
                        cmds.push(Command::SetSquelch { rx: RxId::Main, db: sql });
                    }
                    let nb = self.state.noise_blanker;
                    if crate::chrome::chip(ui, nb, "NB")
                        .on_hover_text("Impulse noise blanker")
                        .clicked()
                    {
                        cmds.push(Command::SetNoiseBlanker(!nb));
                    }
                    // Auto-notch — cancels constant tones (heterodynes / carriers).
                    let anc = self.state.rx[0].auto_notch;
                    if crate::chrome::chip(ui, anc, "ANC")
                        .on_hover_text("Auto-notch: cancel constant tone elements (heterodynes)")
                        .clicked()
                    {
                        self.state.rx[0].auto_notch = !anc; // optimistic echo
                        cmds.push(Command::SetAutoNotch { rx: RxId::Main, on: !anc });
                    }
                    // Noise reduction — cycles Off → AI Low/Med/High (neural
                    // RNNoise) → NR Low/Mid/High (spectral) → Off.
                    let nr = self.state.rx[0].noise_reduction;
                    let nr_label =
                        if nr.is_on() { format!("NR {}", nr.label()) } else { "NR".to_string() };
                    if crate::chrome::chip(ui, nr.is_on(), nr_label)
                        .on_hover_text(
                            "Noise reduction (voice) — click to cycle: AI Low/Med/High (neural RNNoise), then NR Low/Mid/High (spectral), then Off",
                        )
                        .clicked()
                    {
                        let next = nr.next();
                        self.state.rx[0].noise_reduction = next; // optimistic echo
                        cmds.push(Command::SetNoiseReduction { rx: RxId::Main, level: next });
                    }
                    // WFM broadcast stereo: lit while a 19 kHz pilot is locked,
                    // click to force mono. Only WFM has a pilot to find.
                    if self.state.rx[0].mode == Mode::Wfm {
                        let want = self.state.rx[0].wfm_stereo;
                        let locked = self.meters.as_ref().is_some_and(|m| m.stereo);
                        let hover = if !want {
                            "WFM stereo forced off — click for automatic stereo"
                        } else if locked {
                            "WFM stereo: pilot locked. Click to force mono"
                        } else {
                            "WFM stereo: automatic, no pilot on this station"
                        };
                        if crate::chrome::chip(ui, want && locked, "ST")
                            .on_hover_text(hover)
                            .clicked()
                        {
                            self.state.rx[0].wfm_stereo = !want; // optimistic echo
                            cmds.push(Command::SetWfmStereo { rx: RxId::Main, on: !want });
                        }
                    }
                });
            });
        });
    }

    /// The sub receiver's own controls, shown only while it is running. The sub
    /// has a frequency, a mode and a filter of its own — none of which the main
    /// receiver's controls can reach — so without this module it is a second
    /// receiver that can only be switched on and off.
    fn sub_rx_module(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        // The sub tunes anywhere inside the device passband and nowhere outside
        // it: both receivers are DDCs on the same IQ stream.
        let half = self.state.sample_rate / 2.0;
        let (dev_lo, dev_hi) = (self.state.center_hz - half, self.state.center_hz + half);
        // Field height, and the height every row is told to be. egui sizes a
        // horizontal row from `interact_size.y` and then grows it as taller
        // widgets land in it — which drops everything added after the first
        // chip a few pixels below everything added before it. Starting the row
        // at the height its tallest widget will be leaves nothing to grow.
        const FIELD_H: f32 = 22.0;
        crate::chrome::module_bare_h(ui, 404.0, crate::chrome::MODULE_TALL_H, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                ui.spacing_mut().interact_size.y = FIELD_H;
                // Frequency, mode, and the two moves worth a single click:
                // send the sub to the dial, or bring the dial to the sub.
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("SUB")
                            .color(crate::widgets::spectrum_view::SUB_COLOR)
                            .size(11.0)
                            .strong(),
                    );
                    let mut hz = self.state.sub_rx_hz;
                    let resp = ui
                        .add_sized(
                            [116.0, FIELD_H],
                            DragValue::new(&mut hz)
                                .speed(10.0)
                                .range(dev_lo..=dev_hi)
                                // Typed and shown in MHz — the unit the operator
                                // reads a frequency in — while the drag step
                                // stays in Hz so it tunes like a dial.
                                .custom_formatter(|v, _| format!("{:.6}", v / 1e6))
                                .custom_parser(|s| s.trim().parse::<f64>().ok().map(|m| m * 1e6))
                                .suffix(" MHz"),
                        )
                        .on_hover_text(
                            "Where the sub receiver listens. Shift-click the waterfall, or \
                             drag inside the sub's passband, to move it.",
                        );
                    if resp.changed() {
                        self.state.sub_rx_hz = hz; // optimistic echo
                        cmds.push(Command::SetSubRxFreq(hz));
                    }
                    let mode = self.state.rx[1].mode;
                    ComboBox::from_id_salt("sub-mode")
                        .selected_text(mode.label())
                        .width(74.0)
                        .show_ui(ui, |ui| {
                            // Audio modes only. The digital modes are wired to
                            // the main receiver alone (one decoder, one TX), and
                            // SPEC produces no audio at all — a sub receiver you
                            // cannot hear is a trap, not a setting.
                            for m in [
                                Mode::Lsb,
                                Mode::Usb,
                                Mode::Cw,
                                Mode::Am,
                                Mode::Sam,
                                Mode::Nfm,
                                Mode::Wfm,
                                Mode::Digu,
                                Mode::Digl,
                                Mode::Dsb,
                            ] {
                                if ui.selectable_label(mode == m, m.label()).clicked() {
                                    cmds.push(Command::SetMode { rx: RxId::Sub, mode: m });
                                }
                            }
                        });
                    if crate::chrome::chip(ui, false, "←DIAL")
                        .on_hover_text("Move the sub receiver to the main dial")
                        .clicked()
                    {
                        cmds.push(Command::SetSubRxFreq(self.state.rx_freq_hz()));
                    }
                    if crate::chrome::chip(ui, false, "DIAL←")
                        .on_hover_text("Move the main dial to the sub receiver")
                        .clicked()
                    {
                        cmds.push(Command::SetVfo {
                            vfo: self.state.active_vfo,
                            hz: self.state.sub_rx_hz,
                        });
                    }
                });
                // Filter, level, mute.
                ui.horizontal(|ui| {
                    let rx1 = self.state.rx[1];
                    let max = rx1.mode.max_filter_hz();
                    ui.label("Filter").on_hover_text("Sub receiver passband edges, in Hz");
                    let mut lo = rx1.filter_lo;
                    let mut hi = rx1.filter_hi;
                    let changed = ui
                        .add_sized(
                            [70.0, FIELD_H],
                            DragValue::new(&mut lo).speed(10).range(-max..=max),
                        )
                        .changed()
                        | ui.add_sized(
                            [70.0, FIELD_H],
                            DragValue::new(&mut hi).speed(10).range(-max..=max),
                        )
                        .changed();
                    if changed {
                        // Same 50 Hz floor the waterfall grips enforce, so the
                        // passband can't be dragged shut from either route.
                        let (lo, hi) = (lo.min(hi - 50.0), hi.max(lo + 50.0));
                        (self.state.rx[1].filter_lo, self.state.rx[1].filter_hi) = (lo, hi);
                        cmds.push(Command::SetFilter { rx: RxId::Sub, lo, hi });
                    }
                    let mut vol = rx1.volume;
                    ui.label("Vol").on_hover_text("Sub receiver level (it plays in the right ear)");
                    if ui
                        .scope(|ui| {
                            ui.spacing_mut().slider_width = 64.0;
                            crate::chrome::slider(
                                ui,
                                Slider::new(&mut vol, 0.0..=1.0).show_value(false),
                            )
                        })
                        .inner
                        .changed()
                    {
                        self.state.rx[1].volume = vol; // optimistic echo
                        cmds.push(Command::SetVolume { rx: RxId::Sub, v: vol });
                    }
                    if crate::chrome::chip_accent(
                        ui,
                        rx1.muted,
                        "MUTE",
                        crate::theme::PINK,
                        Color32::WHITE,
                    )
                    .clicked()
                    {
                        cmds.push(Command::SetMute { rx: RxId::Sub, muted: !rx1.muted });
                    }
                });
            });
        });
    }

    fn tx_module(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        // The voice keyer's button only appears where the keyer can transmit —
        // every voice mode plus RADE, which takes a message as its microphone.
        let keyer_ok = self.state.rx[0].mode.allows_voice_keyer();
        let width = if keyer_ok { 520.0 } else { 470.0 };
        crate::chrome::module(ui, "Transmit", width, |ui| {
            let tx = self.state.tx;
            if crate::chrome::chip_accent(
                ui,
                tx.ptt,
                RichText::new(" PTT ").size(15.0).strong(),
                crate::theme::PINK,
                Color32::WHITE,
            )
            .clicked()
            {
                cmds.push(Command::SetPtt(!tx.ptt));
            }
            if crate::chrome::chip_accent(
                ui,
                tx.tune,
                RichText::new(" TUNE ").size(15.0),
                crate::theme::YELLOW,
                crate::theme::INK_ON_CYAN,
            )
            .clicked()
            {
                cmds.push(Command::SetTune(!tx.tune));
            }
            if keyer_ok {
                // Lit while a message is on the air, so the button doubles as
                // the "something is transmitting from the keyer" indicator.
                let playing = self.voice.playing.is_some();
                let hover = match self.voice.playing {
                    Some(i) => format!(
                        "Transmitting {} — click to open the voice keyer",
                        sdroxide_types::slot_label(i as usize, &self.voice.slot(i as usize).name)
                    ),
                    None => "Voice keyer: record and transmit stored messages".to_string(),
                };
                if crate::chrome::chip_accent(
                    ui,
                    playing || self.show_voice,
                    RichText::new(" ▶ ").size(15.0),
                    if playing { crate::theme::PINK } else { crate::theme::CYAN },
                    if playing { Color32::WHITE } else { crate::theme::INK_ON_CYAN },
                )
                .on_hover_text(hover)
                .clicked()
                {
                    self.show_voice = !self.show_voice;
                }
            }
            let mut drive = tx.drive;
            ui.label("Drive");
            if crate::chrome::slider(
                ui,
                Slider::new(&mut drive, 0.0..=1.0)
                    .show_value(true)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            )
            .changed()
            {
                cmds.push(Command::SetTxDrive(drive));
            }
            let mut tune_drive = tx.tune_drive;
            ui.label("Tune");
            if crate::chrome::slider(
                ui,
                Slider::new(&mut tune_drive, 0.0..=1.0)
                    .show_value(true)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            )
            .changed()
            {
                cmds.push(Command::SetTuneDrive(tune_drive));
            }
            let mut mic = tx.mic_gain;
            ui.label("Mic");
            if crate::chrome::slider(ui, Slider::new(&mut mic, 0.0..=1.0).show_value(false))
                .changed()
            {
                cmds.push(Command::SetMicGain(mic));
            }
        });
    }

    /// The SKIM chip: lit while any skimmer runs, and a popup with one row per
    /// kind (CW / PSK / RTTY) — an on/off chip plus that skimmer's squelch, the
    /// SNR a track must reach before it earns a box on the waterfall. Fades out
    /// on its own like the band/mode popup.
    fn skimmer_button(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let btn = crate::chrome::chip(ui, self.state.skimmer.any_enabled(), "SKIM").on_hover_text(
            "CW / PSK / RTTY skimmers — decode signals across the band and mark them on the waterfall",
        );
        // A CAT rig feeding demodulated audio has no IQ span to skim; the engine
        // forces the skimmers off there, so the rows are shown disabled.
        let wideband = self.caps.as_ref().is_none_or(|c| !c.audio_mode);
        let popup_id = egui::Popup::default_response_id(&btn);
        let now = ui.input(|i| i.time);
        let alpha =
            crate::chrome::popup_fade_alpha(ui.ctx(), popup_id, now, &mut self.skimmer_popup_since);
        let resp = egui::Popup::from_toggle_button_response(&btn)
            .frame(crate::chrome::window_frame_alpha(alpha))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_opacity(alpha);
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                ui.label(
                    RichText::new("SKIMMERS").color(crate::theme::CYAN_DIM).size(9.5).strong(),
                );
                // Edit a copy and send the whole struct on any change; the
                // engine echoes it back in the next RadioState.
                let mut cfg = self.state.skimmer;
                // A grid so the squelch fields line up under each other despite
                // the kind chips having different widths.
                egui::Grid::new("skimmer-kinds").num_columns(3).spacing([6.0, 5.0]).show(
                    ui,
                    |ui| {
                        if !wideband {
                            ui.disable();
                        }
                        for kind in SkimmerKind::ALL {
                            if crate::chrome::chip(ui, cfg.enabled(kind), kind.label())
                                .on_hover_text("Run this skimmer")
                                .clicked()
                            {
                                cfg.set_enabled(kind, !cfg.enabled(kind));
                            }
                            ui.label(RichText::new("sql").size(10.0).color(crate::theme::CYAN_DIM));
                            let mut sql = cfg.squelch_db(kind);
                            if ui
                                .add(
                                    DragValue::new(&mut sql)
                                        .speed(0.25)
                                        .range(0..=40)
                                        .suffix(" dB"),
                                )
                                .on_hover_text("Minimum SNR a decoded signal needs to be spotted")
                                .changed()
                            {
                                cfg.set_squelch_db(kind, sql);
                            }
                            ui.end_row();
                        }
                    },
                );
                if !wideband {
                    ui.label(
                        RichText::new("needs a wideband IQ source")
                            .size(9.5)
                            .color(Color32::from_gray(150)),
                    );
                }
                if cfg != self.state.skimmer {
                    cmds.push(Command::SetSkimmerConfig(cfg));
                }
            });
        if let Some(r) = &resp {
            crate::chrome::paint_popup_cut_border(ui.ctx(), &r.response, alpha);
            if r.response.contains_pointer() {
                self.skimmer_popup_since = Some(now); // keep it up while the pointer is on it
            }
        }
    }

    fn display_module(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        // The module reserves its width before the content is drawn, so the
        // WIDE chip has to be paid for before it is known to be drawn.
        const DISPLAY_W: f32 = 348.0;
        const WIDE_CHIP_W: f32 = 60.0;

        // Only a front end with a full-band lane has ever sent one of these, so
        // its presence is what says the strip is on offer at all — there is no
        // capability flag for it, and inventing one would mean a wire-format
        // change for something the frames themselves already answer.
        let has_wide = self.wide_frame.is_some();
        let width = if has_wide { DISPLAY_W + WIDE_CHIP_W } else { DISPLAY_W };

        crate::chrome::module(ui, "Display", width, |ui| {
            if crate::chrome::chip(ui, false, "FIT")
                .on_hover_text("Auto-set floor/ceiling for best waterfall contrast")
                .clicked()
            {
                self.auto_levels();
            }
            if crate::chrome::chip(ui, self.view.peak_hold, "PEAK")
                .on_hover_text("Decaying peak-hold trace")
                .clicked()
            {
                self.view.peak_hold = !self.view.peak_hold;
            }
            // Lit when the spectrum line is visible (not collapsed).
            if crate::chrome::chip(ui, !self.view.spectrum_collapsed, "SPEC")
                .on_hover_text("Show/hide the spectrum line above the waterfall")
                .clicked()
            {
                self.view.spectrum_collapsed = !self.view.spectrum_collapsed;
            }
            if has_wide
                && crate::chrome::chip(ui, self.view.wide_waterfall, "WIDE")
                    .on_hover_text(
                        "Show/hide the full-band waterfall strip above the panadapter — \
                         everything this receiver can see at once",
                    )
                    .clicked()
            {
                self.view.wide_waterfall = !self.view.wide_waterfall;
                // History kept while the strip is hidden would come back as a
                // block of minutes-old band, drawn as if it were the last few
                // seconds. Start it again from now instead.
                self.wide_wf.clear();
            }
            self.skimmer_button(ui, cmds);
            self.solar_button(ui);
            // Floor/ceiling + FFT size live in a popup off this button.
            let fft_btn = crate::chrome::chip(ui, false, "FFT")
                .on_hover_text("Spectrum floor / ceiling and FFT size");
            let fft_id = egui::Popup::default_response_id(&fft_btn);
            let now = ui.input(|i| i.time);
            let alpha =
                crate::chrome::popup_fade_alpha(ui.ctx(), fft_id, now, &mut self.fft_popup_since);
            let fft_resp = egui::Popup::from_toggle_button_response(&fft_btn)
                .frame(crate::chrome::window_frame_alpha(alpha))
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    ui.set_opacity(alpha);
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    ui.label(
                        RichText::new("SPECTRUM").color(crate::theme::CYAN_DIM).size(9.5).strong(),
                    );
                    ui.horizontal(|ui| {
                        ui.label("floor");
                        ui.add(
                            DragValue::new(&mut self.view.db_floor)
                                .speed(1.0)
                                .range(-160.0..=-40.0)
                                .suffix(" dB"),
                        );
                        ui.label("ceil");
                        ui.add(
                            DragValue::new(&mut self.view.db_ceil)
                                .speed(1.0)
                                .range(-100.0..=20.0)
                                .suffix(" dB"),
                        );
                    });
                    // Chips rather than a ComboBox: the combo opens a second popup
                    // layer, and clicking it counts as "outside" and closes this one.
                    ui.label(
                        RichText::new("FFT SIZE").color(crate::theme::CYAN_DIM).size(9.5).strong(),
                    );
                    ui.horizontal_wrapped(|ui| {
                        for n in [2048u32, 4096, 8192, 16384, 32768] {
                            if crate::chrome::chip(ui, self.view.fft_size == n, format!("{n}"))
                                .clicked()
                            {
                                self.view.fft_size = n;
                            }
                        }
                    });
                    ui.label(
                        RichText::new("WATERFALL").color(crate::theme::CYAN_DIM).size(9.5).strong(),
                    );
                    if crate::chrome::chip(ui, self.view.waterfall_flip, "FLIP")
                        .on_hover_text(
                            "Scroll the waterfall upwards — newest row at the bottom (V)",
                        )
                        .clicked()
                    {
                        self.view.waterfall_flip = !self.view.waterfall_flip;
                    }
                });
            if let Some(r) = &fft_resp {
                crate::chrome::paint_popup_cut_border(ui.ctx(), &r.response, alpha);
                if r.response.contains_pointer() {
                    self.fft_popup_since = Some(now);
                }
            }
        });
    }

    fn windows_module(&mut self, ui: &mut egui::Ui) {
        crate::chrome::module(ui, "System", 285.0, |ui| {
            if crate::chrome::chip(ui, self.show_logbook, "LOG")
                .on_hover_text("Logbook — all QSOs (digital + manual)")
                .clicked()
            {
                self.show_logbook = !self.show_logbook;
            }
            if crate::chrome::chip(ui, self.show_spots, "SPOTS")
                .on_hover_text("Live spots — DX cluster, POTA, SOTA, PSK Reporter")
                .clicked()
            {
                self.show_spots = !self.show_spots;
            }
            if crate::chrome::chip(ui, self.show_awards, "AWARDS")
                .on_hover_text("Award tracking — DXCC / WAS / WAZ / grids")
                .clicked()
            {
                self.show_awards = !self.show_awards;
            }
            if crate::chrome::chip(ui, self.show_memories, "MEM")
                .on_hover_text("Memory channels")
                .clicked()
            {
                self.show_memories = !self.show_memories;
            }
            if crate::chrome::chip(ui, self.show_settings, "⚙ SETTINGS")
                .on_hover_text("Settings — device gains, antennas, audio devices")
                .clicked()
            {
                self.show_settings = !self.show_settings;
            }
            if crate::chrome::chip(ui, self.help.open, "? HELP")
                .on_hover_text("User manual (F1)")
                .clicked()
            {
                self.help.open = !self.help.open;
            }
        });
    }
}
