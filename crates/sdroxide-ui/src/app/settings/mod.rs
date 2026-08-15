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
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::app) mod remote;
pub(in crate::app) mod servers;
pub(in crate::app) mod tle;
pub(in crate::app) mod ui_tab;

use eframe::egui::{self, Color32, ComboBox, RichText};
use sdroxide_types::{Command, LookupProvider, NetworkConfig};

use self::controls::settings_controls_tab;
use self::general::{device_combo, region_combo, remote_access_settings};
use self::net::{
    broadcast_stations_settings, net_heading, net_row, net_secret, operator_identity_note,
    settings_freedv_tab,
};
use self::radio::{
    settings_airspyhf_tab, settings_cat_tab, settings_hackrf_tab, settings_hpsdr_tab,
    settings_icomnet_tab,
    settings_pluto_tab, settings_rtlsdr_tab, settings_rtltcp_tab, settings_rx888_tab,
    settings_sdrplay_tab, settings_smartsdr_tab, settings_soapy_devices, settings_tci_tab,
};
#[cfg(not(target_arch = "wasm32"))]
use self::remote::settings_remote_tab;
use self::servers::{
    settings_rigctld_tab, settings_rotator_tab, settings_tci_server_tab, settings_wsjtx_tab,
};
use self::tle::settings_tle_tab;
use self::ui_tab::settings_ui_tab;
use crate::app::SdroxideApp;
use crate::app::persist::{persist_speech_settings, persist_ui_settings};
use crate::theme::ThemedScroll as _;

/// Settings dialog tabs: General (station identity + audio devices), the radio
/// interface and its settings, display/UI preferences and spoken
/// announcements, control inputs
/// (keyboard/mouse bindings), the network cockpit (spot feeds + uploads), the
/// built-in TCI server, and — the other direction — the sdroxide server this
/// screen connects *out* to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum SettingsTab {
    General,
    Radio,
    Ui,
    Controls,
    Spots,
    FreeDv,
    Uploads,
    Winlink,
    Servers,
    /// Dial another station. Native only: a browser client is already attached
    /// to the server that served it and has nowhere to put a second one.
    #[cfg(not(target_arch = "wasm32"))]
    Remote,
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
    /// Whether the radio is on *this* machine's buses and network.
    ///
    /// The interface's settings travel to a remote client — they describe the
    /// device, and the engine writes them wherever it lives. What cannot travel
    /// is everything that answers a question about a machine: which dongles are
    /// on the USB bus, which serial ports exist, what a Discover broadcast
    /// finds, whether an address answers. Those are enumerated locally, so a
    /// remote client asking them would get its *own* answers — a laptop's sound
    /// cards offered as the shack rig's, a dongle list from the wrong bus. The
    /// controls that do it are disabled rather than left to lie, and choosing
    /// the interface itself goes with them: the operator is not there to plug
    /// the new one in.
    local_devices: bool,
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
    /// Re-enumerate the USB bus for Airspy HF+ receivers. Opens nothing.
    airspyhf_rescan: &'a mut bool,
    /// Copy the last Airspy HF+ session's trace to the clipboard.
    airspyhf_copy_report: &'a mut bool,
    hackrf_rescan: &'a mut bool,
    hackrf_copy_report: &'a mut bool,
    /// Ask the SDRplay API service for its device list. Brief and
    /// non-invasive, so it cannot disturb a running stream.
    sdrplay_rescan: &'a mut bool,
    /// Re-run the SoapySDR enumeration. Opens nothing, but loads every
    /// installed module and asks each to scan, so it is not instant.
    soapy_rescan: &'a mut bool,
    tci_test: &'a mut bool,
    /// Connect to the Icom, report what it is, and disconnect (blocking).
    icomnet_test: &'a mut bool,
    /// Copy the last Icom LAN session's trace to the clipboard.
    icomnet_copy_report: &'a mut bool,
    /// Listen for FlexRadio discovery broadcasts (a couple of seconds, blocking).
    smartsdr_discover: &'a mut bool,
    smartsdr_test: &'a mut bool,
    /// Copy the last SmartSDR session's protocol trace to the clipboard.
    smartsdr_copy_report: &'a mut bool,
    /// Ask mDNS for IIO devices, and try the USB gadget's address (~1.5 s,
    /// blocking).
    pluto_discover: &'a mut bool,
    pluto_test: &'a mut bool,
    /// Copy the last PlutoSDR session's protocol trace to the clipboard.
    pluto_copy_report: &'a mut bool,
    apply_iface: &'a mut bool,
    ui_edit: &'a mut sdroxide_types::UiSettings,
    /// Who may connect to this machine's server, or `None` where this client
    /// is in no position to say — a remote one, and every browser one. Those
    /// credentials are `config.toml` on the machine the radio is attached to,
    /// and this is not it.
    access_edit: Option<&'a mut sdroxide_types::RemoteAccess>,
    /// The other direction — which station this screen dials from the Remote
    /// tab, and whether CONNECT was pressed. Editable everywhere `access_edit`
    /// is not: it is this machine's own setting, so a remote client is exactly
    /// as entitled to it as the shack machine.
    #[cfg(not(target_arch = "wasm32"))]
    remote_edit: &'a mut sdroxide_types::RemoteServer,
    #[cfg(not(target_arch = "wasm32"))]
    remote_connect: &'a mut bool,
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
    rbn_cmds: &'a mut String,
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
    /// The rotctld *client* — the satellite lock steering a motorized antenna.
    rot_edit: &'a mut sdroxide_types::RotatorConfig,
    rot_apply: &'a mut bool,
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
    /// Radio-management actions from the roster strip at the top of the Radio
    /// page — switch/add/close/mute/rename — collected here like every other
    /// edit and handed to the multi-radio shell after the frame.
    #[cfg(not(target_arch = "wasm32"))]
    radio_tabs: &'a mut Vec<crate::app::RadioTabRequest>,
    /// The rename field's buffer: (radio id, text as typed). UI-owned until
    /// the edit commits, so the roster's per-frame republish cannot fight the
    /// keyboard — see `SdroxideApp::radio_name_edit`.
    #[cfg(not(target_arch = "wasm32"))]
    radio_name_edit: &'a mut Option<(u32, String)>,
    /// Spoken announcements, edited in place and written back after the
    /// window closure like every other buffer here.
    speech_edit: &'a mut sdroxide_types::SpeechSettings,
    /// Voices found on disk, listed once when the dialog opened.
    speech_voices: &'a [String],
    speech_status: &'a crate::app::speech::SpeechStatus,
    /// The TEST button was pressed; answered after the closure, where the
    /// announcer is reachable.
    speech_test: &'a mut bool,
    /// The station's IARU region. Applied and sent the moment it changes —
    /// there is no APPLY step on the General tab, and the whole point of it is
    /// that the band plan follows immediately.
    region_edit: &'a mut sdroxide_types::Region,
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
            // Reading a directory listing is cheap, but not per-frame cheap.
            self.speech_voices = crate::app::speech::SpeechRuntime::voices();
            // Only when SoapySDR is the interface being configured: enumeration
            // loads every installed module and asks each to scan its bus, which
            // is not something to do to an operator who is here to change their
            // waterfall colours. Rescan re-runs it for anyone switching to it.
            if self.radio_cfg.as_ref().is_some_and(|c| c.backend == sdroxide_types::Backend::Soapy)
            {
                self.soapy_devices = Some(self.ctrl.list_soapy());
            }
            // The RSP tab draws itself from the model in this list — which
            // antenna ports exist, whether there is an HDR path, how far the
            // LNA goes. Asking the service is one round trip and opens no
            // device, so it happens on open: without it the tab falls back to
            // the RSP1B feature set, and an RSPdx owner is left with no
            // antenna selector until they think to press Rescan.
            if self
                .radio_cfg
                .as_ref()
                .is_some_and(|c| c.backend == sdroxide_types::Backend::SdrPlay)
            {
                self.sdrplay_devices = self.ctrl.list_sdrplay();
            }
            // Cheap and opens nothing, same as the RTL-SDR list — and without
            // it a HackRF owner arriving on this tab sees an empty device combo
            // until they think to press Rescan.
            if self
                .radio_cfg
                .as_ref()
                .is_some_and(|c| c.backend == sdroxide_types::Backend::HackRf)
            {
                self.hackrf_devices = self.ctrl.list_hackrf();
            }
            self.audio_devices_queried = true;
        } else if self.radio_cfg.is_none() {
            // Still waiting for the interface configuration. On a remote client
            // it arrives with the connect-time replay, so this is normally
            // answered before anyone opens the dialog — but a client that
            // opened it during a reconnect would otherwise sit on "only
            // available in the native app" until the dialog was closed and
            // reopened. One `Option` clone a frame, and only while it is empty.
            self.radio_cfg = self.ctrl.radio_config();
        }
        // Edits collected here and applied after the window closure, which
        // borrows `&self` and so can't touch `&mut self.ctrl`.
        let mut audio_pick: Option<(bool, Option<String>)> = None;
        let mut speech_edit = self.speech.settings().clone();
        let speech_status = self.speech.status();
        let mut speech_test = false;
        let mut hpsdr_discover = false;
        let mut rtlsdr_rescan = false;
        let mut rx888_rescan = false;
        let mut airspyhf_rescan = false;
        let mut airspyhf_copy_report = false;
        let mut hackrf_rescan = false;
        let mut hackrf_copy_report = false;
        let mut sdrplay_rescan = false;
        let mut soapy_rescan = false;
        let mut tci_test = false;
        let mut icomnet_test = false;
        let mut icomnet_copy_report = false;
        let mut smartsdr_discover = false;
        let mut smartsdr_test = false;
        let mut smartsdr_copy_report = false;
        let mut pluto_discover = false;
        let mut pluto_test = false;
        let mut pluto_copy_report = false;
        let mut apply_iface = false;
        let mut radio_edit = self.radio_cfg.clone();
        let mut converter_hz = self.converter_edit_hz;
        let mut ranges = self.range_edit.clone();
        let mut ui_edit = self.ui_settings;
        // Only where the engine is in this process: see `SettingsIo`.
        let owns_server = !self.ctrl.engine_is_remote();
        let mut access_edit = self.remote_access.clone();
        #[cfg(not(target_arch = "wasm32"))]
        let mut remote_edit = self.remote_server.clone();
        #[cfg(not(target_arch = "wasm32"))]
        let mut remote_connect = false;
        let mut digi_edit = self.digi_cfg_edit.clone();
        let digi_seeded = self.digi_cfg_seeded;
        let mut region_edit = self.region_edit;
        let mut net_edit = self.net_cfg_edit.clone();
        let mut net_cmds = self.net_cluster_cmds.clone();
        let mut rbn_cmds = self.net_rbn_cmds.clone();
        let mut net_apply = false;
        let mut net_sync = false;
        let mut tci_srv_edit = self.tci_srv_edit.clone();
        let mut tci_srv_apply = false;
        let mut rigctld_edit = self.rigctld_edit.clone();
        let mut rigctld_apply = false;
        let mut wsjtx_edit = self.wsjtx_edit.clone();
        let mut wsjtx_apply = false;
        let mut rot_edit = self.rot_cfg_edit.clone();
        let mut rot_apply = false;
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
        // Pure-Rust UDP, no system library — in every build variant, as TCI is.
        iface_opts.push(sdroxide_types::Backend::IcomNet);
        iface_opts.push(sdroxide_types::Backend::SmartSdr);
        // Pure-Rust IIOD over TCP — no libiio, no libusb — so like the two USB
        // backends below it is in every build variant.
        iface_opts.push(sdroxide_types::Backend::Pluto);
        // Ungated, unlike SoapySDR: the RTL-SDR driver is pure Rust and needs
        // no system library, so it is compiled into every build variant.
        iface_opts.push(sdroxide_types::Backend::RtlSdr);
        // The same driver over a socket instead of the USB bus — pure Rust and
        // std::net, so it is in every build variant too. Listed next to the USB
        // entry because the choice between them is only where the dongle is.
        iface_opts.push(sdroxide_types::Backend::RtlTcp);
        // Same reasoning as the RTL-SDR: pure Rust over `nusb`, no system
        // library, so it is in every build variant.
        iface_opts.push(sdroxide_types::Backend::Rx888);
        // Same reasoning again: pure Rust over `nusb`, no libairspyhf and no
        // system library, so it is in every build variant.
        iface_opts.push(sdroxide_types::Backend::AirspyHf);
        // And again — pure Rust over `nusb`, no libhackrf. The only one of
        // these USB backends that transmits, which is why it is the only one
        // whose settings tab has a switch to arm before it will.
        iface_opts.push(sdroxide_types::Backend::HackRf);
        // Also in every build variant, but for a different reason: nothing is
        // linked at build time — the vendor's sdrplay_api library is found
        // with dlopen at runtime, and opening explains what to install when
        // it is absent.
        iface_opts.push(sdroxide_types::Backend::SdrPlay);

        let mut tab = self.settings_tab;
        let mut open = self.show_settings;
        // The 3D window owns the live copy of its own settings — `view.solar3d`
        // is only the snapshot persisted from it — so this is read out of the
        // window here and handed back to it below, the way `ui_edit` is.
        #[cfg(not(target_arch = "wasm32"))]
        let mut solar_cloud_march = self.solar.cloud_march();
        #[cfg(not(target_arch = "wasm32"))]
        let mut radio_tab_reqs: Vec<crate::app::RadioTabRequest> = Vec::new();
        #[cfg(not(target_arch = "wasm32"))]
        let mut radio_name_edit = self.radio_name_edit.take();
        // The window does its own scrolling, so its bar can only be themed
        // through the context style — lend the palette for the length of the
        // call and hand the body back the normal one.
        let bars = crate::theme::ScrollPalette::push(ctx);
        // Sized like the other overlays instead of by its contents. An
        // auto-sized egui window lays the body out inside `Resize`'s remembered
        // size, which grows to the widest tab ever shown and never shrinks
        // again — so the width drifted with wherever the operator had been
        // while the height stayed at egui's 420 pt default no matter how tall
        // the display was.
        let want =
            egui::vec2(crate::layout::window_w(ctx, 900.0), crate::layout::window_h(ctx, 760.0));
        let resp = egui::Window::new("Settings")
            // Pinned id, versioned: egui persists the remembered size and
            // position under it, and the suffix drops the stale (often very
            // wide, always 420 pt tall) geometry left by the builds before this
            // one. Position is centred on first use because it goes with it.
            .id(crate::layout::salted_id(ctx, "settings-window-v2"))
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(true)
            .default_pos(ctx.content_rect().center() - want * 0.5)
            .default_size(want)
            .min_width(crate::layout::window_w(ctx, 380.0))
            .min_height(crate::layout::window_h(ctx, 300.0))
            .vscroll(true)
            .show(ctx, |ui| {
                crate::chrome::window_body_bg(ui);
                bars.restore(ui);
                // The tabs that are wider than the window get a scrollbar
                // instead of widening it: the Controls tables and the TLE
                // element editors are laid out at fixed widths, and egui grows a
                // window to whatever its content asked for and never gives the
                // width back. Text still wraps at the window's width — pinning
                // the body's max width is what keeps `available_width` finite
                // inside a horizontally scrollable region.
                let body_w = ui.available_width();
                egui::ScrollArea::horizontal().show_themed(ui, |ui| {
                    ui.set_max_width(body_w);
                    self.settings_body(
                        ui,
                        cmds,
                        &mut SettingsIo {
                            iface_opts: &iface_opts,
                            radio_edit: &mut radio_edit,
                            local_devices: owns_server,
                            converter_hz: &mut converter_hz,
                            ranges: &mut ranges,
                            audio_pick: &mut audio_pick,
                            hpsdr_discover: &mut hpsdr_discover,
                            rtlsdr_rescan: &mut rtlsdr_rescan,
                            rx888_rescan: &mut rx888_rescan,
                            airspyhf_rescan: &mut airspyhf_rescan,
                            airspyhf_copy_report: &mut airspyhf_copy_report,
                            hackrf_rescan: &mut hackrf_rescan,
                            hackrf_copy_report: &mut hackrf_copy_report,
                            sdrplay_rescan: &mut sdrplay_rescan,
                            soapy_rescan: &mut soapy_rescan,
                            tci_test: &mut tci_test,
                            smartsdr_discover: &mut smartsdr_discover,
                            smartsdr_test: &mut smartsdr_test,
                            icomnet_test: &mut icomnet_test,
                            icomnet_copy_report: &mut icomnet_copy_report,
                            smartsdr_copy_report: &mut smartsdr_copy_report,
                            pluto_discover: &mut pluto_discover,
                            pluto_test: &mut pluto_test,
                            pluto_copy_report: &mut pluto_copy_report,
                            apply_iface: &mut apply_iface,
                            ui_edit: &mut ui_edit,
                            access_edit: owns_server.then_some(&mut access_edit),
                            #[cfg(not(target_arch = "wasm32"))]
                            remote_edit: &mut remote_edit,
                            #[cfg(not(target_arch = "wasm32"))]
                            remote_connect: &mut remote_connect,
                            digi_edit: &mut digi_edit,
                            digi_seeded,
                            net_edit: &mut net_edit,
                            net_seeded: self.net_cfg_seeded,
                            net_cmds: &mut net_cmds,
                            rbn_cmds: &mut rbn_cmds,
                            net_apply: &mut net_apply,
                            bc_reload: &mut bc_reload,
                            bc_refetch: &mut bc_refetch,
                            bc_fetching: self.broadcast_fetch.is_some(),
                            bc_status: self.broadcast_fetch_status.as_ref(),
                            speech_edit: &mut speech_edit,
                            speech_voices: &self.speech_voices,
                            speech_status: &speech_status,
                            speech_test: &mut speech_test,
                            net_sync: &mut net_sync,
                            tci_srv_edit: &mut tci_srv_edit,
                            tci_srv_apply: &mut tci_srv_apply,
                            rigctld_edit: &mut rigctld_edit,
                            rigctld_apply: &mut rigctld_apply,
                            wsjtx_edit: &mut wsjtx_edit,
                            wsjtx_apply: &mut wsjtx_apply,
                            rot_edit: &mut rot_edit,
                            rot_apply: &mut rot_apply,
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
                            #[cfg(not(target_arch = "wasm32"))]
                            radio_tabs: &mut radio_tab_reqs,
                            #[cfg(not(target_arch = "wasm32"))]
                            radio_name_edit: &mut radio_name_edit,
                            region_edit: &mut region_edit,
                            tab: &mut tab,
                        },
                    );
                });
            });
        bars.pop(ctx);
        if let Some(r) = &resp {
            crate::chrome::paint_window_border(ctx, &r.response);
        }
        self.show_settings = open;
        self.settings_tab = tab;
        // The multi-radio shell drains these after the frame.
        #[cfg(not(target_arch = "wasm32"))]
        self.radio_tab_requests.append(&mut radio_tab_reqs);
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.radio_name_edit = radio_name_edit;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if solar_cloud_march != self.solar.cloud_march() {
            self.solar.set_cloud_march(solar_cloud_march);
        }
        // Persist net-config edits (kept across frames) and apply on demand.
        if self.net_cfg_seeded {
            self.net_cfg_edit = net_edit;
            self.net_cluster_cmds = net_cmds;
            self.net_rbn_cmds = rbn_cmds;
        }
        if net_apply && self.net_cfg_seeded {
            let split = |s: &str| -> Vec<String> {
                s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
            };
            self.net_cfg_edit.cluster.commands = split(&self.net_cluster_cmds);
            self.net_cfg_edit.rbn.commands = split(&self.net_rbn_cmds);
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
        self.rot_cfg_edit = rot_edit;
        if rot_apply {
            // The engine persists rotator.json when it (re)starts the client.
            cmds.push(Command::SetRotatorConfig(self.rot_cfg_edit.clone()));
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
        if sdrplay_rescan {
            self.sdrplay_devices = self.ctrl.list_sdrplay();
        }
        if soapy_rescan {
            // Loads every installed SoapySDR module and asks each to scan, so
            // it can take a moment; on demand only, never per frame.
            self.soapy_devices = Some(self.ctrl.list_soapy());
        }
        if rx888_rescan {
            self.rx888_devices = self.ctrl.list_rx888();
        }
        if airspyhf_rescan {
            self.airspyhf_devices = self.ctrl.list_airspyhf();
        }
        if airspyhf_copy_report {
            // This backend has not been verified against hardware, so the trace
            // is how a fault gets reported without asking anybody to reproduce
            // it under a log filter.
            let report = self
                .ctrl
                .airspyhf_diagnostics()
                .unwrap_or_else(|| "No diagnostics available on this client.".to_string());
            ctx.copy_text(report);
        }
        if hackrf_rescan {
            self.hackrf_devices = self.ctrl.list_hackrf();
        }
        if hackrf_copy_report {
            // Worth more on this backend than on the receive-only ones: a
            // transmit fault is about the *order* control transfers went out
            // in around a key-down, which nobody can reconstruct from a
            // spectrum.
            let report = self
                .ctrl
                .hackrf_diagnostics()
                .unwrap_or_else(|| "No diagnostics available on this client.".to_string());
            ctx.copy_text(report);
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
        // Blocking connect (~up to 5 s); after the closure so it can take
        // `&self.ctrl`. Tests what is *typed in the dialog*, not what was last
        // applied — a test that ignored an edited address would be answering a
        // question nobody asked.
        if let (true, Some(cfg)) = (icomnet_test, &radio_edit) {
            self.icomnet_test_result = Some(if cfg.icomnet.address.trim().is_empty() {
                Err("enter the radio's address first".to_string())
            } else {
                self.ctrl.test_icomnet(&cfg.icomnet)
            });
        }
        if icomnet_copy_report {
            let report = self
                .ctrl
                .icomnet_diagnostics()
                .unwrap_or_else(|| "No diagnostics available on this client.".to_string());
            ctx.copy_text(report);
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
        if pluto_discover {
            // An mDNS query plus a direct try of the USB gadget's address
            // (~1.5 s); after the closure so it can take `&self.ctrl`.
            self.pluto_devices = self.ctrl.discover_pluto();
        }
        if pluto_test {
            if let Some(cfg) = &radio_edit {
                self.pluto_test_result = Some(self.ctrl.test_pluto(&cfg.pluto.target()));
            }
        }
        if pluto_copy_report {
            let report = self
                .ctrl
                .pluto_diagnostics()
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
        if &speech_edit != self.speech.settings() {
            // Live too: rate and volume reach the running worker, and only a
            // change of voice or output device restarts it.
            self.speech.set_settings(speech_edit.clone());
            persist_speech_settings(&speech_edit);
        }
        if speech_test {
            self.speech.announcer.say_sample(ctx.input(|i| i.time));
        }
        // Written as it is typed, like the control bindings: the server rereads
        // the file for every sign-in, so there is no APPLY step to hang this
        // off. Gated on owning the server, so a remote client cannot write its
        // own machine's config.toml from a tab it was never shown.
        if owns_server && access_edit != self.remote_access {
            self.remote_access = access_edit;
            crate::app::persist::persist_remote_access(&self.remote_access);
        }
        // The address to dial, written as it is typed for the same reason: a
        // pressed CONNECT is a poor moment to find out the address was never
        // saved, and there is no APPLY step here to hang it off. Not gated on
        // `owns_server` — this is the operator's own machine's setting, so a
        // remote client is as entitled to it as the shack machine is.
        #[cfg(not(target_arch = "wasm32"))]
        {
            if remote_edit != self.remote_server {
                self.remote_server = remote_edit;
                crate::app::persist::persist_remote_server(&self.remote_server);
            }
            if remote_connect && !self.remote_server.host.trim().is_empty() {
                // Dialled by the shell after the frame: it owns the tab set,
                // and this connection needs a tab to live in.
                self.remote_status = Some(Ok(format!("Dialling {}…", self.remote_server.url())));
                self.radio_tab_requests.push(crate::app::RadioTabRequest::Connect {
                    url: self.remote_server.url(),
                    name: self.remote_server.label(),
                });
            }
        }
        // Callsign/grid from the General tab — same store as the FT8/SSTV setup
        // dialog. Only apply once seeded so we can't overwrite the engine's saved
        // config with defaults.
        if digi_seeded && digi_edit != self.digi_cfg_edit {
            self.digi_cfg_edit = digi_edit;
            cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
        }
        // The station's region. Applied here as well as sent, so the band
        // buttons and the waterfall's band strip redraw on this frame instead
        // of on whichever one the engine's echo lands in — and so the change is
        // visible even before a remote station has confirmed it. The engine
        // persists it and announces the authoritative value back.
        if region_edit != self.region_edit {
            self.region_edit = region_edit;
            sdroxide_types::set_region(region_edit);
            cmds.push(Command::SetRegion(region_edit));
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

        // Built rather than written out, because one of the tabs only exists on
        // native (see [`SettingsTab::Remote`]).
        let mut tabs = vec![
            (SettingsTab::General, "General"),
            (SettingsTab::Radio, "Radio"),
            (SettingsTab::Ui, "UI"),
            (SettingsTab::Controls, "Controls"),
            (SettingsTab::Spots, "Spots"),
            (SettingsTab::FreeDv, "FreeDV"),
            (SettingsTab::Uploads, "Uploads"),
            (SettingsTab::Winlink, "Winlink"),
            (SettingsTab::Servers, "Servers"),
        ];
        // Next to Servers: the two are the same subject from opposite ends —
        // what this station offers others, and where this screen goes.
        #[cfg(not(target_arch = "wasm32"))]
        tabs.push((SettingsTab::Remote, "Remote"));
        tabs.push((SettingsTab::Tle, "TLE"));
        // Wrapped: the tab strip no longer fits the window's width on one line.
        ui.horizontal_wrapped(|ui| {
            for (t, label) in tabs {
                if crate::chrome::chip(ui, *io.tab == t, label).clicked() {
                    *io.tab = t;
                }
            }
        });
        ui.separator();

        let backend = io.radio_edit.as_ref().map(|c| c.backend);

        match io.tab {
            SettingsTab::General => {
                // Which build this is, taken from the crate metadata at compile
                // time — so a bug report can name the version without the
                // operator having to find the binary.
                ui.label(
                    RichText::new(format!("SDRoxide {}", env!("CARGO_PKG_VERSION")))
                        .size(15.0)
                        .strong(),
                );
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                ui.label(RichText::new("Station").size(14.0).strong().color(crate::theme::CYAN()));
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
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Your callsign and grid, shared across FT8/FT4/FT2, SSTV image headers, and \
                         the logbook. Also editable from the FT8 / SSTV setup dialog.",
                    )
                    .weak(),
                );

                // Its own grid, outside the enabled-ui above: the region comes
                // from `config.toml` by way of the station announcement, not
                // from the digi config, so it is neither waiting on the same
                // seeding nor sent with the same command.
                ui.add_space(8.0);
                egui::Grid::new("general-region-grid").num_columns(2).spacing([12.0, 8.0]).show(
                    ui,
                    |ui| {
                        ui.label("IARU region");
                        region_combo(ui, io.region_edit);
                        ui.end_row();
                    },
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "Where the station is. Sets every band plan: the band edges, the CW / \
                         data / phone sub-segments on the waterfall strip, where the skimmers \
                         listen, the calling frequencies offered, and what counts as out of band \
                         for transmit. In Region {} that makes 70 cm {}. Takes effect at once, \
                         and applies to every radio at this station.",
                        io.region_edit.number(),
                        match io.region_edit {
                            sdroxide_types::Region::R1 =>
                                "430–440 MHz — so 446 is out of band, and 40 m stops at 7.200",
                            sdroxide_types::Region::R2 => "420–450 MHz, and 40 m runs to 7.300",
                            sdroxide_types::Region::R3 => "430–450 MHz, and 80 m stops at 3.900",
                        }
                    ))
                    .weak(),
                );
                ui.add_space(6.0);
                self.settings_band_plan_file(ui, cmds);

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                self.settings_user_audio(ui, io.audio_pick);
                // The radio's own sound card is only used by the CAT / Audio
                // interface; every other backend carries its audio in-band.
                //
                // Local only, and not because the setting is: `audio_devices`
                // is *this* screen's sound cards, and the rig is plugged into
                // the engine's. Offering a laptop's built-in microphone as the
                // shack transceiver's transmit path is worse than offering
                // nothing at all.
                if backend == Some(Backend::Cat) && !io.local_devices {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Radio audio (sound card)").strong());
                    ui.label(
                        RichText::new(
                            "Set on the machine the radio is plugged into — the sound cards \
                             listed here are this screen's, not its.",
                        )
                        .weak(),
                    );
                }
                if backend == Some(Backend::Cat) && io.local_devices {
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
                            ui.add(
                                egui::Label::new(
                                    RichText::new("Reconnects the radio without restarting.")
                                        .weak(),
                                )
                                .wrap(),
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
                // The station's radios, managed from here: this page is where
                // radios are configured, and — with the main window's tab area
                // hidden until there is more than one — where the second radio
                // is added in the first place.
                #[cfg(not(target_arch = "wasm32"))]
                self.settings_radio_roster(ui, io.radio_tabs, io.radio_name_edit);
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
                    // Which interface, only where the hardware is. Everything
                    // below this row describes the device and travels; *this*
                    // row would reopen the far end onto something else, and
                    // nobody is standing there to plug it in if it isn't there.
                    if io.local_devices {
                        enum_combo(ui, "iface", &mut cfg.backend, io.iface_opts, Backend::label);
                    } else {
                        ui.label(backend.label()).on_hover_text(
                            "Which interface the server uses is set on the machine it runs \
                             on. Its settings are below and can be changed from here.",
                        );
                    }
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
                        settings_soapy_devices(
                            ui,
                            self.soapy_devices.as_deref(),
                            io.soapy_rescan,
                            io.local_devices,
                        );
                    }
                    Backend::Hpsdr => settings_hpsdr_tab(
                        ui,
                        &self.hpsdr_devices,
                        io.radio_edit,
                        io.hpsdr_discover,
                        io.local_devices,
                        cmds,
                    ),
                    Backend::Cat => {
                        settings_cat_tab(ui, &self.serial_ports, io.radio_edit, io.local_devices)
                    }
                    Backend::Tci => settings_tci_tab(
                        ui,
                        io.radio_edit,
                        io.tci_test,
                        &self.tci_test_result,
                        io.local_devices,
                    ),
                    Backend::IcomNet => settings_icomnet_tab(
                        ui,
                        io.radio_edit,
                        io.icomnet_test,
                        io.icomnet_copy_report,
                        &self.icomnet_test_result,
                        io.local_devices,
                    ),
                    Backend::SmartSdr => settings_smartsdr_tab(
                        ui,
                        &self.smartsdr_devices,
                        io.radio_edit,
                        io.smartsdr_discover,
                        io.smartsdr_test,
                        io.smartsdr_copy_report,
                        &self.smartsdr_test_result,
                        io.local_devices,
                    ),
                    Backend::Pluto => settings_pluto_tab(
                        ui,
                        &self.pluto_devices,
                        io.radio_edit,
                        io.pluto_discover,
                        io.pluto_test,
                        io.pluto_copy_report,
                        &self.pluto_test_result,
                        io.local_devices,
                        cmds,
                    ),
                    Backend::RtlSdr => settings_rtlsdr_tab(
                        ui,
                        &self.rtlsdr_devices,
                        io.radio_edit,
                        io.rtlsdr_rescan,
                        io.local_devices,
                        cmds,
                    ),
                    // No device list: the dongle is the server's, and the
                    // protocol has no enumeration — there is nothing to rescan
                    // and nothing to pick from.
                    Backend::RtlTcp => settings_rtltcp_tab(ui, io.radio_edit, cmds),
                    Backend::Rx888 => settings_rx888_tab(
                        ui,
                        &self.rx888_devices,
                        io.radio_edit,
                        io.rx888_rescan,
                        io.apply_iface,
                        io.local_devices,
                        cmds,
                    ),
                    Backend::AirspyHf => settings_airspyhf_tab(
                        ui,
                        &self.airspyhf_devices,
                        self.caps.as_ref(),
                        io.radio_edit,
                        io.airspyhf_rescan,
                        io.airspyhf_copy_report,
                        io.apply_iface,
                        io.local_devices,
                        cmds,
                    ),
                    Backend::HackRf => settings_hackrf_tab(
                        ui,
                        &self.hackrf_devices,
                        io.radio_edit,
                        io.hackrf_rescan,
                        io.hackrf_copy_report,
                        io.apply_iface,
                        io.local_devices,
                        cmds,
                    ),
                    Backend::SdrPlay => settings_sdrplay_tab(
                        ui,
                        &self.sdrplay_devices,
                        self.caps.as_ref(),
                        io.radio_edit,
                        io.sdrplay_rescan,
                        io.apply_iface,
                        io.local_devices,
                        cmds,
                    ),
                    // Legacy configs may still carry the removed auto-detect
                    // backend; prompt the user to pick a concrete interface.
                    // Which nobody can do from a remote client — the row above
                    // is a label there — so say who has to.
                    Backend::Auto => {
                        ui.label(
                            RichText::new(if io.local_devices {
                                "Pick a radio interface above (this configuration used the \
                                 removed auto-detect mode)."
                            } else {
                                "This configuration used the removed auto-detect mode. An \
                                 interface has to be chosen on the machine the radio is \
                                 attached to."
                            })
                            .weak(),
                        );
                    }
                    // A freshly added radio tab: nothing opens until an
                    // interface is chosen, so choosing one is the whole page.
                    Backend::None => {
                        ui.label(
                            RichText::new(if io.local_devices {
                                "This radio has no interface yet — pick one above and press \
                                 Apply / reconnect."
                            } else {
                                "This radio has no interface yet. One has to be chosen on the \
                                 machine the radio is attached to."
                            })
                            .weak(),
                        );
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    // Remotely this button reopens the radio the server already
                    // has, on the settings just edited — it cannot switch to a
                    // different interface, because the row that would choose
                    // one is a label there.
                    if ui
                        .button("Apply / reconnect")
                        .on_hover_text(if io.local_devices {
                            "Switch to this interface now — no restart needed"
                        } else {
                            "Reopen the server's radio on these settings — no restart needed"
                        })
                        .clicked()
                    {
                        *io.apply_iface = true;
                    }
                    // Labels in a horizontal row default to Extend; wrap so a
                    // narrow window doesn't push this under the scrollbar.
                    ui.add(
                        egui::Label::new(
                            RichText::new(if io.local_devices {
                                "Switches the live radio without restarting."
                            } else {
                                "Everything above is the server's own configuration, and \
                                 changes are saved there. Most settings apply as you change \
                                 them; the ones fixed when the device is opened — the sample \
                                 rate, an address — need this button."
                            })
                            .weak(),
                        )
                        .wrap(),
                    );
                });
            }
            SettingsTab::Ui => {
                settings_ui_tab(ui, io.ui_edit, io.solar_cloud_march.as_deref_mut());
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                ui_tab::speech_settings(
                    ui,
                    io.speech_edit,
                    io.speech_voices,
                    self.audio_devices.as_ref().map(|d| d.outputs.as_slice()).unwrap_or(&[]),
                    io.speech_status,
                    io.speech_test,
                );
            }
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

                net_heading(ui, "Reverse Beacon Network");
                ui.checkbox(&mut io.net_edit.rbn.enabled, "Enabled").on_hover_text(
                    "Read the world's CW/RTTY skimmers and feed the propagation map with \
                     them. This is what makes the map show bands this radio is not \
                     listening to. On by default: it puts nothing on the air, needs no \
                     account, and uses the callsign from the General tab. RBN spots do not \
                     appear in the spot list — they are measurements, not invitations.",
                );
                net_row(ui, "Host", &mut io.net_edit.rbn.host, 220.0);
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 22.0], egui::Label::new("Port"));
                    ui.add(egui::DragValue::new(&mut io.net_edit.rbn.port).range(1..=65535))
                        .on_hover_text("7000 is the CW/RTTY feed, 7001 the FT8/FT4 one");
                });
                net_row(ui, "Login call", &mut io.net_edit.rbn.login, 140.0);
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 22.0], egui::Label::new("Commands"));
                    ui.add(
                        egui::TextEdit::multiline(io.rbn_cmds)
                            .desired_rows(2)
                            .hint_text("one per line, e.g. set/filter cont=eu")
                            .desired_width(220.0),
                    )
                    .on_hover_text(
                        "Sent after login. The place to narrow the feed — without a filter \
                         this is every skimmer on Earth.",
                    );
                });
                ui.label(
                    egui::RichText::new(
                        "RBN paths are placed from country centres, not locators — accurate \
                         for a small country, out by a long way for a large one. They are a \
                         separate layer on the propagation map and can be switched off there.",
                    )
                    .size(9.5)
                    .italics()
                    .color(egui::Color32::from_gray(110)),
                );

                net_heading(ui, "POTA / SOTA / PSK Reporter");
                ui.checkbox(&mut io.net_edit.pota.enabled, "POTA activator spots");
                ui.checkbox(&mut io.net_edit.sota.enabled, "SOTA spots");
                ui.checkbox(&mut io.net_edit.psk.enabled, "PSK Reporter (current band)");
                ui.checkbox(&mut io.net_edit.psk.report, "Upload my FT8/FT4/FT2 decodes")
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

                net_heading(ui, "WSPRnet");
                ui.checkbox(&mut io.net_edit.wspr.upload, "Upload my WSPR decodes").on_hover_text(
                    "Send every WSPR reception to wsprnet.org. On by default: it puts \
                         nothing on the air, and reporting what you hear is what makes a WSPR \
                         receiver part of the network rather than a private curiosity. A slot \
                         that decoded nothing is reported too, which is how the network tells a \
                         shut band from a receiver that was switched off.",
                );
                ui.checkbox(&mut io.net_edit.wspr.download_heard_us, "Download who heard me")
                    .on_hover_text(
                        "Ask wsprnet.org which stations decoded this one. WSPR has no \
                         acknowledgement of any kind, so this is the only way a transmitting \
                         beacon learns anything about its own reach.",
                    );
                if io.net_edit.wspr.download_heard_us {
                    ui.horizontal(|ui| {
                        ui.add_sized([96.0, 22.0], egui::Label::new("Ask every"));
                        ui.add(
                            egui::DragValue::new(&mut io.net_edit.wspr.download_interval_secs)
                                .range(60..=3600)
                                .suffix(" s"),
                        );
                        ui.add_sized([56.0, 22.0], egui::Label::new("looking back"));
                        ui.add(
                            egui::DragValue::new(&mut io.net_edit.wspr.download_window_min)
                                .range(2..=180)
                                .suffix(" min"),
                        );
                    });
                }

                ui.add_space(8.0);
                if crate::chrome::chip_accent(
                    ui,
                    false,
                    RichText::new(" APPLY ").strong(),
                    crate::theme::GREEN(),
                    crate::theme::INK_ON_CYAN(),
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
            SettingsTab::Winlink => {
                if !net_seeded_note(ui, io.net_seeded) {
                    return;
                }
                let wl = &mut io.net_edit.winlink;
                net_heading(ui, "Winlink account");
                net_row(ui, "Callsign", &mut wl.callsign, 140.0);
                net_secret(ui, "Password", &mut wl.password, 140.0);
                ui.label(
                    RichText::new(
                        "The Winlink account password, not the gateway password. It is \
                         case-sensitive — enter it exactly as it was issued.",
                    )
                    .weak(),
                );
                net_row(ui, "Locator", &mut wl.locator, 100.0);

                ui.add_space(6.0);
                net_heading(ui, "How to connect");
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 22.0], egui::Label::new("Route"));
                    ui.selectable_value(
                        &mut wl.lane,
                        sdroxide_types::WinlinkLane::Telnet,
                        "Internet",
                    )
                    .on_hover_text("Forward with the CMS over the internet");
                    ui.selectable_value(
                        &mut wl.lane,
                        sdroxide_types::WinlinkLane::Packet,
                        "Radio (packet)",
                    )
                    .on_hover_text(
                        "Call an RMS gateway on the air. The radio must be in PACKET or \
                         PACKET-HF.",
                    );
                });

                if wl.lane == sdroxide_types::WinlinkLane::Packet {
                    ui.add_space(4.0);
                    net_row(ui, "Gateway", &mut wl.gateway, 140.0);
                    let mut via = wl.gateway_via.join(" ");
                    ui.horizontal(|ui| {
                        ui.add_sized([96.0, 22.0], egui::Label::new("Via"));
                        if ui.add_sized([200.0, 22.0], egui::TextEdit::singleline(&mut via)).changed()
                        {
                            wl.gateway_via = via
                                .split_whitespace()
                                .map(|s| s.to_uppercase())
                                .collect();
                        }
                    });
                    ui.label(
                        RichText::new(
                            "Digipeaters, in order, separated by spaces. Usually empty — a \
                             gateway you can hear directly is a gateway you should call \
                             directly.",
                        )
                        .weak(),
                    );

                    ui.add_space(6.0);
                    net_heading(ui, "My gateways");
                    ui.label(
                        RichText::new(
                            "Winlink's published gateway list needs an API key sdroxide does \
                             not have, so keep your own. The two or three gateways reachable \
                             from one location are learned by trying, and they rarely change.",
                        )
                        .weak(),
                    );

                    let mut remove = None;
                    let mut pick = None;
                    for (i, g) in wl.gateways.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui
                                .button("USE")
                                .on_hover_text("Call this one on the next connect")
                                .clicked()
                            {
                                pick = Some(i);
                            }
                            let freq = if g.freq_hz > 0.0 {
                                format!("{:.4} MHz", g.freq_hz / 1e6)
                            } else {
                                "current dial".to_string()
                            };
                            let via = if g.via.is_empty() {
                                String::new()
                            } else {
                                format!(" via {}", g.via.join(" "))
                            };
                            ui.label(format!(
                                "{}{via} — {freq}, {} baud{}{}",
                                g.callsign,
                                g.baud.label(),
                                if g.label.is_empty() { "" } else { " — " },
                                g.label,
                            ));
                            // Text, not a glyph: this font has no ✕ and draws a
                            // tofu box, which reads as a broken button.
                            if ui.button("FORGET").on_hover_text("Forget this gateway").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = pick {
                        let g = wl.gateways[i].clone();
                        wl.gateway = g.callsign;
                        wl.gateway_via = g.via;
                    }
                    if let Some(i) = remove {
                        wl.gateways.remove(i);
                    }

                    ui.add_space(4.0);
                    if ui
                        .button("+ ADD GATEWAY")
                        .on_hover_text("Save the gateway above to the list")
                        .clicked()
                        && !wl.gateway.trim().is_empty()
                    {
                        wl.gateways.push(sdroxide_types::WinlinkGateway {
                            callsign: wl.gateway.trim().to_uppercase(),
                            via: wl.gateway_via.clone(),
                            freq_hz: 0.0,
                            baud: sdroxide_types::PacketBaud::default(),
                            label: String::new(),
                        });
                    }
                }

                ui.add_space(6.0);
                net_heading(ui, "Internet gateway");
                net_row(ui, "CMS address", &mut wl.cms_address, 220.0);
                net_row(ui, "Client name", &mut wl.app_name, 140.0);
                ui.label(
                    RichText::new(
                        "Winlink's production servers only accept client names they know, and \
                         answer an unknown one with \"Unknown client types are not allowed on \
                         production servers\". Until sdroxide is registered with the Winlink \
                         Development Team, connecting needs a name they recognise.",
                    )
                    .weak(),
                );

                ui.add_space(6.0);
                net_heading(ui, "Automatic connection");
                ui.checkbox(&mut wl.auto_connect, "Connect on a timer");
                ui.add_enabled_ui(wl.auto_connect, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_sized([96.0, 22.0], egui::Label::new("Every"));
                        ui.add(
                            egui::DragValue::new(&mut wl.auto_connect_minutes)
                                .range(5..=1440)
                                .suffix(" min"),
                        );
                    });
                });

                // Every network tab commits through its own APPLY: the edits
                // above live in a scratch copy until one is pressed. Without
                // this button the account is typed in, appears to stick, and
                // the engine never hears about it — so connecting reports that
                // no callsign has been set.
                ui.add_space(8.0);
                if crate::chrome::chip_accent(
                    ui,
                    false,
                    RichText::new(" APPLY ").strong(),
                    crate::theme::GREEN(),
                    crate::theme::INK_ON_CYAN(),
                )
                .on_hover_text("Persist the Winlink account")
                .clicked()
                {
                    *io.net_apply = true;
                }
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
                        crate::theme::GREEN(),
                        crate::theme::INK_ON_CYAN(),
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
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                settings_rotator_tab(
                    ui,
                    io.rot_edit,
                    self.rot_cfg_seeded,
                    &self.rotator_status,
                    io.rot_apply,
                );
            }
            #[cfg(not(target_arch = "wasm32"))]
            SettingsTab::Remote => settings_remote_tab(
                ui,
                io.remote_edit,
                io.remote_connect,
                // A session with no shell around it has nowhere to put the
                // connection. Every native build has one; this is what keeps
                // the button honest if that ever stops being true.
                !self.radio_roster.is_empty(),
                self.remote_status.as_ref(),
            ),
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

/// The radio-management strip at the top of Settings → Radio: the same chips
/// the main window's tab area shows, drawn where radios are configured. The
/// main window only shows its copy once there is more than one radio, so this
/// is where the second one is added. Actions cannot be taken here — the tab
/// set lives in the multi-radio shell, not in this tab — so they are queued as
/// [`crate::app::RadioTabRequest`]s and the shell acts on them after the frame.
#[cfg(not(target_arch = "wasm32"))]
impl SdroxideApp {
    fn settings_radio_roster(
        &self,
        ui: &mut egui::Ui,
        requests: &mut Vec<crate::app::RadioTabRequest>,
        name_edit: &mut Option<(u32, String)>,
    ) {
        // Nothing to manage: no roster at all (the browser client), or a
        // session that holds one radio it cannot add to — a client dialled
        // straight at somebody else's station, which has exactly one thing on
        // screen and no hardware of its own to open beside it.
        let Some(station) = self.radio_roster.first() else { return };
        if self.radio_roster.len() < 2 && !self.can_add_radio {
            return;
        }
        let station_id = station.id;
        ui.horizontal_wrapped(|ui| {
            for chip in &self.radio_roster {
                let mut label = RichText::new(chip.display_name()).size(12.5);
                if chip.focused {
                    label = label.strong().color(crate::theme::TEXT_STRONG());
                }
                if crate::chrome::chip(ui, chip.focused, label)
                    .on_hover_text(if chip.focused {
                        "This radio's settings are below"
                    } else {
                        "Switch to this radio (the dialog follows)"
                    })
                    .clicked()
                    && !chip.focused
                {
                    requests.push(crate::app::RadioTabRequest::Focus(chip.id));
                }
                if chip.tx_on {
                    ui.label(RichText::new("● TX").size(11.0).color(crate::theme::ALERT()));
                } else if chip.error {
                    ui.label(RichText::new("⚠").size(11.0).color(crate::theme::ALERT()));
                }
                let mute = crate::chrome::chip(
                    ui,
                    chip.muted,
                    RichText::new(if chip.muted { "🔇" } else { "🔊" }).size(11.0),
                );
                if mute.on_hover_text("Mute this radio's audio").clicked() {
                    requests.push(crate::app::RadioTabRequest::Mute {
                        id: chip.id,
                        muted: !chip.muted,
                    });
                }
                // The first radio is the station: it runs the shared network
                // services and the legacy configuration, and it stays.
                if chip.id != station_id
                    && crate::chrome::chip(ui, false, RichText::new("×").size(11.0))
                        .on_hover_text("Close this radio (its configuration is kept)")
                        .clicked()
                {
                    requests.push(crate::app::RadioTabRequest::Close(chip.id));
                }
                ui.add_space(6.0);
            }
            // Absent where there is nothing to add: a client that only drives
            // somebody else's station has no local radios to open. Connecting
            // to a further server is still offered, on the Remote tab.
            if self.can_add_radio
                && crate::chrome::chip(ui, false, RichText::new("+").size(13.0))
                    .on_hover_text("Add a radio")
                    .clicked()
            {
                requests.push(crate::app::RadioTabRequest::Add);
            }
        });
        // The focused radio's name. By default a radio is named after its
        // interface — the box is empty and the hint shows what that resolves
        // to — and typing here gives it a name of the operator's own.
        // Committed when the field loses focus (Enter included); cleared, the
        // derived default takes over again.
        if let Some(chip) = self.radio_roster.iter().find(|c| c.focused) {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Name").strong());
                let stale = !matches!(name_edit, Some((id, _)) if *id == chip.id);
                if stale {
                    *name_edit = Some((chip.id, chip.name.clone()));
                }
                let buf = &mut name_edit.as_mut().expect("seeded above").1;
                let resp = ui.add(
                    egui::TextEdit::singleline(buf)
                        .hint_text(chip.default_name.as_str())
                        .desired_width(220.0),
                );
                if resp.lost_focus() {
                    let name = buf.trim().to_string();
                    if name != chip.name {
                        requests.push(crate::app::RadioTabRequest::Rename { id: chip.id, name });
                    }
                }
            });
        }
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(6.0);
    }
}
