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

/// CAT / Audio interface: serial + PTT parameters (the interface itself is
/// chosen by the selector in `settings_body`).
pub(in crate::app) fn settings_cat_tab(
    ui: &mut egui::Ui,
    serial_ports: &[String],
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
) {
    use sdroxide_types::{
        CatFamily, DigiMode, LineState, ModeControl, Parity, PttMethod, SoundFormat, StopBits,
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
        ComboBox::from_id_salt("serport").width(260.0).selected_text(shown).show_ui(ui, |ui| {
            for p in serial_ports {
                if ui.selectable_label(&cfg.cat.serial.path == p, p).clicked() {
                    cfg.cat.serial.path = p.clone();
                }
            }
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

        ui.label("Poll rate");
        ui.add(DragValue::new(&mut cfg.cat.poll_hz).speed(0.5).range(0.5..=20.0).suffix(" Hz"));
        ui.end_row();

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
    cmds: &mut Vec<Command>,
) {
    use sdroxide_types::HpsdrConfig;
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };
    egui::Grid::new("hpsdr-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Devices");
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

        ui.label("Invert spectrum");
        ui.checkbox(&mut cfg.hpsdr.invert_spectrum, "Swap I/Q").on_hover_text(
            "Mirror the board's spectrum about the tuned frequency, on transmit as well \
             as receive. On by default: a Hermes-Lite 2 needs it. Turn it off only if \
             signals show up on the wrong side of the dial and nothing decodes — the \
             giveaway is a waterfall full of convincing traces while SSB lands on the \
             wrong sideband and FT8 returns no decodes at all.",
        );
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
    cmds: &mut Vec<Command>,
) {
    use sdroxide_types::{RtlSdrAgc, RtlSdrConfig, RtlSdrHfMode};
    let Some(cfg) = radio_edit.as_mut() else {
        ui.label("Radio configuration is only available in the native app.");
        return;
    };

    egui::Grid::new("rtlsdr-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Dongle");
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
            let shown = cfg.rtlsdr.serial.clone().unwrap_or_else(|| "— first one found —".into());
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
            .color(crate::theme::YELLOW),
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

/// TCI interface: WebSocket server address, IQ sample rate, and a
/// Test-connection button (the interface is chosen by the selector in
/// `settings_body`).
pub(in crate::app) fn settings_tci_tab(
    ui: &mut egui::Ui,
    radio_edit: &mut Option<sdroxide_types::RadioConfig>,
    tci_test: &mut bool,
    test_result: &Option<Result<String, String>>,
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

        ui.label("");
        if ui.button("Test connection").clicked() {
            *tci_test = true;
        }
        ui.end_row();
    });
    match test_result {
        Some(Ok(s)) => {
            ui.label(
                RichText::new(format!("Connected: {s}")).color(Color32::from_rgb(90, 200, 110)),
            );
        }
        Some(Err(e)) => {
            ui.label(RichText::new(format!("Failed: {e}")).color(Color32::from_rgb(230, 90, 80)));
        }
        None => {}
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Wideband IQ receive, audio transmit. Press \"Apply / reconnect\" to switch without a restart.",
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
        ui.label(RichText::new(&caps.label).size(14.0).strong().color(crate::theme::CYAN));
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
        if caps.antennas_rx.len() > 1 {
            ui.separator();
            ComboBox::from_id_salt("ant-rx").selected_text(self.state.antenna_rx.clone()).show_ui(
                ui,
                |ui| {
                    for a in &caps.antennas_rx {
                        if ui.selectable_label(self.state.antenna_rx == *a, a).clicked() {
                            cmds.push(Command::SetAntenna { dir: Direction::Rx, name: a.clone() });
                        }
                    }
                },
            );
        }
    }
}
