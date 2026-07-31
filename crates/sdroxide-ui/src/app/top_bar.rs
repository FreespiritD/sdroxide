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

/// Width of the VFO A/B column in the frequency box.
const AB_W: f32 = 68.0;
/// Width of the frequency box's right column (inactive VFO + band/mode chip).
const RIGHT_W: f32 = 96.0;
/// Width of the S-meter box at its design size.
const SMETER_W: f32 = 250.0;
/// Text size of the band/mode chip's label.
const BAND_MODE_TEXT: f32 = 14.0;
/// Below this the frequency digits stop reading as a dial, so the box sheds
/// something else rather than shrinking them further.
const MIN_DIGIT: f32 = 22.0;
/// Largest digit size the phone box uses, and its height. Digits big enough to
/// tune with a thumb, in a box that leaves the waterfall the screen.
const PHONE_DIGIT_MAX: f32 = 30.0;
const PHONE_FREQ_H: f32 = 42.0;
/// Height of the phone's S-meter box.
const PHONE_SMETER_H: f32 = 40.0;
/// How narrow the phone's S-meter may be squeezed to make room for the menu
/// chips beside it. Below this the scale has no room left to be read on.
const PHONE_SMETER_MIN_W: f32 = 80.0;
/// How wide it may grow. The needle's radius comes from the box width — its arc
/// is a chord across it — so a wider box means a *taller* arc, and past this it
/// would draw the ends of the scale below a 40 pt box. See
/// `the_phone_smeter_keeps_its_scale_inside_its_box`.
const PHONE_SMETER_MAX_W: f32 = 220.0;

/// What a digit size costs the frequency readout, and what size fits a width.
///
/// The readout is ten fixed-width digits, three group separators and a " Hz"
/// tail, spaced 1 pt apart — so its width is linear in the digit size, and one
/// measurement of the live fonts gives the slope. Inverting that is what lets
/// one formula serve a 360 pt phone and a 2560 pt desktop.
struct ReadoutFit {
    /// Width per point of digit size.
    per_pt: f32,
    /// Height per point of digit size.
    h_per_pt: f32,
}

impl ReadoutFit {
    /// Everything up to the group separators scales with the digit size, and
    /// `freq_display` draws " Hz" at 0.3x it, so one reference measurement of
    /// each glyph is enough.
    fn measure(ui: &egui::Ui) -> Self {
        const REF: f32 = 40.0;
        let w = |s: &str, f: egui::FontId| {
            ui.painter().layout_no_wrap(s.to_owned(), f, Color32::WHITE).size()
        };
        let digit = w("0", egui::FontId::monospace(REF));
        let dot = w(".", egui::FontId::monospace(REF)).x;
        let hz = w(" Hz", egui::FontId::proportional(REF)).x;
        Self { per_pt: (10.0 * digit.x + 3.0 * dot + 0.3 * hz) / REF, h_per_pt: digit.y / REF }
    }

    /// Width of the readout at `size`, including `freq_display`'s 1 pt spacing
    /// between its fourteen pieces.
    fn width(&self, size: f32) -> f32 {
        size * self.per_pt + 13.0
    }

    fn height(&self, size: f32) -> f32 {
        size * self.h_per_pt
    }

    /// The largest digit size whose readout fits `budget`. Uncapped and
    /// unfloored — callers clamp to their own limits.
    fn fit(&self, budget: f32) -> f32 {
        (budget - 13.0) / self.per_pt
    }
}

impl SdroxideApp {
    pub(in crate::app) fn top_bar(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        // All controls are captioned (or bare) modules that reflow when the
        // window is narrow. The frequency box is always first, the S-meter
        // second; the rest follow and wrap to further rows.
        let tier = crate::layout::tier(ui.ctx());
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true), |ui| {
            let band_mode_shown = self.freq_module(ui, cmds, tier);
            // A window too narrow for the boxes gets them as menus instead.
            // Wrapping alone cannot save it: a module reserves its width before
            // it draws, so it wraps whole or overflows, and never shrinks.
            if tier.compact() {
                // The meter is the one thing here that can be any width, so it
                // is what gives: measure the chips that follow it and hand it
                // the rest of the row, rather than let one of them wrap onto a
                // row of its own.
                let beside = if tier == crate::layout::Tier::Phone {
                    self.menu_chips_w(ui, band_mode_shown)
                } else {
                    0.0
                };
                self.smeter_module(ui, tier, beside);
                self.menu_bar(ui, cmds, tier, band_mode_shown);
                return;
            }
            self.smeter_module(ui, tier, 0.0);
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

    /// Width the menu row's chips will take, gaps included, so the S-meter can
    /// be handed whatever is left over.
    ///
    /// Measured against the same conditions [`Self::menu_bar`] draws under, and
    /// it has to stay in step with it: a chip counted here that is not drawn
    /// (or the reverse) only costs the meter a few points of width, but the two
    /// lists are meant to be read together.
    fn menu_chips_w(&self, ui: &egui::Ui, band_mode_shown: bool) -> f32 {
        let tx_capable = self.caps.as_ref().is_some_and(|c| c.is_transmit_capable());
        let gap = ui.spacing().item_spacing.x;
        let mut w = 0.0;
        let mut add = |label: &str, size: Option<f32>| w += chip_w(ui, label, size) + gap;
        if !band_mode_shown {
            add(&self.band_mode_label(), Some(BAND_MODE_TEXT));
        }
        if tx_capable {
            add(" PTT ", Some(15.0));
        }
        add("RX", None);
        add("VFO", None);
        if self.state.sub_rx_enabled {
            add("SUB", None);
        }
        if tx_capable {
            add("TX", None);
        }
        add("DISP", None);
        add("SYS", None);
        w
    }

    /// The compact control strip: PTT under a thumb, and one menu chip per
    /// control box the layout gave up.
    ///
    /// `band_mode_shown` says whether the frequency box could afford the
    /// band/mode chip; when it could not, this row carries it instead.
    fn menu_bar(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        tier: crate::layout::Tier,
        band_mode_shown: bool,
    ) {
        let tx_capable = self.caps.as_ref().is_some_and(|c| c.is_transmit_capable());
        if !band_mode_shown {
            self.band_mode_button(ui, cmds);
        }
        if tx_capable {
            self.held_ptt(ui, cmds);
        }

        let btn = crate::chrome::chip(ui, false, "RX")
            .on_hover_text("Volume, gain, AGC, squelch and the noise controls");
        crate::chrome::menu_popup(ui, &btn, |ui| {
            crate::chrome::menu_caption(ui, "Receiver");
            self.rx_controls(ui, cmds, true);
        });

        let btn = crate::chrome::chip(ui, self.state.split, "VFO")
            .on_hover_text("VFO A/B, split, and the RIT/XIT offsets");
        crate::chrome::menu_popup(ui, &btn, |ui| {
            // On a phone the frequency box shows only which VFO is being tuned;
            // the selector and the other VFO's frequency live here.
            if tier == crate::layout::Tier::Phone {
                crate::chrome::menu_caption(ui, "VFO");
                let active = self.state.active_vfo;
                ui.horizontal(|ui| {
                    vfo_ab_chips(ui, active, cmds);
                    ui.label(
                        RichText::new(self.inactive_vfo_label())
                            .monospace()
                            .size(12.0)
                            .color(Color32::from_gray(120)),
                    );
                });
            }
            crate::chrome::menu_caption(ui, "Tuning");
            self.vfo_controls(ui, cmds, true);
        });

        if self.state.sub_rx_enabled {
            let btn = crate::chrome::chip(ui, true, "SUB")
                .on_hover_text("The second receiver's frequency, mode, filter and level");
            crate::chrome::menu_popup(ui, &btn, |ui| {
                crate::chrome::menu_caption(ui, "Sub receiver");
                self.sub_controls(ui, cmds, true);
            });
        }

        if tx_capable {
            let btn = crate::chrome::chip(ui, self.state.tx.tune, "TX")
                .on_hover_text("Tune, the voice keyer, and the drive and mic levels");
            crate::chrome::menu_popup(ui, &btn, |ui| {
                crate::chrome::menu_caption(ui, "Transmit");
                // PTT is on the strip already; TUNE rides with the levels it is
                // set up with.
                self.tx_controls(ui, cmds, true, false);
                if crate::chrome::chip_accent(
                    ui,
                    self.state.tx.tune,
                    RichText::new(" TUNE ").size(15.0),
                    crate::theme::YELLOW,
                    crate::theme::INK_ON_CYAN,
                )
                .clicked()
                {
                    cmds.push(Command::SetTune(!self.state.tx.tune));
                }
            });
        }

        let btn = crate::chrome::chip(ui, false, "DISP")
            .on_hover_text("Waterfall contrast, FFT size, peak hold and the skimmers");
        crate::chrome::menu_popup(ui, &btn, |ui| {
            crate::chrome::menu_caption(ui, "Display");
            self.display_controls(ui, cmds, true);
        });

        let btn = crate::chrome::chip(ui, false, "SYS")
            .on_hover_text("Logbook, spots, awards, memories, settings and the manual");
        crate::chrome::menu_popup(ui, &btn, |ui| {
            crate::chrome::menu_caption(ui, "System");
            self.windows_controls(ui, true);
        });
    }

    /// PTT on a compact layout: held rather than latched.
    ///
    /// A finger down keys the transmitter and lifting it unkeys it. A latching
    /// chip an inch from a pannable waterfall is one mis-tap away from a
    /// transmitter left on with nobody watching; held, letting go always drops
    /// it — including when the browser takes the touch away because the tab
    /// went to the background, which arrives here as the pointer simply no
    /// longer being down on the chip.
    fn held_ptt(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let resp = crate::chrome::chip_hold(
            ui,
            self.state.tx.ptt,
            RichText::new(" PTT ").size(15.0).strong(),
            crate::theme::PINK,
            Color32::WHITE,
        )
        .on_hover_text("Hold to transmit");
        let down = resp.is_pointer_button_down_on();
        // Against our own last edge, not the engine's echo: the echo lags a
        // round trip, and comparing to it would re-send the same command every
        // frame until it caught up.
        if down != self.ptt_held {
            self.ptt_held = down;
            cmds.push(Command::SetPtt(down));
        }
    }

    /// The VFO frequency controls (A/B select + big readout + the inactive
    /// VFO's frequency) in a label-less box, always the first module.
    ///
    /// On a compact layout the box gives up its side columns instead of being
    /// clipped: the A/B selector and the inactive VFO's frequency move to the
    /// VFO menu, and the digits shrink to whatever the row can actually spare.
    /// Returns whether the band/mode chip found room here — when it did not,
    /// the menu row shows it instead, so it is never simply lost.
    fn freq_module(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        tier: crate::layout::Tier,
    ) -> bool {
        let fit = ReadoutFit::measure(ui);
        if tier == crate::layout::Tier::Phone {
            return self.freq_module_compact(ui, cmds, &fit);
        }

        // Wide enough for the full box: size the digits to their design size,
        // dropping only as far as the row makes necessary. The S-meter is the
        // one thing that has to stay on this row with them — a tablet in
        // portrait has just enough width for both if the digits give a little.
        //
        // Both side columns are measured rather than assumed. Their design
        // widths hold for a desktop, but a touched layout pads every chip out
        // past them — and a column reserved too narrow does not clip, it
        // overflows the box, which is what would push the meter onto a row of
        // its own on a tablet in portrait.
        let ab_w = AB_W.max(2.0 * chip_w(ui, "A", Some(15.0)) + 6.0);
        let right_w = RIGHT_W
            .max(chip_w(ui, &self.band_mode_label(), Some(BAND_MODE_TEXT)))
            .max(text_w(ui, &self.inactive_vfo_label(), egui::FontId::monospace(12.0)));
        let overhead = ab_w + right_w + 38.0; // side columns, gaps, box margins
        // A few points of slack, so a readout that comes out exactly the width
        // of the space left does not round its way onto the next row.
        let beside = SMETER_W + 8.0 + 4.0;
        let size =
            fit.fit(ui.available_width() - overhead - beside).clamp(MIN_DIGIT, tier.digit_cap());
        let (readout_w, readout_h) = (fit.width(size), fit.height(size));
        // The box still hugs its contents: that keeps the right column against
        // the box edge (no empty space) and lets the readout be centred
        // vertically by exact geometry rather than a fragile layout hint.
        let box_w = 8.0 + ab_w + 10.0 + readout_w + 12.0 + right_w + 8.0;

        crate::chrome::module_bare_h(ui, box_w, crate::chrome::MODULE_TALL_H, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0; // control every gap explicitly
            let active = self.state.active_vfo;
            let full_h = ui.available_height();

            // VFO A/B selector, vertically centred in the full box height.
            ui.allocate_ui_with_layout(
                egui::vec2(ab_w, full_h),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    vfo_ab_chips(ui, active, cmds);
                },
            );
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
                        size,
                    );
                },
            );
            if let Some(hz) = new_hz {
                cmds.push(Command::SetVfo { vfo: active, hz });
            }
            ui.add_space(12.0);

            // Right column: inactive VFO frequency anchored top-right, band/mode
            // selector anchored bottom-right, hard against the box edge.
            let inactive = self.inactive_vfo_label();
            ui.allocate_ui_with_layout(
                egui::vec2(right_w, full_h),
                egui::Layout::top_down(egui::Align::Max),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(
                        RichText::new(inactive)
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
        true
    }

    /// The frequency box for a phone: which VFO is being tuned, the digits, and
    /// the band/mode chip if it fits.
    ///
    /// The A/B chips and the inactive VFO's frequency are in the VFO menu
    /// instead. At this width they cost more than the digits can spare, and a
    /// readout too small to read is worse than a selector one tap away.
    fn freq_module_compact(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        fit: &ReadoutFit,
    ) -> bool {
        let active = self.state.active_vfo;
        let tag = match active {
            Vfo::A => "A",
            Vfo::B => "B",
        };
        let tag_w = ui
            .painter()
            .layout_no_wrap(tag.to_owned(), egui::FontId::proportional(13.0), Color32::WHITE)
            .size()
            .x;

        // What the band/mode chip would cost, measured rather than guessed: its
        // label runs from "20m · USB" to "160m · DIGU" and the chip's padding
        // grows with the touch style.
        let bm_label = self.band_mode_label();
        let bm_w = ui
            .painter()
            .layout_no_wrap(bm_label, egui::FontId::proportional(BAND_MODE_TEXT), Color32::WHITE)
            .size()
            .x
            + 2.0 * (ui.spacing().button_padding.x + 2.0);

        let fixed = 16.0 + tag_w + 6.0; // box margins + the VFO tag
        let avail = ui.available_width();
        // Try to keep the chip; give it up only when keeping it would push the
        // digits below the size at which they stop reading as a dial.
        let with_chip = fit.fit(avail - fixed - 8.0 - bm_w).min(PHONE_DIGIT_MAX);
        let band_mode = with_chip >= MIN_DIGIT;
        let size = if band_mode {
            with_chip
        } else {
            fit.fit(avail - fixed).clamp(MIN_DIGIT, PHONE_DIGIT_MAX)
        };

        let box_w = fixed + fit.width(size) + if band_mode { 8.0 + bm_w } else { 0.0 };
        crate::chrome::module_bare_h(ui, box_w, PHONE_FREQ_H, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0; // control every gap explicitly
            ui.label(RichText::new(tag).size(13.0).strong().color(crate::theme::CYAN));
            ui.add_space(6.0);
            let new_hz = freq_display::show(
                ui,
                egui::Id::new("main-freq"),
                self.state.active_freq_hz(),
                self.input.cfg.wheel,
                size,
            );
            if let Some(hz) = new_hz {
                cmds.push(Command::SetVfo { vfo: active, hz });
            }
            if band_mode {
                ui.add_space(8.0);
                self.band_mode_button(ui, cmds);
            }
        });
        band_mode
    }

    /// The band/mode chip's label, e.g. `20m · USB`.
    fn band_mode_label(&self) -> String {
        format!("{} · {}", self.state.band.label(), self.state.rx[0].mode.label())
    }

    /// The VFO that is *not* being tuned, as a MHz label.
    fn inactive_vfo_label(&self) -> String {
        let hz = match self.state.active_vfo {
            Vfo::A => self.state.vfo_b_hz,
            Vfo::B => self.state.vfo_a_hz,
        };
        format!("{hz:.6} MHz", hz = hz / 1e6)
    }

    /// The S-meter in a label-less box, always pinned top-right. Clicking it
    /// cycles the needle / bar / trace faces.
    fn smeter_module(&mut self, ui: &mut egui::Ui, tier: crate::layout::Tier, beside: f32) {
        // The meter lays itself out against its box height, so a shorter box is
        // simply a smaller instrument, and it is the one thing in the compact
        // strip that will take any width at all. On a phone it is therefore
        // what gives: `beside` is what the menu chips need, and the meter takes
        // the rest of the row so none of them wraps onto a row of its own.
        //
        // Bounded at both ends. Too narrow and the scale has nothing left to be
        // read on; too wide and the needle's arc — a chord across the box, so
        // its radius and its height both follow the width — would reach below
        // the box. The operator's choice of face is never touched.
        let (w, h) = match tier {
            crate::layout::Tier::Phone => {
                // Measured against the room left on *this* row where the meter
                // and the chips both still fit in it — in landscape the
                // frequency box has already taken part of it — and against a
                // fresh row where they do not, because that is where the
                // wrapping layout is about to put them.
                // Not `available_width()`: in a wrapping layout that reports
                // the width of the row this item would wrap *onto*, which is
                // the full row however much of the current one is already
                // spoken for. The cursor is what knows.
                let row = ui.max_rect().width();
                let left = row - (ui.cursor().min.x - ui.max_rect().min.x).max(0.0);
                let space = if left >= PHONE_SMETER_MIN_W + beside { left } else { row };
                // A few points of slack: a width that comes out exactly equal
                // to the space left rounds the wrong way often enough, and the
                // cost of being wrong is the last chip on a row of its own.
                let w = (space - beside - 6.0).clamp(PHONE_SMETER_MIN_W, PHONE_SMETER_MAX_W);
                (w, PHONE_SMETER_H)
            }
            _ => (SMETER_W, crate::chrome::MODULE_TALL_H),
        };
        // A box this shape has no room for an arc, so the needle is skipped —
        // both in what is drawn and in what a click cycles to. The persisted
        // choice is left alone: the desktop it was made on still honours it.
        let compact = tier == crate::layout::Tier::Phone;
        let style = self.view.smeter_style;
        let shown = if compact { style.compact() } else { style };
        crate::chrome::module_bare_flush_h(ui, w, h, |ui| {
            let resp = smeter::show(ui, self.meters.as_ref(), shown).on_hover_text(if compact {
                "Click to cycle meter face: bar / trace"
            } else {
                "Click to cycle meter face: needle / bar / trace"
            });
            if resp.clicked() {
                self.view.smeter_style = if compact { style.next_compact() } else { style.next() };
            }
        });
    }

    /// Combined VFO + RIT/XIT box: the VFO A/B utility chips on top, with the
    /// RIT/XIT tuning-offset controls stacked underneath. Bare and tall — this
    /// replaces the separate VFO and RIT/XIT boxes.
    fn vfo_rit_module(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        crate::chrome::module_bare_h(ui, 270.0, crate::chrome::MODULE_TALL_H, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                self.vfo_controls(ui, cmds, false);
            });
        });
    }

    /// The VFO utility chips and the RIT/XIT offsets — the body of the VFO box,
    /// and of the VFO menu. See [`crate::chrome::control_row`] for `narrow`.
    fn vfo_controls(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>, narrow: bool) {
        let tx_capable = self.caps.as_ref().is_some_and(|c| c.is_transmit_capable());
        // Wide enough for a signed 4-digit offset plus " Hz", and tall enough
        // to hit with a finger where the layout expects one.
        let hz_field = if narrow {
            egui::vec2(96.0, ui.spacing().interact_size.y.max(22.0))
        } else {
            egui::vec2(74.0, 22.0)
        };
        // VFO utility chips.
        crate::chrome::control_row(ui, narrow, |ui| {
            if crate::chrome::chip(ui, false, "A↔B").on_hover_text("Swap VFOs").clicked() {
                cmds.push(Command::SwapVfos);
            }
            if crate::chrome::chip(ui, false, "A→B").on_hover_text("Copy A to B").clicked() {
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
        crate::chrome::control_row(ui, narrow, |ui| {
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
                        DragValue::new(&mut xit_hz).speed(5).range(-9999..=9999).suffix(" Hz"),
                    )
                    .changed()
                {
                    cmds.push(Command::SetXit { enabled: xit.enabled, hz: xit_hz });
                }
            }
        });
    }

    /// The band/mode selector button plus the floating popup with the band +
    /// mode + digital button rows.
    fn band_mode_button(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let mode = self.state.rx[0].mode;
        let btn = crate::chrome::chip(
            ui,
            false,
            RichText::new(self.band_mode_label()).size(BAND_MODE_TEXT),
        );

        let popup_id = egui::Popup::default_response_id(&btn);
        let now = ui.input(|i| i.time);
        let alpha =
            crate::chrome::popup_fade_alpha(ui.ctx(), popup_id, now, &mut self.mode_popup_since);
        let resp = egui::Popup::from_toggle_button_response(&btn)
            .frame(crate::chrome::window_frame_alpha(alpha))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_opacity(alpha);
                ui.set_max_width(crate::layout::window_w(ui.ctx(), 430.0));
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
        let width = if self.rx_gain().is_some() { 506.0 } else { 356.0 };
        crate::chrome::module_bare_h(ui, width, crate::chrome::MODULE_TALL_H, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                self.rx_controls(ui, cmds, false);
            });
        });
    }

    /// The device's front-end RX gain, if it has one the software can set — the
    /// Hermes-Lite 2's LNA, a SoapySDR device's first RX stage. A rig with none
    /// (a CAT radio on a sound card) gets no slider and no extra module width,
    /// so nothing moves for the people who can't use it.
    fn rx_gains(&self) -> Vec<GainElement> {
        self.caps
            .as_ref()
            .map(|c| c.gains.iter().filter(|g| g.direction == Direction::Rx).cloned().collect())
            .unwrap_or_default()
    }

    fn rx_gain(&self) -> Option<GainElement> {
        self.rx_gains().first().cloned()
    }

    /// The receiver and filter/noise controls — the body of the RX box, and of
    /// the RX menu. See [`crate::chrome::control_row`] for `narrow`.
    fn rx_controls(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>, narrow: bool) {
        let rx_gains = self.rx_gains();
        let rx_gain = rx_gains.first().cloned();
        // Receiver: volume, RF gain, AGC, mute.
        crate::chrome::control_row(ui, narrow, |ui| {
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
                // and the module has to stay inside one wrapped row. In
                // a menu the column is the constraint instead, and
                // `control_row` has already sized the rail to it.
                let resp = ui
                    .scope(|ui| {
                        if !narrow {
                            ui.spacing_mut().slider_width = 76.0;
                        }
                        crate::chrome::slider(
                            ui,
                            Slider::new(&mut db, g.min_db..=g.max_db).step_by(step).suffix(" dB"),
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
                    cmds.push(Command::SetGain { dir: Direction::Rx, element: g.name.clone(), db });
                }
            }
            // A cycling chip rather than a combo: a combo inside a menu
            // opens a second popup layer, and clicking it counts as
            // "outside" and closes the menu it was opened from. Four
            // settings is few enough to walk through anyway.
            let agc = self.state.rx[0].agc;
            if crate::chrome::chip(ui, agc != AgcMode::Off, format!("AGC {}", agc.label()))
                .on_hover_text("AGC hang time — click to cycle: Off / Slow / Med / Fast")
                .clicked()
            {
                cmds.push(Command::SetAgc { rx: RxId::Main, agc: agc.next() });
            }
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
        crate::chrome::control_row(ui, narrow, |ui| {
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
            if crate::chrome::chip(ui, nb, "NB").on_hover_text("Impulse noise blanker").clicked() {
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
            let nr_label = if nr.is_on() { format!("NR {}", nr.label()) } else { "NR".to_string() };
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
                if crate::chrome::chip(ui, want && locked, "ST").on_hover_text(hover).clicked() {
                    self.state.rx[0].wfm_stereo = !want; // optimistic echo
                    cmds.push(Command::SetWfmStereo { rx: RxId::Main, on: !want });
                }
            }
        });
    }

    /// The sub receiver's own controls, shown only while it is running. The sub
    /// has a frequency, a mode and a filter of its own — none of which the main
    /// receiver's controls can reach — so without this module it is a second
    /// receiver that can only be switched on and off.
    fn sub_rx_module(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        crate::chrome::module_bare_h(ui, 404.0, crate::chrome::MODULE_TALL_H, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                self.sub_controls(ui, cmds, false);
            });
        });
    }

    /// The sub receiver's controls — the body of the SUB box, and of the SUB
    /// menu. See [`crate::chrome::control_row`] for `narrow`.
    fn sub_controls(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>, narrow: bool) {
        // The sub tunes anywhere inside the device passband and nowhere outside
        // it: both receivers are DDCs on the same IQ stream.
        let half = self.state.sample_rate / 2.0;
        let (dev_lo, dev_hi) = (self.state.center_hz - half, self.state.center_hz + half);
        // Field height, and the height every row is told to be. egui sizes a
        // horizontal row from `interact_size.y` and then grows it as taller
        // widgets land in it — which drops everything added after the first
        // chip a few pixels below everything added before it. Starting the row
        // at the height its tallest widget will be leaves nothing to grow.
        // A touched layout has already raised `interact_size` to a fingertip;
        // don't shrink it back down.
        let field_h = ui.spacing().interact_size.y.max(22.0);
        ui.spacing_mut().interact_size.y = field_h;
        // Frequency, mode, and the two moves worth a single click:
        // send the sub to the dial, or bring the dial to the sub.
        crate::chrome::control_row(ui, narrow, |ui| {
            ui.label(
                RichText::new("SUB")
                    .color(crate::widgets::spectrum_view::SUB_COLOR)
                    .size(11.0)
                    .strong(),
            );
            let mut hz = self.state.sub_rx_hz;
            let resp = ui
                .add_sized(
                    [116.0, field_h],
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
            if let Some(m) = sub_mode_picker(ui, self.state.rx[1].mode, narrow) {
                cmds.push(Command::SetMode { rx: RxId::Sub, mode: m });
            }
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
                cmds.push(Command::SetVfo { vfo: self.state.active_vfo, hz: self.state.sub_rx_hz });
            }
        });
        // Filter, level, mute.
        crate::chrome::control_row(ui, narrow, |ui| {
            let rx1 = self.state.rx[1];
            let max = rx1.mode.max_filter_hz();
            ui.label("Filter").on_hover_text("Sub receiver passband edges, in Hz");
            let mut lo = rx1.filter_lo;
            let mut hi = rx1.filter_hi;
            let changed = ui
                .add_sized([70.0, field_h], DragValue::new(&mut lo).speed(10).range(-max..=max))
                .changed()
                | ui.add_sized(
                    [70.0, field_h],
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
                    crate::chrome::slider(ui, Slider::new(&mut vol, 0.0..=1.0).show_value(false))
                })
                .inner
                .changed()
            {
                self.state.rx[1].volume = vol; // optimistic echo
                cmds.push(Command::SetVolume { rx: RxId::Sub, v: vol });
            }
            if crate::chrome::chip_accent(ui, rx1.muted, "MUTE", crate::theme::PINK, Color32::WHITE)
                .clicked()
            {
                cmds.push(Command::SetMute { rx: RxId::Sub, muted: !rx1.muted });
            }
        });
    }

    fn tx_module(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        // The voice keyer's button only appears where the keyer can transmit —
        // every voice mode plus RADE, which takes a message as its microphone.
        let keyer_ok = self.state.rx[0].mode.allows_voice_keyer();
        let width = if keyer_ok { 520.0 } else { 470.0 };
        crate::chrome::module(ui, "Transmit", width, |ui| {
            self.tx_controls(ui, cmds, false, true);
        });
    }

    /// The transmit controls — the body of the TX box, and of the TX menu.
    ///
    /// `ptt` draws the PTT and TUNE buttons here. A compact layout keys the
    /// transmitter from the menu row instead, where it is one tap away rather
    /// than two: burying push-to-talk in a menu is not a thing to do to an
    /// operator. See [`crate::chrome::control_row`] for `narrow`.
    fn tx_controls(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>, narrow: bool, ptt: bool) {
        crate::chrome::control_row(ui, narrow, |ui| {
            let tx = self.state.tx;
            if ptt {
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
            }
            if self.state.rx[0].mode.allows_voice_keyer() {
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
                crate::chrome::menu_caption(ui, "Skimmers");
                self.skimmer_controls(ui, cmds);
            });
        if let Some(r) = &resp {
            crate::chrome::paint_popup_cut_border(ui.ctx(), &r.response, alpha);
            if r.response.contains_pointer() {
                self.skimmer_popup_since = Some(now); // keep it up while the pointer is on it
            }
        }
    }

    /// One row per skimmer kind: an on/off chip plus that skimmer's squelch.
    ///
    /// Its own function because a menu has to inline this rather than open it
    /// as a popup — a popup opened from a popup counts as a click outside the
    /// first, which closes the menu out from under the control being reached
    /// for.
    fn skimmer_controls(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        // A CAT rig feeding demodulated audio has no IQ span to skim; the engine
        // forces the skimmers off there, so the rows are shown disabled.
        let wideband = self.caps.as_ref().is_none_or(|c| !c.audio_mode);
        {
            {
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
            self.display_controls(ui, cmds, false);
        });
    }

    /// The display controls — the body of the Display box, and of the DISP
    /// menu. See [`crate::chrome::control_row`] for `narrow`.
    fn display_controls(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>, narrow: bool) {
        // A phone draws the waterfall alone, so the two chips that choose what
        // else is drawn have nothing to control there.
        let has_wide = self.wide_frame.is_some();
        let picks_layers = !crate::layout::tier(ui.ctx()).waterfall_only();
        crate::chrome::control_row(ui, narrow, |ui| {
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
            if picks_layers
                && crate::chrome::chip(ui, !self.view.spectrum_collapsed, "SPEC")
                    .on_hover_text("Show/hide the spectrum line above the waterfall")
                    .clicked()
            {
                self.view.spectrum_collapsed = !self.view.spectrum_collapsed;
            }
            if picks_layers
                && has_wide
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
            self.solar_button(ui);
            // In a box these two hang off chips of their own. A menu inlines
            // them below instead: a popup opened from a popup counts as a click
            // outside the first and closes it.
            if narrow {
                return;
            }
            self.skimmer_button(ui, cmds);
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
                    self.spectrum_controls(ui);
                });
            if let Some(r) = &fft_resp {
                crate::chrome::paint_popup_cut_border(ui.ctx(), &r.response, alpha);
                if r.response.contains_pointer() {
                    self.fft_popup_since = Some(now);
                }
            }
        });
        if narrow {
            crate::chrome::menu_caption(ui, "Skimmers");
            self.skimmer_controls(ui, cmds);
            self.spectrum_controls(ui);
        }
    }

    /// Spectrum floor/ceiling, FFT size and the waterfall's scroll direction.
    /// Inlined by the DISP menu, behind the FFT chip in the Display box — see
    /// [`Self::skimmer_controls`] for why a menu cannot use the popup.
    fn spectrum_controls(&mut self, ui: &mut egui::Ui) {
        crate::chrome::menu_caption(ui, "Spectrum");
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
        crate::chrome::menu_caption(ui, "FFT size");
        ui.horizontal_wrapped(|ui| {
            for n in [2048u32, 4096, 8192, 16384, 32768] {
                if crate::chrome::chip(ui, self.view.fft_size == n, format!("{n}")).clicked() {
                    self.view.fft_size = n;
                }
            }
        });
        crate::chrome::menu_caption(ui, "Waterfall");
        if crate::chrome::chip(ui, self.view.waterfall_flip, "FLIP")
            .on_hover_text("Scroll the waterfall upwards — newest row at the bottom (V)")
            .clicked()
        {
            self.view.waterfall_flip = !self.view.waterfall_flip;
        }
    }

    fn windows_module(&mut self, ui: &mut egui::Ui) {
        crate::chrome::module(ui, "System", 285.0, |ui| {
            self.windows_controls(ui, false);
        });
    }

    /// The window buttons — the body of the System box, and of the SYS menu.
    /// See [`crate::chrome::control_row`] for `narrow`.
    fn windows_controls(&mut self, ui: &mut egui::Ui, narrow: bool) {
        crate::chrome::control_row(ui, narrow, |ui| {
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

/// How wide `text` lays out in `font`.
fn text_w(ui: &egui::Ui, text: &str, font: egui::FontId) -> f32 {
    ui.painter().layout_no_wrap(text.to_owned(), font, Color32::WHITE).size().x
}

/// What a chip carrying `label` will measure, padding included. `size` is the
/// text size where the chip sets one, or `None` for the button style.
///
/// The same arithmetic [`crate::chrome::chip`] does, so a caller can budget a
/// row before drawing it. Kept here rather than in `chrome` because it is the
/// layout that needs to know, not the chrome.
fn chip_w(ui: &egui::Ui, label: &str, size: Option<f32>) -> f32 {
    let font = match size {
        Some(pt) => egui::FontId::proportional(pt),
        None => egui::TextStyle::Button.resolve(ui.style()),
    };
    text_w(ui, label, font) + 2.0 * (ui.spacing().button_padding.x + 2.0)
}

/// The VFO A/B selector chips. In the frequency box on a desktop, in the VFO
/// menu on a phone — one definition either way.
fn vfo_ab_chips(ui: &mut egui::Ui, active: Vfo, cmds: &mut Vec<Command>) {
    for (v, label) in [(Vfo::A, "A"), (Vfo::B, "B")] {
        if crate::chrome::chip(ui, active == v, RichText::new(label).size(15.0)).clicked() {
            cmds.push(Command::SelectVfo(v));
        }
    }
}

/// The sub receiver's mode, picked from the audio modes it can wear. Returns
/// the newly chosen mode, or `None` if the current one still stands.
///
/// Audio modes only. The digital modes are wired to the main receiver alone
/// (one decoder, one TX), and SPEC produces no audio at all — a sub receiver
/// you cannot hear is a trap, not a setting.
///
/// A combo where a fixed-width row has to hold ten of them; a wrapped row of
/// chips in a menu, where a combo would open a second popup layer and clicking
/// it would count as "outside" and close the menu underneath it. One list
/// either way, so the two cannot come to disagree about what the sub can do.
fn sub_mode_picker(ui: &mut egui::Ui, cur: Mode, narrow: bool) -> Option<Mode> {
    const MODES: [Mode; 10] = [
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
    ];
    let mut picked = None;
    if narrow {
        ui.horizontal_wrapped(|ui| {
            for m in MODES {
                if crate::chrome::chip(ui, cur == m, m.label()).clicked() {
                    picked = Some(m);
                }
            }
        });
    } else {
        ComboBox::from_id_salt("sub-mode").selected_text(cur.label()).width(74.0).show_ui(
            ui,
            |ui| {
                for m in MODES {
                    if ui.selectable_label(cur == m, m.label()).clicked() {
                        picked = Some(m);
                    }
                }
            },
        );
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Share Tech Mono advances 0.540 em and Chakra's " Hz" 1.392 em, so the
    /// readout costs 7.438 pt of width per point of digit size. Measured from
    /// the shipped fonts; the assertions below are what that buys at each of
    /// the viewport widths the tiers were drawn for.
    fn shipped() -> ReadoutFit {
        ReadoutFit { per_pt: 10.0 * 0.540 + 3.0 * 0.540 + 0.3 * 1.392, h_per_pt: 1.0 }
    }

    #[test]
    fn fitting_a_size_and_measuring_it_back_agree() {
        let f = shipped();
        for size in [22.0f32, 25.0, 30.0, 34.0, 40.0] {
            let round_trip = f.fit(f.width(size));
            assert!((round_trip - size).abs() < 1e-3, "fit(width({size})) came back {round_trip}");
        }
    }

    #[test]
    fn the_desktop_readout_keeps_its_design_width() {
        // The box the desktop has always drawn: 512.5 pt wide overall.
        let f = shipped();
        let readout = f.width(crate::widgets::freq_display::DIGIT_SIZE);
        assert!((readout - 310.5).abs() < 0.5, "readout measured {readout}");
        let box_w = 8.0 + AB_W + 10.0 + readout + 12.0 + RIGHT_W + 8.0;
        assert!((box_w - 512.5).abs() < 0.5, "box measured {box_w}");
    }

    /// "20m \u{b7} USB" at 14 pt Chakra with a touched layout's chip padding.
    /// `freq_module` measures this rather than assuming [`RIGHT_W`], because it
    /// runs past that column's design width — which is what used to push the
    /// S-meter onto a row of its own on a tablet in portrait.
    const TOUCH_BAND_CHIP_W: f32 = 101.0;

    /// The digit size `freq_module` settles on for a `viewport` this wide, and
    /// the width of the box it builds around it.
    fn tablet_box(f: &ReadoutFit, viewport: f32) -> (f32, f32) {
        // Less the top panel's 8+8 and angled_frame's 10+10.
        let content = viewport - 36.0;
        let right_w = RIGHT_W.max(TOUCH_BAND_CHIP_W);
        let overhead = AB_W + right_w + 38.0;
        let size = f.fit(content - overhead - (SMETER_W + 12.0)).clamp(MIN_DIGIT, 40.0);
        (size, 8.0 + AB_W + 10.0 + f.width(size) + 12.0 + right_w + 8.0)
    }

    #[test]
    fn a_tablet_in_portrait_fits_the_readout_beside_the_smeter() {
        let f = shipped();
        let content = 768.0 - 36.0;
        let (size, box_w) = tablet_box(&f, 768.0);
        assert!((MIN_DIGIT..=40.0).contains(&size), "size {size} left its range");
        // Both boxes and the gap between them stay inside the row.
        assert!(box_w + 8.0 + SMETER_W <= content, "{box_w} + meter overflowed {content}");
    }

    /// The needle's radius is set by the box *width* (its arc is a chord
    /// across it), so the arc gets taller as the box gets wider. A meter
    /// stretched across a phone would draw the ends of its scale below its own
    /// box; holding the aspect is what stops that.
    #[test]
    fn the_phone_smeter_keeps_its_scale_inside_its_box() {
        // The arc hangs from `13·k` below the top of the box (smeter.rs).
        let k = (PHONE_SMETER_H / 72.0).clamp(0.55, 2.0);
        let room = PHONE_SMETER_H - 13.0 * k;
        let half: f32 = 31.0_f32.to_radians();
        for w in [PHONE_SMETER_MIN_W, 150.0, PHONE_SMETER_MAX_W] {
            let rad = ((w - 14.0) / (2.0 * half.sin())).max(24.0);
            // How far the arc drops from its ends to its centre.
            let extent = rad * (1.0 - half.cos());
            assert!(extent <= room, "a {w} pt wide arc spans {extent} of {room} pt");
        }
    }

    #[test]
    fn a_landscape_tablet_gets_the_full_size_digits() {
        let f = shipped();
        let (size, box_w) = tablet_box(&f, 1024.0);
        assert_eq!(size, 40.0, "a 1024 pt tablet has room for the design size");
        assert!(box_w + 8.0 + SMETER_W <= 1024.0 - 36.0, "{box_w} + meter overflowed");
    }
}
