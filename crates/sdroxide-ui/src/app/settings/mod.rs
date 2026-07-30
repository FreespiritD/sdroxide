//! The Settings dialog.
//!
//! The window closure borrows `&self`, so it cannot reach `&mut self.ctrl`;
//! every edit is written into a [`SettingsIo`] and applied by
//! [`SdroxideApp::settings_window`] after the closure returns. That is also
//! where the blocking operations live — an HPSDR scan, a TCI connection test,
//! a subscription fetch — so none of them run inside a layout pass.
//!
//! [`SdroxideApp::settings_body`] draws the tab strip and dispatches to one
//! submodule per tab.

pub(in crate::app) mod controls;
pub(in crate::app) mod general;
pub(in crate::app) mod net;
pub(in crate::app) mod radio;
pub(in crate::app) mod servers;
pub(in crate::app) mod tle;
pub(in crate::app) mod ui_tab;

use eframe::egui::{self, Color32, ComboBox, RichText};
use sdroxide_types::{Command, LookupProvider, NetworkConfig};

use self::controls::settings_controls_tab;
use self::general::device_combo;
use self::net::{
    broadcast_stations_settings, net_heading, net_row, net_secret, operator_identity_note,
    settings_freedv_tab,
};
use self::radio::{settings_cat_tab, settings_hpsdr_tab, settings_rtlsdr_tab, settings_tci_tab};
use self::servers::{settings_rigctld_tab, settings_tci_server_tab, settings_wsjtx_tab};
use self::tle::settings_tle_tab;
use self::ui_tab::settings_ui_tab;
use crate::app::SdroxideApp;
use crate::app::persist::{
    load_broadcast_stations, persist_sat_config, persist_ui_settings,
    restore_bundled_broadcast_stations,
};

/// Settings dialog tabs: General (station identity + audio devices), the radio
/// interface and its settings, display/UI preferences, control inputs
/// (keyboard/mouse bindings), the network cockpit (spot feeds + uploads), and
/// the built-in TCI server.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum SettingsTab {
    General,
    Radio,
    Ui,
    Controls,
    Spots,
    FreeDv,
    Uploads,
    Servers,
    Tle,
}

/// Transient state of the TLE tab — what is in the paste box, which rows are
/// unfolded. Not persisted: none of it is a setting, it is where the operator
/// happens to be in the dialog.
#[derive(Default)]
pub(in crate::app) struct SatEditState {
    /// The "paste element sets here" box.
    paste: String,
    /// Catalogue number and name for a new frequency entry.
    new_freq_id: String,
    new_freq_name: String,
    /// Index of the pasted element set whose two lines are shown for editing.
    open_tle: Option<usize>,
    /// Index of the frequency entry whose links are shown for editing.
    open_freq: Option<usize>,
    /// What the last add attempt did, good or bad, so a paste that yielded
    /// nothing says so instead of appearing to have been ignored.
    note: String,
}

/// One subscription's fetch state, in a form both targets have.
///
/// The native type lives in `sdroxide-solar`, which the browser build does not
/// compile the fetching half of; copying the three fields the dialog shows
/// keeps the tab itself target-agnostic.
#[derive(Clone, Default)]
pub(in crate::app) struct SubStatusView {
    url: String,
    fetched_unix: i64,
    count: usize,
    /// How many of the listing's satellites are in the built-in curated list.
    /// Zero for everything that is not the amateur group.
    curated: usize,
    error: Option<String>,
}

/// Everything the settings dialog can change, collected in one place.
///
/// The window closure borrows `&self`, so `settings_body` can't reach
/// `&mut self.ctrl` — edits are written here and applied by `settings_window`
/// after the closure returns.
pub(in crate::app) struct SettingsIo<'a> {
    iface_opts: &'a [sdroxide_types::Backend],
    radio_edit: &'a mut Option<sdroxide_types::RadioConfig>,
    audio_pick: &'a mut Option<(bool, Option<String>)>,
    hpsdr_discover: &'a mut bool,
    /// Re-enumerate the USB bus for RTL-SDR dongles. Cheap and non-invasive —
    /// no device is opened — so it cannot disturb a running stream.
    rtlsdr_rescan: &'a mut bool,
    tci_test: &'a mut bool,
    apply_iface: &'a mut bool,
    ui_edit: &'a mut sdroxide_types::UiSettings,
    digi_edit: &'a mut sdroxide_types::DigiConfig,
    digi_seeded: bool,
    net_edit: &'a mut NetworkConfig,
    net_cmds: &'a mut String,
    net_apply: &'a mut bool,
    net_sync: &'a mut bool,
    /// The built-in TCI *server* — this app acting as a rig for third-party
    /// clients, as opposed to the TCI client configured on the Radio tab.
    tci_srv_edit: &'a mut sdroxide_types::TciServerConfig,
    tci_srv_apply: &'a mut bool,
    /// The built-in Hamlib rigctld server — the control-only surface every
    /// "NET rigctl" client speaks.
    rigctld_edit: &'a mut sdroxide_types::RigctldConfig,
    rigctld_apply: &'a mut bool,
    /// The WSJT-X UDP broadcast — decodes, status and logged QSOs for
    /// GridTracker, JTAlert, N1MM+ and Log4OM.
    wsjtx_edit: &'a mut sdroxide_types::WsjtxConfig,
    wsjtx_apply: &'a mut bool,
    /// Control-input bindings, plus the row (if any) waiting to capture a
    /// keypress. Persisted on close, since a rebind has no APPLY step.
    input_edit: &'a mut sdroxide_types::InputSettings,
    key_capture: &'a mut Option<usize>,
    midi_learn: &'a mut Option<crate::input::MidiLearn>,
    midi_rescan: &'a mut bool,
    /// The operator's satellite additions, and the transient state of the
    /// dialog that edits them. Persisted on change, like the input bindings:
    /// there is no APPLY step to hang it off.
    sat_edit: &'a mut sdroxide_types::SatConfig,
    sat_ui: &'a mut SatEditState,
    sat_subs: &'a [SubStatusView],
    /// Fetch every subscription now. Blocking, so it is done after the window
    /// closure the way the HPSDR scan is.
    sat_sub_refresh: &'a mut bool,
    /// How the 3D view draws its cloud deck: `Some(true)` marches the volume,
    /// `Some(false)` stacks shells through it. `None` where there is no 3D view
    /// to set it for — the browser client, whose solar view is a separate tab
    /// with its own settings — because a switch that provably does nothing is
    /// worse than no switch.
    solar_cloud_march: Option<&'a mut bool>,
    /// Reload the broadcast station list from disk, and restore the bundled one
    /// over the top of it. Both act on a file rather than on an edit buffer, so
    /// they are done after the window closure like the HPSDR scan.
    bc_reload: &'a mut bool,
    bc_restore: &'a mut bool,
    tab: &'a mut SettingsTab,
}

pub(in crate::app) fn enum_combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id: &str,
    cur: &mut T,
    all: &[T],
    label: impl Fn(T) -> &'static str,
) {
    ComboBox::from_id_salt(id).selected_text(label(*cur)).show_ui(ui, |ui| {
        for &opt in all {
            if ui.selectable_label(*cur == opt, label(opt)).clicked() {
                *cur = opt;
            }
        }
    });
}

impl SdroxideApp {
    pub(in crate::app) fn settings_window(&mut self, ctx: &egui::Context, cmds: &mut Vec<Command>) {
        // Query slow lists (cpal devices, serial ports, radio config) once per
        // dialog-open; a pick invalidates so the selection refreshes.
        if !self.show_settings {
            self.audio_devices = None;
            self.audio_devices_queried = false;
            return;
        } else if !self.audio_devices_queried {
            self.audio_devices = self.ctrl.audio_devices();
            self.radio_cfg = self.ctrl.radio_config();
            self.serial_ports = self.ctrl.serial_ports();
            (self.midi_in_ports, self.midi_out_ports) = self.input.midi_ports();
            // The TCI server lives with the engine, so only a native client
            // owns its config; the browser remote gets `None` and a note.
            if let Some(cfg) = self.ctrl.tci_server_config() {
                self.tci_srv_edit = cfg;
                self.tci_srv_seeded = true;
            }
            if let Some(cfg) = self.ctrl.rigctld_config() {
                self.rigctld_edit = cfg;
                self.rigctld_seeded = true;
            }
            if let Some(cfg) = self.ctrl.wsjtx_config() {
                self.wsjtx_edit = cfg;
                self.wsjtx_seeded = true;
            }
            // The satellite config is the client's own, so it comes from the
            // live copy rather than from the engine. Subscription status is
            // read from the disk cache, which is the only source that has an
            // answer when the solar window has never been opened.
            self.sat_cfg_edit = (*self.sat_cfg).clone();
            self.refresh_sat_sub_status();
            self.audio_devices_queried = true;
        }
        // Edits collected here and applied after the window closure, which
        // borrows `&self` and so can't touch `&mut self.ctrl`.
        let mut audio_pick: Option<(bool, Option<String>)> = None;
        let mut hpsdr_discover = false;
        let mut rtlsdr_rescan = false;
        let mut tci_test = false;
        let mut apply_iface = false;
        let mut radio_edit = self.radio_cfg.clone();
        let mut ui_edit = self.ui_settings;
        let mut digi_edit = self.digi_cfg_edit.clone();
        let digi_seeded = self.digi_cfg_seeded;
        let mut net_edit = self.net_cfg_edit.clone();
        let mut net_cmds = self.net_cluster_cmds.clone();
        let mut net_apply = false;
        let mut net_sync = false;
        let mut tci_srv_edit = self.tci_srv_edit.clone();
        let mut tci_srv_apply = false;
        let mut rigctld_edit = self.rigctld_edit.clone();
        let mut rigctld_apply = false;
        let mut wsjtx_edit = self.wsjtx_edit.clone();
        let mut wsjtx_apply = false;
        let mut input_edit = self.input.cfg.clone();
        let mut key_capture = self.input.key_capture;
        let mut midi_learn = self.input.midi_learn;
        let mut midi_rescan = false;
        let mut sat_edit = self.sat_cfg_edit.clone();
        let mut sat_ui = std::mem::take(&mut self.sat_ui);
        let mut sat_sub_refresh = false;
        let sat_subs = self.sat_sub_views();
        let mut bc_reload = false;
        let mut bc_restore = false;

        // The concrete interface types the user chooses between. SoapySDR only
        // appears when compiled in; there is no auto-detect (an unavailable
        // interface falls back to a null source so the user can reconfigure).
        let mut iface_opts: Vec<sdroxide_types::Backend> = Vec::new();
        if self.soapy_supported {
            iface_opts.push(sdroxide_types::Backend::Soapy);
        }
        iface_opts.push(sdroxide_types::Backend::Hpsdr);
        iface_opts.push(sdroxide_types::Backend::Cat);
        iface_opts.push(sdroxide_types::Backend::Tci);
        // Ungated, unlike SoapySDR: the RTL-SDR driver is pure Rust and needs
        // no system library, so it is compiled into every build variant.
        iface_opts.push(sdroxide_types::Backend::RtlSdr);

        let mut tab = self.settings_tab;
        let mut open = self.show_settings;
        // The 3D window owns the live copy of its own settings — `view.solar3d`
        // is only the snapshot persisted from it — so this is read out of the
        // window here and handed back to it below, the way `ui_edit` is.
        #[cfg(not(target_arch = "wasm32"))]
        let mut solar_cloud_march = self.solar.cloud_march();
        // The window does its own scrolling, so its bar can only be themed
        // through the context style — lend the palette for the length of the
        // call and hand the body back the normal one.
        let bars = crate::theme::ScrollPalette::push(ctx);
        let resp = egui::Window::new("Settings")
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(false)
            .vscroll(true)
            .show(ctx, |ui| {
                bars.restore(ui);
                self.settings_body(
                    ui,
                    cmds,
                    &mut SettingsIo {
                        iface_opts: &iface_opts,
                        radio_edit: &mut radio_edit,
                        audio_pick: &mut audio_pick,
                        hpsdr_discover: &mut hpsdr_discover,
                        rtlsdr_rescan: &mut rtlsdr_rescan,
                        tci_test: &mut tci_test,
                        apply_iface: &mut apply_iface,
                        ui_edit: &mut ui_edit,
                        digi_edit: &mut digi_edit,
                        digi_seeded,
                        net_edit: &mut net_edit,
                        net_cmds: &mut net_cmds,
                        net_apply: &mut net_apply,
                        bc_reload: &mut bc_reload,
                        bc_restore: &mut bc_restore,
                        net_sync: &mut net_sync,
                        tci_srv_edit: &mut tci_srv_edit,
                        tci_srv_apply: &mut tci_srv_apply,
                        rigctld_edit: &mut rigctld_edit,
                        rigctld_apply: &mut rigctld_apply,
                        wsjtx_edit: &mut wsjtx_edit,
                        wsjtx_apply: &mut wsjtx_apply,
                        input_edit: &mut input_edit,
                        key_capture: &mut key_capture,
                        midi_learn: &mut midi_learn,
                        midi_rescan: &mut midi_rescan,
                        sat_edit: &mut sat_edit,
                        sat_ui: &mut sat_ui,
                        sat_subs: &sat_subs,
                        sat_sub_refresh: &mut sat_sub_refresh,
                        #[cfg(not(target_arch = "wasm32"))]
                        solar_cloud_march: Some(&mut solar_cloud_march),
                        #[cfg(target_arch = "wasm32")]
                        solar_cloud_march: None,
                        tab: &mut tab,
                    },
                );
            });
        bars.pop(ctx);
        if let Some(r) = &resp {
            crate::chrome::paint_window_border(ctx, &r.response);
        }
        self.show_settings = open;
        self.settings_tab = tab;
        #[cfg(not(target_arch = "wasm32"))]
        if solar_cloud_march != self.solar.cloud_march() {
            self.solar.set_cloud_march(solar_cloud_march);
        }
        // Persist net-config edits (kept across frames) and apply on demand.
        self.net_cfg_edit = net_edit;
        self.net_cluster_cmds = net_cmds;
        if net_apply {
            self.net_cfg_edit.cluster.commands = self
                .net_cluster_cmds
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            // The engine persists net.json when it applies this.
            cmds.push(Command::SetNetworkConfig(self.net_cfg_edit.clone()));
        }
        if net_sync {
            cmds.push(Command::SyncConfirmations);
        }
        self.input.key_capture = key_capture;
        self.input.midi_learn = midi_learn;
        if midi_rescan {
            (self.midi_in_ports, self.midi_out_ports) = self.input.midi_ports();
        }
        if input_edit != self.input.cfg {
            // Bindings take effect on the next frame and are written straight
            // out — a rebind the operator can't see saved is a rebind they
            // will make again after the next restart.
            self.input.cfg = input_edit;
            self.input.persist();
        }
        self.tci_srv_edit = tci_srv_edit;
        if tci_srv_apply {
            // The engine persists tciserver.json when it binds (or fails to).
            cmds.push(Command::SetTciServerConfig(self.tci_srv_edit.clone()));
        }
        self.rigctld_edit = rigctld_edit;
        if rigctld_apply {
            // The engine persists rigctld.json when it binds (or fails to).
            cmds.push(Command::SetRigctldConfig(self.rigctld_edit.clone()));
        }
        self.wsjtx_edit = wsjtx_edit;
        if wsjtx_apply {
            // The engine persists wsjtx.json when it opens the socket.
            cmds.push(Command::SetWsjtxConfig(self.wsjtx_edit.clone()));
        }
        self.sat_ui = sat_ui;
        if sat_edit != self.sat_cfg_edit {
            // Written straight out, like the input bindings: there is no APPLY
            // step here, and a satellite the operator cannot see saved is one
            // they will add again after the next restart. The solar window
            // picks the new `Arc` up on its next frame.
            self.sat_cfg_edit = sat_edit;
            self.sat_cfg_edit.prune();
            self.sat_cfg = std::sync::Arc::new(self.sat_cfg_edit.clone());
            persist_sat_config(&self.sat_cfg_edit);
        }
        if sat_sub_refresh {
            // Blocking: one HTTPS round trip per subscription. After the window
            // closure, the way the HPSDR scan is.
            self.refresh_sat_subs_now();
        }
        if bc_restore {
            restore_bundled_broadcast_stations();
        }
        if bc_reload || bc_restore {
            self.broadcast = load_broadcast_stations();
            // Force a rebuild rather than waiting up to a minute for the tick.
            self.broadcast_minute = -1;
        }
        if let Some((output, name)) = audio_pick {
            self.ctrl.set_audio_device(output, name);
            self.audio_devices_queried = false;
        }
        if hpsdr_discover {
            // Blocking LAN scan (~1.5 s); done after the window closure so it can
            // take `&self.ctrl`. Results feed the device dropdown next frame.
            self.hpsdr_devices = self.ctrl.discover_hpsdr();
        }
        if rtlsdr_rescan {
            // USB enumeration only — no device is opened, so this is safe to
            // press at any time, including while a dongle is streaming.
            self.rtlsdr_devices = self.ctrl.list_rtlsdr();
        }
        if tci_test {
            // Blocking connect (~up to 3 s); after the closure so it can take
            // `&self.ctrl`. The result is shown in the TCI section next frame.
            if let Some(cfg) = &radio_edit {
                self.tci_test_result = Some(self.ctrl.test_tci(&cfg.tci.address));
            }
        }
        if apply_iface {
            // Persist the latest edits, then rebuild the live source (no restart).
            if let Some(cfg) = &radio_edit {
                self.ctrl.set_radio_config(cfg.clone());
            }
            self.ctrl.reopen_source();
        }
        if radio_edit != self.radio_cfg {
            if let Some(cfg) = &radio_edit {
                self.ctrl.set_radio_config(cfg.clone());
            }
            self.radio_cfg = radio_edit;
        }
        if ui_edit != self.ui_settings {
            // Live: fps + averaging flow to the engine via the spectrum-config
            // diff next frame; waterfall speed is read each frame. Persist too.
            self.ui_settings = ui_edit;
            persist_ui_settings(&self.ui_settings);
        }
        // Callsign/grid from the General tab — same store as the FT8/SSTV setup
        // dialog. Only apply once seeded so we can't overwrite the engine's saved
        // config with defaults.
        if digi_seeded && digi_edit != self.digi_cfg_edit {
            self.digi_cfg_edit = digi_edit;
            cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
        }
    }

    /// The Settings body: a General tab (station identity + the sound devices)
    /// and a Radio tab whose single interface selector drives the
    /// interface-specific section below it.
    ///
    /// Everything the dialog changes goes out through `io`, because the window
    /// closure borrows `&self` — see [`SettingsIo`].
    pub(in crate::app) fn settings_body(
        &self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        io: &mut SettingsIo,
    ) {
        use sdroxide_types::Backend;

        // Wrapped: the tab strip no longer fits the window's width on one line.
        ui.horizontal_wrapped(|ui| {
            for (t, label) in [
                (SettingsTab::General, "General"),
                (SettingsTab::Radio, "Radio"),
                (SettingsTab::Ui, "UI"),
                (SettingsTab::Controls, "Controls"),
                (SettingsTab::Spots, "Spots"),
                (SettingsTab::FreeDv, "FreeDV"),
                (SettingsTab::Uploads, "Uploads"),
                (SettingsTab::Servers, "Servers"),
                (SettingsTab::Tle, "TLE"),
            ] {
                if crate::chrome::chip(ui, *io.tab == t, label).clicked() {
                    *io.tab = t;
                }
            }
        });
        ui.separator();

        let backend = io.radio_edit.as_ref().map(|c| c.backend);

        match io.tab {
            SettingsTab::General => {
                ui.label(RichText::new("Station").size(14.0).strong().color(crate::theme::CYAN));
                ui.add_space(6.0);
                if !io.digi_seeded {
                    ui.label(
                        RichText::new(
                            "Enter a digital mode (FT8 / SSTV / …) once to load the saved values.",
                        )
                        .weak(),
                    );
                }
                ui.add_enabled_ui(io.digi_seeded, |ui| {
                    egui::Grid::new("general-grid").num_columns(2).spacing([12.0, 8.0]).show(
                        ui,
                        |ui| {
                            ui.label("Callsign");
                            if ui.text_edit_singleline(&mut io.digi_edit.my_call).changed() {
                                io.digi_edit.my_call = io.digi_edit.my_call.to_uppercase();
                            }
                            ui.end_row();
                            ui.label("Grid square");
                            ui.text_edit_singleline(&mut io.digi_edit.my_grid);
                            ui.end_row();
                        },
                    );
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Your callsign and grid, shared across FT8/FT4, SSTV image headers, and \
                         the logbook. Also editable from the FT8 / SSTV setup dialog.",
                    )
                    .weak(),
                );

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                self.settings_user_audio(ui, io.audio_pick);
                // The radio's own sound card is only used by the CAT / Audio
                // interface; every other backend carries its audio in-band.
                if backend == Some(Backend::Cat) {
                    if let (Some(devs), Some(cfg)) =
                        (self.audio_devices.as_ref(), io.radio_edit.as_mut())
                    {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Radio audio (sound card)").strong());
                        egui::Grid::new("radio-audio").num_columns(2).spacing([12.0, 6.0]).show(
                            ui,
                            |ui| {
                                let (ci, co) =
                                    (cfg.radio_audio_in.clone(), cfg.radio_audio_out.clone());
                                ui.label("From radio (RX)");
                                device_combo(ui, "r-in", &devs.inputs, &ci, |n| {
                                    cfg.radio_audio_in = n
                                });
                                ui.end_row();
                                ui.label("To radio (TX)");
                                device_combo(ui, "r-out", &devs.outputs, &co, |n| {
                                    cfg.radio_audio_out = n
                                });
                                ui.end_row();
                            },
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui
                                .button("Apply / reconnect")
                                .on_hover_text(
                                    "Reopen the CAT rig with these sound cards — no restart",
                                )
                                .clicked()
                            {
                                *io.apply_iface = true;
                            }
                            ui.label(
                                RichText::new("Reconnects the radio without restarting.").weak(),
                            );
                        });
                    }
                }
            }
            SettingsTab::Radio => {
                let Some(cfg) = io.radio_edit.as_mut() else {
                    ui.label("Radio configuration is only available in the native app.");
                    return;
                };
                // The single "which radio interface" selector.
                egui::Grid::new("iface-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new("Radio interface").strong());
                    enum_combo(ui, "iface", &mut cfg.backend, io.iface_opts, Backend::label);
                    ui.end_row();
                });
                ui.separator();

                match cfg.backend {
                    Backend::Soapy => {
                        self.settings_device_tab(ui, cmds);
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "Choose the SoapySDR device with --device or device_args in \
                                 config.toml.",
                            )
                            .weak(),
                        );
                    }
                    Backend::Hpsdr => settings_hpsdr_tab(
                        ui,
                        &self.hpsdr_devices,
                        io.radio_edit,
                        io.hpsdr_discover,
                        cmds,
                    ),
                    Backend::Cat => settings_cat_tab(ui, &self.serial_ports, io.radio_edit),
                    Backend::Tci => {
                        settings_tci_tab(ui, io.radio_edit, io.tci_test, &self.tci_test_result)
                    }
                    Backend::RtlSdr => settings_rtlsdr_tab(
                        ui,
                        &self.rtlsdr_devices,
                        io.radio_edit,
                        io.rtlsdr_rescan,
                        cmds,
                    ),
                    // Legacy configs may still carry the removed auto-detect
                    // backend; prompt the user to pick a concrete interface.
                    Backend::Auto => {
                        ui.label(
                            RichText::new(
                                "Pick a radio interface above (this configuration used the \
                                 removed auto-detect mode).",
                            )
                            .weak(),
                        );
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button("Apply / reconnect")
                        .on_hover_text("Switch to this interface now — no restart needed")
                        .clicked()
                    {
                        *io.apply_iface = true;
                    }
                    ui.label(RichText::new("Switches the live radio without restarting.").weak());
                });
            }
            SettingsTab::Ui => settings_ui_tab(ui, io.ui_edit, io.solar_cloud_march.as_deref_mut()),
            SettingsTab::Spots => {
                operator_identity_note(ui, io.digi_edit, io.digi_seeded);

                net_heading(ui, "DX cluster (telnet)");
                ui.checkbox(&mut io.net_edit.cluster.enabled, "Enabled");
                net_row(ui, "Host", &mut io.net_edit.cluster.host, 220.0);
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 22.0], egui::Label::new("Port"));
                    ui.add(egui::DragValue::new(&mut io.net_edit.cluster.port).range(1..=65535));
                });
                net_row(ui, "Login call", &mut io.net_edit.cluster.login, 140.0);
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 22.0], egui::Label::new("Commands"));
                    ui.add(
                        egui::TextEdit::multiline(io.net_cmds)
                            .desired_rows(2)
                            .hint_text("one per line, e.g. SET/FT8")
                            .desired_width(220.0),
                    );
                });

                net_heading(ui, "POTA / SOTA / PSK Reporter");
                ui.checkbox(&mut io.net_edit.pota.enabled, "POTA activator spots");
                ui.checkbox(&mut io.net_edit.sota.enabled, "SOTA spots");
                ui.checkbox(&mut io.net_edit.psk.enabled, "PSK Reporter (current band)");
                ui.checkbox(&mut io.net_edit.psk.report, "Upload my FT8/FT4 decodes")
                    .on_hover_text(
                        "Report what this station hears to pskreporter.info, so it appears \
                         there as a receiver. Uses the callsign and grid from the General tab.",
                    );
                if io.net_edit.psk.report {
                    net_row(ui, "Antenna", &mut io.net_edit.psk.antenna, 200.0);
                    ui.horizontal(|ui| {
                        ui.add_sized([96.0, 22.0], egui::Label::new("Collector"));
                        ui.add(
                            egui::TextEdit::singleline(&mut io.net_edit.psk.host)
                                .desired_width(140.0),
                        );
                        ui.add(egui::DragValue::new(&mut io.net_edit.psk.port).range(1..=65535))
                            .on_hover_text("4739 is the live collector, 14739 the test one");
                    });
                }
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 22.0], egui::Label::new("Max age (s)"));
                    let age = &mut io.net_edit.spot_max_age_secs;
                    ui.add(egui::DragValue::new(age).range(60..=7200));
                });

                ui.add_space(8.0);
                if crate::chrome::chip_accent(
                    ui,
                    false,
                    RichText::new(" APPLY ").strong(),
                    crate::theme::GREEN,
                    crate::theme::INK_ON_CYAN,
                )
                .on_hover_text("Persist and (re)connect the feeds")
                .clicked()
                {
                    *io.net_apply = true;
                }

                net_heading(ui, "Broadcast stations");
                broadcast_stations_settings(ui, io.bc_reload, io.bc_restore);
            }
            SettingsTab::Uploads => {
                net_heading(ui, "Callsign lookup");
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 22.0], egui::Label::new("Provider"));
                    egui::ComboBox::from_id_salt("lookup_provider")
                        .selected_text(io.net_edit.lookup_provider.label())
                        .show_ui(ui, |ui| {
                            for p in LookupProvider::ALL {
                                let cur = &mut io.net_edit.lookup_provider;
                                ui.selectable_value(cur, p, p.label());
                            }
                        });
                });
                ui.checkbox(
                    &mut io.net_edit.auto_lookup,
                    "Auto-fill name/QTH/grid on spot click & QSO",
                );
                net_row(ui, "QRZ user", &mut io.net_edit.qrz.user, 140.0);
                net_secret(ui, "QRZ pass", &mut io.net_edit.qrz.password, 140.0);
                net_row(ui, "HamQTH user", &mut io.net_edit.hamqth.user, 140.0);
                net_secret(ui, "HamQTH pass", &mut io.net_edit.hamqth.password, 140.0);

                net_heading(ui, "Upload — eQSL / QRZ / Club Log");
                net_row(ui, "eQSL user", &mut io.net_edit.eqsl.user, 140.0);
                net_secret(ui, "eQSL pass", &mut io.net_edit.eqsl.password, 140.0);
                net_secret(ui, "QRZ log key", &mut io.net_edit.qrz_logbook_key, 200.0);
                net_row(ui, "Club Log email", &mut io.net_edit.clublog.user, 200.0);
                net_secret(ui, "Club Log pass", &mut io.net_edit.clublog.password, 140.0);
                net_secret(ui, "Club Log key", &mut io.net_edit.clublog_api_key, 200.0);
                ui.checkbox(&mut io.net_edit.auto_upload, "Auto-upload each new QSO");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut io.net_edit.auto_upload_eqsl, "eQSL");
                    ui.checkbox(&mut io.net_edit.auto_upload_qrz, "QRZ");
                    ui.checkbox(&mut io.net_edit.auto_upload_clublog, "Club Log");
                });

                net_heading(ui, "Confirmations (download)");
                net_row(ui, "LoTW user", &mut io.net_edit.lotw.user, 140.0);
                net_secret(ui, "LoTW pass", &mut io.net_edit.lotw.password, 140.0);
                ui.label(
                    RichText::new(
                        "LoTW upload uses TQSL — export ADIF from the logbook and sign it. \
                         LoTW/eQSL confirmations are downloaded here to mark worked-vs-confirmed.",
                    )
                    .size(10.5)
                    .color(Color32::from_gray(140)),
                );

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if crate::chrome::chip_accent(
                        ui,
                        false,
                        RichText::new(" APPLY ").strong(),
                        crate::theme::GREEN,
                        crate::theme::INK_ON_CYAN,
                    )
                    .clicked()
                    {
                        *io.net_apply = true;
                    }
                    if crate::chrome::chip(ui, false, "SYNC CONFIRMATIONS").clicked() {
                        *io.net_sync = true;
                    }
                });
            }
            SettingsTab::FreeDv => {
                // The reported identity is the operator's, from the General tab.
                let call = io.digi_edit.my_call.trim().to_string();
                let grid = io.digi_edit.my_grid.trim().to_string();
                settings_freedv_tab(
                    ui,
                    io.net_edit,
                    &call,
                    &grid,
                    io.digi_seeded,
                    &self.net_status,
                    io.net_apply,
                )
            }
            SettingsTab::Controls => settings_controls_tab(
                ui,
                io,
                &self.memories,
                &self.midi_in_ports,
                &self.midi_out_ports,
                &self.input.midi_status(),
                self.input.last_midi,
            ),
            SettingsTab::Servers => {
                settings_rigctld_tab(
                    ui,
                    io.rigctld_edit,
                    self.rigctld_seeded,
                    &self.rigctld_status,
                    io.rigctld_apply,
                );
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                settings_tci_server_tab(
                    ui,
                    io.tci_srv_edit,
                    self.tci_srv_seeded,
                    &self.tci_srv_status,
                    io.tci_srv_apply,
                );
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                settings_wsjtx_tab(ui, io.wsjtx_edit, self.wsjtx_seeded, io.wsjtx_apply);
            }
            SettingsTab::Tle => settings_tle_tab(ui, io),
        }
    }

    /// Subscription status for the settings dialog.
    ///
    /// The live feed is preferred — it has the result of the fetch it just did
    /// — but it only exists while the solar window is open, so the disk cache
    /// answers for the far more common case of the dialog being opened with the
    /// window shut.
    #[cfg(not(target_arch = "wasm32"))]
    fn refresh_sat_sub_status(&mut self) {
        let live = self.solar.tle_sub_status();
        let subs: Vec<_> = self.sat_cfg.subs.clone();
        self.sat_sub_status =
            if live.is_empty() { sdroxide_solar::tlesub::status_all(&subs) } else { live }
                .into_iter()
                .map(|s| SubStatusView {
                    url: s.url,
                    fetched_unix: s.fetched_unix,
                    count: s.count,
                    curated: s.curated,
                    error: s.error,
                })
                .collect();
    }

    #[cfg(target_arch = "wasm32")]
    fn refresh_sat_sub_status(&mut self) {}

    fn sat_sub_views(&self) -> Vec<SubStatusView> {
        self.sat_sub_status.clone()
    }

    /// Fetch every enabled subscription now, from the settings dialog's UPDATE
    /// NOW button. Blocking — up to one HTTPS round trip per subscription.
    ///
    /// The solar window's feed shares the same disk cache, so a listing fetched
    /// here is what it serves next time it looks, without a second request.
    #[cfg(not(target_arch = "wasm32"))]
    fn refresh_sat_subs_now(&mut self) {
        let subs: Vec<_> = self.sat_cfg_edit.subs.clone();
        let done = sdroxide_solar::tlesub::refresh_all(&subs);
        let failed = done.iter().filter(|s| s.error.is_some()).count();
        let total: usize = done.iter().map(|s| s.count).sum();
        self.sat_ui.note = match (done.len(), failed) {
            (0, _) => "No enabled subscriptions to update.".to_string(),
            (n, 0) => format!("Updated {n} subscription(s): {total} satellites."),
            (n, f) => format!("Updated {} of {n}; {f} failed — see the rows above.", n - f),
        };
        self.refresh_sat_sub_status();
        // The window's feed is told to re-read the cache rather than being left
        // on what it loaded at open time.
        self.solar.reload_tle_subs();
    }

    #[cfg(target_arch = "wasm32")]
    fn refresh_sat_subs_now(&mut self) {}
}
