//! The packet panel: what is on the channel, and what the link is doing.
//!
//! A packet operator watches two things that move independently — every frame
//! heard, and the state of their own connection — so they get a pane each
//! rather than sharing one.
//!
//! There is no text entry here on purpose. Packet is not a keyboard mode: the
//! traffic is either somebody else's, a beacon on a timer, or a Winlink session
//! driving the link from the MAIL window. A text box would imply a
//! conversational mode that does not exist.

use eframe::egui::{self, RichText};
use sdroxide_types::{Command, PacketStatus};

use crate::app::SdroxideApp;
use crate::theme;

impl SdroxideApp {
    pub(in crate::app) fn packet_panel(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        panel_h: f32,
    ) {
        let st: Option<PacketStatus> = self.digi_status.as_ref().and_then(|s| s.packet.clone());
        let Some(st) = st else {
            ui.label(RichText::new("starting the packet modem…").weak());
            return;
        };

        ui.horizontal(|ui| {
            // ── MONITOR ───────────────────────────────────────────────────
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() * 0.62);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("MONITOR").strong().color(theme::CYAN()));
                    ui.label(RichText::new(format!("{} baud", st.baud.label())).weak());
                    // Channel busy and bad frames are the two numbers that
                    // explain a link that is not working: one says somebody
                    // else is talking, the other says the path is marginal.
                    if st.dcd {
                        ui.label(RichText::new("BUSY").strong().color(theme::ALERT()));
                    }
                    if st.bad_frames > 0 {
                        ui.label(
                            RichText::new(format!("{} bad", st.bad_frames))
                                .weak()
                                .color(theme::ALERT()),
                        )
                        .on_hover_text(
                            "Frames that arrived but failed their check sequence — a \
                             collision, a fade, or a signal too weak to read.",
                        );
                    }
                });

                egui::ScrollArea::vertical()
                    .id_salt("packet-monitor")
                    .max_height(panel_h - 40.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if st.heard.is_empty() {
                            ui.label(RichText::new("nothing heard yet").weak());
                        }
                        for h in &st.heard {
                            let via = if h.via.is_empty() {
                                String::new()
                            } else {
                                format!(",{}", h.via.join(","))
                            };
                            // Our own traffic in a different colour: an
                            // operator needs to tell their beacon going out
                            // from somebody answering it.
                            let colour = if h.sent { theme::GREEN() } else { theme::CYAN_DIM() };
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(format!("{}>{}{via}", h.from, h.to))
                                        .monospace()
                                        .color(colour),
                                );
                                ui.label(RichText::new(&h.kind).monospace().weak());
                                if !h.text.is_empty() {
                                    ui.label(RichText::new(&h.text).monospace());
                                }
                            });
                        }
                    });
            });

            ui.separator();

            // ── LINK ──────────────────────────────────────────────────────
            ui.vertical(|ui| {
                ui.label(RichText::new("LINK").strong().color(theme::CYAN()));
                match &st.link {
                    Some(peer) => {
                        ui.label(
                            RichText::new(format!("connected to {peer}"))
                                .strong()
                                .color(theme::GREEN()),
                        );
                    }
                    None => {
                        ui.label(RichText::new("no connection").weak());
                    }
                }

                ui.add_space(6.0);
                ui.label(RichText::new("A Winlink session drives the link from the").weak());
                ui.label(RichText::new("MAIL window — set the route to Radio").weak());
                ui.label(RichText::new("in Settings → Winlink first.").weak());

                ui.add_space(10.0);
                if crate::chrome::chip_accent(
                    ui,
                    false,
                    RichText::new(" BEACON ").strong(),
                    theme::GREEN(),
                    theme::INK_ON_CYAN(),
                )
                .on_hover_text("Send one UNPROTO identification frame now")
                .clicked()
                {
                    cmds.push(Command::PacketBeacon);
                }
            });
        });
    }
}
