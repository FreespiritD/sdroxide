//! Settings and radio-data persistence under the user config directory
//! (`~/.config/sdroxide/` on Linux).

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no home/config directory available")]
    NoConfigDir,
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("serialize: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// User settings (`config.toml`). Everything has a default so a missing or
/// partial file always loads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// SoapySDR device args, e.g. "driver=hackrf". Empty = first device found.
    pub device_args: String,
    /// Preferred hardware sample rate in Hz.
    pub sample_rate: f64,
    /// dB offset applied to convert dBFS to dBm for the S-meter.
    pub cal_offset_db: f64,
    pub spectrum_fft: u32,
    pub spectrum_fps: u8,
    /// Server mode bind address.
    pub server_bind: String,
    pub server_port: u16,
    /// Refuse to transmit outside amateur bands.
    pub tx_ham_only: bool,
    /// Preferred audio output device name; `None` = system default.
    pub audio_output: Option<String>,
    /// Preferred audio input (microphone) device name; `None` = system default.
    pub audio_input: Option<String>,
    /// UI / display preferences (frame rate, waterfall + spectrum speed).
    pub ui: sdroxide_types::UiSettings,
    /// Username and password a remote client must present in server mode.
    /// Empty (the default) leaves the server open, exactly as it was before
    /// this existed.
    ///
    /// Last in the struct because TOML puts tables after values, and serde
    /// emits fields in declaration order: a table declared above a plain value
    /// would swallow that value into itself on the next write.
    pub remote_access: sdroxide_types::RemoteAccess,
    /// Spoken announcements. A client-side preference like `[ui]`: what the
    /// operator at this screen wants to hear, not how the station is set up.
    ///
    /// Also a table, so it goes after every plain value for the reason above.
    pub speech: sdroxide_types::SpeechSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            device_args: String::new(),
            sample_rate: 1_536_000.0,
            cal_offset_db: 0.0,
            spectrum_fft: 4096,
            spectrum_fps: 30,
            server_bind: "0.0.0.0".into(),
            server_port: 4950,
            tx_ham_only: true,
            audio_output: None,
            audio_input: None,
            ui: sdroxide_types::UiSettings::default(),
            remote_access: sdroxide_types::RemoteAccess::default(),
            speech: sdroxide_types::SpeechSettings::default(),
        }
    }
}

/// Load just the UI/display preferences (frame rate, waterfall + spectrum speed).
pub fn load_ui_settings() -> sdroxide_types::UiSettings {
    Settings::load().ui
}

/// Persist the UI/display preferences, preserving every other setting
/// (read-modify-write so a concurrent edit elsewhere isn't clobbered).
pub fn save_ui_settings(ui: &sdroxide_types::UiSettings) -> Result<(), ConfigError> {
    let mut s = Settings::load();
    s.ui = *ui;
    s.save()
}

/// Load just the remote-access credentials.
///
/// Read fresh rather than cached: the server calls this once per connection, so
/// an edit to `config.toml` — by hand, or from the settings dialog of the GUI
/// running on the same machine — takes effect on the next sign-in instead of
/// waiting for the server to be restarted.
pub fn load_remote_access() -> sdroxide_types::RemoteAccess {
    Settings::load().remote_access
}

/// Persist the remote-access credentials, preserving every other setting
/// (read-modify-write, like [`save_ui_settings`]).
pub fn save_remote_access(access: &sdroxide_types::RemoteAccess) -> Result<(), ConfigError> {
    let mut s = Settings::load();
    s.remote_access = access.clone();
    s.save()
}

/// Load just the spoken-announcement preferences.
pub fn load_speech_settings() -> sdroxide_types::SpeechSettings {
    Settings::load().speech
}

/// Persist the spoken-announcement preferences, preserving every other setting
/// (read-modify-write, like [`save_ui_settings`]).
pub fn save_speech_settings(speech: &sdroxide_types::SpeechSettings) -> Result<(), ConfigError> {
    let mut s = Settings::load();
    s.speech = speech.clone();
    s.save()
}

/// A sign-in the operator asked this client to remember (`remote_login.json`).
///
/// A *client*-side file, like `input.json`: it is what this machine types into
/// somebody else's server, not what this machine demands of anyone. Written
/// only when the sign-in dialog's "remember" box is ticked, and holding the
/// password in the clear — same as every other credential sdroxide stores, and
/// noted as such in the manual and in the dialog itself.
pub fn load_remote_login() -> Option<sdroxide_types::RemoteAccess> {
    let login: sdroxide_types::RemoteAccess = load_json("remote_login.json");
    login.is_enforced().then_some(login)
}

pub fn save_remote_login(login: Option<&sdroxide_types::RemoteAccess>) -> Result<(), ConfigError> {
    match login {
        Some(l) => save_json("remote_login.json", l),
        // Forgetting has to remove the file, not write an empty one: an empty
        // record and a deleted one mean the same thing, and leaving a password
        // field behind that says `""` invites the belief that something was
        // scrubbed when the old file is simply still there.
        None => {
            let path = config_dir()?.join("remote_login.json");
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        }
    }
}

pub fn config_dir() -> Result<PathBuf, ConfigError> {
    directories::ProjectDirs::from("org", "sdroxide", "sdroxide")
        .map(|d| d.config_dir().to_path_buf())
        .ok_or(ConfigError::NoConfigDir)
}

/// Directory for a mode's received pictures (`~/.config/sdroxide/<kind>_rx`),
/// created on demand.
///
/// One store per mode rather than one for everything: an SSTV picture and a
/// weather chart are browsed for entirely different reasons, and a
/// fifteen-minute chart arriving every half hour would bury a session's SSTV.
pub fn image_rx_dir(kind: &str) -> Result<PathBuf, ConfigError> {
    // The caller's `kind` is a literal today, but it ends up in a path.
    let safe: String = kind.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let dir = config_dir()?.join(format!("{}_rx", if safe.is_empty() { "image" } else { &safe }));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Directory for received weather-fax charts, created on demand: the user's
/// pictures directory (`<Pictures>/sdroxide/wefax`), or the config directory
/// (`~/.config/sdroxide/wefax_rx`) when the platform exposes no pictures folder.
///
/// Charts go where pictures go, unlike every other store here, because that is
/// what they are for. A weather chart is printed, mailed, dropped into a
/// passage plan or opened next to a routing program — all of which happen
/// outside this program, in a file manager, and none of which anyone will do
/// from a hidden directory under `~/.config`.
pub fn wefax_rx_dir() -> Result<PathBuf, ConfigError> {
    let dir = match directories::UserDirs::new()
        .and_then(|u| u.picture_dir().map(std::path::Path::to_path_buf))
    {
        Some(pictures) => pictures.join("sdroxide").join("wefax"),
        None => config_dir()?.join("wefax_rx"),
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Where charts were kept before they moved to the pictures directory.
///
/// Read-only and never created: the gallery lists it alongside the current
/// store so an existing collection does not appear to have been lost. `None`
/// when it is the current store anyway, or when there is no config directory.
pub fn wefax_legacy_rx_dir() -> Option<PathBuf> {
    let old = config_dir().ok()?.join("wefax_rx");
    let current = wefax_rx_dir().ok()?;
    (old != current && old.is_dir()).then_some(old)
}

/// Directory for the operator's transmit-image slots
/// (`~/.config/sdroxide/sstv_tx`), created on demand.
pub fn sstv_tx_dir() -> Result<PathBuf, ConfigError> {
    let dir = config_dir()?.join("sstv_tx");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Directory for the voice keyer's recorded messages
/// (`~/.config/sdroxide/voice`), created on demand. One 48 kHz mono WAV per
/// slot, so a message can be edited or replaced with any audio editor.
pub fn voice_dir() -> Result<PathBuf, ConfigError> {
    let dir = config_dir()?.join("voice");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Directory for operator-supplied speech voices
/// (`~/.config/sdroxide/speech_voices`), created on demand. One Piper voice per
/// `.onnx` + `.onnx.json` pair.
///
/// Deliberately not [`voice_dir`], which is the voice *keyer*'s recordings —
/// two unrelated meanings of the word that would be a nasty surprise to share
/// a directory.
pub fn speech_voice_dir() -> Result<PathBuf, ConfigError> {
    let dir = config_dir()?.join("speech_voices");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Directory for cached solar imagery and space-weather JSON
/// (`~/.config/sdroxide/solar`), created on demand.
///
/// The 3D solar view loads this before its first network request, so the window
/// opens with the last-known data and stays useful with no connection at all.
pub fn solar_cache_dir() -> Result<PathBuf, ConfigError> {
    let dir = config_dir()?.join("solar");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Directory for audio recordings, created on demand: the user's music/audio
/// directory (`<Music>/sdroxide`), or the config directory
/// (`~/.config/sdroxide/recordings`) when the platform exposes no music folder.
pub fn recordings_dir() -> Result<PathBuf, ConfigError> {
    let dir = match directories::UserDirs::new()
        .and_then(|u| u.audio_dir().map(std::path::Path::to_path_buf))
    {
        Some(music) => music.join("sdroxide"),
        None => config_dir()?.join("recordings"),
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

impl Settings {
    /// Load settings; missing file or unreadable content falls back to
    /// defaults (with a warning), so startup never fails on config.
    pub fn load() -> Settings {
        let path = match config_dir() {
            Ok(d) => d.join("config.toml"),
            Err(e) => {
                warn!("no config dir: {e}; using default settings");
                return Settings::default();
            }
        };
        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(s) => s,
                Err(e) => {
                    warn!("failed to parse {}: {e}; using defaults", path.display());
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        let dir = config_dir()?;
        fs::create_dir_all(&dir)?;
        let text = toml::to_string_pretty(self)?;
        fs::write(dir.join("config.toml"), text)?;
        Ok(())
    }
}

/// Where the operator left the radio (`session.json`): the dial, the mode, the
/// selected antennas, and the audio/RF levels. Restored on the next start, so
/// the program comes back up where it was instead of on a fixed default
/// frequency and whichever port and level the driver happens to power up on.
///
/// Deliberately not part of `config.toml`. That file holds preferences the
/// operator sets once; this is written by the engine as the radio is used, and
/// the command line still wins over it (`--freq`, `--mode`, `--antenna`,
/// `--tx-antenna`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Dial frequency of VFO A, in Hz.
    pub freq_hz: f64,
    /// Mode of the main receiver.
    pub mode: sdroxide_types::Mode,
    /// RX antenna port, as the device names it ("LNAH", "TX/RX"). `None` on a
    /// front end that has no antenna to choose, and on every session written
    /// before this was remembered.
    pub antenna_rx: Option<String>,
    /// TX antenna port, likewise ("BAND1", "BAND2").
    pub antenna_tx: Option<String>,
    /// Main receiver's AF volume, 0.0..=1.0.
    pub volume: f32,
    /// Main receiver's manual (AGC-off) gain, in dB.
    pub rx_gain_db: f32,
    /// Main receiver's AGC mode.
    pub agc: sdroxide_types::AgcMode,
    /// TX drive, 0.0..=1.0 fraction of maximum.
    pub drive: f32,
    /// Drive used while tuning, 0.0..=1.0 fraction of maximum.
    pub tune_drive: f32,
    /// Mic gain, 0.0..=1.0.
    pub mic_gain: f32,
    /// Whether a recording mixes RX/TX down to one channel instead of putting
    /// RX left and TX right. A preference the operator sets once and expects to
    /// still hold next time, not something the engine moves on its own — but it
    /// rides here rather than in `config.toml` because the UI is the only thing
    /// that sets it, and the engine is what owns writing it back.
    pub recording_mono: bool,
}

impl Default for Session {
    fn default() -> Self {
        // The 20 m band-stack default — exactly where the program started every
        // time before it remembered anything.
        let (freq_hz, mode) = sdroxide_types::Band::M20.default_entry();
        // No antenna preference: whatever the driver selects on open stands,
        // which is what every start did before this was remembered.
        // Levels match `RadioState::default()` so an operator who has never
        // touched them comes up at the same drive and mic gain a fresh start
        // always used, rather than a dead mic.
        let radio = sdroxide_types::RadioState::default();
        Session {
            freq_hz,
            mode,
            antenna_rx: None,
            antenna_tx: None,
            volume: radio.rx[0].volume,
            rx_gain_db: radio.rx[0].manual_gain_db,
            agc: radio.rx[0].agc,
            drive: radio.tx.drive,
            tune_drive: radio.tx.tune_drive,
            mic_gain: radio.tx.mic_gain,
            recording_mono: radio.recording_mono,
        }
    }
}

impl Session {
    /// Whether this record can be restored.
    ///
    /// The frequency is handed straight to a front end as its centre, so a
    /// hand-edited or truncated file must not be able to open the receiver on
    /// 0 Hz or NaN — a far worse failure than a forgotten session.
    fn is_usable(&self) -> bool {
        self.freq_hz.is_finite() && self.freq_hz > 0.0
    }
}

/// The remembered dial and mode, or the defaults on a first run.
pub fn load_session() -> Session {
    let s: Session = load_json("session.json");
    if s.is_usable() { s } else { Session::default() }
}

pub fn save_session(session: &Session) -> Result<(), ConfigError> {
    save_json("session.json", session)
}

/// Band-stack registers: up to 3 remembered (freq, mode, filter) per band.
pub type BandStacks =
    std::collections::HashMap<sdroxide_types::Band, Vec<sdroxide_types::BandStackEntry>>;

fn load_json<T: serde::de::DeserializeOwned + Default>(file: &str) -> T {
    let Ok(dir) = config_dir() else { return T::default() };
    match fs::read_to_string(dir.join(file)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            warn!("failed to parse {file}: {e}; starting fresh");
            T::default()
        }),
        Err(_) => T::default(),
    }
}

fn save_json<T: serde::Serialize>(file: &str, value: &T) -> Result<(), ConfigError> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(value).expect("serialize");
    fs::write(dir.join(file), text)?;
    Ok(())
}

pub fn load_bandstacks() -> BandStacks {
    load_json("bandstacks.json")
}

pub fn save_bandstacks(stacks: &BandStacks) -> Result<(), ConfigError> {
    save_json("bandstacks.json", stacks)
}

pub fn load_memories() -> Vec<sdroxide_types::MemoryChannel> {
    load_json("memories.json")
}

pub fn save_memories(memories: &[sdroxide_types::MemoryChannel]) -> Result<(), ConfigError> {
    save_json("memories.json", &memories)
}

/// Radio backend config (SoapySDR vs CAT rig; serial + sound-card settings).
pub fn load_radio_config() -> sdroxide_types::RadioConfig {
    load_json("radio.json")
}

pub fn save_radio_config(cfg: &sdroxide_types::RadioConfig) -> Result<(), ConfigError> {
    save_json("radio.json", cfg)
}

/// FT8/FT4 operator config (own call, grid, message templates).
pub fn load_digi_config() -> sdroxide_types::DigiConfig {
    load_json("digi.json")
}

pub fn save_digi_config(cfg: &sdroxide_types::DigiConfig) -> Result<(), ConfigError> {
    save_json("digi.json", cfg)
}

/// Skimmer preferences (per-kind enable + squelch). The operator's choice, kept
/// separate from the live `RadioState.skimmer`: a narrowband (audio-mode)
/// source forces the skimmers off, and that must not overwrite what the
/// operator picked for a wideband one.
pub fn load_skimmer_config() -> sdroxide_types::SkimmerSettings {
    load_json("skimmer.json")
}

pub fn save_skimmer_config(cfg: &sdroxide_types::SkimmerSettings) -> Result<(), ConfigError> {
    save_json("skimmer.json", cfg)
}

/// Scanner settings: what to scan, how hard a signal has to be to stop it, and
/// which memories to pass over. Restored at startup so a scan set up once is
/// one keypress away afterwards.
pub fn load_scanner_config() -> sdroxide_types::ScannerConfig {
    load_json("scanner.json")
}

pub fn save_scanner_config(cfg: &sdroxide_types::ScannerConfig) -> Result<(), ConfigError> {
    save_json("scanner.json", cfg)
}

/// FSQ contacts (address book for directed FSQCALL messaging).
pub fn load_contacts() -> Vec<sdroxide_types::FsqContact> {
    load_json("contacts.json")
}

pub fn save_contacts(contacts: &[sdroxide_types::FsqContact]) -> Result<(), ConfigError> {
    save_json("contacts.json", &contacts)
}

/// Persistent logbook (digital + manual QSO entries).
pub fn load_qso_log() -> Vec<sdroxide_types::QsoRecord> {
    load_json("qso_log.json")
}

pub fn save_qso_log(log: &[sdroxide_types::QsoRecord]) -> Result<(), ConfigError> {
    save_json("qso_log.json", &log)
}

/// Network cockpit config (spot feeds, callsign lookup, uploads; credentials).
pub fn load_network_config() -> sdroxide_types::NetworkConfig {
    load_json("net.json")
}

pub fn save_network_config(cfg: &sdroxide_types::NetworkConfig) -> Result<(), ConfigError> {
    save_json("net.json", cfg)
}

/// Built-in TCI server config (the listener third-party TCI clients connect
/// to). Owned by the engine, like the network-cockpit config above.
pub fn load_tci_server_config() -> sdroxide_types::TciServerConfig {
    load_json("tciserver.json")
}

pub fn save_tci_server_config(cfg: &sdroxide_types::TciServerConfig) -> Result<(), ConfigError> {
    save_json("tciserver.json", cfg)
}

/// Built-in Hamlib rigctld server config (the listener "NET rigctl" clients
/// connect to). Owned by the engine, like the TCI server config above.
pub fn load_rigctld_config() -> sdroxide_types::RigctldConfig {
    load_json("rigctld.json")
}

pub fn save_rigctld_config(cfg: &sdroxide_types::RigctldConfig) -> Result<(), ConfigError> {
    save_json("rigctld.json", cfg)
}

/// WSJT-X UDP broadcast config (where decode/QSO datagrams are sent). Owned by
/// the engine, like the server configs above.
pub fn load_wsjtx_config() -> sdroxide_types::WsjtxConfig {
    load_json("wsjtx.json")
}

pub fn save_wsjtx_config(cfg: &sdroxide_types::WsjtxConfig) -> Result<(), ConfigError> {
    save_json("wsjtx.json", cfg)
}

/// Control-input bindings: keyboard chords, panadapter mouse behaviour and the
/// MIDI mapping. Unlike the configs above this one belongs to the *client*, not
/// the engine — it describes the hardware on the operator's desk, so a knob
/// keeps working when the UI drives a remote engine over `--connect`.
pub fn load_input_settings() -> sdroxide_types::InputSettings {
    load_json("input.json")
}

pub fn save_input_settings(cfg: &sdroxide_types::InputSettings) -> Result<(), ConfigError> {
    save_json("input.json", cfg)
}

/// SSTV per-slot transmit overlay messages (one entry per image slot). The
/// image pixels live as PNGs under [`sstv_tx_dir`]; this stores just the text
/// that is composited over each slot's picture.
pub fn load_sstv_messages() -> Vec<String> {
    load_json("sstv_messages.json")
}

pub fn save_sstv_messages(messages: &[String]) -> Result<(), ConfigError> {
    save_json("sstv_messages.json", &messages)
}

/// The operator's satellite additions: element sets pasted in by hand, and
/// frequency entries that override or extend the built-in table.
///
/// An engine-side file like `net.json`, despite describing something only the
/// UI draws. The subscribed listings are fetched over HTTPS and cached on disk,
/// and in server mode that machine's tracker is also what feeds the browser's
/// 3D view — so a browser client, which has neither, has to be able to
/// configure the one that does.
pub fn load_sat_config() -> sdroxide_types::SatConfig {
    let mut cfg: sdroxide_types::SatConfig = load_json("satellites.json");
    // The amateur satellites and the ISS used to be fetched unconditionally.
    // They are subscriptions now, so a config that predates them — or a fresh
    // install with no file at all — has to be given them once, or the sky comes
    // up empty. Written back immediately so the seeding happens exactly once
    // and unsubscribing sticks.
    if cfg.seed_defaults() {
        if let Err(e) = save_sat_config(&cfg) {
            warn!("could not write the seeded satellite subscriptions: {e}");
        }
    }
    cfg
}

pub fn save_sat_config(cfg: &sdroxide_types::SatConfig) -> Result<(), ConfigError> {
    save_json("satellites.json", cfg)
}

// ── Broadcast station schedules ──────────────────────────────────────────────
//
// Three layers, in the order they win:
//
//   1. the schedule EiBi publishes for the current season, downloaded and cached
//      under `broadcast/`, or the copy compiled into the binary until one
//      arrives;
//   2. the hand-kept longwave and standard-time entries, merged in by
//      `sdroxide_types::broadcast::merge` because EiBi covers neither;
//   3. `broadcast_stations.json`, the operator's own additions and corrections,
//      which is never written by sdroxide.
//
// This is the arrangement `sdroxide-solar`'s satellite frequencies already use —
// a built-in table plus user overrides — rather than seeding a copy of everything
// into the config directory, which cannot survive a schedule that is reissued
// twice a year.

/// The operator's own broadcast stations (`broadcast_stations.json`).
pub const BROADCAST_STATIONS_FILE: &str = "broadcast_stations.json";
/// Where the downloaded season schedules are cached.
const BROADCAST_CACHE_DIR: &str = "broadcast";
/// EiBi's schedule files, published free for exactly this use.
///
/// Plain HTTP because the site's certificate is expired; nothing is trusted on
/// the strength of the transport, the payload is parsed into typed rows and
/// rejected unless it looks like a schedule.
const EIBI_SKED_URL: &str = "http://www.eibispace.de/dx/sked-{season}.csv";

/// Where the operator's own station list lives, for showing in the settings panel.
pub fn broadcast_stations_path() -> Result<PathBuf, ConfigError> {
    Ok(config_dir()?.join(BROADCAST_STATIONS_FILE))
}

/// Where a season's downloaded schedule is cached.
pub fn broadcast_cache_path(season: &str) -> Result<PathBuf, ConfigError> {
    // The season is used in a filename, so it must not be able to escape the
    // directory even though it is computed rather than typed in.
    let season: String = season.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
    Ok(config_dir()?.join(BROADCAST_CACHE_DIR).join(format!("sked-{season}.csv")))
}

/// The operator's own broadcast entries. Absent by default — this file holds
/// additions and corrections, not a copy of the schedule.
pub fn load_broadcast_overrides() -> Vec<sdroxide_types::BroadcastStation> {
    let Ok(path) = broadcast_stations_path() else { return Vec::new() };
    retire_seeded_broadcast_list(&path);
    let Ok(text) = fs::read_to_string(&path) else { return Vec::new() };
    match serde_json::from_str::<sdroxide_types::BroadcastStations>(&text) {
        Ok(f) => f.stations,
        Err(e) => {
            warn!("failed to parse {BROADCAST_STATIONS_FILE}: {e}; ignoring it");
            Vec::new()
        }
    }
}

/// Move aside a `broadcast_stations.json` that is a copy of a shipped schedule.
///
/// Earlier versions seeded the whole table into this file. Now that the schedule
/// is downloaded and this file holds only the operator's own entries, such a copy
/// would lay a stale season back over a fresh one — hundreds of duplicated,
/// out-of-date transmissions.
///
/// Generated copies are recognised by the `source` or `updated` keys, which only
/// sdroxide's own table generators ever set, and nothing hand-written would
/// carry them. So this never touches a file an operator actually wrote, and it
/// is kept as `.bak` either way.
fn retire_seeded_broadcast_list(path: &std::path::Path) {
    let Ok(text) = fs::read_to_string(path) else { return };
    let Ok(file) = serde_json::from_str::<sdroxide_types::BroadcastStations>(&text) else {
        return;
    };
    if file.source.is_empty() && file.updated.is_empty() {
        return;
    }
    let backup = path.with_extension("json.bak");
    match fs::rename(path, &backup) {
        Ok(()) => info!(
            "{BROADCAST_STATIONS_FILE} was a copy of a bundled schedule; kept as \
             {} and replaced by the downloaded one",
            backup.display()
        ),
        Err(e) => warn!("could not retire the seeded {BROADCAST_STATIONS_FILE}: {e}"),
    }
}

// There is deliberately no writer for `broadcast_stations.json`. It is the one
// file here that belongs entirely to the operator, and "sdroxide never writes
// it" is a contract the manual states — shipping a save function would be an
// invitation to break it, and would give `retire_seeded_broadcast_list` a case
// it cannot distinguish from a stale seeded copy.

/// The full station list: the cached (or compiled-in) schedule, plus the
/// hand-kept longwave entries, with the operator's own entries laid over the top.
pub fn load_broadcast_stations() -> Vec<sdroxide_types::BroadcastStation> {
    let schedule = match cached_schedule() {
        Some(stations) => stations,
        None => sdroxide_types::broadcast::builtin().to_vec(),
    };
    apply_broadcast_overrides(schedule, load_broadcast_overrides())
}

/// Lay the operator's entries over a schedule: one with the same name and
/// frequency replaces the scheduled row, anything else is added.
fn apply_broadcast_overrides(
    mut schedule: Vec<sdroxide_types::BroadcastStation>,
    overrides: Vec<sdroxide_types::BroadcastStation>,
) -> Vec<sdroxide_types::BroadcastStation> {
    for own in overrides {
        let same = |s: &sdroxide_types::BroadcastStation| {
            s.name == own.name && (s.freq_khz - own.freq_khz).abs() < 0.001
        };
        schedule.retain(|s| !same(s));
        schedule.push(own);
    }
    schedule.sort_by(|a, b| {
        a.freq_khz
            .total_cmp(&b.freq_khz)
            .then_with(|| a.start_utc.cmp(&b.start_utc))
            .then_with(|| a.name.cmp(&b.name))
    });
    schedule
}

/// The cached schedule for the season we are in, if it has been downloaded.
fn cached_schedule() -> Option<Vec<sdroxide_types::BroadcastStation>> {
    let season = sdroxide_types::broadcast::season_file(now_unix());
    let path = broadcast_cache_path(&season).ok()?;
    let bytes = fs::read(&path).ok()?;
    let text = sdroxide_types::broadcast::decode_latin1(&bytes);
    let stations = sdroxide_types::broadcast::parse_schedule(&text);
    if stations.len() < MIN_SCHEDULE_ROWS {
        warn!("cached schedule {season} has only {} entries; ignoring it", stations.len());
        return None;
    }
    Some(sdroxide_types::broadcast::merge(stations))
}

/// A schedule with fewer transmissions than this is not a schedule — a captive
/// portal's login page, a truncated download, a season file that has not been
/// published yet. Whatever it is, the compiled-in copy is better.
const MIN_SCHEDULE_ROWS: usize = 500;

/// Whether the current season's schedule still needs downloading.
///
/// True on a first run, and again after each changeover, because the cache is
/// keyed by season: October's file simply is not March's.
pub fn broadcast_schedule_due() -> bool {
    let season = sdroxide_types::broadcast::season_file(now_unix());
    match broadcast_cache_path(&season) {
        Ok(p) => !p.exists(),
        Err(_) => false,
    }
}

/// The season sdroxide is currently using, and whether it came from the network.
pub fn broadcast_schedule_status() -> (String, bool) {
    let season = sdroxide_types::broadcast::season_file(now_unix());
    let cached = broadcast_cache_path(&season).map(|p| p.exists()).unwrap_or(false);
    (season, cached)
}

/// Download the current season's schedule and cache it.
///
/// Blocking, so callers put it on a worker thread. Returns the merged station
/// list on success. The download is written to the cache only after it parses
/// into a plausible schedule, so a failure leaves the previous file in place
/// rather than replacing it with a captive portal's login page.
pub fn fetch_broadcast_schedule() -> Result<Vec<sdroxide_types::BroadcastStation>, String> {
    let season = sdroxide_types::broadcast::season_file(now_unix());
    let url = EIBI_SKED_URL.replace("{season}", &season);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .timeout_global(Some(std::time::Duration::from_secs(120)))
        .user_agent(concat!("sdroxide/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();

    let mut resp = agent.get(&url).call().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())?;

    let text = sdroxide_types::broadcast::decode_latin1(&bytes);
    let stations = sdroxide_types::broadcast::parse_schedule(&text);
    if stations.len() < MIN_SCHEDULE_ROWS {
        return Err(format!(
            "{url} yielded {} transmissions, which is not a schedule",
            stations.len()
        ));
    }

    let path = broadcast_cache_path(&season).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    // Last season's file is only dead weight once this one has landed.
    prune_broadcast_cache(&season);
    info!("downloaded the {season} broadcast schedule: {} transmissions", stations.len());

    Ok(apply_broadcast_overrides(
        sdroxide_types::broadcast::merge(stations),
        load_broadcast_overrides(),
    ))
}

/// Drop cached schedules for seasons other than `keep`.
fn prune_broadcast_cache(keep: &str) {
    let Ok(path) = broadcast_cache_path(keep) else { return };
    let Some(dir) = path.parent() else { return };
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if entry.path() != path && entry.path().extension().is_some_and(|e| e == "csv") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Forget the cached schedule so the next check downloads it again.
pub fn clear_broadcast_cache() -> Result<(), ConfigError> {
    let season = sdroxide_types::broadcast::season_file(now_unix());
    let path = broadcast_cache_path(&season)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Seconds since the Unix epoch.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Voice-keyer slot labels (one entry per slot). The recordings themselves are
/// WAV files under [`voice_dir`]; this stores only what each slot is called.
pub fn load_voice_names() -> Vec<String> {
    load_json("voice_names.json")
}

pub fn save_voice_names(names: &[String]) -> Result<(), ConfigError> {
    save_json("voice_names.json", &names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digi_config_roundtrip_via_json() {
        let cfg = sdroxide_types::DigiConfig {
            my_call: "AB1CD".into(),
            my_grid: "FN42".into(),
            ..Default::default()
        };
        let text = serde_json::to_string_pretty(&cfg).unwrap();
        let back: sdroxide_types::DigiConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back.my_call, "AB1CD");
        assert_eq!(back.my_grid, "FN42");
        assert_eq!(back, cfg);
    }

    #[test]
    fn skimmer_config_roundtrip_via_json() {
        use sdroxide_types::{SkimmerKind, SkimmerSettings};
        let mut cfg = SkimmerSettings::default();
        cfg.set_enabled(SkimmerKind::Psk, false);
        cfg.set_squelch_db(SkimmerKind::Cw, 12);
        let back: SkimmerSettings =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back, cfg);
        assert!(!back.enabled(SkimmerKind::Psk));
        assert_eq!(back.squelch_db(SkimmerKind::Cw), 12);
    }

    #[test]
    fn skimmer_config_fills_missing_fields() {
        // A file written before one of the fields existed still loads.
        let cfg: sdroxide_types::SkimmerSettings =
            serde_json::from_str(r#"{"enabled":[false,false,true]}"#).unwrap();
        assert_eq!(cfg.enabled, [false, false, true]);
        assert_eq!(cfg.squelch_db, sdroxide_types::SkimmerSettings::default().squelch_db);
    }

    #[test]
    fn bandstacks_roundtrip_via_json() {
        use sdroxide_types::{Band, BandStackEntry, Mode};
        let mut stacks = BandStacks::default();
        stacks.insert(
            Band::M40,
            vec![BandStackEntry {
                freq_hz: 7_100_000.0,
                mode: Mode::Lsb,
                filter_lo: -2850.0,
                filter_hi: -150.0,
            }],
        );
        let text = serde_json::to_string(&stacks).unwrap();
        let back: BandStacks = serde_json::from_str(&text).unwrap();
        assert_eq!(back, stacks);
    }

    #[test]
    fn session_roundtrips_via_json() {
        let s = Session {
            freq_hz: 7_074_000.0,
            mode: sdroxide_types::Mode::Ft8,
            antenna_rx: Some("LNAW".into()),
            antenna_tx: Some("BAND2".into()),
            volume: 0.8,
            rx_gain_db: 35.0,
            agc: sdroxide_types::AgcMode::Fast,
            drive: 0.4,
            tune_drive: 0.2,
            mic_gain: 0.6,
            recording_mono: true,
        };
        let back: Session = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    /// Every `session.json` written before the levels were remembered has to
    /// keep restoring its dial and mode, and fall back to the levels a fresh
    /// start has always come up at.
    #[test]
    fn a_session_written_before_levels_still_loads() {
        let old: Session =
            serde_json::from_str(r#"{"freq_hz":7074000.0,"mode":"Ft8"}"#).expect("parses");
        let radio = sdroxide_types::RadioState::default();
        assert_eq!(old.volume, radio.rx[0].volume);
        assert_eq!(old.rx_gain_db, radio.rx[0].manual_gain_db);
        assert_eq!(old.agc, radio.rx[0].agc);
        assert_eq!(old.drive, radio.tx.drive);
        assert_eq!(old.tune_drive, radio.tx.tune_drive);
        assert_eq!(old.mic_gain, radio.tx.mic_gain);
        assert_eq!(old.recording_mono, radio.recording_mono);
    }

    /// The first run, and every run before this file existed, has to land where
    /// the program has always started.
    #[test]
    fn the_session_default_is_where_the_program_always_started() {
        let s = Session::default();
        assert_eq!(s.freq_hz, 14_200_000.0);
        assert_eq!(s.mode, sdroxide_types::Mode::Usb);
        assert_eq!(s.antenna_rx, None, "no port preference until one is expressed");
        assert_eq!(s.antenna_tx, None);
        // A file missing a key still loads; only what it names is used.
        let partial: Session = serde_json::from_str(r#"{"freq_hz":3573000.0}"#).unwrap();
        assert_eq!(partial.freq_hz, 3_573_000.0);
        assert_eq!(partial.mode, s.mode);
    }

    /// Every `session.json` written before the antennas were remembered has to
    /// keep restoring its dial and mode, and simply express no preference.
    #[test]
    fn a_session_written_before_antennas_still_loads() {
        let old: Session =
            serde_json::from_str(r#"{"freq_hz":7074000.0,"mode":"Ft8"}"#).expect("parses");
        assert_eq!(old.freq_hz, 7_074_000.0);
        assert_eq!(old.mode, sdroxide_types::Mode::Ft8);
        assert_eq!(old.antenna_rx, None);
        assert_eq!(old.antenna_tx, None);
    }

    /// This frequency is handed straight to a front end as its centre, so a
    /// nonsense one has to be dropped rather than passed on: a receiver opening
    /// at 0 Hz or NaN is a much worse failure than a forgotten session.
    #[test]
    fn a_session_frequency_that_is_not_one_is_refused() {
        for bad in [0.0, -14_200_000.0, f64::NAN, f64::INFINITY] {
            let s = Session { freq_hz: bad, ..Session::default() };
            assert!(!s.is_usable(), "{bad} should not be accepted as a dial frequency");
        }
        assert!(Session { freq_hz: 1_840_000.0, ..Session::default() }.is_usable());
        assert!(Session::default().is_usable(), "the fallback must itself be restorable");
    }

    #[test]
    fn default_settings_roundtrip_via_toml() {
        let s = Settings::default();
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let s: Settings = toml::from_str("sample_rate = 2400000.0").unwrap();
        assert_eq!(s.sample_rate, 2_400_000.0);
        assert_eq!(s.server_port, Settings::default().server_port);
    }

    /// The credentials survive a write and a read, and — the part that is easy
    /// to get wrong — every plain value above them is still a plain value
    /// afterwards. TOML puts tables last, so a table declared before
    /// `tx_ham_only` would quietly adopt it into itself and the next start
    /// would come up with the band-edge lockout in a different place.
    #[test]
    fn remote_access_survives_a_write_without_swallowing_the_settings_above_it() {
        let s = Settings {
            remote_access: sdroxide_types::RemoteAccess {
                username: "oe1test".into(),
                password: "hunter2".into(),
            },
            tx_ham_only: false,
            server_port: 4951,
            ..Settings::default()
        };
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.remote_access.password, "hunter2");
        assert!(!back.tx_ham_only, "a value below the table must not become part of it");
        assert_eq!(back.server_port, 4951);
    }

    /// The same hazard, with three tables rather than two: `[speech]` carries
    /// sub-tables of its own, and a scalar of *its* declared after them would
    /// be swallowed just as surely.
    #[test]
    fn speech_settings_survive_a_write_without_swallowing_anything() {
        let mut speech = sdroxide_types::SpeechSettings {
            enabled: true,
            voice: "en_US-hfc_female-medium".into(),
            rate: 1.4,
            verbosity: sdroxide_types::Verbosity::Full,
            ..Default::default()
        };
        speech.cat.filters = true;
        speech.text.cw = true;
        speech.tune.period_s = 3.0;

        let s = Settings { speech: speech.clone(), tx_ham_only: false, ..Settings::default() };
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.speech, speech);
        assert!(back.speech.enabled);
        assert!(back.speech.cat.filters);
        assert!(!back.tx_ham_only, "a value below a table must not become part of it");
        assert_eq!(back.ui, s.ui, "the table above must survive too");
    }

    /// A `config.toml` written before this feature existed comes up silent,
    /// which is what that operator has always had.
    #[test]
    fn a_config_without_a_speech_table_stays_quiet() {
        let s: Settings = toml::from_str("server_port = 4950").unwrap();
        assert!(!s.speech.enabled);
    }

    /// A `config.toml` written before this feature existed leaves the server
    /// open, which is what that operator has always had.
    #[test]
    fn a_config_without_credentials_leaves_the_server_open() {
        let s: Settings = toml::from_str("server_port = 4950").unwrap();
        assert!(!s.remote_access.is_enforced());
    }

    #[test]
    fn network_config_loads_without_the_freedv_section() {
        // A net.json written before FreeDV Reporter existed.
        let c: sdroxide_types::NetworkConfig =
            serde_json::from_str(r#"{"spot_max_age_secs":600}"#).unwrap();
        assert_eq!(c.spot_max_age_secs, 600);
        assert_eq!(c.freedv_reporter, sdroxide_types::FreeDvReporterConfig::default());
    }

    #[test]
    fn network_config_ignores_the_retired_operator_identity_keys() {
        // net.json used to hold its own copy of the operator callsign and grid,
        // and the reporter section briefly held a third. All of that now comes
        // from the digi config, so a file still carrying them must load and
        // ignore them rather than fail.
        let c: sdroxide_types::NetworkConfig = serde_json::from_str(
            r#"{"my_call":"AB1CD","my_grid":"FN42","spot_max_age_secs":600,
                "cluster":{"enabled":true,"host":"cluster.example","port":7373},
                "freedv_reporter":{"enabled":true,"callsign":"OLD","grid":"AA00"}}"#,
        )
        .unwrap();
        assert_eq!(c.spot_max_age_secs, 600, "the rest of the file still applies");
        assert!(c.cluster.enabled);
        assert!(c.freedv_reporter.enabled);
    }

    /// A scratch directory of our own, so the station-list tests never touch the
    /// operator's real config. No `tempfile` dependency for a handful of tests.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sdroxide-bc-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir.join(BROADCAST_STATIONS_FILE)
    }

    fn station(name: &str, khz: f64) -> sdroxide_types::BroadcastStation {
        sdroxide_types::BroadcastStation {
            name: name.into(),
            freq_khz: khz,
            site: String::new(),
            country: String::new(),
            lat: None,
            lon: None,
            power_kw: None,
            lang: String::new(),
            target: String::new(),
            mode: None,
            start_utc: None,
            end_utc: None,
            days: String::new(),
            season: None,
        }
    }

    #[test]
    fn an_override_replaces_the_scheduled_row_it_names() {
        let schedule = vec![station("BBC", 15400.0), station("Voice of Greece", 9420.0)];
        let mine = vec![
            // Same name and frequency: a correction, so it wins.
            sdroxide_types::BroadcastStation {
                site: "Woofferton".into(),
                ..station("BBC", 15400.0)
            },
            // Not in the schedule: an addition.
            station("My Local Pirate", 6295.0),
        ];
        let merged = apply_broadcast_overrides(schedule, mine);
        assert_eq!(merged.len(), 3, "the correction replaced rather than duplicated");
        let bbc: Vec<_> = merged.iter().filter(|s| s.name == "BBC").collect();
        assert_eq!(bbc.len(), 1);
        assert_eq!(bbc[0].site, "Woofferton");
        assert!(merged.iter().any(|s| s.name == "My Local Pirate"));
        // Frequency order is what the spot list expects.
        assert!(merged.windows(2).all(|w| w[0].freq_khz <= w[1].freq_khz));
    }

    #[test]
    fn an_override_on_a_different_frequency_is_an_addition() {
        // Same station, another channel — not a correction of the first.
        let merged =
            apply_broadcast_overrides(vec![station("BBC", 15400.0)], vec![station("BBC", 12095.0)]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn a_seeded_copy_of_a_shipped_schedule_is_retired_not_used() {
        let path = scratch("retire");
        // What an older sdroxide wrote: a generated table, marked with `source`.
        fs::write(
            &path,
            r#"{"version":2,"source":"EiBi A26","stations":[
                 {"name":"Stale Station","freq_khz":6070}]}"#,
        )
        .unwrap();
        retire_seeded_broadcast_list(&path);
        assert!(!path.exists(), "the seeded copy should have been moved aside");
        let backup = path.with_extension("json.bak");
        assert!(backup.exists(), "and kept as .bak, not deleted");
        assert!(fs::read_to_string(&backup).unwrap().contains("Stale Station"));
    }

    #[test]
    fn an_older_seeded_copy_is_recognised_by_its_datestamp() {
        // The first version of this feature seeded a table with `updated` but no
        // `source`. It is still a copy of a shipped list and must not be laid
        // back over a downloaded schedule.
        let path = scratch("retire-dated");
        fs::write(
            &path,
            r#"{"version":1,"updated":"2026-07-30","note":"bundled",
                "stations":[{"name":"Stale","freq_khz":6070}]}"#,
        )
        .unwrap();
        retire_seeded_broadcast_list(&path);
        assert!(!path.exists());
        assert!(path.with_extension("json.bak").exists());
    }

    #[test]
    fn what_the_overrides_writer_produces_is_never_retired() {
        // A file in the shape the manual documents has to survive the next
        // start, or an operator's list would vanish exactly once.
        let path = scratch("roundtrip");
        let file = sdroxide_types::BroadcastStations {
            version: 1,
            updated: String::new(),
            source: String::new(),
            note: "mine".into(),
            stations: vec![station("My Local Pirate", 6295.0)],
        };
        fs::write(&path, serde_json::to_string_pretty(&file).unwrap()).unwrap();
        retire_seeded_broadcast_list(&path);
        assert!(path.exists(), "the operator's own file must survive");
    }

    #[test]
    fn a_hand_written_list_is_left_alone() {
        let path = scratch("keep");
        // No `source` key, so it is the operator's own and must survive.
        let mine = r#"{"version":1,"stations":[{"name":"My Local Pirate","freq_khz":6295}]}"#;
        fs::write(&path, mine).unwrap();
        retire_seeded_broadcast_list(&path);
        assert!(path.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), mine);
    }

    #[test]
    fn the_cache_path_is_named_for_the_season_and_cannot_escape_it() {
        let a = broadcast_cache_path("a26").unwrap();
        let b = broadcast_cache_path("b26").unwrap();
        assert_ne!(a, b, "a season change has to miss the previous cache");
        assert!(a.to_string_lossy().ends_with("sked-a26.csv"));
        // The season is interpolated into a filename, so nothing in it may walk
        // out of the cache directory even though it is computed, not typed.
        let nasty = broadcast_cache_path("../../etc/passwd").unwrap();
        assert_eq!(nasty.parent(), a.parent());
        assert!(!nasty.to_string_lossy().contains(".."));
    }

    #[test]
    fn a_short_download_is_not_a_schedule() {
        // The guard that stops a captive portal's login page replacing the real
        // list. One row parses fine; it just is not a season's worth.
        let csv = "kHz;Time;Days;ITU;Station;Lng;Target;Remarks;P;Start;Stop;\n\
                   9420;0000-2400;;GRC;Voice of Greece;G;Eu;a;1;;\n";
        let parsed = sdroxide_types::broadcast::parse_schedule(csv);
        assert_eq!(parsed.len(), 1);
        assert!(parsed.len() < MIN_SCHEDULE_ROWS);
        // Whereas the compiled-in fallback comfortably clears the bar.
        assert!(sdroxide_types::broadcast::builtin().len() > MIN_SCHEDULE_ROWS);
    }
}
