//! The Radio tab: one section per radio interface.
//!
//! Which section is drawn follows the interface selector above it, so the
//! dialog only ever shows the settings of the backend being configured. The
//! discovery buttons (HPSDR scan, RTL-SDR rescan, TCI connection test) only
//! set a flag — the blocking work happens after the window closure.

use eframe::egui::{self, Color32, ComboBox, DragValue, RichText, Slider};
use sdroxide_types::{Command, Direction};

use crate::app::SdroxideApp;
use crate::app::settings::enum_combo;

/// Why a discovery or test control is greyed out.
///
/// One wording rather than a dozen: every one of these buttons asks a question
/// about a *machine* — what is on its USB bus, which serial ports it has,
/// whether an address answers from where it stands — and on a remote client
/// this screen is not that machine. The settings around them describe the
/// device instead, so they travel.
const NOT_FROM_HERE: &str = "Only from the machine the radio is attached to: this asks about \
                             its hardware and its network, not this screen's.";

/// Draw a control that only works where the radio is, greyed out and explained
/// when this screen is somewhere else.
fn local_only<R>(ui: &mut egui::Ui, local: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
    let group = ui.add_enabled_ui(local, add);
    if !local {
        group.response.on_hover_text(NOT_FROM_HERE);
    }
}

/// CAT / Audio interface: serial + PTT parameters (the interface itself is
/// chosen by the selector in `settings_body`).
pub(in crate::app) fn settings_cat_tab(
    ui: &mut egui::Ui,
    serial_ports: &[String],
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    local: bool,
) {
    use sdroxide_types::{
        CatFamily, CwKeying, DigiMode, KenwoodSend, LineState, ModeControl, Parity, PttMethod,
        SoundFormat, StopBits,
    };
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };
    egui::Grid::new("cat-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Sound format");
        enum_combo(ui, "sfmt", &mut cfg.cat.format, &SoundFormat::ALL, SoundFormat::label);
        ui.end_row();

        if matches!(cfg.cat.format, SoundFormat::DemodAudio) {
            ui.label("Panadapter BW");
            ui.add(
                DragValue::new(&mut cfg.cat.audio_bw_hz)
                    .speed(100.0)
                    .range(1000.0..=24000.0)
                    .suffix(" Hz"),
            );
            ui.end_row();
        }

        ui.label("Serial port");
        let shown = if cfg.cat.serial.path.is_empty() {
            "— select —".to_string()
        } else {
            cfg.cat.serial.path.clone()
        };
        // The list is of *this* machine's ports. Where the rig is elsewhere the
        // stored path is still worth showing — it says which port the engine is
        // using — but there is nothing here to choose from.
        local_only(ui, local, |ui| {
            ComboBox::from_id_salt("serport").width(260.0).selected_text(shown).show_ui(ui, |ui| {
                for p in serial_ports {
                    if ui.selectable_label(&cfg.cat.serial.path == p, p).clicked() {
                        cfg.cat.serial.path = p.clone();
                    }
                }
            });
        });
        ui.end_row();

        ui.label("CAT family");
        enum_combo(ui, "fam", &mut cfg.cat.family, &CatFamily::ALL, CatFamily::label);
        ui.end_row();

        ui.label("Baud");
        ComboBox::from_id_salt("baud").selected_text(cfg.cat.serial.baud.to_string()).show_ui(
            ui,
            |ui| {
                for b in [4800u32, 9600, 19200, 38400, 57600, 115200] {
                    if ui.selectable_label(cfg.cat.serial.baud == b, b.to_string()).clicked() {
                        cfg.cat.serial.baud = b;
                    }
                }
            },
        );
        ui.end_row();

        ui.label("Data bits");
        ComboBox::from_id_salt("databits")
            .selected_text(cfg.cat.serial.data_bits.to_string())
            .show_ui(ui, |ui| {
                for d in [7u8, 8] {
                    if ui.selectable_label(cfg.cat.serial.data_bits == d, d.to_string()).clicked() {
                        cfg.cat.serial.data_bits = d;
                    }
                }
            });
        ui.end_row();

        ui.label("Parity");
        enum_combo(ui, "parity", &mut cfg.cat.serial.parity, &Parity::ALL, Parity::label);
        ui.end_row();

        ui.label("Stop bits");
        enum_combo(ui, "stop", &mut cfg.cat.serial.stop_bits, &StopBits::ALL, StopBits::label);
        ui.end_row();

        ui.label("Force RTS");
        enum_combo(ui, "rts", &mut cfg.cat.serial.force_rts, &LineState::ALL, LineState::label);
        ui.end_row();
        ui.label("Force DTR");
        enum_combo(ui, "dtr", &mut cfg.cat.serial.force_dtr, &LineState::ALL, LineState::label);
        ui.end_row();

        ui.label("PTT method");
        enum_combo(ui, "ptt", &mut cfg.cat.ptt, &PttMethod::ALL, PttMethod::label);
        ui.end_row();

        ui.label("Mode control");
        enum_combo(ui, "modectl", &mut cfg.cat.mode_control, &ModeControl::ALL, ModeControl::label);
        ui.end_row();

        ui.label("Digimode mode");
        enum_combo(ui, "digimode", &mut cfg.cat.digi_mode, &DigiMode::ALL, DigiMode::label);
        ui.end_row();

        ui.label("CW keying").on_hover_text(
            "How the CW panel's keyer transmits. A rig in CW keys its own \
             transmitter and ignores what arrives at its sound card, so \"Rig keyer\" \
             — handing the text to the radio to send — is the only route that puts \
             CW on the air there. It uses the rig's keyer speed (set from the panel's \
             WPM), needs break-in on, and on Yaesu it sends by way of keyer memory 1, \
             overwriting whatever was stored in it.\n\n\
             \"Sound card\" sends the keyed sidetone as audio instead: nothing at all \
             on a rig in CW, and a tone on the sideband (MCW) at dial + pitch if you \
             keep the rig in USB/DATA.",
        );
        enum_combo(ui, "cwkey", &mut cfg.cat.cw_keying, &CwKeying::ALL, CwKeying::label);
        ui.end_row();

        ui.label("Poll rate");
        ui.add(DragValue::new(&mut cfg.cat.poll_hz).speed(0.5).range(0.5..=20.0).suffix(" Hz"));
        ui.end_row();

        if cfg.cat.family == CatFamily::Kenwood {
            ui.label("Send command").on_hover_text(
                "Which transceiver generation keys the rig, for PTT method \
                 \"CAT\". The two disagree about what the TX parameter means \
                 and nothing on the wire tells them apart, so pick the one \
                 that matches your radio.\n\n\
                 \"TS-2000 style (TX;)\" — TS-480, TS-570, TS-870, TS-2000: \
                 the ordinary send, on the main band.\n\n\
                 \"TS-590 style (TX1;)\" — TS-590S/SG, TS-890, TS-990: DATA \
                 SEND, which keys with the ACC2/USB audio input live. On these \
                 rigs the plain send selects the microphone instead and mutes \
                 the audio sdroxide transmits.\n\n\
                 Set wrong, a TS-590 transmits silence — but a TS-2000 \
                 transmits on the sub-band, which is another band entirely.",
            );
            enum_combo(
                ui,
                "kwsend",
                &mut cfg.cat.kenwood_send,
                &KenwoodSend::ALL,
                KenwoodSend::label,
            );
            ui.end_row();
        }

        if matches!(cfg.cat.family, CatFamily::Icom | CatFamily::Xiegu) {
            ui.label("Radio ID (hex)");
            let mut hex = format!("{:02X}", cfg.cat.icom_radio_id);
            let resp = ui.add(egui::TextEdit::singleline(&mut hex).desired_width(48.0));
            if resp.changed() {
                if let Ok(v) = u8::from_str_radix(hex.trim().trim_start_matches("0x"), 16) {
                    cfg.cat.icom_radio_id = v;
                }
            }
            ui.end_row();
        }
    });
    ui.add_space(6.0);
    ui.label(RichText::new("Press \"Apply / reconnect\" to switch without a restart.").weak());
}

/// HPSDR interface: network device discovery / manual IP / sample rate (the
/// interface itself is chosen by the selector in `settings_body`).
pub(in crate::app) fn settings_hpsdr_tab(
    ui: &mut egui::Ui,
    devices: &[sdroxide_types::HpsdrDevice],
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    discover: &mut bool,
    local: bool,
    cmds: &mut Vec<Command>,
) {
    use sdroxide_types::HpsdrConfig;
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };
    egui::Grid::new("hpsdr-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Devices");
        // The scan goes out on this machine's LAN; the radio is on the
        // engine's. The manual IP below is typed, so it still works from here.
        local_only(ui, local, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Discover").clicked() {
                    *discover = true;
                }
                let shown = cfg.hpsdr.selected_ip.clone().unwrap_or_else(|| "— none —".into());
                ComboBox::from_id_salt("hpsdr_dev").width(320.0).selected_text(shown).show_ui(
                    ui,
                    |ui| {
                        if devices.is_empty() {
                            ui.label(RichText::new("no devices — press Discover").weak());
                        }
                        for d in devices {
                            // Both protocols are drivable; anything else is greyed out.
                            if d.supported() {
                                let sel = cfg.hpsdr.selected_ip.as_deref() == Some(d.ip.as_str());
                                if ui.selectable_label(sel, d.label()).clicked() {
                                    cfg.hpsdr.selected_ip = Some(d.ip.clone());
                                }
                            } else {
                                ui.label(RichText::new(d.label()).weak());
                            }
                        }
                    },
                );
            });
        });
        ui.end_row();

        ui.label("Manual IP");
        let mut ip = cfg.hpsdr.manual_ip.clone().unwrap_or_default();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut ip)
                .desired_width(160.0)
                .hint_text("optional, e.g. 192.168.1.50"),
        );
        if resp.changed() {
            let t = ip.trim();
            cfg.hpsdr.manual_ip = if t.is_empty() { None } else { Some(t.to_string()) };
        }
        ui.end_row();

        ui.label("Sample rate");
        // Show only rates valid for the selected device's protocol (P1 ≤ 384 kHz).
        let proto = devices
            .iter()
            .find(|d| Some(d.ip.as_str()) == cfg.hpsdr.selected_ip.as_deref())
            .map(|d| d.protocol)
            .unwrap_or(2);
        let shown = format!("{} kHz", (cfg.hpsdr.sample_rate_hz / 1000.0) as u32);
        ComboBox::from_id_salt("hpsdr_rate").selected_text(shown).show_ui(ui, |ui| {
            for &r in HpsdrConfig::rates_for(proto) {
                let sel = (cfg.hpsdr.sample_rate_hz - r).abs() < 1.0;
                if ui.selectable_label(sel, format!("{} kHz", (r / 1000.0) as u32)).clicked() {
                    cfg.hpsdr.sample_rate_hz = r;
                }
            }
        });
        ui.end_row();

        // Which of the board's DDCs this radio runs. Protocol 2 only — a P1
        // board refuses anything but the first at open, with a clear message.
        // Shown 1-based, stored 0-based as the wire counts.
        ui.label("Receiver (DDC)");
        let shown = format!("DDC{}", cfg.hpsdr.ddc + 1);
        ComboBox::from_id_salt("hpsdr_ddc")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for ddc in 0u8..4 {
                    if ui
                        .selectable_label(cfg.hpsdr.ddc == ddc, format!("DDC{}", ddc + 1))
                        .clicked()
                    {
                        cfg.hpsdr.ddc = ddc;
                    }
                }
            })
            .response
            .on_hover_text(
                "A Protocol 2 board carries several independently tunable receivers (DDCs) on \
                 one connection — run this radio on DDC1 and another radio, same address, on \
                 DDC2. The transmitter belongs to the DDC1 radio. Protocol 1 boards have DDC1 \
                 only.",
            );
        ui.end_row();

        ui.label("LNA gain").on_hover_text(
            "Front-end gain of a Hermes-Lite 2. Takes effect immediately — no reconnect — \
             and is remembered as the level the radio starts at. Too high clips the ADC and \
             the whole band looks distorted; too low and the receiver goes deaf.",
        );
        // Applies live as well as being persisted: this is the gain an operator
        // retunes per band, and making it wait for Apply/reconnect would mean
        // dropping the stream every time they nudge it.
        if crate::chrome::slider(
            ui,
            Slider::new(
                &mut cfg.hpsdr.lna_gain_db,
                HpsdrConfig::LNA_GAIN_MIN_DB..=HpsdrConfig::LNA_GAIN_MAX_DB,
            )
            .step_by(1.0)
            .suffix(" dB"),
        )
        .changed()
        {
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: sdroxide_types::HpsdrConfig::LNA_GAIN_ELEMENT.to_string(),
                db: cfg.hpsdr.lna_gain_db,
            });
        }
        ui.end_row();

        ui.label("Filter board").on_hover_text(
            "Accessory board on the Hermes-Lite 2's J16 header. Leave this at \"None\" \
             unless a filter board is actually fitted: those seven pins are \
             general-purpose open-collector outputs, and operators also wire them to \
             amplifier PTT, antenna relays and transverter switching. Driving them from \
             band data would start operating whatever is connected.",
        );
        ComboBox::from_id_salt("hpsdr_filter")
            .width(220.0)
            .selected_text(cfg.hpsdr.filter_board.label())
            .show_ui(ui, |ui| {
                for b in sdroxide_types::HpsdrFilterBoard::ALL {
                    if ui.selectable_label(cfg.hpsdr.filter_board == b, b.label()).clicked() {
                        cfg.hpsdr.filter_board = b;
                    }
                }
            });
        ui.end_row();

        ui.label("IO board RX input").on_hover_text(
            "Where an N2ADR HL2IOBoard takes its receive signal from. The board itself is found \
             automatically and needs no setting; this one exists only for operators who have \
             wired its own SMA jacks. Leave it at \"Radio's own input\" otherwise — selecting the \
             IO board's J9 with nothing connected to it leaves the receiver deaf. Applies on \
             Apply / reconnect.",
        );
        ComboBox::from_id_salt("hpsdr_io_rx")
            .width(220.0)
            .selected_text(cfg.hpsdr.io_rx_input.label())
            .show_ui(ui, |ui| {
                for m in sdroxide_types::HpsdrIoRxInput::ALL {
                    if ui.selectable_label(cfg.hpsdr.io_rx_input == m, m.label()).clicked() {
                        cfg.hpsdr.io_rx_input = m;
                    }
                }
            });
        ui.end_row();

        ui.label("Power amplifier");
        ui.checkbox(&mut cfg.hpsdr.pa_enable, "Use the Hermes-Lite 2's onboard PA").on_hover_text(
            "On by default, and what you want unless an external amplifier is driven from the \
             board's low-power RF1 output. With it off the radio still keys — the T/R relay \
             throws and any accessory board follows — but the antenna jack makes no power at \
             all, and the relay is deliberately held in receive. Ignored on boards other than a \
             Hermes-Lite.",
        );
        ui.end_row();

        ui.label("Invert spectrum");
        ui.checkbox(&mut cfg.hpsdr.invert_spectrum, "Swap I/Q").on_hover_text(
            "Mirror the board's spectrum about the tuned frequency, on transmit as well \
             as receive. On by default: a Hermes-Lite 2 needs it. Turn it off only if \
             signals show up on the wrong side of the dial and nothing decodes — the \
             giveaway is a waterfall full of convincing traces while SSB lands on the \
             wrong sideband and FT8 returns no decodes at all.",
        );
        ui.end_row();

        ui.label("Frequency correction")
            .on_hover_text("Crystal/TCXO error in ppm, applied to RX and TX. Applies immediately.");
        let mut ppm = cfg.hpsdr.ppm;
        let resp =
            ui.add(egui::DragValue::new(&mut ppm).range(-100.0..=100.0).speed(0.1).suffix(" ppm"));
        if resp.changed() {
            cfg.hpsdr.ppm = ppm;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: HpsdrConfig::PPM_ELEMENT.to_string(),
                db: ppm,
            });
        }
        ui.end_row();
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "A manual IP overrides discovery. Press \"Apply / reconnect\" to switch without a restart.",
        )
        .weak(),
    );
}

/// RTL-SDR interface: which dongle, sample rate, gain/AGC, frequency
/// correction, HF reception and the bias tee.
///
/// Gain, AGC, ppm and the bias tee all apply *live* rather than waiting for
/// Apply/reconnect — these are the controls an operator moves while listening,
/// and dropping the stream on every nudge would make them unusable. The dongle
/// selection and sample rate do need a reconnect, since both are fixed when
/// the device is opened.
pub(in crate::app) fn settings_rtlsdr_tab(
    ui: &mut egui::Ui,
    devices: &[sdroxide_types::RtlSdrDevice],
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    rescan: &mut bool,
    local: bool,
    cmds: &mut Vec<Command>,
) {
    use sdroxide_types::{RtlSdrAgc, RtlSdrConfig, RtlSdrHfMode};
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };

    egui::Grid::new("rtlsdr-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Dongle");
        // Which dongle is the one row here that names a USB bus rather than the
        // radio. Everything below reaches the dongle wherever it is plugged in.
        local_only(ui, local, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("Rescan")
                    .on_hover_text(
                        "Re-list the USB bus. No device is opened, so this is safe \
                         to press while receiving.",
                    )
                    .clicked()
                {
                    *rescan = true;
                }
                let shown =
                    cfg.rtlsdr.serial.clone().unwrap_or_else(|| "— first one found —".into());
                ComboBox::from_id_salt("rtlsdr_dev").width(300.0).selected_text(shown).show_ui(
                    ui,
                    |ui| {
                        if devices.is_empty() {
                            ui.label(RichText::new("no dongles — press Rescan").weak());
                        }
                        if ui
                            .selectable_label(cfg.rtlsdr.serial.is_none(), "— first one found —")
                            .clicked()
                        {
                            cfg.rtlsdr.serial = None;
                        }
                        for d in devices {
                            // Only a dongle with a serial can be pinned; without
                            // one there is nothing stable to remember, since bus
                            // position changes on every replug.
                            if let Some(sn) = &d.serial {
                                let sel = cfg.rtlsdr.serial.as_deref() == Some(sn.as_str());
                                if ui.selectable_label(sel, d.label()).clicked() {
                                    cfg.rtlsdr.serial = Some(sn.clone());
                                }
                            } else {
                                ui.label(RichText::new(d.label()).weak());
                            }
                        }
                    },
                );
            });
        });
        ui.end_row();

        ui.label("Sample rate").on_hover_text(
            "The RTL2832U's resampler reaches 225–300 kHz and 900 kHz–3.2 MHz, \
             nothing between. Takes effect on Apply.",
        );
        let shown = format!("{:.3} Msps", cfg.rtlsdr.sample_rate_hz / 1e6);
        ComboBox::from_id_salt("rtlsdr_rate").selected_text(shown).show_ui(ui, |ui| {
            for &r in &RtlSdrConfig::SAMPLE_RATES {
                let sel = (cfg.rtlsdr.sample_rate_hz - r).abs() < 1.0;
                let mut label = format!("{:.3} Msps", r / 1e6);
                if r >= 3_200_000.0 {
                    label.push_str("  (often drops samples)");
                }
                if ui.selectable_label(sel, label).clicked() {
                    cfg.rtlsdr.sample_rate_hz = r;
                }
            }
        });
        ui.end_row();

        ui.label("AGC").on_hover_text(
            "Manual is the setting for measurement and weak-signal digital modes. \
             The tuner and the demodulator have independent automatic loops.",
        );
        let mut agc = cfg.rtlsdr.agc;
        enum_combo(ui, "rtlsdr_agc", &mut agc, &RtlSdrAgc::ALL, RtlSdrAgc::label);
        if agc != cfg.rtlsdr.agc {
            cfg.rtlsdr.agc = agc;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: RtlSdrConfig::AGC_ELEMENT.to_string(),
                db: agc.code() as f64,
            });
        }
        ui.end_row();

        ui.label("Tuner gain").on_hover_text(
            "Applies immediately — no reconnect. The tuner has 29 discrete steps, \
             so the value snaps to the nearest one it can produce. Ignored while \
             the tuner AGC is running.",
        );
        ui.add_enabled_ui(!cfg.rtlsdr.agc.tuner_auto(), |ui| {
            if crate::chrome::slider(
                ui,
                Slider::new(&mut cfg.rtlsdr.tuner_gain_db, 0.0..=RtlSdrConfig::GAIN_MAX_DB)
                    .step_by(0.1)
                    .suffix(" dB"),
            )
            .changed()
            {
                cmds.push(Command::SetGain {
                    dir: Direction::Rx,
                    element: RtlSdrConfig::TUNER_GAIN_ELEMENT.to_string(),
                    db: cfg.rtlsdr.tuner_gain_db,
                });
            }
        });
        ui.end_row();

        ui.label("Frequency correction").on_hover_text(
            "Crystal error in parts per million. Run with \
             RUST_LOG=sdroxide_rtlsdr=debug and the log prints the measured \
             clock error after about 20 seconds — that is the number to enter. \
             Applies immediately.",
        );
        let mut ppm = cfg.rtlsdr.ppm;
        if ui.add(egui::DragValue::new(&mut ppm).range(-200..=200).suffix(" ppm")).changed() {
            cfg.rtlsdr.ppm = ppm;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: RtlSdrConfig::PPM_ELEMENT.to_string(),
                db: ppm as f64,
            });
        }
        ui.end_row();

        ui.label("HF reception").on_hover_text(
            "The tuner itself starts at 24 MHz. An RTL-SDR Blog V4 upconverts \
             below that in hardware; other dongles reach HF only by sampling the \
             ADC directly, through the V3's HF port. Switching modes briefly \
             interrupts the stream.",
        );
        let mut hf = cfg.rtlsdr.hf_mode;
        enum_combo(ui, "rtlsdr_hf", &mut hf, &RtlSdrHfMode::ALL, RtlSdrHfMode::label);
        if hf != cfg.rtlsdr.hf_mode {
            cfg.rtlsdr.hf_mode = hf;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: RtlSdrConfig::HF_MODE_ELEMENT.to_string(),
                db: hf as u8 as f64,
            });
        }
        ui.end_row();

        ui.label("IQ correction").on_hover_text(
            "Removes the dongle's own DC spike from the centre of the span, and \
             the mirror image every signal leaves reflected about it, by \
             measuring the imbalance in the samples themselves — no calibration, \
             and it applies immediately. The tuner has no offset-tuning mode, so \
             this is the only way to clear the centre.\n\n\
             An AM carrier tuned dead on the dial is at DC too, so it goes with \
             the spike: tune a kilohertz off it, or switch this off.",
        );
        let mut iq = cfg.rtlsdr.iq_correction;
        if ui.checkbox(&mut iq, "Remove the centre spike and mirror image").changed() {
            cfg.rtlsdr.iq_correction = iq;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: RtlSdrConfig::IQ_CORRECTION_ELEMENT.to_string(),
                db: if iq { 1.0 } else { 0.0 },
            });
        }
        ui.end_row();

        ui.label("Bias tee");
        let mut bias = cfg.rtlsdr.bias_tee;
        if ui.checkbox(&mut bias, "Feed ~4.5 V DC up the coax").changed() {
            cfg.rtlsdr.bias_tee = bias;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: RtlSdrConfig::BIAS_TEE_ELEMENT.to_string(),
                db: if bias { 1.0 } else { 0.0 },
            });
        }
        ui.end_row();
    });

    if cfg.rtlsdr.bias_tee {
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Bias tee is ON. Never connect a transceiver, a grounded antenna, \
                 or a preamp powered from elsewhere while this is enabled — the DC \
                 goes straight down the feedline.",
            )
            .color(crate::theme::YELLOW()),
        );
    }

    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Receive only. The dongle and sample rate take effect on Apply; \
             everything else applies as you change it.",
        )
        .weak(),
    );
}

/// rtl_tcp interface: the same dongle as the tab above, on another machine.
///
/// Deliberately the same controls in the same order — an operator who moves a
/// dongle from this machine to a Raspberry Pi on the mast should not have to
/// learn a second panel. What differs is at the top (an address, not a USB
/// serial) and in the hover text, which has to say *whose* hardware each knob
/// reaches: everything here is performed by the server, and nothing it does
/// with the request is ever reported back.
pub(in crate::app) fn settings_rtltcp_tab(
    ui: &mut egui::Ui,
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    cmds: &mut Vec<Command>,
) {
    use sdroxide_types::{RtlSdrAgc, RtlSdrConfig, RtlSdrHfMode, RtlTcpConfig};
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };

    egui::Grid::new("rtltcp-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Server address").on_hover_text(
            "Where rtl_tcp is listening: an address, or an address and port. \
             The port defaults to 1234, which is rtl_tcp's own default.\n\n\
             On the far end, start it as `rtl_tcp -a 0.0.0.0` — bound to \
             127.0.0.1, which is what it does with no -a, it only accepts \
             connections from that same machine.\n\nTakes effect on Apply.",
        );
        ui.add(
            egui::TextEdit::singleline(&mut cfg.rtltcp.address)
                .desired_width(220.0)
                .hint_text("host or host:port, e.g. raspberrypi.local:1234"),
        );
        ui.end_row();

        ui.label("Sample rate").on_hover_text(
            "Requested of the server, with the same resampler limits as a local \
             dongle — it is the same silicon on the far end.\n\n\
             The figure beside each rate is what it costs on the link: the \
             samples are sent uncompressed, and a rate the network cannot carry \
             makes rtl_tcp drop the connection rather than degrade. \
             Takes effect on Apply.",
        );
        let shown = format!(
            "{:.3} Msps  —  {:.0} Mbit/s",
            cfg.rtltcp.sample_rate_hz / 1e6,
            RtlTcpConfig::link_mbit(cfg.rtltcp.sample_rate_hz),
        );
        ComboBox::from_id_salt("rtltcp_rate").width(260.0).selected_text(shown).show_ui(ui, |ui| {
            for &r in &RtlSdrConfig::SAMPLE_RATES {
                let sel = (cfg.rtltcp.sample_rate_hz - r).abs() < 1.0;
                let mbit = RtlTcpConfig::link_mbit(r);
                let mut label = format!("{:.3} Msps  —  {mbit:.0} Mbit/s", r / 1e6);
                // The threshold is where a rate stops fitting comfortably in
                // what a single WiFi hop delivers in practice, which is well
                // under its nominal rate.
                if mbit >= 30.0 {
                    label.push_str("  (wired link)");
                }
                if ui.selectable_label(sel, label).clicked() {
                    cfg.rtltcp.sample_rate_hz = r;
                }
            }
        });
        ui.end_row();

        ui.label("AGC").on_hover_text(
            "Runs on the server's dongle. Manual is the setting for measurement \
             and weak-signal digital modes.",
        );
        let mut agc = cfg.rtltcp.agc;
        enum_combo(ui, "rtltcp_agc", &mut agc, &RtlSdrAgc::ALL, RtlSdrAgc::label);
        if agc != cfg.rtltcp.agc {
            cfg.rtltcp.agc = agc;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: RtlSdrConfig::AGC_ELEMENT.to_string(),
                db: agc.code() as f64,
            });
        }
        ui.end_row();

        ui.label("Tuner gain").on_hover_text(
            "Applies immediately — no reconnect. Sent in tenths of a dB and \
             snapped by the server to a step its tuner has; the protocol has no \
             replies, so what it settled on cannot be read back and this slider \
             keeps showing what was asked for. Ignored while the tuner AGC is \
             running.",
        );
        ui.add_enabled_ui(!cfg.rtltcp.agc.tuner_auto(), |ui| {
            if crate::chrome::slider(
                ui,
                Slider::new(&mut cfg.rtltcp.tuner_gain_db, 0.0..=RtlSdrConfig::GAIN_MAX_DB)
                    .step_by(0.1)
                    .suffix(" dB"),
            )
            .changed()
            {
                cmds.push(Command::SetGain {
                    dir: Direction::Rx,
                    element: RtlSdrConfig::TUNER_GAIN_ELEMENT.to_string(),
                    db: cfg.rtltcp.tuner_gain_db,
                });
            }
        });
        ui.end_row();

        ui.label("Frequency correction").on_hover_text(
            "Crystal error of the *server's* dongle, in parts per million — a \
             property of that hardware, so it is set here and not on this \
             machine's dongles. Applies immediately.\n\n\
             The measured clock error the USB interface prints is not available \
             here: over a network what that measurement sees is the buffering, \
             not the crystal, and it is wrong by thousands of ppm. Calibrate the \
             dongle on USB once and carry the number over, or tune a broadcast \
             station of known frequency and adjust until it sits on the dial.",
        );
        let mut ppm = cfg.rtltcp.ppm;
        if ui.add(egui::DragValue::new(&mut ppm).range(-200..=200).suffix(" ppm")).changed() {
            cfg.rtltcp.ppm = ppm;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: RtlSdrConfig::PPM_ELEMENT.to_string(),
                db: ppm as f64,
            });
        }
        ui.end_row();

        ui.label("HF reception").on_hover_text(
            "The tuner starts at 24 MHz; below that the far end needs help. A \
             Blog V4 upconverts by itself and Automatic leaves it alone — which \
             is the only thing it can do, since the protocol reports the tuner \
             chip and nothing else, and a V4 is indistinguishable from a plain \
             R828D over the wire.\n\n\
             On a V3 or any other dongle, Automatic switches the server to \
             direct sampling below the crossover. Choose Direct sampling \
             explicitly for a plain R828D that hears nothing on HF. Switching \
             briefly interrupts the stream.",
        );
        let mut hf = cfg.rtltcp.hf_mode;
        enum_combo(ui, "rtltcp_hf", &mut hf, &RtlSdrHfMode::ALL, RtlSdrHfMode::label);
        if hf != cfg.rtltcp.hf_mode {
            cfg.rtltcp.hf_mode = hf;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: RtlSdrConfig::HF_MODE_ELEMENT.to_string(),
                db: hf as u8 as f64,
            });
        }
        ui.end_row();

        ui.label("IQ correction").on_hover_text(
            "Removes the dongle's DC spike and mirror image here, from the \
             samples as they arrive — they are artefacts of the hardware, so \
             they travel over the network with everything else. Applies \
             immediately.\n\n\
             An AM carrier tuned dead on the dial sits at DC too, so it goes \
             with the spike: tune a kilohertz off it, or switch this off.",
        );
        let mut iq = cfg.rtltcp.iq_correction;
        if ui.checkbox(&mut iq, "Remove the centre spike and mirror image").changed() {
            cfg.rtltcp.iq_correction = iq;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: RtlSdrConfig::IQ_CORRECTION_ELEMENT.to_string(),
                db: if iq { 1.0 } else { 0.0 },
            });
        }
        ui.end_row();

        ui.label("Bias tee").on_hover_text(
            "Powers a preamp from the far end's dongle. Older servers do not \
             implement the command and ignore it silently — the protocol has no \
             way to say no — so a bias tee that does not come on is not \
             necessarily this end's doing.",
        );
        let mut bias = cfg.rtltcp.bias_tee;
        if ui.checkbox(&mut bias, "Feed ~4.5 V DC up the remote coax").changed() {
            cfg.rtltcp.bias_tee = bias;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: RtlSdrConfig::BIAS_TEE_ELEMENT.to_string(),
                db: if bias { 1.0 } else { 0.0 },
            });
        }
        ui.end_row();
    });

    if cfg.rtltcp.bias_tee {
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Bias tee is ON, on hardware that is somewhere else. Whatever is \
                 on the other end of that feedline — a transceiver, a grounded \
                 antenna, a preamp already powered — is not in front of you to \
                 check.",
            )
            .color(crate::theme::YELLOW()),
        );
    }

    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Receive only. The address and sample rate take effect on Apply; \
             everything else applies as you change it. A dropped connection is \
             retried on its own, so a server that is restarted comes back \
             without touching anything here.",
        )
        .weak(),
    );
}

/// TCI interface: WebSocket server address, IQ sample rate, and a
/// Test-connection button (the interface is chosen by the selector in
/// `settings_body`).
pub(in crate::app) fn settings_tci_tab(
    ui: &mut egui::Ui,
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    tci_test: &mut bool,
    test_result: &Option<Result<String, String>>,
    local: bool,
) {
    use sdroxide_types::TciConfig;
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };
    egui::Grid::new("tci-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Server address");
        ui.add(
            egui::TextEdit::singleline(&mut cfg.tci.address)
                .desired_width(220.0)
                .hint_text("host:port, e.g. 127.0.0.1:50001"),
        );
        ui.end_row();

        ui.label("IQ sample rate");
        let shown = format!("{} kHz", (cfg.tci.iq_sample_rate_hz / 1000.0) as u32);
        ComboBox::from_id_salt("tci_rate").selected_text(shown).show_ui(ui, |ui| {
            for &r in &TciConfig::IQ_RATES {
                let sel = (cfg.tci.iq_sample_rate_hz - r).abs() < 1.0;
                if ui.selectable_label(sel, format!("{} kHz", (r / 1000.0) as u32)).clicked() {
                    cfg.tci.iq_sample_rate_hz = r;
                }
            }
        });
        ui.end_row();

        // Which of the rig's receivers this radio runs. Offered as the two a
        // SunSDR2DX has; the rig reports its real count when the connection
        // opens, and asking for one it doesn't have is refused with that
        // count. Shown 1-based, stored 0-based as the wire counts.
        ui.label("Receiver");
        let shown = format!("RX{}", cfg.tci.rx + 1);
        ComboBox::from_id_salt("tci_rx")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for rx in 0u32..2 {
                    if ui.selectable_label(cfg.tci.rx == rx, format!("RX{}", rx + 1)).clicked() {
                        cfg.tci.rx = rx;
                    }
                }
            })
            .response
            .on_hover_text(
                "A rig with two receivers (SunSDR2DX) can serve two radio tabs from one \
                 connection — run this radio on RX1 and another on RX2. The transmitter \
                 belongs to the RX1 radio.",
            );
        ui.end_row();

        ui.label("");
        // The test opens its own socket from wherever it is pressed, so a
        // green answer here would only say this screen can reach the rig — a
        // different question from the one being asked.
        local_only(ui, local, |ui| {
            if ui.button("Test connection").clicked() {
                *tci_test = true;
            }
        });
        ui.end_row();
    });
    test_result_line(ui, test_result);
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Wideband IQ receive, audio transmit. Press \"Apply / reconnect\" to switch without a restart.",
        )
        .weak(),
    );
}

/// Settings → Radio for an Icom on its LAN or WiFi port.
///
/// No Discover button: an Icom does not announce itself on the network, so the
/// address is always typed in.
pub(in crate::app) fn settings_icomnet_tab(
    ui: &mut egui::Ui,
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    test: &mut bool,
    copy_report: &mut bool,
    test_result: &Option<Result<String, String>>,
    local: bool,
) {
    use sdroxide_types::{CwKeying, IcomNetConfig, IcomRxSource};
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };
    let net = &mut cfg.icomnet;

    egui::Grid::new("icomnet-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Radio address").on_hover_text(
            "The address shown on the radio under SET > Network. Network Control has to \
             be on there, and the radio needs a network user name and password set.",
        );
        ui.add(
            egui::TextEdit::singleline(&mut net.address)
                .desired_width(220.0)
                .hint_text("host or IP, e.g. 192.168.1.50"),
        );
        ui.end_row();

        ui.label("Control port");
        ui.add(egui::DragValue::new(&mut net.control_port).range(1..=65535))
            .on_hover_text("50001 unless it has been changed on the radio.");
        ui.end_row();

        ui.label("Network user");
        ui.add(egui::TextEdit::singleline(&mut net.username).desired_width(220.0));
        ui.end_row();

        ui.label("Password").on_hover_text(
            "Stored in the clear in radio.json. The protocol obfuscates it reversibly on \
             the wire, so nothing here would make it a secret.",
        );
        ui.add(egui::TextEdit::singleline(&mut net.password).password(true).desired_width(220.0));
        ui.end_row();

        ui.label("Receive from").on_hover_text(
            "AF: the radio demodulates and sdroxide shows the audio band. \
             12 kHz IF: the radio sends its DRM intermediate frequency instead and \
             sdroxide demodulates, which brings its own filters, noise reduction and \
             decoders to bear over about ±12 kHz. Either way the wide waterfall is the \
             radio's own scope — no Icom outputs I/Q.",
        );
        ComboBox::from_id_salt("icomnet_rx_source").selected_text(net.rx_source.label()).show_ui(
            ui,
            |ui| {
                for s in IcomRxSource::ALL {
                    if ui.selectable_label(net.rx_source == s, s.label()).clicked() {
                        net.rx_source = s;
                    }
                }
            },
        );
        ui.end_row();

        ui.label("Audio sample rate");
        ComboBox::from_id_salt("icomnet_rate")
            .selected_text(format!("{} Hz", net.sample_rate_hz))
            .show_ui(ui, |ui| {
                for r in IcomNetConfig::SAMPLE_RATES {
                    if ui.selectable_label(net.sample_rate_hz == r, format!("{r} Hz")).clicked() {
                        net.sample_rate_hz = r;
                    }
                }
            });
        ui.end_row();

        // The 12 kHz IF cannot fit below 48 kHz, and silently falling back to AF
        // without saying so would look like the setting had not taken.
        if net.rx_source == IcomRxSource::If12k && !net.if_mode_usable() {
            ui.label("");
            ui.colored_label(
                egui::Color32::from_rgb(0xd0, 0x90, 0x30),
                "A 12 kHz IF needs the 48000 Hz stream — at this rate the radio's \
                 demodulated audio is used instead.",
            );
            ui.end_row();
        }

        if net.rx_source == IcomRxSource::Af {
            ui.label("Displayed bandwidth");
            ui.add(
                egui::DragValue::new(&mut net.audio_bw_hz).range(1000.0..=24_000.0).suffix(" Hz"),
            )
            .on_hover_text("Width of the audio-band panadapter, as for a CAT rig.");
            ui.end_row();
        }

        ui.label("CW keying");
        ComboBox::from_id_salt("icomnet_cw").selected_text(net.cw_keying.label()).show_ui(
            ui,
            |ui| {
                for k in CwKeying::ALL {
                    if ui.selectable_label(net.cw_keying == k, k.label()).clicked() {
                        net.cw_keying = k;
                    }
                }
            },
        );
        ui.end_row();

        ui.label("Transmit buffer");
        ui.add(egui::DragValue::new(&mut net.tx_latency_ms).range(20..=1000).suffix(" ms"))
            .on_hover_text(
                "How much audio the radio holds before modulating. More survives a worse \
                 network, at the cost of transmit latency.",
            );
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut net.scope, "Show the radio's spectrum scope").on_hover_text(
            "Streams the radio's own 475-bin sweep into the full-band waterfall. It is \
             the radio's picture, not sdroxide's DSP — there is no I/Q to compute one \
             from.",
        );
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut net.set_mod_input_on_open, "Switch modulation input to LAN")
            .on_hover_text(
                "Transmit audio is only heard when the radio's MOD input is set to LAN. \
                 sdroxide can write that on a model whose menu numbering it knows; on any \
                 other it says so and leaves the menu alone.",
            );
        ui.end_row();

        ui.label("");
        // Both reach for this machine: the test opens its own socket from here,
        // and the trace is of the session *this* process ran. The engine's own
        // is on the engine's machine.
        local_only(ui, local, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Test connection").clicked() {
                    *test = true;
                }
                if ui
                    .button("Copy diagnostic report")
                    .on_hover_text(
                        "Copies the last session's handshake and CI-V trace to the clipboard, \
                         for a bug report.",
                    )
                    .clicked()
                {
                    *copy_report = true;
                }
            });
        });
        ui.end_row();
    });

    test_result_line(ui, test_result);

    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Control, audio and the radio's own scope over one network connection — no \
             serial cable and no sound card. Press \"Apply / reconnect\" to switch without \
             a restart.",
        )
        .weak(),
    );
    ui.label(
        RichText::new(sdroxide_icomnet_hint())
            .weak()
            .color(egui::Color32::from_rgb(0xd0, 0x90, 0x30)),
    );
}

/// Kept as a function so the UI crate needs no dependency on the backend crate
/// just to repeat one sentence.
fn sdroxide_icomnet_hint() -> &'static str {
    "Not verified against hardware yet: if it misbehaves, please attach the diagnostic \
     report to a bug report."
}

/// The outcome line under a "Test connection" button.
///
/// A successful test gets a second, weak line pointing at Apply / reconnect:
/// the test opens its own short-lived connection and the engine keeps running
/// whatever interface it had, but a green "Connected" on its own reads as
/// "done". A field report came from exactly that gap — a tested Pluto, an
/// unpressed Apply, and a blank screen.
fn test_result_line(ui: &mut egui::Ui, result: &Option<Result<String, String>>) {
    match result {
        Some(Ok(s)) => {
            ui.label(
                RichText::new(format!("Connected: {s}")).color(Color32::from_rgb(90, 200, 110)),
            );
            ui.label(
                RichText::new(
                    "That was only a check — press Apply / reconnect below to start \
                     using this radio.",
                )
                .weak(),
            );
        }
        Some(Err(e)) => {
            ui.label(RichText::new(format!("Failed: {e}")).color(Color32::from_rgb(230, 90, 80)));
        }
        None => {}
    }
}

/// PlutoSDR interface: address, front-end settings, and the diagnostic report.
///
/// Two things about the layout are deliberate. The gain, AGC and ppm controls
/// apply *as you move them* (they push `SetGain` straight through), while the
/// address, sample rate and filter wait for Apply — the first group are things
/// you adjust while listening to a signal, the second are things that rebuild
/// the stream. And the tuning range is not stated here at all: a stock AD9363
/// board and one unlocked to AD9364 differ by an octave and a half, so the
/// number comes from the device, through Test connection.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn settings_pluto_tab(
    ui: &mut egui::Ui,
    devices: &[sdroxide_types::PlutoDevice],
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    discover: &mut bool,
    test: &mut bool,
    copy_report: &mut bool,
    test_result: &Option<Result<String, String>>,
    local: bool,
    cmds: &mut Vec<Command>,
) {
    use sdroxide_types::{PlutoAgc, PlutoConfig};
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };

    egui::Grid::new("pluto-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Radios").on_hover_text(
            "Asks the network for IIO devices, and also tries 192.168.2.1 directly — \
             a Pluto on the end of a USB cable is often unreachable by multicast even \
             though the address works.",
        );
        // The mDNS query and the USB-gadget probe both go out from here, and a
        // Pluto on a USB cable is only reachable from the machine it is plugged
        // into. The Address row below is typed, so it still works from here.
        local_only(ui, local, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Discover").clicked() {
                    *discover = true;
                }
                let shown = cfg.pluto.selected_ip.clone().unwrap_or_else(|| "— none —".into());
                ComboBox::from_id_salt("pluto_dev").width(340.0).selected_text(shown).show_ui(
                    ui,
                    |ui| {
                        if devices.is_empty() {
                            ui.label(RichText::new("no radios — press Discover").weak());
                        }
                        for d in devices {
                            let sel = cfg.pluto.selected_ip.as_deref() == Some(d.ip.as_str());
                            if ui.selectable_label(sel, d.label()).clicked() {
                                cfg.pluto.selected_ip = Some(d.ip.clone());
                                // The typed address wins over a selection, so a
                                // click here has no visible effect until it is
                                // cleared. Do that for the operator.
                                cfg.pluto.address.clear();
                            }
                        }
                    },
                );
            });
        });
        ui.end_row();

        ui.label("Address").on_hover_text(
            "Overrides the selection above. The USB cable presents the Pluto as a \
             network adapter, not a serial port, so this is an IP address even when \
             the radio is on your desk.",
        );
        ui.add(
            egui::TextEdit::singleline(&mut cfg.pluto.address)
                .desired_width(220.0)
                .hint_text(PlutoConfig::DEFAULT_ADDRESS),
        );
        ui.end_row();

        // Which receive chain of the AD9361 this radio runs. Unlike TCI or
        // HPSDR the chains are not independently tunable — one synthesiser
        // serves both — so RX2 is a second *antenna*, not a second frequency.
        ui.label("Receiver");
        let shown = format!("RX{}", cfg.pluto.rx + 1);
        ComboBox::from_id_salt("pluto_rx")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for rx in 0u8..2 {
                    if ui.selectable_label(cfg.pluto.rx == rx, format!("RX{}", rx + 1)).clicked() {
                        cfg.pluto.rx = rx;
                    }
                }
            })
            .response
            .on_hover_text(
                "A Pluto+ or a revision-C Pluto unlocked to 2R2T can serve two radio \
                 tabs from one box — this radio on RX1 and another on RX2, each on its \
                 own antenna. The two chains share the one oscillator, so retuning \
                 either radio moves both; what RX2 buys is a second antenna on the \
                 same spectrum (diversity), not a second band. The transmitter belongs \
                 to the RX1 radio. A stock 1R1T Pluto refuses RX2 when it connects.",
            );
        ui.end_row();

        ui.label("Sample rate").on_hover_text(
            "Width of the spectrum sdroxide receives. The AD9361 reaches 61.44 Msps; \
             the USB network link does not, which is what this list is scaled to. \
             The lowest rates need a filter configuration loaded into the AD9361, \
             which sdroxide does not do — a stock Pluto runs them at about 2.084 \
             Msps instead and says so when it connects. Takes effect on Apply.",
        );
        let shown = format!("{:.3} Msps", cfg.pluto.sample_rate_hz / 1e6);
        ComboBox::from_id_salt("pluto_rate").selected_text(shown).show_ui(ui, |ui| {
            for &r in &PlutoConfig::SAMPLE_RATES {
                let sel = (cfg.pluto.sample_rate_hz - r).abs() < 1.0;
                let mut label = format!("{:.3} Msps", r / 1e6);
                if r >= 3_840_000.0 {
                    label.push_str("  (more than USB 2 will carry)");
                } else if r < PlutoConfig::NO_FIR_FLOOR_HZ {
                    label.push_str("  (a stock Pluto runs at 2.084)");
                }
                if ui.selectable_label(sel, label).clicked() {
                    cfg.pluto.sample_rate_hz = r;
                }
            }
        });
        ui.end_row();

        ui.label("Analog filter").on_hover_text(
            "The AD9361's baseband filter. Leave at 0 for automatic, which opens it \
             to nine tenths of the sample rate — wide on purpose, because the \
             receiver parks its oscillator a quarter of a span off the dial to keep \
             signals clear of the DC spike, and a narrow filter would cut off exactly \
             the part it moved them to. Takes effect on Apply.",
        );
        let mut bw_khz = (cfg.pluto.rf_bandwidth_hz / 1000.0).round() as i64;
        if ui
            .add(DragValue::new(&mut bw_khz).range(0..=56_000).suffix(" kHz").custom_formatter(
                |v, _| {
                    if v <= 0.0 { "auto".to_string() } else { format!("{v:.0}") }
                },
            ))
            .changed()
        {
            cfg.pluto.rf_bandwidth_hz = bw_khz.max(0) as f64 * 1000.0;
        }
        ui.end_row();

        ui.label("AGC").on_hover_text(
            "The AD9361 has four modes, not an on/off switch. Slow attack suits SSB \
             and CW; fast attack suits bursty signals; manual is the setting for \
             measurement and weak-signal digital modes. Applies immediately.",
        );
        let mut agc = cfg.pluto.agc;
        enum_combo(ui, "pluto_agc", &mut agc, &PlutoAgc::ALL, PlutoAgc::label);
        if agc != cfg.pluto.agc {
            cfg.pluto.agc = agc;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: PlutoConfig::AGC_ELEMENT.to_string(),
                db: agc.code(),
            });
        }
        ui.end_row();

        ui.label("RX gain").on_hover_text(
            "Applies immediately — no reconnect. Ignored unless the AGC is set to \
             manual, which is the AD9361's own behaviour, not sdroxide's.",
        );
        ui.add_enabled_ui(cfg.pluto.agc == PlutoAgc::Manual, |ui| {
            if crate::chrome::slider(
                ui,
                Slider::new(&mut cfg.pluto.rx_gain_db, 0.0..=71.0).step_by(1.0).suffix(" dB"),
            )
            .changed()
            {
                cmds.push(Command::SetGain {
                    dir: Direction::Rx,
                    element: PlutoConfig::RF_GAIN_ELEMENT.to_string(),
                    db: cfg.pluto.rx_gain_db,
                });
            }
        });
        ui.end_row();

        ui.label("TX gain").on_hover_text(
            "Negative because the AD9361 expresses transmit level as attenuation: \
             0 dB is full output. Applies immediately. The transmitter is set to its \
             quietest before this value is applied on connect, so nothing the \
             previous program left behind can be live.",
        );
        if crate::chrome::slider(
            ui,
            Slider::new(&mut cfg.pluto.tx_gain_db, -89.75..=0.0).step_by(0.25).suffix(" dB"),
        )
        .changed()
        {
            cmds.push(Command::SetGain {
                dir: Direction::Tx,
                element: PlutoConfig::TX_GAIN_ELEMENT.to_string(),
                db: cfg.pluto.tx_gain_db,
            });
        }
        ui.end_row();

        ui.label("Frequency correction").on_hover_text(
            "Reference error in parts per million. Run with \
             RUST_LOG=sdroxide_pluto=debug and the log prints the measured clock \
             error after about 20 seconds — that is the number to enter. Applied by \
             sdroxide, not written to the radio's own persistent trim. Applies \
             immediately.",
        );
        let mut ppm = cfg.pluto.ppm;
        if ui
            .add(DragValue::new(&mut ppm).range(-200.0..=200.0).speed(0.1).suffix(" ppm"))
            .changed()
        {
            cfg.pluto.ppm = ppm;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: PlutoConfig::PPM_ELEMENT.to_string(),
                db: ppm,
            });
        }
        ui.end_row();

        ui.label("RX / TX port").on_hover_text(
            "The AD9361's rf_port_select. A stock Pluto wires one of each, so leave \
             these empty unless you have a board that does not. Takes effect on Apply.",
        );
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut cfg.pluto.rx_port)
                    .desired_width(120.0)
                    .hint_text("A_BALANCED"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut cfg.pluto.tx_port)
                    .desired_width(80.0)
                    .hint_text("A"),
            );
        });
        ui.end_row();

        ui.label("");
        // Both run here: the test opens the radio from this machine, and the
        // trace is of this process's own session.
        local_only(ui, local, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("Test connection")
                    .on_hover_text(
                        "Opens the radio, reads what it says about itself, and reports the \
                         tuning range this particular board has. Does not start a stream.",
                    )
                    .clicked()
                {
                    *test = true;
                }
                if ui
                    .button("Copy diagnostic report")
                    .on_hover_text(
                        "Copies the last session's protocol trace to the clipboard, for a \
                         bug report.",
                    )
                    .clicked()
                {
                    *copy_report = true;
                }
            });
        });
        ui.end_row();
    });

    test_result_line(ui, test_result);

    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Wideband IQ receive and transmit. Half duplex — receive stops for the \
             length of an over, because the USB network link will not carry both at \
             once. No SoapySDR and no libiio needed.",
        )
        .weak(),
    );
    ui.label(
        RichText::new(
            "Not yet verified against real hardware. If it misbehaves, please send the \
             diagnostic report — it contains every command exchanged with the radio, \
             and the first bytes of the sample stream.",
        )
        .color(crate::theme::YELLOW()),
    );
}

/// SmartSDR (FlexRadio) interface: radio selection, DAX IQ stream settings, and
/// the diagnostic report.
///
/// The report button is not decoration. This backend has never been run against
/// a FLEX, so the first people to use it are the ones who can say whether it
/// works — and asking them to reproduce a fault with the right `RUST_LOG`
/// filter set is asking them to reproduce it twice. The trace is always
/// recorded; this copies it out.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn settings_smartsdr_tab(
    ui: &mut egui::Ui,
    devices: &[sdroxide_types::SmartSdrDevice],
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    discover: &mut bool,
    test: &mut bool,
    copy_report: &mut bool,
    test_result: &Option<Result<String, String>>,
    local: bool,
) {
    use sdroxide_types::SmartSdrConfig;
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };

    egui::Grid::new("smartsdr-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Radios").on_hover_text(
            "A FlexRadio announces itself on the local network about once a second. \
             A radio reached through a router or a VPN never broadcasts to you — \
             enter its address below instead.",
        );
        // The broadcasts a FLEX sends reach its own network segment, which is
        // the engine's, not this screen's. The Address row below is typed, so
        // it still works from here.
        local_only(ui, local, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Discover").clicked() {
                    *discover = true;
                }
                let shown = cfg.smartsdr.selected_ip.clone().unwrap_or_else(|| "— none —".into());
                ComboBox::from_id_salt("flex_dev").width(340.0).selected_text(shown).show_ui(
                    ui,
                    |ui| {
                        if devices.is_empty() {
                            ui.label(RichText::new("no radios — press Discover").weak());
                        }
                        for d in devices {
                            let sel = cfg.smartsdr.selected_ip.as_deref() == Some(d.ip.as_str());
                            // A radio that is already claimed and has multiFLEX off
                            // will refuse us, so it is shown but not selectable.
                            if d.joinable {
                                if ui.selectable_label(sel, d.label()).clicked() {
                                    cfg.smartsdr.selected_ip = Some(d.ip.clone());
                                }
                            } else {
                                ui.label(RichText::new(d.label()).weak()).on_hover_text(
                                    "Another GUI client has this radio and multiFLEX is \
                                     disabled. Disconnect that client, or enable multiFLEX \
                                     on the radio.",
                                );
                            }
                        }
                    },
                );
            });
        });
        ui.end_row();

        ui.label("Address").on_hover_text(
            "Overrides the selection above. Use this for a radio on another subnet, \
             behind a VPN, or on a non-standard port.",
        );
        ui.add(
            egui::TextEdit::singleline(&mut cfg.smartsdr.address)
                .desired_width(220.0)
                .hint_text("optional, e.g. 192.168.1.50"),
        );
        ui.end_row();

        ui.label("IQ sample rate").on_hover_text(
            "Width of the spectrum sdroxide receives. 192 kHz is the radio's maximum \
             for a DAX IQ stream, and so the widest span this interface can show.",
        );
        let shown = format!("{} kHz", (cfg.smartsdr.iq_sample_rate_hz / 1000.0) as u32);
        ComboBox::from_id_salt("flex_rate").selected_text(shown).show_ui(ui, |ui| {
            for &r in &SmartSdrConfig::IQ_RATES {
                let sel = (cfg.smartsdr.iq_sample_rate_hz - r).abs() < 1.0;
                if ui.selectable_label(sel, format!("{} kHz", (r / 1000.0) as u32)).clicked() {
                    cfg.smartsdr.iq_sample_rate_hz = r;
                }
            }
        });
        ui.end_row();

        ui.label("DAX IQ channel").on_hover_text(
            "The radio has four. Change this only if something else on the network \
             is already using channel 1 — the radio refuses a channel twice over.",
        );
        ComboBox::from_id_salt("flex_ch")
            .selected_text(cfg.smartsdr.iq_channel.to_string())
            .show_ui(ui, |ui| {
                for ch in SmartSdrConfig::IQ_CHANNELS {
                    let sel = cfg.smartsdr.iq_channel == ch;
                    if ui.selectable_label(sel, ch.to_string()).clicked() {
                        cfg.smartsdr.iq_channel = ch;
                    }
                }
            });
        ui.end_row();

        ui.label("Station name").on_hover_text(
            "Shown against this session in the radio's client list. The radio also \
             remembers a client by it, so renaming makes the radio treat sdroxide as \
             a new one.",
        );
        ui.add(egui::TextEdit::singleline(&mut cfg.smartsdr.station).desired_width(160.0));
        ui.end_row();

        ui.label("");
        // Both run here: the test opens its own connection from this machine,
        // and the trace is of this process's own session.
        local_only(ui, local, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("Test connection")
                    .on_hover_text(
                        "Checks the radio answers, without registering as a GUI client — \
                         so it will not disturb a SmartSDR session already running.",
                    )
                    .clicked()
                {
                    *test = true;
                }
                if ui
                    .button("Copy diagnostic report")
                    .on_hover_text(
                        "Copies the last session's protocol trace to the clipboard, for a \
                         bug report.",
                    )
                    .clicked()
                {
                    *copy_report = true;
                }
            });
        });
        ui.end_row();
    });

    test_result_line(ui, test_result);

    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Wideband IQ receive over DAX, audio transmit the radio modulates. Press \
             \"Apply / reconnect\" to switch without a restart.",
        )
        .weak(),
    );
    ui.label(
        RichText::new(
            "Not yet verified against real hardware. If it misbehaves, please send the \
             diagnostic report — it contains every protocol line exchanged with the radio.",
        )
        .color(Color32::from_rgb(220, 170, 70)),
    );
}

/// What SoapySDR can see on this machine, and what it means.
///
/// The SoapySDR interface has no device picker — the device is chosen by
/// `--device` or `device_args` in `config.toml`, and until that is typed the
/// first enumerated device wins. That made this list worth showing: on a bundle
/// install (PothosSDR ships every module) the winner can be the sound card, and
/// nothing on screen says so. A driver with a native interface is called out
/// too, because reaching an RSP or a dongle through SoapySDR gives up every
/// model-specific control sdroxide has for it.
pub(in crate::app) fn settings_soapy_devices(
    ui: &mut egui::Ui,
    devices: Option<&[sdroxide_types::SoapyDeviceInfo]>,
    rescan: &mut bool,
    local: bool,
) {
    use sdroxide_types::SoapyDeviceInfo;

    // The whole section, not just the button: the list below is what *this*
    // machine's installed modules found, and shown beside a radio that is
    // somewhere else it would be read as that radio's.
    if !local {
        ui.label(RichText::new("Devices SoapySDR can see").strong());
        ui.label(
            RichText::new(
                "Enumerated on the machine the radio is attached to, where the modules \
                 are installed. Which device it opens is `device_args` in that machine's \
                 config.toml, or its --device.",
            )
            .weak(),
        );
        return;
    }

    ui.horizontal(|ui| {
        ui.label(RichText::new("Devices SoapySDR can see").strong());
        if ui
            .button("Rescan")
            .on_hover_text(
                "Ask every installed SoapySDR module to scan. Nothing is opened, \
                 so this is safe while receiving — but it can take a moment.",
            )
            .clicked()
        {
            *rescan = true;
        }
    });

    let Some(devices) = devices else {
        ui.label(RichText::new("Not enumerated yet — press Rescan.").weak());
        return;
    };
    if devices.is_empty() {
        ui.label(
            RichText::new(
                "No SoapySDR devices found. Check that the module for your radio is \
                 installed and that you may access the device.",
            )
            .weak(),
        );
        return;
    }

    for d in devices {
        ui.horizontal(|ui| {
            ui.label(RichText::new(d.label()).monospace());
            if d.is_pseudo() {
                ui.label(RichText::new("not a radio").color(Color32::from_rgb(220, 170, 70)));
            }
        });
    }

    // The sound-card trap: named, with the reason and the way out.
    if devices.iter().any(SoapyDeviceInfo::is_pseudo) {
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "A sound card is listed above as if it were an SDR. It accepts any \
                 frequency and ignores it, so opening it shows the sound card's input \
                 instead of the band. sdroxide does not pick those automatically — but \
                 a device_args line naming one is still obeyed.",
            )
            .color(Color32::from_rgb(220, 170, 70)),
        );
    }

    // Hardware with a native interface: say so, because the native one is
    // strictly better and the operator has no way to know it exists. Named
    // once each — two RSPs are still one interface to switch to — and by
    // `contains` rather than `dedup`, which would keep a repeat that another
    // driver happens to sit between.
    let mut native: Vec<sdroxide_types::Backend> = Vec::new();
    for b in devices.iter().filter_map(SoapyDeviceInfo::native_backend) {
        if !native.contains(&b) {
            native.push(b);
        }
    }
    if !native.is_empty() {
        ui.add_space(4.0);
        let names = native.iter().map(|b| b.label()).collect::<Vec<_>>().join(", ");
        ui.label(
            RichText::new(format!(
                "Hardware above is supported directly by sdroxide: {names}. Selecting that \
                 interface above gives you its own settings — gain stages, filters and \
                 notches SoapySDR cannot express — and needs no SoapySDR module.",
            ))
            .color(crate::theme::CYAN()),
        );
    }

    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Which one opens is set by --device or device_args in config.toml; with \
             neither, the first radio listed wins.",
        )
        .weak(),
    );
}

impl SdroxideApp {
    /// SoapySDR RX/TX gains + antenna (empty for a CAT rig).
    pub(in crate::app) fn settings_device_tab(&self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let Some(caps) = &self.caps else {
            ui.label("no device");
            return;
        };
        ui.label(RichText::new(&caps.label).size(14.0).strong().color(crate::theme::CYAN()));
        ui.add_space(6.0);
        if caps.gains.iter().all(|g| g.direction != Direction::Rx) {
            ui.label(RichText::new("This rig has no software-adjustable gains.").weak());
        }
        ui.label(RichText::new("RX gains").strong());
        egui::Grid::new("gains").num_columns(2).show(ui, |ui| {
            for g in caps.gains.iter().filter(|g| g.direction == Direction::Rx) {
                ui.label(&g.name);
                let mut db = self
                    .state
                    .gains
                    .iter()
                    .find(|(n, _)| *n == g.name)
                    .map(|(_, d)| *d)
                    .unwrap_or(g.min_db);
                let step = if g.step_db > 0.0 { g.step_db } else { 1.0 };
                if crate::chrome::slider(
                    ui,
                    Slider::new(&mut db, g.min_db..=g.max_db).step_by(step).suffix(" dB"),
                )
                .changed()
                {
                    cmds.push(Command::SetGain { dir: Direction::Rx, element: g.name.clone(), db });
                }
                ui.end_row();
            }
        });
        if caps.gains.iter().any(|g| g.direction == Direction::Tx) {
            ui.separator();
            ui.label(RichText::new("TX gains").strong().color(Color32::from_rgb(240, 90, 60)));
            egui::Grid::new("tx-gains").num_columns(2).show(ui, |ui| {
                for g in caps.gains.iter().filter(|g| g.direction == Direction::Tx) {
                    ui.label(&g.name);
                    let mut db = self
                        .state
                        .tx_gains
                        .iter()
                        .find(|(n, _)| *n == g.name)
                        .map(|(_, d)| *d)
                        .unwrap_or(g.min_db);
                    let step = if g.step_db > 0.0 { g.step_db } else { 1.0 };
                    if crate::chrome::slider(
                        ui,
                        Slider::new(&mut db, g.min_db..=g.max_db).step_by(step).suffix(" dB"),
                    )
                    .changed()
                    {
                        cmds.push(Command::SetGain {
                            dir: Direction::Tx,
                            element: g.name.clone(),
                            db,
                        });
                    }
                    ui.end_row();
                }
            });
        }
        // Only worth a control where there is a choice to make: a front end with
        // one port has nothing to switch to, and a row saying so is noise.
        let rx_ports = caps.antennas_rx.len() > 1;
        let tx_ports = caps.antennas_tx.len() > 1;
        if rx_ports || tx_ports {
            ui.separator();
            ui.label(RichText::new("Antennas").strong());
            egui::Grid::new("antennas").num_columns(2).show(ui, |ui| {
                if rx_ports {
                    ui.label("RX");
                    ComboBox::from_id_salt("ant-rx")
                        .selected_text(self.state.antenna_rx.clone())
                        .show_ui(ui, |ui| {
                            for a in &caps.antennas_rx {
                                if ui.selectable_label(self.state.antenna_rx == *a, a).clicked() {
                                    cmds.push(Command::SetAntenna {
                                        dir: Direction::Rx,
                                        name: a.clone(),
                                    });
                                }
                            }
                        });
                    ui.end_row();
                }
                if tx_ports {
                    ui.label(RichText::new("TX").color(Color32::from_rgb(240, 90, 60)));
                    ComboBox::from_id_salt("ant-tx")
                        .selected_text(self.state.antenna_tx.clone())
                        .show_ui(ui, |ui| {
                            for a in &caps.antennas_tx {
                                if ui.selectable_label(self.state.antenna_tx == *a, a).clicked() {
                                    cmds.push(Command::SetAntenna {
                                        dir: Direction::Tx,
                                        name: a.clone(),
                                    });
                                }
                            }
                        });
                    ui.end_row();
                }
            });
            ui.label(RichText::new("Remembered for the next start.").weak());
        }
    }
}

/// Settings for the RX-888 direct-sampling receiver.
///
/// The layout follows the signal path: which receiver, how fast to clock the
/// ADC, then the two analogue gain stages, then the switches. The ADC rate is
/// the one setting an operator can get badly wrong — it decides both how much
/// spectrum is visible and how much USB bandwidth is needed — so it says what it
/// costs rather than just listing numbers.
pub(in crate::app) fn settings_rx888_tab(
    ui: &mut egui::Ui,
    devices: &[sdroxide_types::Rx888Device],
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    rescan: &mut bool,
    apply: &mut bool,
    local: bool,
    cmds: &mut Vec<Command>,
) {
    use sdroxide_types::Rx888Config;
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };

    // Everything on this panel takes effect as soon as it is touched. The gain
    // stages ride `SetGain` straight to the running device; the rest need the
    // DSP chain rebuilt around a new sample rate, so they ask for a reopen
    // instead of leaving the operator to find a button. That is affordable here
    // in a way it is not for other backends — the device is already programmed,
    // so reopening it costs about a millisecond plus the firmware's own start
    // latency, measured at ~150 ms end to end.
    let before = (cfg.rx888.serial.clone(), cfg.rx888.adc_rate_hz, cfg.rx888.randomize);

    egui::Grid::new("rx888-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Receiver");
        // Which receiver is this panel's one row about a USB bus; everything
        // below reaches the device wherever it is plugged in.
        local_only(ui, local, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("Rescan")
                    .on_hover_text(
                        "Re-list the USB bus. No device is opened, so this is safe \
                         to press while receiving.",
                    )
                    .clicked()
                {
                    *rescan = true;
                }
                let shown = if cfg.rx888.serial.is_empty() {
                    "— first one found —".to_string()
                } else {
                    cfg.rx888.serial.clone()
                };
                ComboBox::from_id_salt("rx888_dev").width(300.0).selected_text(shown).show_ui(
                    ui,
                    |ui| {
                        if devices.is_empty() {
                            ui.label("No RX-888 found — press Rescan");
                        }
                        ui.selectable_value(
                            &mut cfg.rx888.serial,
                            String::new(),
                            "— first one found —",
                        );
                        for d in devices {
                            let serial = d.serial.clone().unwrap_or_default();
                            ui.selectable_value(&mut cfg.rx888.serial, serial, d.label());
                        }
                    },
                );
            });
        });
        ui.end_row();

        ui.label("ADC clock");
        ui.horizontal(|ui| {
            let rate = cfg.rx888.adc_rate_hz;
            ComboBox::from_id_salt("rx888_rate")
                .width(150.0)
                .selected_text(format!("{:.1} Msps", rate / 1e6))
                .show_ui(ui, |ui| {
                    for r in Rx888Config::ADC_RATES {
                        ui.selectable_value(
                            &mut cfg.rx888.adc_rate_hz,
                            r,
                            format!("{:.1} Msps", r / 1e6),
                        );
                    }
                });
            // Inside a grid (and a horizontal row) labels default to Extend,
            // which pushes the row off the window edge instead of wrapping.
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "0–{:.1} MHz coverage, {:.0} MB/s over USB",
                        rate / 2e6,
                        rate * 2.0 / 1e6
                    ))
                    .weak(),
                )
                .wrap(),
            );
        });
        ui.end_row();
        ui.label("");
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "129.6 Msps needs a SuperSpeed link and a fast host; 64.8 is the \
                     safe default. Changing it reopens the receiver automatically, \
                     which takes a moment but needs no restart.",
                )
                .weak(),
            )
            .wrap(),
        );
        ui.end_row();

        ui.label("VGA gain");
        if ui
            .add(egui::Slider::new(&mut cfg.rx888.vga_db, -6.0..=34.0).suffix(" dB"))
            .on_hover_text("AD8370 variable-gain amplifier ahead of the ADC.")
            .changed()
        {
            cmds.push(Command::SetGain {
                dir: sdroxide_types::Direction::Rx,
                element: Rx888Config::VGA_ELEMENT.into(),
                db: cfg.rx888.vga_db,
            });
        }
        ui.end_row();

        ui.label("Attenuator");
        if ui
            .add(egui::Slider::new(&mut cfg.rx888.attenuator_db, -31.5..=0.0).suffix(" dB"))
            .on_hover_text("PE4304 step attenuator, in 0.5 dB steps.")
            .changed()
        {
            cmds.push(Command::SetGain {
                dir: sdroxide_types::Direction::Rx,
                element: Rx888Config::ATT_ELEMENT.into(),
                db: cfg.rx888.attenuator_db,
            });
        }
        ui.end_row();

        ui.label("ADC range");
        if ui
            .checkbox(&mut cfg.rx888.pga, "Wide (2.25 Vp-p)")
            .on_hover_text(
                "Selects the ADC's wider input range: more headroom for strong \
                 broadcast signals, fewer counts for weak ones. Off selects the \
                 more sensitive 1.5 Vp-p range.",
            )
            .changed()
        {
            cmds.push(Command::SetGain {
                dir: sdroxide_types::Direction::Rx,
                element: Rx888Config::PGA_ELEMENT.into(),
                db: cfg.rx888.pga as u8 as f64,
            });
        }
        ui.end_row();

        ui.label("Dither");
        if ui
            .checkbox(&mut cfg.rx888.dither, "Enable")
            .on_hover_text(
                "Adds a small dither signal ahead of the ADC: costs a little \
                 noise floor, buys spurious-free dynamic range.",
            )
            .changed()
        {
            cmds.push(Command::SetGain {
                dir: sdroxide_types::Direction::Rx,
                element: Rx888Config::DITHER_ELEMENT.into(),
                db: cfg.rx888.dither as u8 as f64,
            });
        }
        ui.end_row();

        ui.label("Randomiser");
        ui.checkbox(&mut cfg.rx888.randomize, "Enable").on_hover_text(
            "The ADC scrambles its output so the digital bus stops radiating \
                 into the front end; the driver unscrambles it. Leave this on \
                 unless you are debugging. Applies on reconnect.",
        );
        ui.end_row();

        ui.label("Bias tee");
        if ui
            .checkbox(&mut cfg.rx888.bias_tee_hf, "DC on the HF antenna port")
            .on_hover_text("Powers an active antenna or preamp down the coax.")
            .changed()
        {
            cmds.push(Command::SetGain {
                dir: sdroxide_types::Direction::Rx,
                element: Rx888Config::BIAS_TEE_ELEMENT.into(),
                db: cfg.rx888.bias_tee_hf as u8 as f64,
            });
        }
        ui.end_row();

        ui.label("Clock trim");
        let r = ui
            .add(
                egui::DragValue::new(&mut cfg.rx888.ppm)
                    .speed(0.1)
                    .range(-200.0..=200.0)
                    .suffix(" ppm"),
            )
            .on_hover_text(
                "Corrects the reference oscillator. Applied when you let go of \
                 the value — reopening on every pixel of a drag would restart \
                 the receiver hundreds of times.",
            );
        if r.drag_stopped() || r.lost_focus() {
            *apply = true;
        }
        ui.end_row();
    });

    if before != (cfg.rx888.serial.clone(), cfg.rx888.adc_rate_hz, cfg.rx888.randomize) {
        *apply = true;
    }

    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "Receive only, 0–32 MHz by direct sampling. There is no hardware \
             downconverter: the full ADC stream is converted to baseband on the \
             host, so retuning anywhere in HF is instant. Every setting here \
             applies straight away — there is no Apply button to press.",
        )
        .weak(),
    );
}

/// Queue a receive-side gain (or pseudo-gain) change.
///
/// The Airspy HF+ panel drives seven of these — one real gain and six switches
/// riding `SetGain` so the backend needs no `Command` variants of its own — and
/// seven copies of the struct literal would bury the settings among them.
fn push_gain(cmds: &mut Vec<Command>, element: &str, db: f64) {
    cmds.push(Command::SetGain { dir: Direction::Rx, element: element.to_string(), db });
}

/// Airspy HF+ interface: receiver, rate, and the front end's own controls.
///
/// The rate list is the interesting part. Which rates an HF+ has depends on the
/// model *and* the firmware together, and only an opened receiver knows — so
/// once one is connected its own list is shown, and before that the union of
/// everything any HF+ is known to offer, annotated with who each one belongs to.
///
/// The report button is not decoration: this backend has never been run against
/// a real receiver, so the first people to use it are the ones who can say
/// whether it works.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn settings_airspyhf_tab(
    ui: &mut egui::Ui,
    devices: &[sdroxide_types::AirspyHfDevice],
    caps: Option<&sdroxide_types::DeviceCaps>,
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    rescan: &mut bool,
    copy_report: &mut bool,
    apply: &mut bool,
    local: bool,
    cmds: &mut Vec<Command>,
) {
    use sdroxide_types::AirspyHfConfig;
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };

    // Only the receiver and the rate rebuild the session — the rate because the
    // engine builds its whole downconversion chain around it. Everything else
    // rides `SetGain` straight to the running device.
    let before = (cfg.airspyhf.serial.clone(), cfg.airspyhf.sample_rate_hz);

    // Once a receiver has been opened, its own list is the honest one. Before
    // that, offer everything any HF+ is known to do.
    let queried = caps
        .filter(|c| c.driver == "airspyhf" && !c.sample_rates.is_empty())
        .map(|c| c.sample_rates.as_slice());
    let rates = queried.unwrap_or(&AirspyHfConfig::SAMPLE_RATES);
    // The attenuator's range comes from the receiver too — the models differ.
    let att_max = caps
        .filter(|c| c.driver == "airspyhf")
        .and_then(|c| c.gains.first())
        .map(|g| -g.min_db)
        .unwrap_or(AirspyHfConfig::ATT_MAX_DB);
    let att_step = caps
        .filter(|c| c.driver == "airspyhf")
        .and_then(|c| c.gains.first())
        .map(|g| g.step_db)
        .unwrap_or(AirspyHfConfig::ATT_STEP_DB);

    egui::Grid::new("airspyhf-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Receiver");
        // Which receiver is this panel's one row about a USB bus; everything
        // below reaches the device wherever it is plugged in.
        local_only(ui, local, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("Rescan")
                    .on_hover_text(
                        "Re-list the USB bus. No device is opened, so this is safe \
                         to press while receiving.",
                    )
                    .clicked()
                {
                    *rescan = true;
                }
                let shown = if cfg.airspyhf.serial.is_empty() {
                    "— first one found —".to_string()
                } else {
                    cfg.airspyhf.serial.clone()
                };
                ComboBox::from_id_salt("airspyhf_dev").width(300.0).selected_text(shown).show_ui(
                    ui,
                    |ui| {
                        if devices.is_empty() {
                            ui.label(RichText::new("no receivers — press Rescan").weak());
                        }
                        ui.selectable_value(
                            &mut cfg.airspyhf.serial,
                            String::new(),
                            "— first one found —",
                        );
                        for d in devices {
                            // Only a receiver whose serial parsed can be pinned;
                            // without one there is nothing stable to remember.
                            match &d.serial {
                                Some(sn) => {
                                    ui.selectable_value(
                                        &mut cfg.airspyhf.serial,
                                        sn.clone(),
                                        d.label(),
                                    );
                                }
                                None => {
                                    ui.label(RichText::new(d.label()).weak());
                                }
                            }
                        }
                    },
                );
            });
        });
        ui.end_row();

        ui.label("Sample rate").on_hover_text(
            "Which rates a receiver has depends on the model and the firmware. \
             Takes effect on Apply.",
        );
        ui.horizontal(|ui| {
            let shown = format!("{:.0} kSPS", cfg.airspyhf.sample_rate_hz / 1e3);
            ComboBox::from_id_salt("airspyhf_rate").width(150.0).selected_text(shown).show_ui(
                ui,
                |ui| {
                    for &r in rates {
                        let label = if queried.is_some() {
                            format!("{:.0} kSPS", r / 1e3)
                        } else {
                            format!("{:.0} kSPS  ({})", r / 1e3, AirspyHfConfig::rate_note(r))
                        };
                        if ui
                            .selectable_label((cfg.airspyhf.sample_rate_hz - r).abs() < 1.0, label)
                            .clicked()
                        {
                            cfg.airspyhf.sample_rate_hz = r;
                        }
                    }
                },
            );
            if queried.is_none() {
                // Inside a horizontal row a label defaults to Extend, which
                // pushes the row off the window edge instead of wrapping.
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "every rate any HF+ model offers — connect one to see its own",
                        )
                        .weak(),
                    )
                    .wrap(),
                );
            }
        });
        ui.end_row();

        ui.label("AGC").on_hover_text(
            "The receiver's own gain control. Leave it on for general listening; \
             turn it off to set the attenuator by hand for measurement.",
        );
        let mut agc = cfg.airspyhf.agc;
        if ui.checkbox(&mut agc, "Automatic").changed() {
            cfg.airspyhf.agc = agc;
            push_gain(cmds, AirspyHfConfig::AGC_ELEMENT, f64::from(u8::from(agc)));
        }
        ui.end_row();

        ui.label("AGC threshold");
        ui.add_enabled_ui(cfg.airspyhf.agc, |ui| {
            let mut high = cfg.airspyhf.agc_threshold_high;
            if ui
                .checkbox(&mut high, "High")
                .on_hover_text(
                    "High trades a little sensitivity for headroom against strong \
                     neighbours — the right setting on a crowded band at night.",
                )
                .changed()
            {
                cfg.airspyhf.agc_threshold_high = high;
                push_gain(cmds, AirspyHfConfig::AGC_THRESHOLD_ELEMENT, f64::from(u8::from(high)));
            }
        });
        ui.end_row();

        ui.label("Attenuator").on_hover_text(
            "Front-end attenuation, as a gain — 0 dB is none. Only obeyed with \
             the AGC off.",
        );
        ui.add_enabled_ui(!cfg.airspyhf.agc, |ui| {
            let mut db = cfg.airspyhf.attenuator_db;
            if ui
                .add(Slider::new(&mut db, -att_max..=0.0).step_by(att_step).suffix(" dB"))
                .changed()
            {
                cfg.airspyhf.attenuator_db = db;
                push_gain(cmds, AirspyHfConfig::ATT_ELEMENT, db);
            }
        });
        ui.end_row();

        ui.label("Preamp").on_hover_text(
            "The HF low-noise amplifier. Buys sensitivity at the cost of \
             intermodulation, so it is off by default — which is usually right \
             on a real antenna.",
        );
        let mut lna = cfg.airspyhf.lna;
        if ui.checkbox(&mut lna, "LNA on").changed() {
            cfg.airspyhf.lna = lna;
            push_gain(cmds, AirspyHfConfig::LNA_ELEMENT, f64::from(u8::from(lna)));
        }
        ui.end_row();

        ui.label("Frequency calibration").on_hover_text(
            "Parts per billion — this receiver's own unit, a thousand times finer \
             than the ppm an RTL-SDR uses. Nothing here is ever written to the \
             receiver's flash: this overrides the stored value for the session only.",
        );
        ui.horizontal(|ui| {
            let mut stored = cfg.airspyhf.calibration_ppb.is_none();
            if ui.checkbox(&mut stored, "Use the receiver's stored value").changed() {
                cfg.airspyhf.calibration_ppb = if stored { None } else { Some(0) };
                if let Some(ppb) = cfg.airspyhf.calibration_ppb {
                    push_gain(cmds, AirspyHfConfig::PPB_ELEMENT, ppb as f64);
                } else {
                    // Back to the receiver's own figure needs a reopen: the
                    // flash value is only read when the device is opened.
                    *apply = true;
                }
            }
            if let Some(ppb) = cfg.airspyhf.calibration_ppb.as_mut()
                && ui.add(DragValue::new(ppb).speed(10).suffix(" ppb")).changed()
            {
                push_gain(cmds, AirspyHfConfig::PPB_ELEMENT, *ppb as f64);
            }
        });
        ui.end_row();

        ui.label("Bias tee");
        let mut bias = cfg.airspyhf.bias_tee;
        if ui
            .checkbox(&mut bias, "Feed DC up the coax")
            .on_hover_text("Not every HF+ has one; on a receiver without, this does nothing.")
            .changed()
        {
            cfg.airspyhf.bias_tee = bias;
            push_gain(cmds, AirspyHfConfig::BIAS_TEE_ELEMENT, f64::from(u8::from(bias)));
        }
        ui.end_row();

        ui.label("Host DSP").on_hover_text(
            "The image balancer, the zero-IF offset and the fine-tuning \
             oscillator. Turn it off only to see raw hardware output — with it \
             off, the mirror image appears on the zero-IF rates and the dial is \
             accurate only to the nearest kilohertz.",
        );
        let mut dsp = cfg.airspyhf.lib_dsp;
        if ui.checkbox(&mut dsp, "Correct the image and fine-tune").changed() {
            cfg.airspyhf.lib_dsp = dsp;
            push_gain(cmds, AirspyHfConfig::LIB_DSP_ELEMENT, f64::from(u8::from(dsp)));
        }
        ui.end_row();

        ui.label("");
        // The trace is of the session *this* process ran; the engine's own is
        // on the engine's machine.
        local_only(ui, local, |ui| {
            if ui
                .button("Copy diagnostic report")
                .on_hover_text(
                    "Copies the last session's trace to the clipboard, for a bug \
                     report: every command exchanged with the receiver, and the \
                     first bytes of the sample stream.",
                )
                .clicked()
            {
                *copy_report = true;
            }
        });
        ui.end_row();
    });

    if cfg.airspyhf.bias_tee {
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Bias tee is ON. Never connect a transceiver, a grounded antenna, \
                 or a preamp powered from elsewhere while this is enabled — the DC \
                 goes straight down the feedline.",
            )
            .color(crate::theme::YELLOW()),
        );
    }

    if (cfg.airspyhf.serial.clone(), cfg.airspyhf.sample_rate_hz) != before {
        *apply = true;
    }

    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Receive only. No SoapySDR and no libairspyhf needed. Below the \
             synthesiser's floor the host oscillator does the tuning, which is how \
             this receiver reaches VLF. The receiver and sample rate take effect on \
             Apply; everything else applies as you change it.",
        )
        .weak(),
    );
    ui.label(
        RichText::new(
            "Not yet verified against real hardware. If it misbehaves, please send the \
             diagnostic report — it contains every command exchanged with the receiver, \
             and the first bytes of the sample stream decoded as I/Q pairs.",
        )
        .color(crate::theme::YELLOW()),
    );
}

/// SDRplay RSP interface: device, rate, and the RSP's gain model (IF gain
/// reduction + LNA state + hardware AGC), with the rows a given model lacks
/// hidden.
pub(in crate::app) fn settings_sdrplay_tab(
    ui: &mut egui::Ui,
    devices: &[sdroxide_types::SdrPlayDevice],
    caps: Option<&sdroxide_types::DeviceCaps>,
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    rescan: &mut bool,
    apply: &mut bool,
    local: bool,
    cmds: &mut Vec<Command>,
) {
    use sdroxide_types::{SdrPlayAgc, SdrPlayConfig, SdrPlayDuoTuner, SdrPlayModel};
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };

    // Device, rate, bandwidth and RSPduo tuner rebuild the session; the rest
    // rides `SetGain` (or `SetAntenna`) straight to the running device.
    let before = (
        cfg.sdrplay.serial.clone(),
        cfg.sdrplay.sample_rate_hz,
        cfg.sdrplay.bw_khz,
        cfg.sdrplay.duo_tuner,
    );

    // Which rows to draw comes from the *selected* device's model, and with
    // nothing enumerated (service down, mid-replug) from the RSP1A/1B feature
    // set: the driver ignores a switch the real hardware lacks, whereas a
    // hidden switch cannot be un-hidden by an operator whose service just
    // isn't running yet.
    let listed = devices.iter().find(|d| d.serial == cfg.sdrplay.serial).or(devices.first());
    let model = listed.map(|d| d.model()).unwrap_or(SdrPlayModel::Rsp1b);

    // Except that RSP1B is the one model with *no* antenna ports and the
    // shortest LNA ladder, so falling back to it does the very thing the rule
    // above forbids: it hides controls. Where a receiver is already open its
    // own capabilities are the honest account of what it has, and they hold
    // whether the service enumerated nothing, listed no serial to match, or
    // left out the device it has already handed to us.
    let open = caps.filter(|c| c.driver == "sdrplay" && listed.is_none());
    let open_ports = open.map(|c| c.antennas_rx.as_slice()).filter(|p| !p.is_empty());

    // A device listed without a serial number (or an unrecognised hardware
    // version) is the signature of a USB communication problem: it opens and
    // streams, but often deaf. Say so here, where the operator is already
    // looking for what went wrong — picking such an entry also stores an
    // empty serial, indistinguishable from "first one found".
    if let Some(w) = devices.iter().find_map(|d| d.identity_warning()) {
        ui.label(RichText::new(w).color(Color32::from_rgb(220, 170, 70)));
        ui.add_space(6.0);
    }

    egui::Grid::new("sdrplay-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Receiver");
        // The service that answers this is the one on the engine's machine;
        // everything below reaches the RSP through it.
        local_only(ui, local, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("Rescan")
                    .on_hover_text(
                        "Ask the SDRplay API service for its device list. Nothing is \
                         opened, so this is safe to press while receiving.",
                    )
                    .clicked()
                {
                    *rescan = true;
                }
                let shown = if cfg.sdrplay.serial.is_empty() {
                    "— first one found —".to_string()
                } else {
                    cfg.sdrplay.serial.clone()
                };
                ComboBox::from_id_salt("sdrplay_dev").width(300.0).selected_text(shown).show_ui(
                    ui,
                    |ui| {
                        if devices.is_empty() {
                            ui.label(
                                RichText::new(
                                    "no RSPs — press Rescan (needs the SDRplay API service)",
                                )
                                .weak(),
                            );
                        }
                        ui.selectable_value(
                            &mut cfg.sdrplay.serial,
                            String::new(),
                            "— first one found —",
                        );
                        for d in devices {
                            ui.selectable_value(
                                &mut cfg.sdrplay.serial,
                                d.serial.clone(),
                                d.label(),
                            );
                        }
                    },
                );
            });
        });
        ui.end_row();

        ui.label("Sample rate").on_hover_text(
            "Rates below 2 Msps run the ADC at 2 Msps and decimate in the \
             service. Takes effect on Apply.",
        );
        let shown = format!("{:.3} Msps", cfg.sdrplay.sample_rate_hz / 1e6);
        ComboBox::from_id_salt("sdrplay_rate").selected_text(shown).show_ui(ui, |ui| {
            for &r in &SdrPlayConfig::SAMPLE_RATES {
                let sel = (cfg.sdrplay.sample_rate_hz - r).abs() < 1.0;
                let mut label = format!("{:.3} Msps", r / 1e6);
                if r > 6_048_000.0 {
                    // The ADC trades resolution for speed past 6.048 Msps.
                    label.push_str("  (reduced ADC resolution)");
                }
                if ui.selectable_label(sel, label).clicked() {
                    cfg.sdrplay.sample_rate_hz = r;
                }
            }
        });
        ui.end_row();

        ui.label("IF bandwidth").on_hover_text(
            "The tuner's analog filter. Auto picks the widest one that fits \
             the sample rate. Takes effect on Apply.",
        );
        let shown = if cfg.sdrplay.bw_khz == 0 {
            "Auto".to_string()
        } else {
            format!("{} kHz", cfg.sdrplay.bw_khz)
        };
        ComboBox::from_id_salt("sdrplay_bw").selected_text(shown).show_ui(ui, |ui| {
            if ui.selectable_label(cfg.sdrplay.bw_khz == 0, "Auto").clicked() {
                cfg.sdrplay.bw_khz = 0;
            }
            for &khz in &SdrPlayConfig::BANDWIDTHS_KHZ {
                // Filters wider than the rate would only alias; don't offer them.
                if (khz as f64) * 1000.0 > cfg.sdrplay.sample_rate_hz {
                    continue;
                }
                if ui.selectable_label(cfg.sdrplay.bw_khz == khz, format!("{khz} kHz")).clicked() {
                    cfg.sdrplay.bw_khz = khz;
                }
            }
        });
        ui.end_row();

        ui.label("AGC").on_hover_text(
            "The RSP's own IF-gain loop, run by the API service. Off hands the \
             IF gain slider back to you — the setting for measurement and \
             weak-signal digital modes.",
        );
        let mut agc = cfg.sdrplay.agc;
        enum_combo(ui, "sdrplay_agc", &mut agc, &SdrPlayAgc::ALL, SdrPlayAgc::label);
        if agc != cfg.sdrplay.agc {
            cfg.sdrplay.agc = agc;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: SdrPlayConfig::AGC_ELEMENT.to_string(),
                db: agc.code(),
            });
        }
        ui.end_row();

        if cfg.sdrplay.agc != SdrPlayAgc::Off {
            ui.label("AGC set point").on_hover_text(
                "Signal level the loop holds the ADC at. Lower leaves more \
                 headroom for signals off-channel.",
            );
            if ui
                .add(Slider::new(&mut cfg.sdrplay.agc_setpoint_dbfs, -72..=-20).suffix(" dBFS"))
                .changed()
            {
                cmds.push(Command::SetGain {
                    dir: Direction::Rx,
                    element: SdrPlayConfig::AGC_SETPOINT_ELEMENT.to_string(),
                    db: cfg.sdrplay.agc_setpoint_dbfs as f64,
                });
            }
            ui.end_row();
        }

        ui.label("IF gain reduction").on_hover_text(
            "The RSP's native gain unit: 20 dB is maximum gain, 59 dB minimum. \
             Applies immediately. Ignored while the AGC is running — the loop \
             owns this value then, and the S-meter shows what it settled on.",
        );
        ui.add_enabled_ui(cfg.sdrplay.agc == SdrPlayAgc::Off, |ui| {
            if ui
                .add(
                    Slider::new(
                        &mut cfg.sdrplay.if_gr_db,
                        SdrPlayConfig::IF_GR_MIN..=SdrPlayConfig::IF_GR_MAX,
                    )
                    .suffix(" dB"),
                )
                .changed()
            {
                cmds.push(Command::SetGain {
                    dir: Direction::Rx,
                    element: SdrPlayConfig::IF_GAIN_ELEMENT.to_string(),
                    db: -(cfg.sdrplay.if_gr_db as f64),
                });
            }
        });
        ui.end_row();

        ui.label("LNA state").on_hover_text(
            "Front-end attenuation in steps: 0 is maximum gain, each step up \
             switches more attenuation in. Some bands have fewer steps — the \
             driver clamps and keeps your choice for when you tune back. \
             Applies immediately.",
        );
        // Same story as the ports: an RSPdx guessed to be an RSP1B would lose
        // two thirds of its LNA range. The open device publishes the real one.
        let max_lna = open
            .and_then(|c| c.gains.iter().find(|g| g.name == SdrPlayConfig::LNA_ELEMENT))
            .map(|g| (-g.min_db).round().clamp(0.0, 255.0) as u8)
            .filter(|&n| n > 0)
            .unwrap_or_else(|| model.max_lna_state());
        if ui
            .add(Slider::new(&mut cfg.sdrplay.lna_state, 0..=max_lna))
            .on_hover_text("0 = max gain")
            .changed()
        {
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: SdrPlayConfig::LNA_ELEMENT.to_string(),
                db: -(cfg.sdrplay.lna_state as f64),
            });
        }
        ui.end_row();

        ui.label("Frequency correction").on_hover_text(
            "Reference error in parts per million, applied by the device \
             itself. Applies immediately.",
        );
        let mut ppm = cfg.sdrplay.ppm;
        if ui
            .add(DragValue::new(&mut ppm).speed(0.1).range(-200.0..=200.0).suffix(" ppm"))
            .changed()
        {
            cfg.sdrplay.ppm = ppm;
            cmds.push(Command::SetGain {
                dir: Direction::Rx,
                element: SdrPlayConfig::PPM_ELEMENT.to_string(),
                db: ppm,
            });
        }
        ui.end_row();

        if model == SdrPlayModel::RspDuo {
            ui.label("Tuner").on_hover_text(
                "Which of the RSPduo's two tuners to run (one at a time). \
                 Takes effect on Apply.",
            );
            let mut tuner = cfg.sdrplay.duo_tuner;
            enum_combo(
                ui,
                "sdrplay_duo_tuner",
                &mut tuner,
                &SdrPlayDuoTuner::ALL,
                SdrPlayDuoTuner::label,
            );
            if tuner != cfg.sdrplay.duo_tuner {
                cfg.sdrplay.duo_tuner = tuner;
                // The port list belongs to the tuner; a remembered tuner-1
                // port name means nothing on tuner 2.
                cfg.sdrplay.antenna = String::new();
            }
            ui.end_row();
        }

        let antennas: Vec<&str> = match open_ports {
            Some(ports) => ports.iter().map(String::as_str).collect(),
            None => model.antennas(cfg.sdrplay.duo_tuner).to_vec(),
        };
        if !antennas.is_empty() {
            ui.label("Antenna").on_hover_text("Applies immediately.");
            let shown = if cfg.sdrplay.antenna.is_empty() {
                antennas[0].to_string()
            } else {
                cfg.sdrplay.antenna.clone()
            };
            ComboBox::from_id_salt("sdrplay_antenna").selected_text(shown).show_ui(ui, |ui| {
                for a in &antennas {
                    if ui.selectable_label(cfg.sdrplay.antenna == *a, *a).clicked() {
                        cfg.sdrplay.antenna = a.to_string();
                        cmds.push(Command::SetAntenna { dir: Direction::Rx, name: a.to_string() });
                    }
                }
            });
            ui.end_row();
        }

        if model.has_rf_notch() {
            ui.label("FM broadcast notch");
            let mut on = cfg.sdrplay.rf_notch;
            if ui
                .checkbox(&mut on, "Enable")
                .on_hover_text(
                    "Hardware notch over the 88–108 MHz broadcast band, for \
                     when a local transmitter overloads everything else. \
                     Applies immediately.",
                )
                .changed()
            {
                cfg.sdrplay.rf_notch = on;
                cmds.push(Command::SetGain {
                    dir: Direction::Rx,
                    element: SdrPlayConfig::RF_NOTCH_ELEMENT.to_string(),
                    db: if on { 1.0 } else { 0.0 },
                });
            }
            ui.end_row();
        }

        if model.has_dab_notch() {
            ui.label("DAB notch");
            let mut on = cfg.sdrplay.dab_notch;
            if ui
                .checkbox(&mut on, "Enable")
                .on_hover_text(
                    "Hardware notch over the 165–230 MHz DAB band. Applies \
                     immediately.",
                )
                .changed()
            {
                cfg.sdrplay.dab_notch = on;
                cmds.push(Command::SetGain {
                    dir: Direction::Rx,
                    element: SdrPlayConfig::DAB_NOTCH_ELEMENT.to_string(),
                    db: if on { 1.0 } else { 0.0 },
                });
            }
            ui.end_row();
        }

        if model.has_hdr() {
            ui.label("HDR mode");
            let mut on = cfg.sdrplay.hdr;
            if ui
                .checkbox(&mut on, "Enable below 2 MHz")
                .on_hover_text(
                    "The RSPdx's high-dynamic-range path for LF/MF. Not yet \
                     verified against hardware. Applies immediately.",
                )
                .changed()
            {
                cfg.sdrplay.hdr = on;
                cmds.push(Command::SetGain {
                    dir: Direction::Rx,
                    element: SdrPlayConfig::HDR_ELEMENT.to_string(),
                    db: if on { 1.0 } else { 0.0 },
                });
            }
            ui.end_row();
        }

        if model.has_bias_tee() {
            ui.label("Bias tee");
            let mut on = cfg.sdrplay.bias_tee;
            if ui
                .checkbox(&mut on, "Feed ~4.7 V DC up the coax")
                .on_hover_text("Powers an active antenna or preamp down the coax.")
                .changed()
            {
                cfg.sdrplay.bias_tee = on;
                cmds.push(Command::SetGain {
                    dir: Direction::Rx,
                    element: SdrPlayConfig::BIAS_TEE_ELEMENT.to_string(),
                    db: if on { 1.0 } else { 0.0 },
                });
            }
            ui.end_row();
        }
    });

    if cfg.sdrplay.bias_tee && model.has_bias_tee() {
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Bias tee is ON. Never connect a transceiver, a grounded antenna, \
                 or a preamp powered from elsewhere while this is enabled — the DC \
                 goes straight down the feedline.",
            )
            .color(crate::theme::YELLOW()),
        );
    }

    if before
        != (
            cfg.sdrplay.serial.clone(),
            cfg.sdrplay.sample_rate_hz,
            cfg.sdrplay.bw_khz,
            cfg.sdrplay.duo_tuner,
        )
    {
        *apply = true;
    }

    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Receive only, 1 kHz–2 GHz. Needs the vendor's SDRplay API service \
             (sdrplay.com/api) — the RSPs after the original RSP1 have no open \
             protocol. Device, sample rate, bandwidth and RSPduo tuner take \
             effect on Apply; everything else applies as you change it.",
        )
        .weak(),
    );
}
