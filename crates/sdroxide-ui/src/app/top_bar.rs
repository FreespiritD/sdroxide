//! The control strip along the top of the window.
//!
//! One method per module in the strip, all called from [`SdroxideApp::top_bar`]
//! in the order they appear: frequency, S-meter, VFO/RIT, band and mode,
//! RX filter, sub-RX, TX, the skimmer and display popups, and the window
//! buttons. Each pushes [`Command`]s rather than touching the controller, so
//! the whole strip is a pure function of the state it draws from.
//!
//! Only the desktop draws that full strip. A tablet keeps the readout and the
//! S-meter at full size and folds everything else into menus; a phone stacks
//! a compact readout, the meter and one row of menu chips. And a tablet-tier
//! window too *short* for even two stacked rows — a 1280x720 panel — gets the
//! single-row strip: the VFO box (a type-in readout over an S-meter bar)
//! beside a thumb-sized PTT and two rows of menu buttons stretched to the
//! edge of the screen.

use eframe::egui::{self, Color32, ComboBox, DragValue, RichText, Slider};
use sdroxide_types::{
    AgcMode, Band, Command, CwSkimmerDecoder, DeviceCaps, Direction, GainElement, Mode, NrEngine,
    NrLevel, NrStrength, RadioState, RxId, SkimmerKind, SubTone, Vfo,
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
/// Digit sizes of the single-row strip's readout. It is a type-in field — tap
/// and type, no per-digit targets — so the floor is about reading the
/// frequency, not hitting one digit of it.
const STRIP_DIGIT_MIN: f32 = 18.0;
const STRIP_DIGIT_MAX: f32 = 30.0;
/// Vertical gap between the strip's two button rows — and so, with two chip
/// heights, what sets the height of everything on the strip.
const STRIP_ROW_GAP: f32 = 6.0;
/// Text size of the strip's PTT label. The chip around it is padded wider
/// than any grid button and stands the strip's full height, because it is the
/// one control worth a whole thumb.
const STRIP_PTT_TEXT: f32 = 17.0;

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

/// The geometry of the short-screen single-row strip, planned before anything
/// is drawn.
///
/// Everything on the strip shares one height — two grid rows of chips — and
/// the width splits three ways: the VFO box hugs its readout, the PTT hugs its
/// label, and the button grid stretches over whatever is left, which is what
/// makes the buttons scale with the screen.
struct ShortStrip {
    /// Digit size the readout gets.
    digit: f32,
    /// The VFO box, outer width.
    box_w: f32,
    /// The one shared height: two chip rows and the gap between them.
    box_h: f32,
    /// The button grid's width, and the uniform cell width of each of its rows.
    grid_w: f32,
    cell1_w: f32,
    cell2_w: f32,
}

/// The measured widths and heights [`plan_short_strip`] works from — taken
/// off the live style when the strip is drawn, and given as plain numbers by
/// the layout tests.
struct StripChips {
    /// A grid chip's height.
    chip_h: f32,
    /// The active-VFO tag's width.
    tag_w: f32,
    /// The band/mode chip's width at its current label.
    bm_w: f32,
    /// The PTT's width; 0 for a rig that cannot transmit.
    ptt_w: f32,
    /// Each grid row: its cell count, and the width its widest label's chip
    /// measures on its own.
    row1: (usize, f32),
    row2: (usize, f32),
}

/// Plan the strip for `avail` points of row. A free function of measured
/// numbers so the arithmetic can be tested without an app around it. `gap`
/// separates the strip's three blocks, `cell_gap` the grid's cells.
fn plan_short_strip(
    avail: f32,
    fit: &ReadoutFit,
    c: &StripChips,
    gap: f32,
    cell_gap: f32,
) -> ShortStrip {
    let box_h = 2.0 * c.chip_h + STRIP_ROW_GAP;
    // Equal cells sized by the row's widest label, so stretching the rows to
    // the same width never squeezes one chip below its own text.
    let row_min = |(n, w): (usize, f32)| n as f32 * w + (n - 1) as f32 * cell_gap;
    let grid_min = row_min(c.row1).max(row_min(c.row2));
    let gaps = if c.ptt_w > 0.0 { 2.0 } else { 1.0 } * gap;
    // The digits get whatever the PTT and the grid at its minimum leave over —
    // box margins, the VFO tag and its gap already spoken for — with a few
    // points of slack so rounding never wraps the row.
    let overhead = 16.0 + c.tag_w + 6.0;
    let digit = fit
        .fit(avail - c.ptt_w - grid_min - gaps - overhead - 4.0)
        .clamp(STRIP_DIGIT_MIN, STRIP_DIGIT_MAX);
    // The box hugs the wider of its rows: the readout above, or the band/mode
    // chip plus the meter at its narrowest below.
    let inner = (c.tag_w + 6.0 + fit.width(digit)).max(c.bm_w + 6.0 + PHONE_SMETER_MIN_W);
    let box_w = inner + 16.0;
    // The buttons take every point the box and the PTT left on the row.
    let grid_w = (avail - box_w - c.ptt_w - gaps - 4.0).max(grid_min);
    let cell = |(n, _): (usize, f32)| (grid_w - (n - 1) as f32 * cell_gap) / n as f32;
    ShortStrip { digit, box_w, box_h, grid_w, cell1_w: cell(c.row1), cell2_w: cell(c.row2) }
}

impl SdroxideApp {
    pub(in crate::app) fn top_bar(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        // All controls are captioned (or bare) modules that reflow when the
        // window is narrow. The frequency box is always first, the S-meter
        // second; the rest follow and wrap to further rows.
        let tier = crate::layout::tier(ui.ctx());
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true), |ui| {
            // A tablet-tier window too *short* for even the two stacked
            // compact rows — a 1280x720 panel — gets the single-row strip:
            // everything beside everything, and the height goes to the
            // waterfall. Taller tablet windows keep the stacked layout below,
            // with its full-size readout.
            if crate::layout::short_tablet(ui.ctx()) {
                self.short_strip(ui, cmds);
                return;
            }
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
        let mut add =
            |label: &str, size: Option<f32>| w += crate::chrome::chip_width(ui, label, size) + gap;
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

    /// The strip a short tablet-tier window wears: everything on one row.
    ///
    /// The VFO box — a type-in readout over an S-meter bar — then PTT at
    /// thumb size, then the menu buttons in two rows (RX and VFO above; TX,
    /// DISP and SYS below) stretched to the edge of the screen. On a screen
    /// under [`crate::layout::SHORT_H`] it stands in for the stacked tablet
    /// rows — a full-width frequency box above the meter and the menu chips —
    /// which would cost a 720 pt screen a quarter of its height before the
    /// waterfall got any.
    fn short_strip(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let tx_capable = self.caps.as_ref().is_some_and(|c| c.is_transmit_capable());
        let sub = self.state.sub_rx_enabled;
        let gap = ui.spacing().item_spacing.x;
        let chip_h = crate::chrome::chip_height(ui, None);
        let fit = ReadoutFit::measure(ui);

        let active = self.state.active_vfo;
        let tag = match active {
            Vfo::A => "A",
            Vfo::B => "B",
        };
        let tag_w = crate::chrome::text_width(ui, tag, egui::FontId::proportional(13.0));
        let bm_w = crate::chrome::chip_width(ui, &self.band_mode_label(), Some(BAND_MODE_TEXT));
        let ptt_w = if tx_capable {
            crate::chrome::chip_width(ui, "PTT", Some(STRIP_PTT_TEXT)) + 14.0
        } else {
            0.0
        };
        let widest = |labels: &[&str]| {
            labels.iter().map(|l| crate::chrome::chip_width(ui, l, None)).fold(0.0, f32::max)
        };
        let chips = StripChips {
            chip_h,
            tag_w,
            bm_w,
            ptt_w,
            row1: if sub {
                (3, widest(&["RX", "VFO", "SUB"]))
            } else {
                (2, widest(&["RX", "VFO"]))
            },
            row2: if tx_capable {
                (3, widest(&["TX", "DISP", "SYS"]))
            } else {
                (2, widest(&["DISP", "SYS"]))
            },
        };
        let plan = plan_short_strip(ui.available_width(), &fit, &chips, gap, gap);

        // The VFO box. The A/B selector and the other VFO's frequency are in
        // the VFO menu (see [`Self::vfo_menu`]); the box shows which VFO the
        // dial is, the dial itself, and — under it — the band/mode chip beside
        // the meter.
        crate::chrome::module_bare_h(ui, plan.box_w, plan.box_h, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tag).size(13.0).strong().color(crate::theme::CYAN));
                    if let Some(hz) = freq_display::show_typed(
                        ui,
                        egui::Id::new("main-freq"),
                        self.state.active_freq_hz(),
                        plan.digit,
                    ) {
                        cmds.push(Command::SetVfo { vfo: active, hz });
                    }
                });
                let meter_h = ui.available_height();
                ui.horizontal(|ui| {
                    self.band_mode_button(ui, cmds);
                    // The meter takes whatever width the chip left and
                    // whatever height the readout did. Bar or trace only — a
                    // strip this shape cannot hold the needle's arc, see
                    // [`smeter::SmeterStyle::compact`].
                    let style = self.view.smeter_style;
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), meter_h),
                        egui::Layout::left_to_right(egui::Align::Min),
                        |ui| {
                            let resp = smeter::show(ui, self.meters.as_ref(), style.compact())
                                .on_hover_text("Click to cycle meter face: bar / trace");
                            if resp.clicked() {
                                self.view.smeter_style = style.next_compact();
                            }
                        },
                    );
                });
            });
        });

        if tx_capable {
            let resp = crate::chrome::chip_hold_sized(
                ui,
                self.state.tx.ptt,
                RichText::new("PTT").size(STRIP_PTT_TEXT).strong(),
                crate::theme::PINK,
                Color32::WHITE,
                egui::vec2(ptt_w, plan.box_h),
            )
            .on_hover_text("Hold to transmit");
            self.apply_held_ptt(&resp, cmds);
        }

        // The menu buttons, stretched over the rest of the row.
        ui.allocate_ui_with_layout(
            egui::vec2(plan.grid_w, plan.box_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(gap, STRIP_ROW_GAP);
                let cell1 = egui::vec2(plan.cell1_w, chip_h);
                ui.horizontal(|ui| {
                    let btn = crate::chrome::chip_sized(ui, false, "RX", cell1);
                    self.rx_menu(ui, btn, cmds);
                    let btn = crate::chrome::chip_sized(ui, self.state.split, "VFO", cell1);
                    self.vfo_menu(ui, btn, cmds, true);
                    if sub {
                        let btn = crate::chrome::chip_sized(ui, true, "SUB", cell1);
                        self.sub_menu(ui, btn, cmds);
                    }
                });
                let cell2 = egui::vec2(plan.cell2_w, chip_h);
                ui.horizontal(|ui| {
                    if tx_capable {
                        let btn = crate::chrome::chip_sized(ui, self.state.tx.tune, "TX", cell2);
                        self.tx_menu(ui, btn, cmds);
                    }
                    let btn = crate::chrome::chip_sized(ui, false, "DISP", cell2);
                    self.disp_menu(ui, btn, cmds);
                    let btn = crate::chrome::chip_sized(ui, false, "SYS", cell2);
                    self.sys_menu(ui, btn, cmds);
                });
            },
        );
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
        let btn = crate::chrome::chip(ui, false, "RX");
        self.rx_menu(ui, btn, cmds);
        let btn = crate::chrome::chip(ui, self.state.split, "VFO");
        // The tablet's full frequency box already carries the A/B selector
        // and the other VFO's frequency; the phone box shows only a tag.
        self.vfo_menu(ui, btn, cmds, tier == crate::layout::Tier::Phone);
        if self.state.sub_rx_enabled {
            let btn = crate::chrome::chip(ui, true, "SUB");
            self.sub_menu(ui, btn, cmds);
        }
        if tx_capable {
            let btn = crate::chrome::chip(ui, self.state.tx.tune, "TX");
            self.tx_menu(ui, btn, cmds);
        }
        let btn = crate::chrome::chip(ui, false, "DISP");
        self.disp_menu(ui, btn, cmds);
        let btn = crate::chrome::chip(ui, false, "SYS");
        self.sys_menu(ui, btn, cmds);
    }

    /// The RX menu: the receiver and filter/noise controls. Takes the chip it
    /// hangs off — the phone's hugging chip or the tablet's stretched one —
    /// and dresses it with its hover text, so the two strips cannot drift.
    fn rx_menu(&mut self, ui: &mut egui::Ui, btn: egui::Response, cmds: &mut Vec<Command>) {
        let btn = btn.on_hover_text("Volume, gain, AGC, squelch and the noise controls");
        crate::chrome::menu_popup(ui, &btn, |ui| {
            crate::chrome::menu_caption(ui, "Receiver");
            self.rx_controls(ui, cmds, true);
        });
    }

    /// The VFO menu: the utility chips and the RIT/XIT offsets.
    ///
    /// With `selector`, the A/B chips and the other VFO's frequency lead it —
    /// for the layouts whose frequency box shows only which VFO is being
    /// tuned (the phone box, the short strip). The tablet's full box already
    /// carries both, and would show them twice.
    fn vfo_menu(
        &mut self,
        ui: &mut egui::Ui,
        btn: egui::Response,
        cmds: &mut Vec<Command>,
        selector: bool,
    ) {
        let btn = btn.on_hover_text("VFO A/B, split, and the RIT/XIT offsets");
        crate::chrome::menu_popup(ui, &btn, |ui| {
            if selector {
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
    }

    /// The SUB menu, shown only while the second receiver runs.
    fn sub_menu(&mut self, ui: &mut egui::Ui, btn: egui::Response, cmds: &mut Vec<Command>) {
        let btn = btn.on_hover_text("The second receiver's frequency, mode, filter and level");
        crate::chrome::menu_popup(ui, &btn, |ui| {
            crate::chrome::menu_caption(ui, "Sub receiver");
            self.sub_controls(ui, cmds, true);
        });
    }

    /// The TX menu: tune, the voice keyer, and the drive and mic levels.
    fn tx_menu(&mut self, ui: &mut egui::Ui, btn: egui::Response, cmds: &mut Vec<Command>) {
        let btn = btn.on_hover_text("Tune, the voice keyer, and the drive and mic levels");
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

    /// The DISP menu: waterfall, spectrum and skimmer controls.
    fn disp_menu(&mut self, ui: &mut egui::Ui, btn: egui::Response, cmds: &mut Vec<Command>) {
        let btn = btn.on_hover_text("Waterfall contrast, FFT size, peak hold and the skimmers");
        crate::chrome::menu_popup(ui, &btn, |ui| {
            crate::chrome::menu_caption(ui, "Display");
            self.display_controls(ui, cmds, true);
        });
    }

    /// The SYS menu: the window buttons.
    fn sys_menu(&mut self, ui: &mut egui::Ui, btn: egui::Response, _cmds: &mut Vec<Command>) {
        let btn = btn.on_hover_text(
            "Logbook, spots, awards, memories, the scanner, settings and the manual",
        );
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
        self.apply_held_ptt(&resp, cmds);
    }

    /// Key or unkey from a held PTT chip's response.
    fn apply_held_ptt(&mut self, resp: &egui::Response, cmds: &mut Vec<Command>) {
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
        let ab_w = AB_W.max(2.0 * crate::chrome::chip_width(ui, "A", Some(15.0)) + 6.0);
        let right_w = RIGHT_W
            .max(crate::chrome::chip_width(ui, &self.band_mode_label(), Some(BAND_MODE_TEXT)))
            .max(crate::chrome::text_width(
                ui,
                &self.inactive_vfo_label(),
                egui::FontId::monospace(12.0),
            ));
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
            let readout = ui.allocate_ui_with_layout(
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
            // When the VFO sits exactly on a stored memory, say which one.
            // Painted rather than laid out, so the readout never shifts as it
            // appears — anchored to the bottom of the box, not the digit row
            // (`readout.response.rect` is the *used* rect, which ends at the
            // digits), so it clears their ink instead of hugging the baseline.
            if let Some(name) = self.memory_name_at_vfo() {
                let r = readout.response.rect;
                ui.painter().text(
                    egui::pos2(r.left(), r.top() + full_h),
                    egui::Align2::LEFT_BOTTOM,
                    name,
                    egui::FontId::proportional(10.0),
                    crate::theme::CYAN_DIM,
                );
            }
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
                    // Push the band/mode chip to the bottom of the column by
                    // its measured height. A literal here would leave the
                    // column taller than the box on a touched layout, where a
                    // chip is half again as tall — and a box that outgrows
                    // `MODULE_TALL_H` no longer lines up with the S-meter.
                    let pad = (ui.available_height()
                        - crate::chrome::chip_height(ui, Some(BAND_MODE_TEXT)))
                    .max(0.0);
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
    /// readout too small to read is worse than a selector one tap away. The
    /// readout is the type-in kind: at this size a fingertip covers three
    /// digits, so per-digit tuning would be a lottery.
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
            let new_hz = freq_display::show_typed(
                ui,
                egui::Id::new("main-freq"),
                self.state.active_freq_hz(),
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

    /// The name of the stored memory channel the active VFO is parked on, if
    /// any: same frequency to the Hz the readout shows, and same mode.
    fn memory_name_at_vfo(&self) -> Option<&str> {
        let hz = self.state.active_freq_hz().round() as i64;
        let mode = self.state.rx[0].mode;
        self.memories
            .iter()
            .find(|m| m.mode == mode && m.freq_hz.round() as i64 == hz)
            .map(|m| m.name.as_str())
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

        // The same scrolled, viewport-sized popup the menu chips use. This is
        // the longest menu in the program — three sections and forty chips —
        // and it opens on every layout, so it is the one that has to be held
        // inside the screen in both directions rather than hang off it.
        let (state, caps) = (&self.state, &self.caps);
        let (conditions, daylight) = (self.band_conditions.as_ref(), self.daylight);
        crate::chrome::fading_menu_popup(ui, &btn, &mut self.mode_popup_since, |ui| {
            band_mode_menu(ui, mode, state, caps.as_ref(), conditions, daylight, cmds);
        });
    }

    /// Combined Receiver + Filter/Noise box: volume, gain and AGC on top, with
    /// the squelch + noise + mute/record chips stacked underneath. Bare and
    /// tall, like the VFO/RIT box — replaces the separate Receiver and Filter
    /// boxes.
    fn rx_filter_module(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        // Two rows in one box, so it is sized for the wider of them, each
        // figure being that row laid out at the desktop tier plus a little
        // slack. Which row leads changes with the rig and the state: the noise
        // row usually, the receive row once it carries both a front-end gain
        // rail and the manual-gain rail that appears with the AGC off.
        // The widest NR chip is now "NR DFNR High" — two characters more than
        // the "NR AI High" this was sized for.
        let noise_row: f32 = 462.0
            + match self.state.rx[0].mode {
                Mode::Wfm => 40.0,
                // The tone chip reads "D023N" at its widest, plus the dot that
                // marks an armed-but-silent squelch.
                Mode::Nfm => 66.0,
                _ => 0.0,
            };
        let rx_row = 205.0
            + if self.rx_gain().is_some() { 180.0 } else { 0.0 }
            + if self.state.rx[0].agc == AgcMode::Off { 170.0 } else { 0.0 };
        let width = noise_row.max(rx_row) + 16.0;
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
        // Receiver: volume, RF gain, AGC and the manual gain it falls back to.
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
            // With the AGC off the audio rides on this fixed gain instead. It
            // has to be here: unlevelled, the demodulator's output is whatever
            // the band handed it, and a weak SSB signal is tens of dB below
            // anything the volume control can reach. Switching the AGC off
            // seeds it from where the AGC had settled, so this starts in the
            // right place and only needs trimming.
            if agc == AgcMode::Off {
                let mut db = self.state.rx[0].manual_gain_db;
                ui.label("Man");
                let resp = ui
                    .scope(|ui| {
                        if !narrow {
                            ui.spacing_mut().slider_width = 76.0;
                        }
                        crate::chrome::slider(
                            ui,
                            Slider::new(&mut db, 0.0..=sdroxide_types::MAX_MANUAL_GAIN_DB)
                                .step_by(1.0)
                                .suffix(" dB"),
                        )
                    })
                    .inner
                    .on_hover_text(
                        "Manual audio gain, used while the AGC is off. Seeded from \
                         the level the AGC was holding when it was switched off.",
                    );
                if resp.changed() {
                    self.state.rx[0].manual_gain_db = db; // optimistic echo
                    cmds.push(Command::SetManualGain { rx: RxId::Main, db });
                }
            }
        });
        // Filter / Noise: squelch and the noise chips, then mute and record —
        // the two that act on the finished audio rather than on the level.
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
            // Noise reduction. The chip says what is running; the picker behind
            // it chooses which of the four engines and how hard. A cycling chip
            // was fine at seven states and two engines; at thirteen and four it
            // is a dozen clicks to cross, and which engine to use is a
            // considered choice rather than something to walk past on the way
            // to the one you wanted.
            if narrow {
                // This row is itself inside the RX menu on a compact layout, and
                // a popup opened from a popup counts as a click outside the
                // first and closes it (see `sub_mode_picker`). So here the chip
                // rides the strength and the picker is inlined below.
                let nr = self.state.rx[0].noise_reduction;
                let label =
                    if nr.is_on() { format!("NR {}", nr.label()) } else { "NR".to_string() };
                if crate::chrome::chip(ui, nr.is_on(), label)
                    .on_hover_text("Noise reduction — click to cycle Off / Low / Med / High")
                    .clicked()
                {
                    let next = nr.next();
                    self.state.rx[0].noise_reduction = next; // optimistic echo
                    cmds.push(Command::SetNoiseReduction { rx: RxId::Main, level: next });
                }
            } else {
                self.nr_button(ui, cmds);
            }
            let muted = self.state.rx[0].muted;
            if crate::chrome::chip_accent(ui, muted, "MUTE", crate::theme::PINK, Color32::WHITE)
                .clicked()
            {
                cmds.push(Command::SetMute { rx: RxId::Main, muted: !muted });
            }
            // Record both sides of the QSO to an MP3 file (toggling).
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
                None => "Record RX (left) and TX (right) audio to MP3".to_string(),
            });
            if rec.clicked() {
                cmds.push(Command::SetRecording(!recording));
            }
            // Channel layout for the *next* recording — has no effect on one
            // already running, hence the disabled look while `recording`.
            let mono = self.state.recording_mono;
            let mono_chip = ui
                .add_enabled_ui(!recording, |ui| {
                    crate::chrome::chip_accent(ui, mono, "MONO", crate::theme::PINK, Color32::WHITE)
                })
                .inner
                .on_hover_text(if mono {
                    "Recording mixes RX/TX to one channel — click for RX left / TX right"
                } else {
                    "Recording splits RX left / TX right — click for a single mixed channel"
                });
            if mono_chip.clicked() {
                cmds.push(Command::SetRecordingMono(!mono));
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
            // CTCSS/DCS: what is coming in, and optionally what has to be
            // present before the audio opens. Only NFM carries either.
            if self.state.rx[0].mode == Mode::Nfm {
                self.tone_button(ui, cmds);
            }
        });
        if narrow {
            // The engine picker the chip above cannot open from inside a menu.
            self.nr_controls(ui, cmds);
        }
    }

    /// The NR chip and the picker behind it: which denoiser, and how hard.
    /// Fades out on its own, like the tone popup.
    fn nr_button(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let nr = self.state.rx[0].noise_reduction;
        let label = if nr.is_on() { format!("NR {}", nr.label()) } else { "NR".to_string() };
        let hover = match nr.engine() {
            Some(e) => {
                format!("Noise reduction: {} — click to change engine or strength", e.name())
            }
            None => "Noise reduction (voice) — click to pick an engine".to_string(),
        };
        let btn = crate::chrome::chip(ui, nr.is_on(), label).on_hover_text(hover);

        let popup_id = egui::Popup::default_response_id(&btn);
        let now = ui.input(|i| i.time);
        let alpha =
            crate::chrome::popup_fade_alpha(ui.ctx(), popup_id, now, &mut self.nr_popup_since);
        let resp = egui::Popup::from_toggle_button_response(&btn)
            .frame(crate::chrome::window_frame_alpha(alpha))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_opacity(alpha);
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                self.nr_controls(ui, cmds);
            });
        if let Some(r) = &resp {
            crate::chrome::paint_popup_cut_border(ui.ctx(), &r.response, alpha);
            // Hovering the popup keeps it up: the fade is for a menu left open
            // and forgotten, not one being read.
            if r.response.contains_pointer() {
                self.nr_popup_since = Some(now);
            }
        }
    }

    /// The engine row and the strength row. Its own function because a menu has
    /// to inline this rather than open it as a popup — see [`Self::rx_controls`].
    fn nr_controls(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let nr = self.state.rx[0].noise_reduction;
        let pick = |app: &mut Self, cmds: &mut Vec<Command>, level: NrLevel| {
            app.state.rx[0].noise_reduction = level; // optimistic echo
            cmds.push(Command::SetNoiseReduction { rx: RxId::Main, level });
        };

        crate::chrome::menu_caption(ui, "Engine");
        ui.horizontal_wrapped(|ui| {
            if crate::chrome::chip(ui, !nr.is_on(), "OFF")
                .on_hover_text("No noise reduction — the decoders never saw it anyway")
                .clicked()
            {
                pick(self, cmds, NrLevel::Off);
            }
            for e in NrEngine::ALL {
                if crate::chrome::chip(ui, nr.engine() == Some(e), e.tag())
                    .on_hover_text(e.name())
                    .clicked()
                {
                    pick(self, cmds, nr.with_engine(e));
                }
            }
        });

        crate::chrome::menu_caption(ui, "Strength");
        ui.horizontal_wrapped(|ui| {
            for st in NrStrength::ALL {
                // These work with NR off too: they switch it on at that strength
                // on RNNoise, which is what reaching for "Med" on a dead chip
                // means.
                if crate::chrome::chip(ui, nr.strength() == Some(st), st.label()).clicked() {
                    pick(self, cmds, nr.with_strength(st));
                }
            }
        });
    }

    /// The sub-audible readout: the CTCSS tone or DCS code being received, and a
    /// popup to require one before the audio gate opens.
    ///
    /// The chip shows what is *heard* in preference to what is *armed*, because
    /// on a monitoring receiver the tone is mostly a label — it says which
    /// repeater or system you are listening to — and the armed code is
    /// something you set once and then stop thinking about.
    fn tone_button(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let heard = self.meters.as_ref().and_then(|m| m.tone);
        let armed = self.state.rx[0].tone_sql;
        let label = match (heard, armed) {
            (Some(t), _) => t.label(),
            // Armed but silent: the dot marks it as a requirement, not a decode.
            (None, Some(t)) => format!("·{}", t.label()),
            (None, None) => "TONE".to_string(),
        };
        let hover = match (heard, armed) {
            (Some(h), Some(a)) if h == a => {
                format!("Receiving {}, which is the tone squelch — audio open", h.label())
            }
            (Some(h), Some(a)) => format!(
                "Receiving {}, but the tone squelch wants {} — audio stays closed",
                h.label(),
                a.label()
            ),
            (Some(h), None) => format!("Receiving CTCSS/DCS {}", h.label()),
            (None, Some(a)) => {
                format!("Tone squelch {}: nothing matching it is being received", a.label())
            }
            (None, None) => "CTCSS / DCS — no sub-audible tone on this signal".to_string(),
        };
        let btn = match armed {
            // Yellow while a gate is armed, so a silent receiver reads as
            // "waiting for its tone" rather than as a dead one.
            Some(_) => crate::chrome::chip_accent(
                ui,
                heard == armed,
                label,
                crate::theme::YELLOW,
                Color32::BLACK,
            ),
            None => crate::chrome::chip(ui, heard.is_some(), label),
        }
        .on_hover_text(hover);

        let popup_id = egui::Popup::default_response_id(&btn);
        let now = ui.input(|i| i.time);
        let alpha =
            crate::chrome::popup_fade_alpha(ui.ctx(), popup_id, now, &mut self.tone_popup_since);
        let resp = egui::Popup::from_toggle_button_response(&btn)
            .frame(crate::chrome::window_frame_alpha(alpha))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_opacity(alpha);
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                crate::chrome::menu_caption(ui, "Tone squelch");
                self.tone_controls(ui, cmds, heard, armed);
            });
        if let Some(r) = &resp {
            crate::chrome::paint_popup_cut_border(ui.ctx(), &r.response, alpha);
            if r.response.contains_pointer() {
                self.tone_popup_since = Some(now);
            }
        }
    }

    /// The tone picker: off, the tone being received, then the 50 CTCSS tones
    /// and the 104 DCS codes in each polarity.
    fn tone_controls(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        heard: Option<SubTone>,
        armed: Option<SubTone>,
    ) {
        let mut pick: Option<Option<SubTone>> = None;
        ui.horizontal(|ui| {
            if crate::chrome::chip(ui, armed.is_none(), "OFF")
                .on_hover_text("Carrier squelch: open on any signal")
                .clicked()
            {
                pick = Some(None);
            }
            // The shortcut that matters in practice — you are listening to a
            // repeater, it is sending its tone, and you want only that.
            if let Some(h) = heard {
                if armed != Some(h)
                    && crate::chrome::chip(ui, false, format!("USE {}", h.label()))
                        .on_hover_text("Require the tone currently being received")
                        .clicked()
                {
                    pick = Some(Some(h));
                }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            ui.label(RichText::new("CTCSS").size(10.0).color(crate::theme::CYAN_DIM));
            egui::Grid::new("ctcss-grid").spacing([3.0, 3.0]).show(ui, |ui| {
                for (i, &tenths) in sdroxide_types::CTCSS_TONES.iter().enumerate() {
                    let t = SubTone::Ctcss(tenths);
                    if crate::chrome::chip(ui, armed == Some(t), t.label()).clicked() {
                        pick = Some(Some(t));
                    }
                    if i % 10 == 9 {
                        ui.end_row();
                    }
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new("DCS").size(10.0).color(crate::theme::CYAN_DIM));
            if crate::chrome::chip(ui, armed == Some(SubTone::Dcs), "ANY DCS")
                .on_hover_text(
                    "Open on any DCS-coded signal. Which of the 104 codes it carries cannot be \
                     read reliably here, so there is nothing finer to choose",
                )
                .clicked()
            {
                pick = Some(Some(SubTone::Dcs));
            }
        });
        if let Some(tone) = pick {
            self.state.rx[0].tone_sql = tone; // optimistic echo
            cmds.push(Command::SetToneSquelch { rx: RxId::Main, tone });
        }
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

                // Which decoder reads the CW, and — for the neural one, the only
                // one whose cost depends on it — how many stations at once.
                if wideband && cfg.enabled(SkimmerKind::Cw) {
                    ui.add_space(2.0);
                    crate::chrome::menu_caption(ui, "CW decoder");
                    ui.horizontal_wrapped(|ui| {
                        for d in CwSkimmerDecoder::ALL {
                            if crate::chrome::chip(ui, cfg.cw_decoder == d, d.label())
                                .on_hover_text(d.hint())
                                .clicked()
                            {
                                cfg.cw_decoder = d;
                            }
                        }
                    });
                    if cfg.cw_decoder == CwSkimmerDecoder::Neural {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("stations").size(10.0).color(crate::theme::CYAN_DIM),
                            );
                            for n in sdroxide_types::CW_SLOT_CHOICES {
                                if crate::chrome::chip(ui, cfg.cw_slots == n, &n.to_string())
                                    .on_hover_text(
                                        "How many signals the model reads at once. \
                                         The rest keep their marker but carry no text.",
                                    )
                                    .clicked()
                                {
                                    cfg.cw_slots = n;
                                }
                            }
                        });
                    }
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
        crate::chrome::module(ui, "System", system_row_w(ui), |ui| {
            self.windows_controls(ui, false);
        });
    }

    /// The window buttons — the body of the System box, and of the SYS menu.
    /// See [`crate::chrome::control_row`] for `narrow`.
    fn windows_controls(&mut self, ui: &mut egui::Ui, narrow: bool) {
        let [log, spots, awards, bands, mem, scan_label, settings, help] = SYSTEM_CHIPS;
        crate::chrome::control_row(ui, narrow, |ui| {
            if crate::chrome::chip(ui, self.show_logbook, log)
                .on_hover_text("Logbook — all QSOs (digital + manual)")
                .clicked()
            {
                self.show_logbook = !self.show_logbook;
            }
            if crate::chrome::chip(ui, self.show_spots, spots)
                .on_hover_text("Live spots — DX cluster, POTA, SOTA, PSK Reporter")
                .clicked()
            {
                self.show_spots = !self.show_spots;
            }
            if crate::chrome::chip(ui, self.show_awards, awards)
                .on_hover_text("Award tracking — DXCC / WAS / WAZ / grids")
                .clicked()
            {
                self.show_awards = !self.show_awards;
            }
            if crate::chrome::chip(ui, self.show_bands, bands)
                .on_hover_text(
                    "Band conditions — the published forecast beside what has \
                     actually been heard on each band",
                )
                .clicked()
            {
                self.show_bands = !self.show_bands;
            }
            if crate::chrome::chip(ui, self.show_memories, mem)
                .on_hover_text("Memory channels")
                .clicked()
            {
                self.show_memories = !self.show_memories;
            }
            // Accented while a scan is actually running, so its state is visible
            // with the window closed — which is how it will usually be.
            let scan = self.state.scan;
            let scan_chip = if scan.running {
                crate::chrome::chip_accent(
                    ui,
                    true,
                    scan_label,
                    if scan.holding { crate::theme::GREEN } else { crate::theme::CYAN },
                    Color32::BLACK,
                )
            } else {
                crate::chrome::chip(ui, self.show_scanner, scan_label)
            };
            if scan_chip
                .on_hover_text(if scan.holding {
                    "Scanner — stopped on a signal"
                } else if scan.running {
                    "Scanner — running"
                } else {
                    "Scan memory channels or a frequency range"
                })
                .clicked()
            {
                self.show_scanner = !self.show_scanner;
            }
            if crate::chrome::chip(ui, self.show_settings, settings)
                .on_hover_text("Settings — device gains, antennas, audio devices")
                .clicked()
            {
                self.show_settings = !self.show_settings;
            }
            if crate::chrome::chip(ui, self.help.open, help)
                .on_hover_text("User manual (F1)")
                .clicked()
            {
                self.help.open = !self.help.open;
            }
        });
    }
}

/// The System box's chips, in the order they are drawn.
///
/// One list, read by both the box that reserves the width and the row that
/// draws into it. `windows_controls` destructures it, so a chip added to the
/// row without a label added here does not compile — which is what keeps the
/// reservation honest. A box reserved narrower than its contents does not clip
/// them: the row simply carries on past the box, and whatever crosses the
/// window edge is lost. That is how SCAN, SETTINGS and HELP came to vanish on
/// the layouts where the strip put this box near the end of a row.
const SYSTEM_CHIPS: [&str; 8] =
    ["LOG", "SPOTS", "AWARDS", "BANDS", "MEM", "SCAN", "⚙ SETTINGS", "? HELP"];

/// Width the System box needs for [`SYSTEM_CHIPS`]: the chips, the gaps between
/// them, and the box's own side margins. Measured against the live style rather
/// than fixed, because a touched layout pads every chip out past its desktop
/// width — see `the_system_box_fits_its_chips`.
fn system_row_w(ui: &egui::Ui) -> f32 {
    let chips: f32 = SYSTEM_CHIPS.iter().map(|l| crate::chrome::chip_width(ui, l, None)).sum();
    let gaps = ui.spacing().item_spacing.x * (SYSTEM_CHIPS.len() - 1) as f32;
    chips + gaps + 2.0 * crate::chrome::MODULE_MARGIN_X
}

/// The band + mode + digital chip rows: the body of the band/mode popup.
///
/// A free function taking the state it draws from, rather than a method, so a
/// test can lay the whole menu out on a phone-sized viewport without an app
/// around it — see `the_band_menu_fits_a_phone_screen`.
fn band_mode_menu(
    ui: &mut egui::Ui,
    mode: Mode,
    state: &RadioState,
    caps: Option<&DeviceCaps>,
    // Passed in rather than read off the app, so the layout test above can
    // still build this menu without one. `None` is the normal state until the
    // solar window has been opened once, and colours nothing.
    conditions: Option<&sdroxide_solar::BandConditions>,
    daylight: bool,
    cmds: &mut Vec<Command>,
) {
    crate::chrome::menu_caption(ui, "Band");
    let digital = mode.is_digital();
    ui.horizontal_wrapped(|ui| {
        for b in Band::ALL {
            // In a digital mode, a band button tunes to the band's standard
            // dial frequency where the mode has one (SetVfo keeps the mode),
            // and the chip carries a cyan underline saying so. A band without
            // one — every band, in RF Paint's case — jumps to the band's
            // default frequency instead, still keeping the mode: any band can
            // be picked in any mode, standard frequency or not. Outside the
            // digital modes a click is a normal band change through the band
            // stack.
            let std_hz = if digital { digi_freq_for_band(mode, b) } else { None };
            let digi_hz = match std_hz {
                Some(hz) => Some(hz),
                None if digital => Some(b.default_entry().0),
                None => None,
            };
            // A radio that publishes no tuning range keeps every band button:
            // `may_rx_hz` reads an empty range list as "the driver didn't say",
            // and greying out the whole bar would be a worse guess than
            // offering a band the radio turns out not to reach.
            let enabled = caps.is_none_or(|c| {
                b.edges().is_none_or(|(lo, hi)| c.may_rx_hz(lo) || c.may_rx_hz(hi))
            });
            let active = match std_hz {
                Some(hz) => (state.active_freq_hz() - hz).abs() < 500.0,
                None => state.band == b,
            };
            // The published forecast, where there is one. Colour only: the
            // chip still says what band it is, and a band nothing is published
            // about — 160 m, 60 m, and everything above 10 m — looks exactly
            // as it did before rather than being given a verdict it has not
            // got. The words, the source and the age are in the tooltip.
            let verdict = conditions.and_then(|c| c.for_band(b, daylight));
            let tint = verdict
                .map(sdroxide_solar::BandRating::of)
                .and_then(crate::app::bands::rating_color);
            let resp = crate::chrome::chip_enabled_tinted(
                ui,
                enabled,
                active,
                b.label(),
                tint,
                std_hz.is_some(),
            );
            let resp = match verdict {
                Some(v) => resp.on_hover_text(format!(
                    "{}: {v} ({}) — forecast by HAMQSL.com from the solar indices, \
                     not a measurement of your own path.",
                    b.label(),
                    if daylight { "daytime" } else { "night" },
                )),
                None => resp,
            };
            if resp.clicked() {
                match digi_hz {
                    Some(hz) => cmds.push(Command::SetVfo { vfo: state.active_vfo, hz }),
                    None => cmds.push(Command::SetBand(b)),
                }
            }
        }
    });
    ui.add_space(6.0);
    crate::chrome::menu_caption(ui, "Mode");
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
    crate::chrome::menu_caption(ui, "Digital");
    ui.horizontal_wrapped(|ui| {
        for m in Mode::DIGITAL {
            if crate::chrome::chip(ui, mode == m, m.label()).clicked() {
                cmds.push(Command::SetMode { rx: RxId::Main, mode: m });
            }
        }
    });
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

    #[test]
    fn a_landscape_tablet_gets_the_full_size_digits() {
        let f = shipped();
        let (size, box_w) = tablet_box(&f, 1024.0);
        assert_eq!(size, 40.0, "a 1024 pt tablet has room for the design size");
        assert!(box_w + 8.0 + SMETER_W <= 1024.0 - 36.0, "{box_w} + meter overflowed");
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

    /// Metrics measured from the touched layout the short strip is laid out
    /// with: a chip stands 41 pt; "VFO" fills a 56 pt chip and "DISP" a 62 pt
    /// one; the band/mode chip runs to 101 pt at its widest label; the PTT
    /// comes out 76 pt wide; the active-VFO tag 9 pt.
    const T_CHIP_H: f32 = 41.0;
    const T_CELL1: f32 = 56.0;
    const T_CELL2: f32 = 62.0;
    const T_BM_W: f32 = 101.0;
    const T_PTT_W: f32 = 76.0;

    fn a_short_strip(avail: f32, tx: bool, sub: bool) -> ShortStrip {
        let f = shipped();
        let chips = StripChips {
            chip_h: T_CHIP_H,
            tag_w: 9.0,
            bm_w: T_BM_W,
            ptt_w: if tx { T_PTT_W } else { 0.0 },
            row1: if sub { (3, T_CELL1) } else { (2, T_CELL1) },
            row2: if tx { (3, T_CELL2) } else { (2, T_CELL2) },
        };
        plan_short_strip(avail, &f, &chips, 8.0, 8.0)
    }

    /// 600 pt is the narrowest viewport the tablet tier dresses; less the top
    /// panel's and `angled_frame`'s margins the strip gets 564. The box, the
    /// PTT and the grid at its minimum all have to land on the one row the
    /// strip is — a block that wrapped would take the very height the strip
    /// exists to give back.
    #[test]
    fn the_short_strip_fits_the_narrowest_short_window() {
        for (tx, sub) in [(true, false), (true, true), (false, false)] {
            let p = a_short_strip(564.0, tx, sub);
            let ptt = if tx { T_PTT_W + 8.0 } else { 0.0 };
            let total = p.box_w + 8.0 + ptt + p.grid_w;
            assert!(total <= 564.0, "tx={tx} sub={sub}: the strip wants {total} of 564");
            assert!(p.digit >= STRIP_DIGIT_MIN, "tx={tx} sub={sub}: digits fell to {}", p.digit);
            // Stretched cells never squeeze a chip below its own label.
            assert!(
                p.cell1_w + 0.5 >= T_CELL1 && p.cell2_w + 0.5 >= T_CELL2,
                "tx={tx} sub={sub}: cells squeezed to {} / {}",
                p.cell1_w,
                p.cell2_w
            );
        }
    }

    /// The screen the strip was drawn for: 1280x720. The readout reaches its
    /// cap and every point the box and the PTT do not take goes to the
    /// buttons — which is what "the buttons scale to fit the width" means —
    /// on a strip nearly half the height of the stacked rows it replaced.
    #[test]
    fn on_a_720p_screen_the_buttons_take_the_slack() {
        let p = a_short_strip(1280.0 - 36.0, true, false);
        assert_eq!(p.digit, STRIP_DIGIT_MAX, "room to spare caps the digits");
        assert!(p.cell1_w > 2.0 * T_CELL1, "row 1 cells stayed at {}", p.cell1_w);
        assert!(p.cell2_w > 2.0 * T_CELL2, "row 2 cells stayed at {}", p.cell2_w);
        // Everything on the strip shares this height. The old tablet layout
        // stacked a 74 pt frequency box over a 74 pt meter-and-menus row.
        assert!(p.box_h <= 90.0, "the strip stands {} pt", p.box_h);
    }

    /// Reserve the System box the way [`SdroxideApp::windows_module`] does and
    /// draw its chips into it the way `windows_controls` does. Hands back the
    /// width the box left for its contents, and how far each chip reached into
    /// it, both measured from the box's inner left edge.
    fn system_box_and_chips(tier: crate::layout::Tier) -> (f32, Vec<(&'static str, f32)>) {
        let ctx = egui::Context::default();
        crate::layout::set_tier(&ctx, tier);
        crate::theme::apply_metrics(&ctx, tier);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(2560.0, 1440.0),
            )),
            ..Default::default()
        };
        let mut out = None;
        let _ = ctx.run_ui(input, |ui| {
            // The strip's own spacing, which the chips inherit — the style's is
            // narrower, and measuring against it would under-reserve the gaps.
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            let mut chips = Vec::new();
            let room = crate::chrome::module(ui, "System", system_row_w(ui), |ui| {
                // Read before the row is drawn: egui grows a Ui's max_rect to
                // cover content that overflowed it, so afterwards it reports
                // what the row took rather than what the box offered.
                let (left, room) = (ui.max_rect().left(), ui.max_rect().width());
                ui.horizontal(|ui| {
                    for label in SYSTEM_CHIPS {
                        let right = crate::chrome::chip(ui, false, label).rect.right();
                        chips.push((label, right - left));
                    }
                });
                room
            });
            out = Some((room, chips));
        });
        out.expect("the box was drawn")
    }

    /// Every chip in the System box has to fit inside the width the box reserved
    /// for it.
    ///
    /// A module reserves its width before its contents are drawn, and a row that
    /// does not fit is not clipped to the box — it carries on past it, and
    /// whatever crosses the window edge is simply gone. The box was sized 285 pt
    /// by hand for five chips and kept that literal through three more (BANDS,
    /// SCAN, and the widened SETTINGS), by which point the row needed twice the
    /// box: SCAN, SETTINGS and HELP fell off the right-hand edge on any layout
    /// that left the box near the end of a row. Nothing about the chips said so
    /// — they were drawn every frame, just past the edge of the window.
    #[test]
    fn the_system_box_fits_its_chips() {
        for tier in [crate::layout::Tier::Desktop, crate::layout::Tier::Tablet] {
            let (room, chips) = system_box_and_chips(tier);
            for (label, right) in chips {
                assert!(
                    right <= room + 0.5,
                    "{tier:?}: {label} reaches {right} pt into a box with room for {room}"
                );
            }
        }
    }

    /// Open the band/mode menu on a `screen`-sized viewport and measure the
    /// popup it produced.
    fn band_menu_rect(screen: egui::Vec2) -> egui::Rect {
        let ctx = egui::Context::default();
        let tier = crate::layout::tier_for(screen, sdroxide_types::LayoutMode::Auto);
        crate::layout::set_tier(&ctx, tier);
        crate::theme::apply_metrics(&ctx, tier);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            ..Default::default()
        };
        let state = RadioState::default();
        let menu = |ui: &mut egui::Ui| {
            let btn = crate::chrome::chip(ui, false, "20m · USB");
            let id = egui::Popup::default_response_id(&btn);
            crate::chrome::menu_popup(ui, &btn, |ui| {
                band_mode_menu(ui, state.rx[0].mode, &state, None, None, true, &mut Vec::new());
            });
            id
        };
        // The first pass gives the chip an id; then it is opened and laid out.
        let mut id = None;
        let _ = ctx.run_ui(input(), |ui| id = Some(menu(ui)));
        let id = id.expect("the chip was drawn");
        egui::Popup::open_id(&ctx, id);
        let _ = ctx.run_ui(input(), |ui| {
            menu(ui);
        });
        ctx.memory(|m| m.area_rect(id)).expect("the menu was shown")
    }

    /// The longest menu in the program, on the smallest screens it opens on.
    ///
    /// A popup is not laid out inside a panel: egui moves one that lands off an
    /// edge, but it cannot shrink one that is simply too big for the screen —
    /// the overflow hangs off the viewport where no finger can reach it. Forty
    /// chips in three sections is well past what a phone in landscape can show,
    /// so this menu only fits because [`crate::chrome::menu_popup`] bounds it
    /// and scrolls the rest.
    #[test]
    fn the_band_menu_fits_a_phone_screen() {
        for screen in [
            egui::vec2(360.0, 800.0),  // small phone, portrait
            egui::vec2(393.0, 852.0),  // common phone, portrait
            egui::vec2(852.0, 393.0),  // and in landscape
            egui::vec2(667.0, 375.0),  // a small phone in landscape: the tightest of all
            egui::vec2(768.0, 1024.0), // tablet, for company
        ] {
            let r = band_menu_rect(screen);
            assert!(
                r.width() <= screen.x && r.height() <= screen.y,
                "{screen:?}: the band menu came out {} x {}",
                r.width(),
                r.height()
            );
            assert!(
                r.left() >= 0.0
                    && r.right() <= screen.x
                    && r.top() >= 0.0
                    && r.bottom() <= screen.y,
                "{screen:?}: the band menu spans {r:?}"
            );
        }
    }
}
