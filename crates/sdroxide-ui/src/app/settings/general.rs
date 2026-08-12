//! The General tab's sound-device pickers and the server's sign-in credentials.
//!
//! The operator's own card is always shown; the radio's is only relevant to
//! the CAT / Audio interface, since every other backend carries its audio
//! in-band.

use eframe::egui::{self, Color32, ComboBox, RichText};
use sdroxide_types::{Region, RemoteAccess};

use crate::app::SdroxideApp;

/// The IARU region dropdown: the number the band plans are published under,
/// with the part of the world it covers next to it.
///
/// Both, because neither alone identifies it for most operators — "Region 2"
/// means nothing until you know it is the Americas, and the number is what
/// every band-plan document and contest rule actually says.
pub(in crate::app) fn region_combo(ui: &mut egui::Ui, region: &mut Region) {
    ComboBox::from_id_salt("iaru-region").width(360.0).selected_text(region.label()).show_ui(
        ui,
        |ui| {
            for r in Region::ALL {
                if ui.selectable_label(*region == r, r.label()).clicked() {
                    *region = r;
                }
            }
        },
    );
}

/// A device dropdown ("System default" + names); calls `pick(Some(name)|None)`.
pub(in crate::app) fn device_combo(
    ui: &mut egui::Ui,
    id: &str,
    names: &[String],
    selected: &Option<String>,
    mut pick: impl FnMut(Option<String>),
) {
    let shown = selected.clone().unwrap_or_else(|| "System default".into());
    ComboBox::from_id_salt(id).width(300.0).selected_text(shown).show_ui(ui, |ui| {
        if ui.selectable_label(selected.is_none(), "System default").clicked() {
            pick(None);
        }
        for n in names {
            if ui.selectable_label(selected.as_deref() == Some(n), n).clicked() {
                pick(Some(n.clone()));
            }
        }
    });
}

/// Who may connect to this machine's server (`[remote_access]` in
/// `config.toml`).
///
/// Only drawn when the engine is in this process. These are a file on the
/// machine the radio is attached to: a remote client has nothing here to read
/// them from, and offering it a box that writes to its own disk instead would
/// be worse than offering nothing — it would look as though the station's
/// password had been changed when it had not.
///
/// Written as it is typed, like the control bindings, rather than behind an
/// APPLY: the server re-reads the file for every sign-in, so there is no
/// separate step for an APPLY to stand for.
pub(in crate::app) fn remote_access_settings(ui: &mut egui::Ui, access: &mut RemoteAccess) {
    ui.label(RichText::new("Remote access").size(14.0).strong().color(crate::theme::CYAN()));
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "What a remote client — the browser page, or another sdroxide started with \
             --connect — has to give before this station will let it operate. Applies in server \
             mode (--server); the next sign-in picks up a change, with no restart.",
        )
        .size(11.5)
        .weak(),
    );
    ui.add_space(8.0);
    egui::Grid::new("remote-access-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Username");
        ui.add(egui::TextEdit::singleline(&mut access.username).desired_width(200.0));
        ui.end_row();
        ui.label("Password");
        ui.add(
            egui::TextEdit::singleline(&mut access.password).password(true).desired_width(200.0),
        );
        ui.end_row();
    });
    ui.add_space(6.0);
    // The state of the door, said plainly. "Both boxes empty means anyone may
    // key my transmitter" is not something an operator should have to infer.
    if access.is_enforced() {
        if access.username.is_empty() {
            ui.label(
                RichText::new(
                    "Clients must give the password. Leaving the username empty is fine.",
                )
                .size(11.5)
                .color(crate::theme::GREEN()),
            );
        } else {
            ui.label(
                RichText::new("Clients must sign in.").size(11.5).color(crate::theme::GREEN()),
            );
        }
    } else {
        ui.label(
            RichText::new(
                "⚠ Empty: anyone who can reach the server's port can operate this radio, on \
                 your callsign. Set a password before forwarding the port.",
            )
            .size(11.5)
            .color(crate::theme::YELLOW()),
        );
    }
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Stored in the clear in config.toml, like the other passwords sdroxide keeps.",
        )
        .size(10.5)
        .color(Color32::from_gray(140)),
    );
}

impl SdroxideApp {
    /// The user's own speakers / microphone (applied live).
    pub(in crate::app) fn settings_user_audio(
        &self,
        ui: &mut egui::Ui,
        audio_pick: &mut Option<(bool, Option<String>)>,
    ) {
        let Some(devs) = &self.audio_devices else {
            return;
        };
        ui.label(RichText::new("Your audio (speakers / microphone)").strong());
        egui::Grid::new("user-audio").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Output");
            device_combo(ui, "u-out", &devs.outputs, &devs.selected_output, |n| {
                *audio_pick = Some((true, n))
            });
            ui.end_row();
            ui.label("Input");
            device_combo(ui, "u-in", &devs.inputs, &devs.selected_input, |n| {
                *audio_pick = Some((false, n))
            });
            ui.end_row();
        });
    }
}
