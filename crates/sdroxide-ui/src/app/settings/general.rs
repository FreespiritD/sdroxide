//! The General tab's sound-device pickers and the server's sign-in credentials.
//!
//! The operator's own card is always shown; the radio's is only relevant to
//! the CAT / Audio interface, since every other backend carries its audio
//! in-band.

use eframe::egui::{self, Color32, ComboBox, RichText};
use sdroxide_types::{CallsignStyle, FreqStyle, RemoteAccess, SpeechSettings, Verbosity};

use crate::app::speech::SpeechStatus;

use crate::app::SdroxideApp;

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
    ui.label(RichText::new("Remote access").size(14.0).strong().color(crate::theme::CYAN));
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
                .color(crate::theme::GREEN),
            );
        } else {
            ui.label(RichText::new("Clients must sign in.").size(11.5).color(crate::theme::GREEN));
        }
    } else {
        ui.label(
            RichText::new(
                "⚠ Empty: anyone who can reach the server's port can operate this radio, on \
                 your callsign. Set a password before forwarding the port.",
            )
            .size(11.5)
            .color(crate::theme::YELLOW),
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

/// Spoken announcements.
///
/// Sized so the General tab stays scannable: the controls an operator sets up
/// once are visible, and the two dozen per-category switches live behind a
/// collapsing header. Every value is written as it is changed, like the control
/// bindings — the rate and volume take effect on the next utterance without a
/// restart, and there is no separate step for an APPLY to stand for.
pub(in crate::app) fn speech_settings(
    ui: &mut egui::Ui,
    cfg: &mut SpeechSettings,
    voices: &[String],
    outputs: &[String],
    status: &SpeechStatus,
    test: &mut bool,
) {
    ui.label(RichText::new("Voice announcements").size(14.0).strong().color(crate::theme::CYAN));
    ui.add_space(6.0);
    ui.checkbox(&mut cfg.enabled, "Speak changes to the radio")
        .on_hover_text("Reads out what changed, so the radio can be operated without seeing it");

    ui.add_enabled_ui(cfg.enabled, |ui| {
        egui::Grid::new("speech-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Voice");
            let shown =
                if cfg.voice.is_empty() { "Shipped voice".to_string() } else { cfg.voice.clone() };
            ComboBox::from_id_salt("speech-voice").width(300.0).selected_text(shown).show_ui(
                ui,
                |ui| {
                    if ui.selectable_label(cfg.voice.is_empty(), "Shipped voice").clicked() {
                        cfg.voice.clear();
                    }
                    for v in voices {
                        if ui.selectable_label(&cfg.voice == v, v).clicked() {
                            cfg.voice = v.clone();
                        }
                    }
                },
            );
            ui.end_row();

            ui.label("Speed");
            ui.add(
                egui::Slider::new(&mut cfg.rate, SpeechSettings::RATE_RANGE)
                    .step_by(0.1)
                    .suffix("×"),
            )
            .on_hover_text(
                "The voice stretches or compresses its own phrasing, so the pitch does not \
                 change. Past about 2× it stops getting shorter.",
            );
            ui.end_row();

            ui.label("Volume");
            ui.add(egui::Slider::new(&mut cfg.volume, 0.0..=1.0).step_by(0.05));
            ui.end_row();

            ui.label("Output");
            // `device_combo` borrows the current selection while handing the
            // new one to the closure, so the two cannot both be `cfg.device`.
            let cur = cfg.device.clone();
            device_combo(ui, "speech-out", outputs, &cur, |n| cfg.device = n);
            ui.end_row();

            ui.label("Detail");
            crate::app::settings::enum_combo(
                ui,
                "speech-verbosity",
                &mut cfg.verbosity,
                &Verbosity::ALL,
                Verbosity::label,
            );
            ui.end_row();

            ui.label("Duck receiver");
            ui.horizontal(|ui| {
                ui.checkbox(&mut cfg.duck_rx, "While speaking");
                ui.add_enabled_ui(cfg.duck_rx, |ui| {
                    ui.add(egui::Slider::new(&mut cfg.duck_level, 0.0..=1.0).step_by(0.05));
                });
            });
            ui.end_row();
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Test").clicked() {
                *test = true;
            }
            if let Some(note) = status.note() {
                let text = RichText::new(note);
                ui.label(if status.is_failed() {
                    text.color(Color32::from_rgb(0xE0, 0x6C, 0x4B))
                } else {
                    text.weak()
                });
            }
        });

        ui.add_space(4.0);
        egui::CollapsingHeader::new("What to announce").default_open(false).show(ui, |ui| {
            egui::Grid::new("speech-cats").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                let c = &mut cfg.cat;
                ui.checkbox(&mut c.frequency, "Frequency");
                ui.checkbox(&mut c.mode_band, "Mode and band");
                ui.end_row();
                ui.checkbox(&mut c.vfo_split, "VFO and split");
                ui.checkbox(&mut c.agc_gain, "AGC and gain");
                ui.end_row();
                ui.checkbox(&mut c.levels, "Drive, tune and mic");
                ui.checkbox(&mut c.ptt, "Transmit and receive");
                ui.end_row();
                ui.checkbox(&mut c.rit_xit, "RIT and XIT");
                ui.checkbox(&mut c.memory_scan, "Memories and scanning");
                ui.end_row();
                ui.checkbox(&mut c.band_edge, "Leaving an amateur band");
                ui.checkbox(&mut c.notices, "Warnings and messages");
                ui.end_row();
                ui.checkbox(&mut c.filters, "Filters, squelch and noise reduction")
                    .on_hover_text("Off by default: these move constantly while chasing a signal");
                ui.end_row();
            });

            ui.add_space(8.0);
            ui.label(RichText::new("Decoded messages").strong());
            egui::Grid::new("speech-decodes").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                let d = &mut cfg.decodes;
                ui.checkbox(&mut d.ft8_to_me, "FT8 calls to me");
                ui.checkbox(&mut d.ft8_cq_for_me, "FT8 CQs I could answer").on_hover_text(
                    "A busy evening on twenty metres is a hundred of these a minute",
                );
                ui.end_row();
                ui.checkbox(&mut d.js8, "JS8 messages to me");
                ui.checkbox(&mut d.js8_allcall, "JS8 @ALLCALL too");
                ui.end_row();
                ui.checkbox(&mut d.fsq, "FSQ messages to me");
                ui.checkbox(&mut d.include_snr, "Include the report");
                ui.end_row();
            });

            ui.add_space(8.0);
            ui.label(RichText::new("Reading decoded text aloud").strong());
            ui.horizontal(|ui| {
                ui.checkbox(&mut cfg.text.cw, "CW");
                ui.checkbox(&mut cfg.text.rtty_psk, "RTTY, PSK, Olivia, THOR, FSQ");
            });
            ui.checkbox(&mut cfg.text.cw_only_when_locked, "CW only while the decoder is locked")
                .on_hover_text("Reading an unlocked decoder's output is worse than silence");
            ui.label(
                RichText::new(
                    "Both are off by default. A decoder produces text faster than speech reads \
                     it, so anything that falls too far behind the live audio is dropped rather \
                     than queued.",
                )
                .weak(),
            );

            ui.add_space(8.0);
            ui.label(RichText::new("Tuning up").strong());
            ui.checkbox(&mut cfg.tune.swr_while_tuning, "Read the SWR out while TUNE is held");
            ui.add_enabled_ui(cfg.tune.swr_while_tuning, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Every");
                    ui.add(
                        egui::Slider::new(&mut cfg.tune.period_s, 1.0..=10.0)
                            .step_by(0.5)
                            .suffix(" s"),
                    );
                });
            });
            ui.checkbox(&mut cfg.tune.summary_after_tune, "Report the best match on release");
            ui.checkbox(&mut cfg.tune.alarm_always, "Warn about high SWR during any transmission");

            ui.add_space(8.0);
            ui.label(RichText::new("How things are read").strong());
            egui::Grid::new("speech-style").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Frequencies");
                crate::app::settings::enum_combo(
                    ui,
                    "speech-freq-style",
                    &mut cfg.freq_style,
                    &FreqStyle::ALL,
                    FreqStyle::label,
                );
                ui.end_row();
                ui.label("Callsigns");
                crate::app::settings::enum_combo(
                    ui,
                    "speech-call-style",
                    &mut cfg.callsign_style,
                    &CallsignStyle::ALL,
                    CallsignStyle::label,
                );
                ui.end_row();
            });

            ui.add_space(6.0);
            ui.checkbox(&mut cfg.duck_on_ptt, "Stay quiet while transmitting").on_hover_text(
                "Speech goes to your speakers, and therefore into your microphone. High-SWR \
                 warnings still get through.",
            );
        });
    });

    ui.add_space(8.0);
    ui.label(
        RichText::new(
            "Announcements play on their own sound device, so they are never recorded and never \
             sent to anyone listening remotely. Keys for speaking the status, repeating the last \
             announcement and stopping mid-sentence are on the Controls tab.",
        )
        .weak(),
    );
}
