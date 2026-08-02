//! The main window's application state and its behaviour.
//!
//! [`SdroxideApp`] is one large struct — everything the window draws is a
//! function of it, and egui redraws all of it every frame — so splitting the
//! state up would only scatter it. What *is* split is the behaviour: this
//! module holds the state and its constructor, and each submodule holds the
//! `impl SdroxideApp` methods for one part of the window:
//!
//! - [`frame`] — the per-frame loop: drain engine events, lay the window out.
//! - [`top_bar`] — the control strip along the top (VFO, filter, TX, display).
//! - [`spectrum`] — panadapter/waterfall config and the overlays drawn on it.
//! - [`panels`] — the bottom panel, one submodule per operating mode.
//! - [`settings`] — the Settings dialog, one submodule per tab.
//! - [`spots`], [`logbook`], [`awards`], [`net`] — the network cockpit.
//! - [`windows`], [`solar`] — the remaining overlays and the 3D view's feed.
//! - [`persist`], [`util`] — on-disk state and small shared helpers.
//!
//! Submodules are descendants of this one, so they reach the private fields
//! below directly; what they expose *to each other* is marked
//! `pub(in crate::app)`, which is as wide as anything here ever gets.

pub(in crate::app) mod awards;
pub(in crate::app) mod frame;
pub(in crate::app) mod logbook;
pub(in crate::app) mod net;
pub(in crate::app) mod panels;
pub(in crate::app) mod persist;
pub(in crate::app) mod settings;
pub(in crate::app) mod solar;
pub(in crate::app) mod spectrum;
pub(in crate::app) mod spots;
pub(in crate::app) mod top_bar;
pub(in crate::app) mod util;
pub(in crate::app) mod windows;

use std::sync::{Arc, Mutex};

use eframe::egui;
use sdroxide_types::{
    AudioDevices, CallsignInfo, Decode, DeviceCaps, DigiStatus, MemoryChannel, Meters,
    NetworkConfig, QsoRecord, RadioController, RadioState, SkimmerSpot, SpectrumConfig,
    SpectrumFrame, Spot, UploadTarget,
};

use crate::view::ViewState;
use crate::waterfall_gpu;
use crate::widgets::spectrum_view;

use self::logbook::LogEditForm;
use self::panels::decodes::DecodeSort;
use self::panels::fsq::fsq_load_contacts;
use self::panels::rf_paint::RfPaintUi;
use self::panels::sstv::SstvUi;
use self::persist::{load_broadcast_stations, load_qso_log, load_ui_settings};
use self::settings::servers::TciServerStatus;
use self::settings::{SatEditState, SettingsTab};

pub struct SdroxideApp {
    ctrl: Box<dyn RadioController>,
    caps: Option<DeviceCaps>,
    state: RadioState,
    /// Latest spectrum frame, shared with the GPU waterfall callback — the Arc
    /// makes the per-repaint handoff a refcount bump instead of a bins clone.
    frame: Option<std::sync::Arc<SpectrumFrame>>,
    /// Latest full-band frame, from a direct-sampling front end that can see far
    /// more than the IQ it delivers. `None` on every other backend, which is
    /// also how the UI decides whether to offer the strip at all.
    wide_frame: Option<std::sync::Arc<SpectrumFrame>>,
    /// Scrolling history behind the full-band strip.
    wide_wf: crate::widgets::wide_spectrum::WideWaterfall,
    meters: Option<Meters>,
    memories: Vec<MemoryChannel>,
    view: ViewState,
    peaks: spectrum_view::PeakHold,
    /// UI-side smoothing for the spectrum *line* (waterfall stays un-averaged).
    spec_smooth: spectrum_view::SpectrumSmooth,
    error: Option<String>,
    /// Persistent, non-fatal operator notice (e.g. radio audio input
    /// unavailable / mono card selected for IQ). Shown as a warning banner.
    radio_notice: Option<String>,
    sent_cfg: Option<SpectrumConfig>,
    desired_cfg: Option<SpectrumConfig>,
    desired_at: f64,
    /// egui time of the last received spectrum frame, for stall detection.
    last_spectrum_at: f64,
    /// Waterfall time-scroll state: wall-clock (UTC secs) of the last tick and
    /// the carried fractional row, so the scroll rate is exact and independent
    /// of the frame rate (keeps the waterfall and time gridlines in lockstep).
    wf_last_now: f64,
    wf_row_accum: f32,
    /// Cached spectrum polylines (recomputed only when frame/view/rect change).
    trace_cache: spectrum_view::TraceCache,
    /// Switchable sound devices, queried once each time the settings dialog
    /// opens (cpal enumeration is too slow for per-frame).
    audio_devices: Option<AudioDevices>,
    audio_devices_queried: bool,
    /// Whether this build can drive SoapySDR (offered as an interface option).
    soapy_supported: bool,
    /// Settings dialog: current tab, plus the radio-backend config + serial
    /// ports loaded once on open (edited live, persisted on change).
    ///
    /// The tab is deliberately session-only — reopening the dialog returns to
    /// wherever you last were, but a restart starts again at General, so it is
    /// never written to storage in [`eframe::App::save`].
    settings_tab: SettingsTab,
    /// Display preferences (frame rate, waterfall + spectrum speed), loaded from
    /// config at startup, edited in the UI tab, persisted on change.
    ui_settings: sdroxide_types::UiSettings,
    radio_cfg: Option<sdroxide_types::RadioConfig>,
    /// The converter offset being typed on the Radio tab, in MHz. Held apart
    /// from `radio_cfg` because every other field on that tab is written to
    /// `radio.json` as it is typed, and the engine rereads that file whenever it
    /// reopens the radio — so a half-typed `125000000` would arrive as an offset
    /// of 12 Hz. This one lands only on Apply. `None` = not being edited.
    converter_edit_mhz: Option<f64>,
    serial_ports: Vec<String>,
    /// HPSDR devices found by the last "Discover" scan in the settings dialog.
    hpsdr_devices: Vec<sdroxide_types::HpsdrDevice>,
    rtlsdr_devices: Vec<sdroxide_types::RtlSdrDevice>,
    rx888_devices: Vec<sdroxide_types::Rx888Device>,
    /// Result of the last TCI "Test connection" (Ok summary / Err message).
    tci_test_result: Option<Result<String, String>>,
    /// FlexRadios found by the last SmartSDR "Discover" listen.
    smartsdr_devices: Vec<sdroxide_types::SmartSdrDevice>,
    /// Result of the last SmartSDR "Test connection".
    smartsdr_test_result: Option<Result<String, String>>,
    seen_first_state: bool,
    show_memories: bool,
    show_settings: bool,
    /// Voice keyer: the engine's slot list and what it is doing, the window's
    /// open state, and the one slot label being typed into (only the focused
    /// row is UI-owned, so the status echo can't fight the keyboard).
    voice: sdroxide_types::VoiceStatus,
    show_voice: bool,
    voice_name_edit: Option<(usize, String)>,
    /// When the band/mode, FFT and skimmer popups opened (egui time), for their
    /// auto-fade.
    mode_popup_since: Option<f64>,
    fft_popup_since: Option<f64>,
    skimmer_popup_since: Option<f64>,
    /// Fade clock for the sub-audible tone popup, like `skimmer_popup_since`.
    tone_popup_since: Option<f64>,
    /// The layout in force last frame, so a change can re-apply the style
    /// metrics (chip padding, text sizes) exactly once instead of every frame.
    tier: crate::layout::Tier,
    /// Whether the compact layout's press-and-hold PTT is being held, so it
    /// sends one command per edge rather than one per frame.
    ptt_held: bool,
    mem_name: String,
    // Skimmer (CW etc.) spots, newest merge-by-id.
    skimmer_spots: Vec<SkimmerSpot>,
    /// Per-spot last-active timestamp (egui seconds), so a box fades out over
    /// `SKIMMER_FADE_SECS` once its signal stops keying instead of vanishing.
    skimmer_active_at: std::collections::HashMap<u64, f64>,
    // FT8/FT4 digital-mode state.
    digi_decodes: Vec<Decode>,
    digi_status: Option<DigiStatus>,
    /// PSK/RTTY outgoing text buffer (UI-owned; streamed to the engine, which
    /// reports back how many characters have been sent so we colour them green).
    text_tx: String,
    qso_log: Vec<QsoRecord>,
    /// QSOs worked since this run started, which is what the FT8 panel's
    /// "Session" readout counts. Deliberately not derived from the logbook: the
    /// log is persisted and grows for ever, so counting it would report every
    /// contact ever made as if it had just been worked.
    session_qsos: usize,
    show_digi_settings: bool,
    /// UI-owned editable copy of the operator config, so typing isn't fought
    /// by the round-tripped status echo. Seeded once from the first status.
    digi_cfg_edit: sdroxide_types::DigiConfig,
    digi_cfg_seeded: bool,
    /// SSTV image-mode panel state (gallery, TX slots, message, textures).
    sstv: SstvUi,
    /// RF Paint (Spectrum Painting) panel state (text/image + previews).
    rf_paint: RfPaintUi,
    /// Hellschreiber receive raster (scrollback ring + texture).
    hell: crate::hell::HellUi,
    /// FSQ directed-message target callsign ("" = broadcast/ALLCALL).
    fsq_target: String,
    /// JS8: the `To:` callsign the composer addresses. Also what the globe
    /// draws the QSO arc to, JS8 having no QSO sequencer to ask instead.
    js8_target: String,
    /// JS8: callsigns a locator has already been requested for this session,
    /// successful or not. Every lookup is an HTTP round trip on its own thread,
    /// and a busy band puts fifty stations in the heard list.
    js8_looked_up: std::collections::HashSet<String>,
    /// JS8: frame time of the last locator lookup, so they go out one at a time
    /// rather than fifty at once the moment the panel opens.
    js8_lookup_at: f64,
    /// JS8: the last message we transmitted. What `AGN?` — "say again" — is
    /// asking for, and the one reply the operator cannot retype from memory.
    js8_last_sent: String,
    /// FSQ contacts (address book), native-persisted in `contacts.json`.
    fsq_contacts: Vec<sdroxide_types::FsqContact>,
    /// FSQ "add contact" input field.
    fsq_new_contact: String,
    /// Whether the FSQ contacts editor window is open.
    fsq_show_contacts: bool,
    /// FSQ received-image gallery (decoded textures, newest first).
    fsq_rx_images: Vec<egui::TextureHandle>,
    /// Picked-image inbox for FSQ image transmit (raw file bytes).
    fsq_img_inbox: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    /// The last decode the user clicked (not REPLY): its call and map
    /// location, shown as a faint preview marker distinct from the active DX.
    digi_preview: Option<(String, (f64, f64))>,
    /// Animated centre/zoom of the FT8 world map (eased toward the fit target).
    map_view: crate::widgets::worldmap::MapView,
    /// Which decoded stations are currently up, and how brightly. Shared with
    /// the 3D globe so the flat map and the globe never disagree.
    digi_stations: crate::digi_map::DigiStations,
    /// Location of the decode row hovered this frame, shown on the map as a
    /// bright yellow dot. Frame-scoped (set by the decode list, read by the map).
    digi_hover_ll: Option<(f64, f64)>,
    /// Decode-list ordering within each turn, and whether to show CQ only.
    digi_sort: DecodeSort,
    /// Sort direction: `true` = descending (strongest / farthest first).
    digi_sort_desc: bool,
    digi_cq_only: bool,
    /// Decode-list filter: only stations that would put something new in the
    /// log (new entity, new band-slot, new grid, or a callsign never worked).
    digi_new_only: bool,
    /// The FT8 free-text entry, sent verbatim in the next transmit slot.
    digi_free_text: String,
    /// Logbook overlay open state, and the in-progress new/edit entry (if any).
    show_logbook: bool,
    log_edit: Option<LogEditForm>,
    // ── Network cockpit (spots / lookup / uploads) ──
    /// Latest merged network spots (DX cluster / POTA / SOTA / PSK Reporter).
    spots: Vec<Spot>,
    /// Latest feed/connection status line (cluster state, feed errors).
    net_status: Option<String>,
    /// Spots window open state.
    show_spots: bool,
    /// Show only spots that fall inside the current panadapter view span.
    spot_in_view_only: bool,
    /// Fuzzy search query for the spot list. Narrows the list in the SPOTS
    /// window only — the waterfall labels are positioned by frequency, so
    /// reordering them by match quality would mean nothing.
    spot_search: String,
    /// The broadcast schedule in use: the cached season file plus the operator's
    /// own entries, or the compiled-in copy until a download lands.
    broadcast: Vec<sdroxide_types::BroadcastStation>,
    /// The subset of `broadcast` on air right now, as spots. Rebuilt when the
    /// UTC minute rolls over — the finest granularity a schedule changes at —
    /// rather than every frame.
    broadcast_spots: Vec<Spot>,
    /// The UTC minute `broadcast_spots` was built for.
    broadcast_minute: i64,
    /// An in-flight schedule download, if one is running.
    broadcast_fetch: Option<std::sync::mpsc::Receiver<persist::ScheduleFetch>>,
    /// What the last download did, for the settings panel. `None` while one is
    /// running or before any has been attempted.
    broadcast_fetch_status: Option<Result<String, String>>,
    /// The UTC day the season was last checked on. A season change is a calendar
    /// event, so checking once a day catches it without a timer.
    broadcast_checked_day: i64,
    /// UI-owned editable copy of the network config (edited in the Settings
    /// dialog's Spots / FreeDV / Uploads tabs). Carries no operator identity —
    /// that comes from the digi config, edited on the General tab.
    ///
    /// Seeded from `RadioEvent::StationConfig` — the engine's copy, wherever
    /// the engine is running — and left alone afterwards so typing sticks. The
    /// tabs that edit it stay disabled until it has arrived: applying an
    /// unseeded copy would write defaults over the station's real config.
    net_cfg_edit: NetworkConfig,
    net_cfg_seeded: bool,
    // ── Built-in TCI server ──
    /// UI-owned editable copy of the TCI server config, seeded from the engine
    /// like the network config above.
    tci_srv_edit: sdroxide_types::TciServerConfig,
    tci_srv_seeded: bool,
    /// Live server status (bound address, connected clients, bind error) from
    /// `RadioEvent::TciServerStatus`.
    tci_srv_status: Option<TciServerStatus>,
    // ── Built-in rigctld server ──
    /// UI-owned editable copy of the rigctld config, seeded from the engine.
    rigctld_edit: sdroxide_types::RigctldConfig,
    rigctld_seeded: bool,
    // ── WSJT-X UDP broadcast (decodes / status / QSOs for the loggers) ──
    /// UI-owned editable copy, seeded from the engine like the configs above.
    wsjtx_edit: sdroxide_types::WsjtxConfig,
    wsjtx_seeded: bool,
    /// Live status from `RadioEvent::RigctldStatus`. Same shape as the TCI
    /// server's, so the two share one status type.
    rigctld_status: Option<TciServerStatus>,
    /// Editable "extra cluster commands" (one per line), split into
    /// `net_cfg_edit.cluster.commands` on apply.
    net_cluster_cmds: String,
    /// Rolling upload/lookup result log for the spots window (newest first).
    net_log: Vec<String>,
    /// Inbox for an ADIF file chosen via the native "Import" dialog (a picker
    /// thread writes; the UI drains it each frame).
    adif_import_inbox: Arc<Mutex<Option<String>>>,
    /// Callsigns queued for lookup, drained into commands each frame.
    pending_lookups: Vec<String>,
    /// Everything callsign lookup has resolved this session, by callsign. Kept
    /// because a JS8 station's locator usually never arrives on the air —
    /// only heartbeats and CQs carry one — so the map has nothing else to
    /// place the rest of the conversation by.
    callsign_cache: std::collections::HashMap<String, CallsignInfo>,
    /// QSO uploads queued (id, single-record ADIF, targets), drained to commands.
    pending_uploads: Vec<(u64, String, Vec<UploadTarget>)>,
    /// Awards dashboard open state + band filter ("" = all bands).
    show_awards: bool,
    awards_band: String,
    /// Cached award tally, keyed by (log length, band filter).
    awards_cache: Option<(usize, String, sdroxide_types::Awards)>,
    /// The same tally placed on the globe for the 3D view's award layer, keyed
    /// the same way. Shared rather than copied: it is three hundred entities
    /// and the window republishes it every frame.
    awards_heat: Option<(usize, String, Arc<Vec<sdroxide_types::EntitySlot>>)>,
    /// Cached set of worked DXCC entity names, keyed by log length (for the
    /// "new entity" spot badge).
    worked_entities_cache: Option<(usize, std::collections::HashSet<String>)>,
    /// Cached membership sets over the log, keyed by log length — the decode
    /// list asks these which stations would be a new one, every row, every slot.
    log_index_cache: Option<(usize, sdroxide_types::LogIndex)>,
    /// F1 help: the embedded user manual with a navigation outline.
    help: crate::help::Help,
    /// Control inputs: keyboard/mouse bindings, MIDI, and what is held right now.
    input: crate::input::InputRuntime,
    /// MIDI ports as `(id, name)`, enumerated when the settings dialog opens
    /// (touching the host MIDI stack is too slow for per-frame).
    midi_in_ports: Vec<(String, String)>,
    midi_out_ports: Vec<(String, String)>,
    /// Solar-system 3D view, shown in its own OS window (native-only).
    #[cfg(not(target_arch = "wasm32"))]
    solar: crate::solar3d::Solar3d,
    /// The operator's satellite additions: element sets they pasted in or
    /// subscribed to, and their frequency corrections. Shared by `Arc` because
    /// the solar window's render closure takes a handle it outlives any borrow
    /// of; replaced wholesale on every edit rather than mutated in place.
    ///
    /// Seeded from the engine like the network config: the subscribed listings
    /// are fetched — and cached — on the machine the engine runs on, which is
    /// the only one a browser client can reach.
    sat_cfg: std::sync::Arc<sdroxide_types::SatConfig>,
    /// The settings dialog's working copy, its transient state, and what each
    /// subscription's last fetch did.
    sat_cfg_edit: sdroxide_types::SatConfig,
    sat_cfg_seeded: bool,
    sat_ui: SatEditState,
    sat_sub_status: Vec<sdroxide_types::TleSubStatus>,
    /// Weather fax: the chart being painted and the gallery of saved ones.
    wefax: crate::wefax::WefaxUi,
    /// Whether the operator has dismissed the out-of-band transmit warning
    /// this session. Never persisted: `--oob-tx` has to be passed again on the
    /// next launch, so the warning has to be acknowledged again too.
    oob_tx_ack: bool,
    /// The sign-in screen a server that asks for a password puts up, in place
    /// of everything above.
    login: crate::login::LoginForm,
    /// Who may connect to *this* machine's server, edited on the General tab.
    ///
    /// Only shown — and only meaningful — when the engine is in this process:
    /// these are `config.toml` on the machine the radio is attached to, so a
    /// remote client has nothing here to read them from and no business
    /// writing them. Left at the default and never persisted in the browser.
    remote_access: sdroxide_types::RemoteAccess,
}

impl SdroxideApp {
    pub fn new(cc: &eframe::CreationContext<'_>, ctrl: Box<dyn RadioController>) -> Self {
        crate::theme::apply(&cc.egui_ctx);
        if let Some(rs) = &cc.wgpu_render_state {
            waterfall_gpu::init(rs);
        }
        let view: ViewState =
            cc.storage.and_then(|s| eframe::get_value(s, "view")).unwrap_or_default();
        // Copied out before `view` is moved into the struct below.
        #[cfg(not(target_arch = "wasm32"))]
        let solar3d_view = view.solar3d;
        let soapy_supported = ctrl.soapy_supported();
        SdroxideApp {
            ctrl,
            caps: None,
            state: RadioState::default(),
            frame: None,
            wide_frame: None,
            wide_wf: Default::default(),
            meters: None,
            memories: Vec::new(),
            view,
            peaks: spectrum_view::PeakHold::default(),
            spec_smooth: spectrum_view::SpectrumSmooth::default(),
            error: None,
            radio_notice: None,
            sent_cfg: None,
            desired_cfg: None,
            desired_at: 0.0,
            last_spectrum_at: 0.0,
            wf_last_now: 0.0,
            wf_row_accum: 0.0,
            trace_cache: spectrum_view::TraceCache::default(),
            audio_devices: None,
            audio_devices_queried: false,
            soapy_supported,
            settings_tab: SettingsTab::General,
            ui_settings: load_ui_settings(cc.storage),
            radio_cfg: None,
            converter_edit_mhz: None,
            serial_ports: Vec::new(),
            hpsdr_devices: Vec::new(),
            rtlsdr_devices: Vec::new(),
            rx888_devices: Vec::new(),
            tci_test_result: None,
            smartsdr_devices: Vec::new(),
            smartsdr_test_result: None,
            seen_first_state: false,
            show_memories: false,
            show_settings: false,
            voice: sdroxide_types::VoiceStatus::default(),
            show_voice: false,
            voice_name_edit: None,
            mode_popup_since: None,
            fft_popup_since: None,
            skimmer_popup_since: None,
            tone_popup_since: None,
            // Corrected on the first frame, once the viewport size is known.
            tier: crate::layout::Tier::Desktop,
            ptt_held: false,
            mem_name: String::new(),
            skimmer_spots: Vec::new(),
            skimmer_active_at: std::collections::HashMap::new(),
            digi_decodes: Vec::new(),
            digi_status: None,
            text_tx: String::new(),
            qso_log: load_qso_log(cc.storage),
            session_qsos: 0,
            show_digi_settings: false,
            digi_cfg_edit: sdroxide_types::DigiConfig::default(),
            sstv: SstvUi::default(),
            rf_paint: RfPaintUi::default(),
            hell: Default::default(),
            fsq_target: String::new(),
            js8_target: String::new(),
            js8_looked_up: Default::default(),
            js8_lookup_at: 0.0,
            js8_last_sent: String::new(),
            fsq_contacts: fsq_load_contacts(),
            fsq_new_contact: String::new(),
            fsq_show_contacts: false,
            fsq_rx_images: Vec::new(),
            fsq_img_inbox: std::sync::Arc::new(std::sync::Mutex::new(None)),
            digi_cfg_seeded: false,
            digi_preview: None,
            map_view: Default::default(),
            digi_stations: Default::default(),
            digi_hover_ll: None,
            digi_sort: DecodeSort::None,
            digi_sort_desc: true,
            digi_cq_only: false,
            digi_new_only: false,
            digi_free_text: String::new(),
            show_logbook: false,
            log_edit: None,
            spots: Vec::new(),
            net_status: None,
            show_spots: false,
            spot_in_view_only: false,
            spot_search: String::new(),
            broadcast: load_broadcast_stations(),
            broadcast_spots: Vec::new(),
            broadcast_minute: -1,
            // Kicked off at startup: the first run has no cached schedule, and
            // after that this only fires again when the season turns over.
            broadcast_fetch: persist::spawn_schedule_fetch(false),
            broadcast_fetch_status: None,
            broadcast_checked_day: -1,
            net_cfg_edit: NetworkConfig::default(),
            net_cfg_seeded: false,
            rigctld_edit: sdroxide_types::RigctldConfig::default(),
            rigctld_seeded: false,
            wsjtx_edit: sdroxide_types::WsjtxConfig::default(),
            wsjtx_seeded: false,
            rigctld_status: None,
            tci_srv_edit: sdroxide_types::TciServerConfig::default(),
            tci_srv_seeded: false,
            tci_srv_status: None,
            net_cluster_cmds: String::new(),
            net_log: Vec::new(),
            adif_import_inbox: Arc::new(Mutex::new(None)),
            pending_lookups: Vec::new(),
            callsign_cache: Default::default(),
            pending_uploads: Vec::new(),
            show_awards: false,
            awards_band: String::new(),
            awards_cache: None,
            awards_heat: None,
            worked_entities_cache: None,
            log_index_cache: None,
            help: crate::help::Help::default(),
            input: crate::input::InputRuntime::new(cc.storage, &cc.egui_ctx),
            midi_in_ports: Vec::new(),
            midi_out_ports: Vec::new(),
            // The GPU resources are built on first open, not here: most
            // sessions never open this window.
            #[cfg(not(target_arch = "wasm32"))]
            solar: crate::solar3d::Solar3d::new(cc.wgpu_render_state.clone(), solar3d_view),
            sat_cfg: Default::default(),
            sat_cfg_edit: Default::default(),
            sat_cfg_seeded: false,
            sat_ui: Default::default(),
            sat_sub_status: Vec::new(),
            wefax: Default::default(),
            oob_tx_ack: false,
            login: Default::default(),
            remote_access: persist::load_remote_access(),
        }
    }
}
