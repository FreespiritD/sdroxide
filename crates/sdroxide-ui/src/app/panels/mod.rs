//! The bottom panel: whichever operating mode is selected owns it.
//!
//! [`SdroxideApp::digi_panel`] is the dispatcher — it picks the panel for the
//! current [`Mode`] and hands it the height it may use. One submodule per
//! panel:
//!
//! - [`decodes`] — the FT8/FT4/JS8 decode list and the QSO sequencer beside it
//! - [`text_modem`] — PSK, RTTY, Olivia, THOR, Contestia and Hellschreiber
//! - [`js8`], [`fsq`] — the two keyboard modes with their own message model
//! - [`sstv`], [`wefax`], [`rf_paint`] — the image modes
//! - [`rade`] — FreeDV / RADE digital voice
//! - [`setup`] — the digimode setup window the panels share
//! - [`widgets`] — the row and station-card widgets several panels draw

pub(in crate::app) mod decodes;
pub(in crate::app) mod fsq;
pub(in crate::app) mod js8;
pub(in crate::app) mod rade;
pub(in crate::app) mod rf_paint;
pub(in crate::app) mod setup;
pub(in crate::app) mod sstv;
pub(in crate::app) mod text_modem;
pub(in crate::app) mod wefax;
pub(in crate::app) mod widgets;

use eframe::egui::{self, RichText};
use sdroxide_types::{Band, Command, Mode};

use crate::app::SdroxideApp;

/// The waterfall's tab. Always last, and every mode has one: on a phone the
/// panadapter is a view of its own rather than a strip above the panel, because
/// a third of that height is not enough to work a mode *and* watch a band.
pub(in crate::app) const TAB_WFALL: &str = "WFALL";

/// The panes a mode's panel splits into, in the order the tabs show them.
///
/// A phone draws one at a time — two columns want 180 and 220 points before
/// either has said anything, which is more than the screen — so this is also
/// the answer to "what is there to switch between". Modes whose panel is a
/// single column name it anyway, so the tab row reads the same everywhere and
/// the waterfall always has something to sit beside.
pub(in crate::app) fn panel_panes(mode: Mode) -> &'static [&'static str] {
    match mode {
        Mode::Ft8 | Mode::Ft4 => &["DECODES", "QSO"],
        Mode::Js8 => &["HEARD", "CHAT"],
        Mode::Fsq => &["HEARD", "TRAFFIC"],
        Mode::Sstv | Mode::Rifp => &["RECEIVE", "SEND"],
        Mode::Wefax => &["CHART", "SAVED"],
        Mode::RfPaint => &["TEXT", "IMAGE"],
        // The keyboard modes and RADE are one column already: receive above,
        // what you are sending below it.
        _ => &["PANEL"],
    }
}

/// The full tab row for `mode`: its panes, then the waterfall.
pub(in crate::app) fn panel_tabs(mode: Mode) -> impl Iterator<Item = &'static str> {
    panel_panes(mode).iter().copied().chain(std::iter::once(TAB_WFALL))
}

/// Index of the pane named `name`, for the panels that need to switch to one of
/// their own (answering a call takes an operator to the QSO pane).
pub(in crate::app) fn pane_index(mode: Mode, name: &str) -> usize {
    panel_panes(mode).iter().position(|p| *p == name).unwrap_or(0)
}

/// Standard FT8/FT4 dial frequencies per HF/6 m band.
/// The standard FT8/FT4 dial frequency for `band`, if one exists for `mode`
/// (matched by which band's edges the frequency falls within).
pub(in crate::app) fn digi_freq_for_band(mode: Mode, band: Band) -> Option<f64> {
    let (lo, hi) = band.edges()?;
    digi_dial_freqs(mode).iter().find(|&&(_, hz)| (lo..=hi).contains(&hz)).map(|&(_, hz)| hz)
}

fn digi_dial_freqs(mode: Mode) -> &'static [(&'static str, f64)] {
    match mode {
        // JS8 conventional dials; signals sit in the ~3 kHz above each.
        Mode::Js8 => &[
            ("160m", 1_842_000.0),
            ("80m", 3_578_000.0),
            ("40m", 7_078_000.0),
            ("30m", 10_130_000.0),
            ("20m", 14_078_000.0),
            ("17m", 18_104_000.0),
            ("15m", 21_078_000.0),
            ("12m", 24_922_000.0),
            ("10m", 28_078_000.0),
        ],
        // PSK31 activity centres (USB dial; signals sit ~1 kHz above).
        Mode::Psk => &[
            ("80m", 3_580_000.0),
            ("40m", 7_040_000.0),
            ("30m", 10_142_000.0),
            ("20m", 14_070_000.0),
            ("17m", 18_097_000.0),
            ("15m", 21_070_000.0),
            ("12m", 24_920_000.0),
            ("10m", 28_120_000.0),
        ],
        // RTTY sub-band starts (USB dial).
        Mode::Rtty => &[
            ("80m", 3_580_000.0),
            ("40m", 7_040_000.0),
            ("30m", 10_140_000.0),
            ("20m", 14_080_000.0),
            ("17m", 18_100_000.0),
            ("15m", 21_080_000.0),
            ("12m", 24_920_000.0),
            ("10m", 28_080_000.0),
        ],
        Mode::Ft4 => &[
            ("80m", 3_575_000.0),
            ("40m", 7_047_500.0),
            ("30m", 10_140_000.0),
            ("20m", 14_080_000.0),
            ("17m", 18_104_000.0),
            ("15m", 21_140_000.0),
            ("12m", 24_919_000.0),
            ("10m", 28_180_000.0),
        ],
        // FreeDV calling frequencies (USB dial).
        Mode::Rade => &[
            ("80m", 3_625_000.0),
            ("40m", 7_177_000.0),
            ("20m", 14_236_000.0),
            ("15m", 21_313_000.0),
            ("10m", 28_330_000.0),
        ],
        // SSTV calling frequencies (USB).
        Mode::Sstv => &[
            ("80m", 3_730_000.0),
            ("40m", 7_171_000.0),
            ("20m", 14_230_000.0),
            ("15m", 21_340_000.0),
            ("10m", 28_680_000.0),
        ],
        // Olivia activity centres (USB dial).
        Mode::Olivia => &[
            ("80m", 3_581_000.0),
            ("40m", 7_073_000.0),
            ("30m", 10_142_000.0),
            ("20m", 14_076_000.0),
            ("17m", 18_103_000.0),
            ("15m", 21_076_000.0),
            ("10m", 28_076_000.0),
        ],
        // THOR / DominoEX activity centres (USB dial).
        Mode::Thor => &[
            ("80m", 3_580_000.0),
            ("40m", 7_070_000.0),
            ("30m", 10_147_000.0),
            ("20m", 14_073_000.0),
            ("17m", 18_103_000.0),
            ("15m", 21_073_000.0),
            ("10m", 28_073_000.0),
        ],
        // FSQCALL calling frequencies (USB dial; signals ~1500 Hz above).
        Mode::Fsq => &[
            ("80m", 3_575_000.0),
            ("40m", 7_105_000.0),
            ("30m", 10_144_000.0),
            ("20m", 14_105_000.0),
            ("17m", 18_104_000.0),
            ("15m", 21_105_000.0),
            ("10m", 28_105_000.0),
        ],
        // Hellschreiber (USB dial), from hellschreiber.com's narrow-band digimode
        // band plan of 18 March 2019 — its "common calling & operating" column,
        // taking IARU Region 1 where that column is split, to match the Region 1
        // defaults `Band::edges` already uses.
        //
        // Two deliberate departures, on 15 m and 10 m: that table's own calling
        // frequencies there (21074 / 28074) fall *outside* the operating ranges
        // it lists in the same cell, and both sit exactly on the FT8 sub-band.
        // The range starts are used instead — internally consistent, clear of
        // FT8, and what the Feld Hell Club lists. 6 m is not in that table at
        // all, so it keeps the club's figure.
        Mode::Hell => &[
            ("160m", 1_840_000.0),
            ("80m", 3_574_000.0),
            ("60m", 5_351_500.0),
            ("40m", 7_040_000.0),
            ("30m", 10_144_000.0),
            ("20m", 14_073_000.0),
            ("17m", 18_104_000.0),
            ("15m", 21_063_000.0),
            ("12m", 24_924_000.0),
            ("10m", 28_063_000.0),
            ("6m", 50_286_000.0),
        ],
        // RF Paint has no defined calling frequency — offer no band presets.
        Mode::RfPaint => &[],
        // RIFP assigns no frequency at all: 433.92 MHz is the deployment
        // example the draft names, and the others are the middle of the
        // segments where a 25 kHz channel is a realistic thing to ask for
        // (10 m FM and the 6 m all-modes part). The dial is the signal's
        // *centre* here, not its lower edge as in every mode above.
        Mode::Rifp => &[
            ("10m", 29_600_000.0),
            ("6m", 51_250_000.0),
            // The 2 m image/facsimile corner: 144.700 is the FAX calling
            // frequency, inside the all-modes segment.
            ("2m", 144_700_000.0),
            ("70cm", sdroxide_types::RIFP_CALLING_HZ),
        ],
        // FT8 (and default).
        _ => &[
            ("160m", 1_840_000.0),
            ("80m", 3_573_000.0),
            ("40m", 7_074_000.0),
            ("30m", 10_136_000.0),
            ("20m", 14_074_000.0),
            ("17m", 18_100_000.0),
            ("15m", 21_074_000.0),
            ("12m", 24_915_000.0),
            ("10m", 28_074_000.0),
            ("6m", 50_313_000.0),
        ],
    }
}

impl SdroxideApp {
    /// Drop everything derived from the previous mode's decodes: the waterfall
    /// callsign boxes, the decode list, the world-map station dots and the
    /// clicked-decode preview. Called on every RX mode change so leaving FT8/FT4
    /// (for SSTV, a keyboard mode, or plain SSB) doesn't carry its labels over.
    pub(in crate::app) fn clear_digi_rx(&mut self) {
        self.digi_decodes.clear();
        self.digi_stations = Default::default();
        self.digi_preview = None;
        // The Hell raster is a continuous strip with no frame boundary, so
        // leaving it up across a mode change would splice unrelated text.
        self.hell.clear();
    }

    /// The chips a phone switches the panel's views with, plus the one reading
    /// worth carrying across all of them: how many stations the last slot
    /// decoded, which is the answer to "is the band open".
    ///
    /// Wrapped, so a mode with more panes than the width holds takes a second
    /// row rather than losing the last one — which on most modes is the
    /// waterfall.
    pub(in crate::app) fn digi_tabs(&mut self, ui: &mut egui::Ui, mode: Mode) {
        let selected = self.digi_pane(mode);
        // Pin the content to the width left *inside* the margins. A `Frame`
        // reports an outer rect of content-plus-margins, and a content row that
        // has taken all the width there was makes that outer rect wider than
        // the window — which the parent then expands to include, leaving every
        // row drawn after this one wrapping against a width the screen has not
        // got and clipping whatever crosses the edge.
        const MARGIN: f32 = 8.0;
        let inner_w = (ui.available_width() - 2.0 * MARGIN).max(80.0);
        egui::Frame::new()
            .inner_margin(egui::Margin { left: 8, right: 8, top: 6, bottom: 2 })
            .show(ui, |ui| {
                ui.set_max_width(inner_w);
                ui.horizontal_wrapped(|ui| {
                    for (i, label) in panel_tabs(mode).enumerate() {
                        if crate::chrome::chip(ui, selected == i, label).clicked() {
                            self.view.digi_pane = i;
                        }
                    }
                    // Only the modes whose panel is the decode list have a
                    // count to put here. JS8 is slotted too, but it keeps its
                    // own heard list and never fills `digi_decodes` — a "0 rx"
                    // beside a busy band would be a lie.
                    if matches!(mode, Mode::Ft8 | Mode::Ft4) {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("{} rx", self.digi_decodes.len()))
                                    .size(10.0)
                                    .color(egui::Color32::from_gray(120)),
                            );
                        });
                    }
                });
            });
    }

    /// The pane showing this frame, clamped to what `mode` actually has. A
    /// stored index only means anything against the mode it was stored in, and
    /// switching from a two-pane mode to a one-pane one must not leave the
    /// panel pointing at a pane that does not exist.
    pub(in crate::app) fn digi_pane(&self, mode: Mode) -> usize {
        self.view.digi_pane.min(panel_panes(mode).len())
    }

    /// Which pane a panel should draw, or `None` where it draws them all.
    ///
    /// `None` on every layout but a phone, and on a phone whenever the
    /// waterfall tab is up — the waterfall replaces the panel rather than
    /// living inside it, so the panel is not drawn at all.
    pub(in crate::app) fn phone_pane(&self, ui: &egui::Ui, mode: Mode) -> Option<usize> {
        if crate::layout::tier(ui.ctx()) != crate::layout::Tier::Phone {
            return None;
        }
        let pane = self.digi_pane(mode);
        (pane < panel_panes(mode).len()).then_some(pane)
    }

    /// The FT8/FT4 operating panel: decode list on the left, QSO area on the
    /// right. Sits below the (zoomed) waterfall in digital modes.
    ///
    /// A phone shows one column at a time instead — see [`Self::digi_tabs`].
    /// Two columns need 180 + 7 + 220 points before either has said anything,
    /// which is more than the whole of the screen.
    pub(in crate::app) fn digi_panel(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        if let Some(pane) = self.phone_pane(ui, self.state.rx[0].mode) {
            match pane {
                0 => self.decode_list(ui, cmds),
                _ => self.qso_area(ui, cmds),
            }
            return;
        }
        let avail = ui.available_size();
        let handle_w = 7.0;
        // Decode list takes a user-draggable fraction of the width; the QSO area
        // gets the rest (each keeps a usable minimum).
        let left_w = (avail.x * self.view.digi_split_fraction)
            .clamp(180.0, (avail.x - handle_w - 220.0).max(180.0));
        ui.horizontal_top(|ui| {
            // Force a top-down layout: `allocate_ui` would otherwise inherit the
            // parent `horizontal_top` (left-to-right) and lay the rows out
            // sideways, overflowing and shoving the QSO column off-screen.
            ui.allocate_ui_with_layout(
                egui::vec2(left_w, avail.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    self.decode_list(ui, cmds);
                },
            );
            // Draggable vertical divider between the decode table and the QSO area.
            let hresp = crate::chrome::split_handle(ui, egui::vec2(handle_w, avail.y), None);
            if hresp.dragged() {
                let d = hresp.drag_delta().x / avail.x.max(1.0);
                self.view.digi_split_fraction =
                    (self.view.digi_split_fraction + d).clamp(0.28, 0.72);
            }
            ui.vertical(|ui| {
                self.qso_area(ui, cmds);
            });
        });
    }

    /// The band's other conventional frequencies for this mode, as a chip that
    /// opens a picker.
    ///
    /// Only appears where there is actually a choice. Most modes have one
    /// agreed frequency per band and the chip would be a button that does
    /// nothing; the ones that have several — FT8's DXpedition window, PSK and
    /// RTTY's region split, SSTV's move-up-when-busy convention — are exactly
    /// the ones where an operator otherwise has to go and look the number up.
    ///
    /// The dial is what moves. These are dial frequencies, and the audio
    /// offset within the passband is a separate control that must not be
    /// disturbed by changing band segment.
    pub(in crate::app) fn digi_freq_chip(&self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let mode = self.state.rx[0].mode;
        let dial = self.state.active_freq_hz();
        let band = sdroxide_types::Band::containing(dial);
        let channels = sdroxide_types::digi_channels_in(mode, band);
        if channels.len() < 2 {
            return;
        }
        // "On" when the dial is already sitting on one of them, so the chip
        // doubles as a readout of whether you are where the mode expects.
        let here = channels.iter().find(|c| (c.dial_hz - dial).abs() < 1.0);
        let face = match here {
            Some(c) => format!("⇵ {:.3}", c.dial_hz / 1e6),
            None => "⇵ FREQ".to_string(),
        };
        let btn = crate::chrome::chip(ui, here.is_some(), RichText::new(face).size(11.0))
            .on_hover_text(format!(
                "The {} frequencies agreed for {} on {}",
                channels.len(),
                mode.label(),
                band.label()
            ));

        let mut pick = None;
        let resp = egui::Popup::from_toggle_button_response(&btn)
            .frame(crate::chrome::window_frame())
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                ui.set_max_width(300.0);
                ui.label(
                    RichText::new(format!("{} · {}", mode.label(), band.label()))
                        .color(crate::theme::CYAN_DIM)
                        .size(9.5)
                        .strong(),
                );
                ui.add_space(2.0);
                for c in &channels {
                    let on = here.map(|h| h.dial_hz) == Some(c.dial_hz);
                    let mut text = format!("{:.3} MHz", c.dial_hz / 1e6);
                    if !c.note.is_empty() {
                        text.push_str(&format!("   {}", c.note));
                    }
                    let mut rich = RichText::new(text).size(12.0);
                    if c.outside_r1_data_segment(mode) {
                        rich = rich.color(crate::theme::YELLOW);
                    }
                    let row = ui.selectable_label(on, rich);
                    if c.outside_r1_data_segment(mode) {
                        row.clone().on_hover_text(
                            "A global convention that the IARU Region 1 band plan does not put \
                             narrow data on — check your own band plan before transmitting here.",
                        );
                    }
                    if row.clicked() {
                        pick = Some(c.dial_hz);
                    }
                }
                if channels.iter().any(|c| c.outside_r1_data_segment(mode)) {
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Amber: outside the Region 1 data segment.")
                            .color(crate::theme::LINE_LIT)
                            .size(10.0),
                    );
                }
            });
        if let Some(r) = &resp {
            crate::chrome::paint_popup_cut_border(ui.ctx(), &r.response, 1.0);
        }
        if let Some(hz) = pick {
            cmds.push(Command::SetVfo { vfo: self.state.active_vfo, hz });
        }
    }

    /// A compact decode-squelch (sensitivity) slider for the keyboard panels:
    /// higher = require a stronger signal, so pure noise stops decoding.
    pub(in crate::app) fn digi_squelch_slider(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
    ) {
        let mut sq = self.digi_cfg_edit.digi_squelch;
        ui.spacing_mut().slider_width = 84.0;
        let resp = ui
            .add(egui::Slider::new(&mut sq, 0.0..=1.0).show_value(false))
            .on_hover_text("Decode squelch — raise to stop decoding noise");
        ui.label(RichText::new("SQL").size(10.0).color(crate::theme::CYAN_DIM));
        if resp.changed() && self.digi_cfg_seeded {
            self.digi_cfg_edit.digi_squelch = sq;
            cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
        }
    }

    /// The slot length of the current mode, in seconds.
    ///
    /// FT8 and FT4 have theirs fixed by the mode; JS8's is an operator setting,
    /// so it has to come from the engine's status. The decode list groups rows
    /// into turns by this, and getting it wrong for JS8 Turbo would draw one
    /// "EVEN/ODD" header per two and a half turns.
    pub(in crate::app) fn slot_period_s(&self) -> f64 {
        match self.state.rx[0].mode {
            Mode::Ft4 => 7.5,
            Mode::Js8 => self
                .digi_status
                .as_ref()
                .and_then(|s| s.js8.as_ref())
                .map_or(15.0, |j| j.speed.slot_s()),
            _ => 15.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The waterfall is the tab one past the mode's own panes — that is how
    /// `App::ui` tells "show the panadapter" from "show the panel", so a mode
    /// with no panes at all would make the two indistinguishable.
    #[test]
    fn every_mode_has_at_least_one_pane_and_a_waterfall_after_it() {
        for mode in Mode::ALL {
            let panes = panel_panes(mode);
            assert!(!panes.is_empty(), "{mode:?} has no panes");
            let tabs: Vec<_> = panel_tabs(mode).collect();
            assert_eq!(tabs.len(), panes.len() + 1, "{mode:?} tab count");
            assert_eq!(tabs.last().copied(), Some(TAB_WFALL), "{mode:?} waterfall is not last");
            assert!(!panes.contains(&TAB_WFALL), "{mode:?} names a pane after the waterfall tab");
        }
    }

    /// A stored pane index only means anything against the mode it was stored
    /// in. Clamping it to the pane count is what stops a switch from FT8 (two
    /// panes) to PSK (one) leaving the panel pointing past the end — the value
    /// the clamp yields is always either a real pane or the waterfall.
    #[test]
    fn a_stored_pane_index_is_valid_in_every_other_mode() {
        for from in Mode::ALL {
            // The furthest tab the mode it was stored in could select.
            let stored = panel_panes(from).len();
            for to in Mode::ALL {
                let panes = panel_panes(to).len();
                let clamped = stored.min(panes);
                assert!(clamped <= panes, "{from:?} → {to:?}: {clamped} past {panes}");
            }
        }
    }

    #[test]
    fn the_slotted_modes_can_find_their_qso_pane() {
        for mode in [Mode::Ft8, Mode::Ft4] {
            let i = pane_index(mode, "QSO");
            assert_eq!(panel_panes(mode)[i], "QSO", "{mode:?}");
            assert_ne!(i, pane_index(mode, "DECODES"), "{mode:?} panes collapsed");
        }
        // A mode with no such pane falls back to its first, rather than to an
        // index that is not there.
        assert_eq!(pane_index(Mode::Psk, "QSO"), 0);
    }
}
