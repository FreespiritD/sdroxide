//! The Settings dialog.
//!
//! The window closure borrows `&self`, so it cannot reach `&mut self.ctrl`;
//! every edit is written into a [`SettingsIo`] and applied by
//! [`SdroxideApp::settings_window`] after the closure returns. That is also
//! where the blocking operations live — an HPSDR scan, a TCI connection test —
//! so neither runs inside a layout pass.
//!
//! Tabs that configure the *station* rather than this screen — Spots, FreeDV,
//! Uploads, Servers, TLE — are seeded from `RadioEvent::StationConfig` and stay
//! disabled until it arrives. They edit files on the engine host, which may not
//! be this machine, so there is nothing here to read them from and applying an
//! unseeded copy would write defaults over the real thing.
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
use self::general::{device_combo, remote_access_settings};
use self::net::{
    broadcast_stations_settings, net_heading, net_row, net_secret, operator_identity_note,
    settings_freedv_tab,
};
use self::radio::{
    settings_cat_tab, settings_hpsdr_tab, settings_rtlsdr_tab, settings_rx888_tab,
    settings_smartsdr_tab, settings_tci_tab,
};
use self::servers::{settings_rigctld_tab, settings_tci_server_tab, settings_wsjtx_tab};
use self::tle::settings_tle_tab;
use self::ui_tab::settings_ui_tab;
use crate::app::SdroxideApp;
use crate::app::persist::persist_ui_settings;

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
    /// Whether UPDATE NOW is waiting on the engine. The fetch happens over
    /// there — one HTTPS round trip per subscription — so the answer arrives as
    /// an event rather than as a return value, and this is what tells the
    /// arriving status it was asked for.
    fetching: bool,
}

/// Everything the settings dialog can change, collected in one place.
///
/// The window closure borrows `&self`, so `settings_body` can't reach
/// `&mut self.ctrl` — edits are written here and applied by `settings_window`
/// after the closure returns.
pub(in crate::app) struct SettingsIo<'a> {
    iface_opts: &'a [sdroxide_types::Backend],
    radio_edit: &'a mut Option<sdroxide_types::RadioConfig>,
    /// Converter offset in Hz, buffered until Apply — see
    /// `SdroxideApp::converter_edit_hz`.
    converter_hz: &'a mut Option<f64>,
    /// The RX and TX tuning ranges as typed, buffered until Apply — see
    /// `SdroxideApp::range_edit`.
    ranges: &'a mut Option<(String, String)>,
    audio_pick: &'a mut Option<(bool, Option<String>)>,
    hpsdr_discover: &'a mut bool,
    /// Re-enumerate the USB bus for RTL-SDR dongles. Cheap and non-invasive —
    /// no device is opened — so it cannot disturb a running stream.
    rtlsdr_rescan: &'a mut bool,
    rx888_rescan: &'a mut bool,
    tci_test: &'a mut bool,
    /// Listen for FlexRadio discovery broadcasts (a couple of seconds, blocking).
    smartsdr_discover: &'a mut bool,
    smartsdr_test: &'a mut bool,
    /// Copy the last SmartSDR session's protocol trace to the clipboard.
    smartsdr_copy_report: &'a mut bool,
    apply_iface: &'a mut bool,
    ui_edit: &'a mut sdroxide_types::UiSettings,
    /// Who may connect to this machine's server, or `None` where this client
    /// is in no position to say — a remote one, and every browser one. Those
    /// credentials are `config.toml` on the machine the radio is attached to,
    /// and this is not it.
    access_edit: Option<&'a mut sdroxide_types::RemoteAccess>,
    digi_edit: &'a mut sdroxide_types::DigiConfig,
    digi_seeded: bool,
    net_edit: &'a mut NetworkConfig,
    /// Whether the engine has said what the station's network config actually
    /// is. Until it has, the tabs that edit it are disabled: applying an
    /// unseeded copy would write defaults over the operator's real settings,
    /// and showing empty boxes would claim the station is unconfigured when it
    /// is not.
    net_seeded: bool,
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
    /// dialog that edits them. Sent to the engine on change, like the input
    /// bindings are written on change: there is no APPLY step to hang it off.
    sat_edit: &'a mut sdroxide_types::SatConfig,
    /// Whether the engine has said what the station tracks. Same contract as
    /// `net_seeded`, and with more at stake: this tab writes on every keystroke.
    sat_seeded: bool,
    sat_ui: &'a mut SatEditState,
    sat_subs: &'a [sdroxide_types::TleSubStatus],
    /// Re-fetch every subscription now. The engine does it, so this is a
    /// command rather than a blocking call.
    sat_sub_refresh: &'a mut bool,
    /// How the 3D view draws its cloud deck: `Some(true)` marches the volume,
    /// `Some(false)` stacks shells through it. `None` where there is no 3D view
    /// to set it for — the browser client, whose solar view is a separate tab
    /// with its own settings — because a switch that provably does nothing is
    /// worse than no switch.
    solar_cloud_march: Option<&'a mut bool>,
    /// Re-read the operator's broadcast station file, and re-download this
    /// season's schedule. Both act on files rather than on an edit buffer, so
    /// they are done after the window closure like the HPSDR scan.
    bc_reload: &'a mut bool,
    bc_refetch: &'a mut bool,
    /// Whether a schedule download is in flight, and what the last one did.
    bc_fetching: bool,
    bc_status: Option<&'a Result<String, String>>,
    tab: &'a mut SettingsTab,
}

/// Guard for the three tabs that edit the network config. Returns whether the
/// station's copy has arrived; if it has not, it says so and the caller draws
/// nothing.
///
/// The alternative — showing the boxes empty — reads as "this station has
/// nothing configured", which is a lie whenever the engine is on another
/// machine, and one the operator could act on by typing over it.
fn net_seeded_note(ui: &mut egui::Ui, seeded: bool) -> bool {
    if !seeded {
        ui.label(RichText::new("Waiting for the station's network configuration…").weak());
    }
    seeded
}

/// One "which frequencies does this radio cover" box, in the megahertz an
/// operator would say them in.
///
/// Whatever is typed is checked as it is typed and the fault named underneath,
/// because the alternative — finding out on Apply, by way of a band that has
/// quietly stopped working — is a poor way to learn that a dash was a slash.
fn freq_range_edit(ui: &mut egui::Ui, id: &str, text: &mut String, hover: &str) {
    ui.vertical(|ui| {
        ui.add(
            egui::TextEdit::singleline(text)
                .id_salt(id)
                .desired_width(220.0)
                .hint_text("as the device reports"),
        )
        .on_hover_text(hover);
        if let Err(e) = sdroxide_types::parse_freq_ranges(text) {
            ui.label(RichText::new(e).color(Color32::from_rgb(230, 90, 80)));
        }
    });
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
            // Drop an uncommitted converter offset rather than carry it to the
            // next open: closing the dialog without pressing Apply means the
            // radio is still on the old one, and the box should say so.
            self.converter_edit_hz = None;
            self.range_edit = None;
            return;
        } else if !self.audio_devices_queried {
            self.audio_devices = self.ctrl.audio_devices();
            self.radio_cfg = self.ctrl.radio_config();
            self.serial_ports = self.ctrl.serial_ports();
            (self.midi_in_ports, self.midi_out_ports) = self.input.midi_ports();
            // Everything the *station* is set to — the network cockpit, the two
            // built-in servers, the WSJT-X broadcast and the satellites — was
            // seeded from `RadioEvent::StationConfig` and needs no query here:
            // those files live wherever the engine does, which may not be this
            // machine at all.
            //
            // Subscription status is the exception worth refreshing: a native
            // client with the solar window open has a fetcher of its own, and
            // what it last did is fresher than what the engine announced.
            self.refresh_sat_sub_status();
            self.audio_devices_queried = true;
        }
        // Edits collected here and applied after the window closure, which
        // borrows `&self` and so can't touch `&mut self.ctrl`.
        let mut audio_pick: Option<(bool, Option<String>)> = None;
        let mut hpsdr_discover = false;
        let mut rtlsdr_rescan = false;
        let mut rx888_rescan = false;
        let mut tci_test = false;
        let mut smartsdr_discover = false;
        let mut smartsdr_test = false;
        let mut smartsdr_copy_report = false;
        let mut apply_iface = false;
        let mut radio_edit = self.radio_cfg.clone();
        let mut converter_hz = self.converter_edit_hz;
        let mut ranges = self.range_edit.clone();
        let mut ui_edit = self.ui_settings;
        // Only where the engine is in this process: see `SettingsIo`.
        let owns_server = !self.ctrl.engine_is_remote();
        let mut access_edit = self.remote_access.clone();
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
        let sat_subs = self.sat_sub_status.clone();
        let mut bc_reload = false;
        let mut bc_refetch = false;

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
        iface_opts.push(sdroxide_types::Backend::SmartSdr);
        // Ungated, unlike SoapySDR: the RTL-SDR driver is pure Rust and needs
        // no system library, so it is compiled into every build variant.
        iface_opts.push(sdroxide_types::Backend::RtlSdr);
        // Same reasoning as the RTL-SDR: pure Rust over `nusb`, no system
        // library, so it is in every build variant.
        iface_opts.push(sdroxide_types::Backend::Rx888);

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
                        converter_hz: &mut converter_hz,
                        ranges: &mut ranges,
                        audio_pick: &mut audio_pick,
                        hpsdr_discover: &mut hpsdr_discover,
                        rtlsdr_rescan: &mut rtlsdr_rescan,
                        rx888_rescan: &mut rx888_rescan,
                        tci_test: &mut tci_test,
                        smartsdr_discover: &mut smartsdr_discover,
                        smartsdr_test: &mut smartsdr_test,
                        smartsdr_copy_report: &mut smartsdr_copy_report,
                        apply_iface: &mut apply_iface,
                        ui_edit: &mut ui_edit,
                        access_edit: owns_server.then_some(&mut access_edit),
                        digi_edit: &mut digi_edit,
                        digi_seeded,
                        net_edit: &mut net_edit,
                        net_seeded: self.net_cfg_seeded,
                        net_cmds: &mut net_cmds,
                        net_apply: &mut net_apply,
                        bc_reload: &mut bc_reload,
                        bc_refetch: &mut bc_refetch,
                        bc_fetching: self.broadcast_fetch.is_some(),
                        bc_status: self.broadcast_fetch_status.as_ref(),
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
                        sat_seeded: self.sat_cfg_seeded,
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
        if self.net_cfg_seeded {
            self.net_cfg_edit = net_edit;
            self.net_cluster_cmds = net_cmds;
        }
        if net_apply && self.net_cfg_seeded {
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
        if self.sat_cfg_seeded && sat_edit != self.sat_cfg_edit {
            // Written straight out, like the input bindings: there is no APPLY
            // step here, and a satellite the operator cannot see saved is one
            // they will add again after the next restart. The engine persists
            // it — the subscribed listings are fetched on its machine — and the
            // solar window picks the new `Arc` up on its next frame.
            //
            // Gated on having been seeded, so a client that has not yet been
            // told what the station tracks cannot write an empty list over it.
            self.sat_cfg_edit = sat_edit;
            self.sat_cfg_edit.prune();
            self.sat_cfg = std::sync::Arc::new(self.sat_cfg_edit.clone());
            cmds.push(Command::SetSatConfig(self.sat_cfg_edit.clone()));
        }
        if sat_sub_refresh {
            self.refresh_sat_subs_now(cmds);
        }
        if bc_refetch {
            self.refetch_broadcast_schedule();
        }
        if bc_reload {
            self.reload_broadcast_stations();
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
        if rx888_rescan {
            self.rx888_devices = self.ctrl.list_rx888();
        }
        if tci_test {
            // Blocking connect (~up to 3 s); after the closure so it can take
            // `&self.ctrl`. The result is shown in the TCI section next frame.
            if let Some(cfg) = &radio_edit {
                self.tci_test_result = Some(self.ctrl.test_tci(&cfg.tci.address));
            }
        }
        if smartsdr_discover {
            // A passive listen (~2.5 s) — radios broadcast unprompted, so
            // nothing is sent and nothing on the network is disturbed.
            self.smartsdr_devices = self.ctrl.discover_smartsdr();
        }
        if smartsdr_test {
            if let Some(cfg) = &radio_edit {
                self.smartsdr_test_result = Some(match cfg.smartsdr.target() {
                    Some(addr) => self.ctrl.test_smartsdr(addr),
                    None => {
                        Err("no radio selected — press Discover, or enter an address".to_string())
                    }
                });
            }
        }
        if smartsdr_copy_report {
            // The whole point of the trace is that a user can hand it over
            // without reproducing the fault under a log filter.
            let report = self
                .ctrl
                .smartsdr_diagnostics()
                .unwrap_or_else(|| "No diagnostics available on this client.".to_string());
            ctx.copy_text(report);
        }
        if apply_iface {
            // The fields that wait for this moment, so the radio is never
            // reopened on a partly-typed offset or a half-written range.
            if let (Some(cfg), Some(hz)) = (radio_edit.as_mut(), converter_hz) {
                cfg.converter_offset_hz = hz;
            }
            if let (Some(cfg), Some((rx, tx))) = (radio_edit.as_mut(), ranges.as_ref()) {
                // Anything that doesn't parse leaves that direction as it was:
                // the box is showing the operator why in red, and applying half
                // of what they meant would be worse than applying none of it.
                if let Ok(r) = sdroxide_types::parse_freq_ranges(rx) {
                    cfg.freq_ranges_rx = r;
                }
                if let Ok(r) = sdroxide_types::parse_freq_ranges(tx) {
                    cfg.freq_ranges_tx = r;
                }
            }
            // Persist the latest edits, then rebuild the live source (no restart).
            if let Some(cfg) = &radio_edit {
                self.ctrl.set_radio_config(cfg.clone());
            }
            self.ctrl.reopen_source();
        }
        self.converter_edit_hz = converter_hz;
        self.range_edit = ranges;
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
        // Written as it is typed, like the control bindings: the server rereads
        // the file for every sign-in, so there is no APPLY step to hang this
        // off. Gated on owning the server, so a remote client cannot write its
        // own machine's config.toml from a tab it was never shown.
        if owns_server && access_edit != self.remote_access {
            self.remote_access = access_edit;
            crate::app::persist::persist_remote_access(&self.remote_access);
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

                if let Some(access) = io.access_edit.as_deref_mut() {
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);
                    remote_access_settings(ui, access);
                }
            }
            SettingsTab::Radio => {
                let Some(cfg) = io.radio_edit.as_mut() else {
                    // A remote client cannot choose the server's interface or
                    // edit its config — that file lives on the other machine.
                    // The hardware controls are a different matter: they ride
                    // ordinary commands to the running device, and both the
                    // capabilities and the current gains/antennas are already
                    // replicated here, so an operator away from the shack can
                    // still swap to the beam or wind the LNA back.
                    self.settings_device_tab(ui, cmds);
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Which radio interface the server uses is set on the machine it \
                             runs on.",
                        )
                        .weak(),
                    );
                    return;
                };
                // The single "which radio interface" selector, and the converter
                // that may sit in front of whichever one is chosen.
                let backend = cfg.backend;
                let converter = io.converter_hz.get_or_insert(cfg.converter_offset_hz);
                let ranges = io.ranges.get_or_insert_with(|| {
                    (
                        sdroxide_types::format_freq_ranges(&cfg.freq_ranges_rx),
                        sdroxide_types::format_freq_ranges(&cfg.freq_ranges_tx),
                    )
                });
                egui::Grid::new("iface-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new("Radio interface").strong());
                    enum_combo(ui, "iface", &mut cfg.backend, io.iface_opts, Backend::label);
                    ui.end_row();

                    ui.label(RichText::new("Converter").strong());
                    let named = sdroxide_types::converter_preset_name(*converter);
                    egui::ComboBox::from_id_salt("converter-preset")
                        .selected_text(named)
                        .show_ui(ui, |ui| {
                            for (name, hz) in sdroxide_types::CONVERTER_PRESETS {
                                if ui.selectable_label(named == name, name).clicked() {
                                    *converter = hz;
                                }
                            }
                            // Whatever the operator typed. Selecting it keeps
                            // that number — the box beside it is always
                            // editable, so there is no mode to unlock, and this
                            // entry is here to say so rather than to do
                            // anything.
                            let _ = ui.selectable_label(named == "Manual", "Manual");
                        })
                        .response
                        .on_hover_text(
                            "A frequency converter in front of the receiver. Pick one, or type \
                             an offset beside it.",
                        );
                    ui.end_row();

                    ui.label(RichText::new("Offset").strong());
                    ui.add(
                        egui::DragValue::new(converter)
                            .speed(1.0)
                            .range(
                                -sdroxide_types::CONVERTER_OFFSET_MAX_HZ
                                    ..=sdroxide_types::CONVERTER_OFFSET_MAX_HZ,
                            )
                            .max_decimals(0)
                            .suffix(" Hz"),
                    )
                    .on_hover_text(
                        "How far a converter moves the signal on its way to the receiver, in Hz \
                         — the same number and sign every converter's documentation and every \
                         other SDR program states. Positive for an upconverter (a Ham It Up is \
                         125000000), negative for a down-converter such as a satellite LNB. \
                         0 = no converter.\n\nDrag to trim it a hertz at a time, which is what \
                         a converter whose oscillator is slightly off wants.\n\nReceive only: a \
                         converter is not in the transmit path, so transmit is switched off \
                         while this is set.\n\nTakes effect on Apply.",
                    );
                    ui.end_row();

                    // The ranges the operator says this radio has, for a driver
                    // that publishes none (SoapySX and friends implement no
                    // frequency-range call at all, which leaves nothing to
                    // check a frequency against) or publishes the tuner chip's
                    // rather than the radio's.
                    ui.label(RichText::new("RX range").strong());
                    freq_range_edit(
                        ui,
                        "rx-range",
                        &mut ranges.0,
                        "Which frequencies this radio receives, in MHz: 144-146, 430-440. Leave \
                         empty to use whatever the device reports about itself.\n\nBand buttons \
                         outside the range are greyed out and the dial will not go there.\n\nTakes \
                         effect on Apply.",
                    );
                    ui.end_row();

                    ui.label(RichText::new("TX range").strong());
                    freq_range_edit(
                        ui,
                        "tx-range",
                        &mut ranges.1,
                        "Which frequencies this radio transmits on, in MHz: 144-146, 430-440. \
                         Leave empty to use whatever the device reports — and if it reports \
                         nothing, the driver is taken at its word and any frequency is \
                         allowed.\n\nThis is a limit you set, not a licence: transmitting outside \
                         the amateur bands is refused regardless unless you have turned that off \
                         in config.toml. Nor does it give a receive-only device a \
                         transmitter.\n\nTakes effect on Apply.",
                    );
                    ui.end_row();
                });
                // The two range boxes are the only megahertz on a tab whose
                // other frequency field is hertz, so the example says the same
                // range both ways rather than leaving anyone to count zeros.
                ui.label(
                    RichText::new(
                        "Ranges are in MHz, low-high, separated by commas: 144-146, 430-440 — \
                         that is 144000000-146000000 Hz and 430000000-440000000 Hz. The \
                         converter offset above is the field in hertz. Leave a range empty to \
                         use whatever the device reports about itself; a device that reports \
                         nothing is taken at its word.",
                    )
                    .weak(),
                );
                if *converter != 0.0 {
                    ui.label(
                        RichText::new(if backend == Backend::RtlSdr && *converter < 0.0 {
                            "Transmit is off while a converter is set. Careful on an RTL-SDR: the \
                             Blog V4 upconverts on its own below 28.8 MHz, so a negative offset \
                             that lands the hardware there shifts twice."
                        } else {
                            "Transmit is off while a converter is set."
                        })
                        .weak(),
                    );
                }
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
                    Backend::SmartSdr => settings_smartsdr_tab(
                        ui,
                        &self.smartsdr_devices,
                        io.radio_edit,
                        io.smartsdr_discover,
                        io.smartsdr_test,
                        io.smartsdr_copy_report,
                        &self.smartsdr_test_result,
                    ),
                    Backend::RtlSdr => settings_rtlsdr_tab(
                        ui,
                        &self.rtlsdr_devices,
                        io.radio_edit,
                        io.rtlsdr_rescan,
                        cmds,
                    ),
                    Backend::Rx888 => settings_rx888_tab(
                        ui,
                        &self.rx888_devices,
                        io.radio_edit,
                        io.rx888_rescan,
                        io.apply_iface,
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
                if !net_seeded_note(ui, io.net_seeded) {
                    return;
                }
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
                broadcast_stations_settings(
                    ui,
                    io.bc_reload,
                    io.bc_refetch,
                    io.bc_fetching,
                    io.bc_status,
                );
            }
            SettingsTab::Uploads => {
                if !net_seeded_note(ui, io.net_seeded) {
                    return;
                }
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
                if !net_seeded_note(ui, io.net_seeded) {
                    return;
                }
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
    /// The engine announces this with the config it annotates, which is the
    /// only source a browser client has. A native client's solar window runs a
    /// fetcher of its own, though, and what *it* last did is both fresher and
    /// what that window is actually drawing — so it wins while it has anything
    /// to say.
    #[cfg(not(target_arch = "wasm32"))]
    fn refresh_sat_sub_status(&mut self) {
        let live = self.solar.tle_sub_status();
        if !live.is_empty() {
            self.sat_sub_status = live;
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn refresh_sat_sub_status(&mut self) {}

    /// Fetch every enabled subscription now, from the settings dialog's UPDATE
    /// NOW button.
    ///
    /// The engine does the fetching: its config directory holds the listings,
    /// and on a server it is also what feeds the browser's solar view. A native
    /// client's own window shares the same disk cache when the engine is local,
    /// so it is told to re-read rather than being left on what it loaded at
    /// open time.
    fn refresh_sat_subs_now(&mut self, cmds: &mut Vec<Command>) {
        cmds.push(Command::RefreshTleSubs);
        self.sat_ui.note = "Fetching subscriptions…".to_string();
        self.sat_ui.fetching = true;
    }

    /// Summarise a refresh the operator asked for, once its status lands.
    pub(in crate::app) fn on_tle_sub_status(&mut self, status: Vec<sdroxide_types::TleSubStatus>) {
        let asked = std::mem::take(&mut self.sat_ui.fetching);
        self.sat_sub_status = status;
        if !asked {
            return;
        }
        let done = &self.sat_sub_status;
        let failed = done.iter().filter(|s| s.error.is_some()).count();
        let total: usize = done.iter().map(|s| s.count).sum();
        self.sat_ui.note = match (done.len(), failed) {
            (0, _) => "No enabled subscriptions to update.".to_string(),
            (n, 0) => format!("Updated {n} subscription(s): {total} satellites."),
            (n, f) => format!("Updated {} of {n}; {f} failed — see the rows above.", n - f),
        };
        // The window's feed shares the disk cache with a local engine, so it is
        // told to re-read rather than being left on what it loaded at open time.
        #[cfg(not(target_arch = "wasm32"))]
        self.solar.reload_tle_subs();
    }
}
