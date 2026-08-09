//! Core domain vocabulary shared by every sdroxide component, native and WASM.
//!
//! This crate must stay free of I/O, threads, and native-only dependencies:
//! it compiles for `wasm32-unknown-unknown`.

mod access;
mod awards;
mod band;
mod band_segments;
pub mod broadcast;
mod callsign;
mod caps;
mod command;
mod contacts;
mod controller;
mod digi;
mod entity;
mod geo;
mod input;
mod js8;
mod memory;
mod meters;
mod mode;
mod netcfg;
mod pictures;
mod prop_store;
mod propagation;
mod radio;
mod rifp;
mod rigctld;
mod rotator;
mod satcfg;
mod satlock;
mod scanner;
mod skimmer;
mod spectrum;
mod speech;
mod spot;
mod sstv;
mod state;
mod station;
mod tciserver;
mod tone;
mod ui;
mod voice;
mod wefax;
mod worldmask;
mod wsjtx;
mod wspr;

pub use access::{AuthPhase, RemoteAccess};
pub use awards::{
    Awards, Coverage, EntitySlot, Highlight, LogIndex, Novelty, Status as AwardStatus, US_STATES,
    compute_awards, counts, coverage_counts, entity_coverage, entity_name,
};
pub use band::Band;
pub use band_segments::{
    DigiChannel, FSQ_DIALS, FT4_DIALS, FT8_DIALS, FT8_DXPED_DIALS, JS8_DIALS, PSK_DIALS,
    PSK_RANGES, RIFP_CALLING, RTTY_DIALS, RTTY_RANGES, SSTV_CALLING, Segment, SegmentKind,
    WSPR_DIALS, digi_channels, digi_channels_in, is_auto_digi, is_cw_segment, is_digi_segment,
    is_psk_segment, is_rtty_segment, segment_kind_at,
};
pub use broadcast::{BroadcastStation, BroadcastStations};
pub use callsign::{CallsignInfo, UploadResult, UploadTarget};
pub use caps::{DeviceCaps, Direction, GainElement};
pub use command::Command;
pub use contacts::FsqContact;
pub use controller::{AudioDevices, RadioController, RadioEvent};
pub use digi::{
    ClockHealth, CwStatus, Decode, DigiConfig, DigiStatus, DxpedMode, FOX_MAX_SLOTS,
    FOX_ZONE_MAX_HZ, FoxCaller, FsqMsg, HOUND_ZONE_MAX_HZ, HellVariant, QsoRecord, QsoStep,
    QueuedCall, RadeStatus, ThorMode, TranscriptLine, adif_band, adif_to_qso_log, clock_health,
    cq_is_for_us, fmt_report, qso_log_to_adif, qso_log_to_text, utc_ymd_hms, worked_before,
    ymd_hms_to_unix,
};
pub use entity::{
    EntityInfo, EntityPlace, all_entities, resolve_callsign, resolve_place, resolve_prefix,
};
pub use geo::{
    distance_km, great_circle_points, grid_bearing, grid_distance_km, grid_to_latlon, is_land,
    land_cell, land_mask_dims,
};
pub use input::{
    Action, ActionInput, ActionKind, BindingTuning, ButtonMode, InputSettings, KeyBinding,
    KeyChord, MidiBinding, MidiMsg, MidiMsgKind, MidiSettings, MouseButton, MouseButtonBinding,
    RelativeMode, WheelAction, WheelSettings,
};
pub use js8::{
    HB_BAND_HI_HZ, HB_BAND_LO_HZ, HB_SLOT_HZ, Js8FrameInfo, Js8FrameKind, Js8Heard, Js8Msg,
    Js8Speed, Js8Status,
};
pub use memory::{BandStackEntry, MemoryChannel, MemoryFolder, RttyMemory};
pub use meters::{Meters, TxMeters, TxTelemetry};
pub use mode::{AgcMode, Mode, NrEngine, NrLevel, NrStrength};
pub use netcfg::{
    ClusterConfig, Credentials, FeedConfig, FreeDvReporterConfig, LookupProvider, NetworkConfig,
    PskConfig, RbnConfig, WsprNetConfig,
};
pub use pictures::{
    IMAGE_NAME_MAX, IMAGE_PAGE_MAX, IMAGE_SLOT_THUMB_EDGE, IMAGE_SLOTS, IMAGE_SOURCE_MAX_EDGE,
    IMAGE_THUMB_EDGE, IMAGE_UPLOAD_MAX, ImageEntry, ImageKind, ImageListing, ImagePresets,
    ImageSlotInfo, received_at, safe_name,
};
pub use prop_store::{PropSources, PropStore};
pub use propagation::{
    BandPlane, DEFAULT_HALFLIFE_S as PROP_DEFAULT_HALFLIFE_S, DEFAULT_HM_KM, GRID_CELLS,
    GRID_H as PROP_GRID_H, GRID_W as PROP_GRID_W, MAX_HOP_KM, MAX_HOPS, MIN_MUF_PATH_KM,
    MIN_MUF_PATHS, PropField, PropMuf, PropObservation, PropPath, PropSource, REF_TX_DBM,
    SPLAT_SIGMA_KM, cell_center, cell_of, fof2_floor_mhz, margin_db, muf3000_floor_mhz,
    obliquity_factor,
};
pub use radio::{
    Backend, CONVERTER_OFFSET_MAX_HZ, CONVERTER_PRESETS, CatConfig, CatFamily, DigiMode,
    FREQ_RANGE_MAX_HZ, HpsdrConfig, HpsdrDevice, HpsdrFilterBoard, LineState, ModeControl, Parity,
    PlutoAgc, PlutoConfig, PlutoDevice, PttMethod, RadioConfig, RtlSdrAgc, RtlSdrConfig,
    RtlSdrDevice, RtlSdrHfMode, Rx888Config, Rx888Device, SdrPlayAgc, SdrPlayConfig, SdrPlayDevice,
    SdrPlayDuoTuner, SdrPlayModel, SerialConfig, SmartSdrConfig, SmartSdrDevice, SoundFormat,
    StopBits, TciConfig, converter_preset_name, format_freq_ranges, parse_freq_ranges,
};
pub use rifp::{
    RIFP_CALLING_HZ, RIFP_MAP_MAX_CHUNKS, RifpEncoding, RifpMeta, RifpProfile, RifpSession,
    RifpSize, RifpStatus,
};
pub use rigctld::RigctldConfig;
pub use rotator::RotatorConfig;
pub use satcfg::{
    CELESTRAK_GROUPS, CelestrakGroup, CustomTle, OrbitRings, Passband, SatConfig, SatFreqs,
    SatLink, TleSubStatus, TleSubscription, fmt_mhz as fmt_sat_mhz, parse_tle_block,
};
pub use satlock::{
    C_KM_S, SatLockConfig, SatPass, SatTrackStatus, SatUplink, doppler_rx_hz, doppler_tx_hz,
};
pub use scanner::{SCAN_STEPS_HZ, ScanKind, ScanResume, ScanState, ScannerConfig};
pub use skimmer::{
    CW_SLOT_CHOICES, CW_SLOTS_DEFAULT, CwSkimmerDecoder, SkimmerKind, SkimmerSettings, SkimmerSpot,
};
pub use spectrum::{SpectrumConfig, SpectrumFrame};
pub use speech::{
    CallsignStyle, CategoryFlags, DecodeSpeech, FreqStyle, SpeechSettings, TextSpeech, TuneSpeech,
    Verbosity,
};
pub use spot::{Spot, SpotKind};
pub use sstv::{SstvMode, SstvStatus};
pub use state::{
    MAX_MANUAL_GAIN_DB, OffsetState, RadioState, RxId, RxState, SQUELCH_OPEN_DB, TxState, Vfo,
};
pub use station::StationConfig;
pub use tciserver::TciServerConfig;
pub use tone::{CTCSS_TONES, SubTone};
pub use ui::{ChromeStyle, LayoutMode, Speed, UiSettings, UiTheme};
pub use voice::{VOICE_MAX_LEN_S, VOICE_SLOTS, VoiceSlotInfo, VoiceStatus, slot_label};
pub use wefax::{WEFAX_STATIONS, WefaxChartMeta, WefaxIoc, WefaxLpm, WefaxStation, WefaxStatus};
pub use wsjtx::WsjtxConfig;
pub use wspr::{
    BURST_S as WSPR_BURST_S, DEFAULT_TX_HZ as WSPR_DEFAULT_TX_HZ, POWERS_DBM as WSPR_POWERS_DBM,
    POWERS_W as WSPR_POWERS_W, SLOT_S as WSPR_SLOT_S, TX_OFFSET_S as WSPR_TX_OFFSET_S,
    WINDOW_HI_HZ as WSPR_WINDOW_HI_HZ, WINDOW_LO_HZ as WSPR_WINDOW_LO_HZ, WsprSpot, WsprStatus,
    dbm_to_mw, grid4 as wspr_grid4, power_dbm_for_watts, power_label, round_power_dbm,
};
