//! Persisted radio-backend configuration (`radio.json`): choose between a
//! SoapySDR device and a CAT-controlled rig whose audio arrives over a USB
//! sound card. Serde-only — no I/O, safe in the wasm client (the settings UI
//! is shared, even though the CAT machinery is native-only).

use serde::{Deserialize, Serialize};

/// Which radio backend to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Backend {
    /// Legacy "SoapySDR if present, else CAT" auto-detect. No longer offered in
    /// the UI, but kept so older `radio.json` files still deserialize.
    Auto,
    #[default]
    Soapy,
    Cat,
    /// OpenHPSDR ethernet SDR (Protocol 2), discovered/reached over the LAN.
    Hpsdr,
    /// TCI (Transceiver Control Interface) over WebSocket — ExpertSDR3, Thetis, …
    Tci,
    /// RTL2832U dongle driven directly over USB by the native driver — no
    /// SoapySDR, no libusb, nothing to install.
    RtlSdr,
    /// RX-888 Mk2 direct-sampling HF receiver, driven over USB by the native
    /// driver. Uploads its own firmware, so nothing needs installing.
    Rx888,
    /// SmartSDR (FlexRadio FLEX-6000 / FLEX-8000) over the LAN. Receive is a DAX
    /// IQ stream, transmit is DAX audio the radio modulates.
    ///
    /// Appended last on purpose: this enum is serde-serialised into `radio.json`
    /// by variant name, but `ALL` fixes the order the UI offers.
    SmartSdr,
    /// ADALM-Pluto (AD9361/AD9363) over the IIOD protocol — reached over the
    /// network, which the USB cable provides as an Ethernet gadget. Appended
    /// last, for the same reason as `SmartSdr` above.
    Pluto,
    /// SDRplay RSP family (RSP1/1A/1B/2/duo/dx), driven through the vendor's
    /// `sdrplay_api` service — the one RSP protocol there is; no open USB
    /// protocol exists for anything after the original RSP1. Appended last,
    /// for the same reason as `SmartSdr` above.
    SdrPlay,
    /// No interface chosen yet. The seeded state of a freshly created radio
    /// tab: it must open *nothing* until the operator picks a device, because
    /// the defaults above would grab the first device found — which is
    /// whatever the station's first radio is already running. Appended last,
    /// for the same reason as `SmartSdr` above; not offered in the picker
    /// (`ALL`), only ever written by the multi-radio seeding.
    None,
}

impl Backend {
    pub const ALL: [Backend; 10] = [
        Backend::Auto,
        Backend::Soapy,
        Backend::Cat,
        Backend::Hpsdr,
        Backend::Tci,
        Backend::SmartSdr,
        Backend::Pluto,
        Backend::RtlSdr,
        Backend::Rx888,
        Backend::SdrPlay,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Backend::Auto => "Auto-detect (SoapySDR / CAT)",
            Backend::Soapy => "SoapySDR",
            Backend::Cat => "CAT / Audio",
            Backend::Hpsdr => "HPSDR (network)",
            Backend::Tci => "TCI (network)",
            Backend::SmartSdr => "SmartSDR / FlexRadio (network)",
            Backend::Pluto => "PlutoSDR (network)",
            Backend::RtlSdr => "RTL-SDR (USB)",
            Backend::Rx888 => "RX-888 (USB)",
            Backend::SdrPlay => "SDRplay RSP (USB)",
            Backend::None => "Not configured",
        }
    }
}

/// One device from a SoapySDR enumeration. Wasm-safe so the list can cross the
/// `RadioController` trait to the settings UI, like [`SdrPlayDevice`] — the
/// SoapySDR types themselves live behind the native `soapy` feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoapyDeviceInfo {
    /// The driver key, as SoapySDR spells it. Case is *not* dependable: an
    /// enumeration reports `audio` where the opened device's `driver_key()`
    /// says `Audio`, so every comparison here folds case.
    pub driver: String,
    /// The human label the module publishes ("Audio (Audio)", "SDRplay
    /// Dev0 RSP1A 2405001234").
    pub label: String,
    /// The full args string that opens exactly this device.
    pub args: String,
}

impl SoapyDeviceInfo {
    /// SoapySDR modules that are not receivers at all.
    ///
    /// `audio` is SoapyAudio, which presents any sound card as an SDR: it
    /// accepts every tuning request, ignores them all, and returns the sound
    /// card's input. On a bundle install (PothosSDR ships every module) it
    /// enumerates ahead of the real hardware, so "the first device found" can
    /// silently be the machine's line input — a spectrum that looks like a
    /// receiver with a dead antenna. `null` is SoapySDR's own test stub.
    ///
    /// These are never what an operator means by "my SDR", so they are only
    /// ever opened when named explicitly.
    pub fn driver_is_pseudo(driver: &str) -> bool {
        matches!(driver.trim().to_ascii_lowercase().as_str(), "audio" | "null")
    }

    /// The native sdroxide interface that drives this hardware directly, for
    /// drivers that have one. The native backends carry the model-specific
    /// controls a generic SoapySDR device cannot express — per-band LNA state
    /// and notches on an RSP, the bias tee and direct sampling on an RTL-SDR —
    /// so an operator reaching them through SoapySDR is losing most of the
    /// radio.
    pub fn native_backend_for(driver: &str) -> Option<Backend> {
        match driver.trim().to_ascii_lowercase().as_str() {
            "sdrplay" => Some(Backend::SdrPlay),
            "rtlsdr" => Some(Backend::RtlSdr),
            "plutosdr" => Some(Backend::Pluto),
            _ => None,
        }
    }

    pub fn is_pseudo(&self) -> bool {
        Self::driver_is_pseudo(&self.driver)
    }

    pub fn native_backend(&self) -> Option<Backend> {
        Self::native_backend_for(&self.driver)
    }

    /// One-line label for a device list.
    pub fn label(&self) -> String {
        format!("{}  (driver {})", self.label, self.driver)
    }

    /// Operator-facing warning for a device that is not a radio, `None` for
    /// real hardware. One composer so the running-source notice, the settings
    /// list and `--probe` all say the same thing.
    pub fn pseudo_warning(driver: &str, label: &str) -> Option<String> {
        if !Self::driver_is_pseudo(driver) {
            return None;
        }
        Some(format!(
            "SoapySDR opened \"{label}\" (driver {driver}) — a sound card, not a radio. \
             It ignores the dial, so what you see is the sound card's input, not the \
             band. Pick a real device with --device / device_args, or choose a native \
             interface in Settings → Radio."
        ))
    }
}

/// CAT protocol family. Only `Xiegu` is hardware-verified so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CatFamily {
    #[default]
    Xiegu,
    Icom,
    Yaesu,
}

impl CatFamily {
    pub const ALL: [CatFamily; 3] = [CatFamily::Xiegu, CatFamily::Icom, CatFamily::Yaesu];
    pub fn label(self) -> &'static str {
        match self {
            CatFamily::Xiegu => "Xiegu",
            CatFamily::Icom => "Icom",
            CatFamily::Yaesu => "Yaesu",
        }
    }
}

/// How the radio's audio is carried over the sound card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SoundFormat {
    /// Stereo L=I, R=Q complex baseband → normal wideband engine path.
    Iq,
    /// Mono already-demodulated audio → audio-band panadapter (engine bypass).
    #[default]
    DemodAudio,
}

impl SoundFormat {
    pub const ALL: [SoundFormat; 2] = [SoundFormat::DemodAudio, SoundFormat::Iq];
    pub fn label(self) -> &'static str {
        match self {
            SoundFormat::Iq => "IQ (stereo)",
            SoundFormat::DemodAudio => "Demod audio",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Parity {
    #[default]
    None,
    Even,
    Odd,
}

impl Parity {
    pub const ALL: [Parity; 3] = [Parity::None, Parity::Even, Parity::Odd];
    pub fn label(self) -> &'static str {
        match self {
            Parity::None => "None",
            Parity::Even => "Even",
            Parity::Odd => "Odd",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StopBits {
    #[default]
    One,
    Two,
}

impl StopBits {
    pub const ALL: [StopBits; 2] = [StopBits::One, StopBits::Two];
    pub fn label(self) -> &'static str {
        match self {
            StopBits::One => "1",
            StopBits::Two => "2",
        }
    }
}

/// A serial control line forced to a fixed level while the port is open (some
/// rigs need DTR/RTS held high to enable CAT). `None` = leave as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LineState {
    #[default]
    None,
    High,
    Low,
}

impl LineState {
    pub const ALL: [LineState; 3] = [LineState::None, LineState::High, LineState::Low];
    pub fn label(self) -> &'static str {
        match self {
            LineState::None => "None",
            LineState::High => "High",
            LineState::Low => "Low",
        }
    }
}

/// How to key the transmitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PttMethod {
    /// Rig keys itself from TX audio; software just routes audio.
    Vox,
    Dtr,
    Rts,
    /// A CAT command keys the rig.
    #[default]
    Cat,
}

impl PttMethod {
    pub const ALL: [PttMethod; 4] =
        [PttMethod::Cat, PttMethod::Dtr, PttMethod::Rts, PttMethod::Vox];
    pub fn label(self) -> &'static str {
        match self {
            PttMethod::Vox => "VOX",
            PttMethod::Dtr => "DTR",
            PttMethod::Rts => "RTS",
            PttMethod::Cat => "CAT",
        }
    }
}

/// Who drives the rig's mode for ordinary modes (USB/LSB/CW/AM/FM/DIGU/DIGL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ModeControl {
    /// The app commands the rig's mode over CAT to match the selected mode.
    #[default]
    Cat,
    /// The operator sets the mode on the radio; the app just follows it.
    Radio,
}

impl ModeControl {
    pub const ALL: [ModeControl; 2] = [ModeControl::Cat, ModeControl::Radio];
    pub fn label(self) -> &'static str {
        match self {
            ModeControl::Cat => "CAT",
            ModeControl::Radio => "Radio controlled",
        }
    }
}

/// What mode the rig should be in for the FT8/FT4 digital engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DigiMode {
    /// Force the rig to USB.
    #[default]
    Usb,
    /// Force the rig to its DATA/PKT (USB-D) mode.
    Data,
    /// Leave the rig's mode as the operator set it.
    Radio,
}

impl DigiMode {
    pub const ALL: [DigiMode; 3] = [DigiMode::Usb, DigiMode::Data, DigiMode::Radio];
    pub fn label(self) -> &'static str {
        match self {
            DigiMode::Usb => "USB",
            DigiMode::Data => "DIGI",
            DigiMode::Radio => "Radio controlled",
        }
    }
}

/// Where a CAT rig's CW comes from when the panel's keyer sends.
///
/// A transceiver in CW mode does not modulate what arrives at its sound card —
/// the transmitter is keyed, by a key line or by its own memory keyer, and
/// nothing else reaches the air. So sidetone written to the rig's playback
/// device (which is all an SDR-side keyer can produce) is silently discarded,
/// and the operator hears nothing go out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CwKeying {
    /// Hand the text to the rig and let its own keyer send it (Yaesu keyer
    /// memory playback, Icom CI-V "send CW"). The rig keys itself, so this is
    /// the only route that puts CW on the air from a rig that is *in* CW.
    #[default]
    Cat,
    /// Send the keyer's sidetone through the rig's sound card as audio. Only
    /// reaches the air if the rig is left in a voice/data mode, where it goes
    /// out as a tone on the sideband (MCW) at dial + pitch rather than as CW on
    /// the dial frequency.
    Audio,
}

impl CwKeying {
    pub const ALL: [CwKeying; 2] = [CwKeying::Cat, CwKeying::Audio];
    pub fn label(self) -> &'static str {
        match self {
            CwKeying::Cat => "Rig keyer (CAT)",
            CwKeying::Audio => "Sound card (MCW)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SerialConfig {
    /// Serial device path (Linux/mac `/dev/tty…`, Windows `COMx`).
    pub path: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub force_rts: LineState,
    pub force_dtr: LineState,
}

impl Default for SerialConfig {
    fn default() -> Self {
        SerialConfig {
            path: String::new(),
            baud: 19200,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: StopBits::One,
            force_rts: LineState::None,
            force_dtr: LineState::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatConfig {
    pub family: CatFamily,
    pub serial: SerialConfig,
    pub ptt: PttMethod,
    /// How often to poll the rig for its dial/mode (Hz).
    pub poll_hz: f32,
    /// Who controls the rig's mode for ordinary modes.
    pub mode_control: ModeControl,
    /// What mode the rig uses for the FT8/FT4 engine.
    pub digi_mode: DigiMode,
    /// Where CW the operator sends comes from — the rig's own keyer, or the
    /// keyer's sidetone over the sound card.
    pub cw_keying: CwKeying,
    /// Icom CI-V transceiver address (hex byte), e.g. 0x70 for many rigs.
    pub icom_radio_id: u8,
    pub format: SoundFormat,
    /// Displayed panadapter bandwidth for demod-audio mode (Hz).
    pub audio_bw_hz: f64,
}

impl Default for CatConfig {
    fn default() -> Self {
        CatConfig {
            family: CatFamily::default(),
            serial: SerialConfig::default(),
            ptt: PttMethod::default(),
            poll_hz: 5.0,
            mode_control: ModeControl::default(),
            digi_mode: DigiMode::default(),
            cw_keying: CwKeying::default(),
            icom_radio_id: 0x70,
            format: SoundFormat::default(),
            audio_bw_hz: 4000.0,
        }
    }
}

/// Which accessory filter board is wired to a Hermes-Lite 2's J16 header, and
/// therefore how its seven open-collector outputs should be driven.
///
/// Those pins are general-purpose openHPSDR outputs, not filter-only: operators
/// also wire them to amplifier PTT, antenna relays and transverter switching.
/// Driving them from band data would start operating that hardware, so the
/// default leaves every one of them off and the operator says what is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HpsdrFilterBoard {
    /// Leave all seven outputs off — the safe default, and correct for a bare
    /// board with nothing on J16.
    #[default]
    None,
    /// N2ADR filter board: one-hot relay select, forwarded by the gateware over
    /// I2C to the board's MCP23008.
    N2adr,
}

impl HpsdrFilterBoard {
    pub const ALL: [HpsdrFilterBoard; 2] = [HpsdrFilterBoard::None, HpsdrFilterBoard::N2adr];

    pub fn label(self) -> &'static str {
        match self {
            HpsdrFilterBoard::None => "None — outputs stay off",
            HpsdrFilterBoard::N2adr => "N2ADR filter board",
        }
    }
}

/// OpenHPSDR (ethernet SDR) backend configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HpsdrConfig {
    /// Explicit target IP (e.g. "192.168.1.50"). When set, connect directly and
    /// skip discovery/selection.
    pub manual_ip: Option<String>,
    /// IP of the device picked from a discovery scan (persisted selection).
    pub selected_ip: Option<String>,
    /// DDC sample rate in Hz (48k, 96k, 192k, 384k, 768k, 1536k).
    pub sample_rate_hz: f64,
    /// Front-end LNA gain in dB applied when the radio is opened, on boards
    /// that have one (Hermes-Lite 2: −12…+48 dB). Adjust it live in
    /// Settings → Device; this is the value the rig starts at.
    #[serde(default = "HpsdrConfig::default_lna_gain_db")]
    pub lna_gain_db: f64,
    /// Accessory board on the Hermes-Lite 2's J16 header. Defaults to `None`,
    /// which leaves the open-collector outputs untouched.
    #[serde(default)]
    pub filter_board: HpsdrFilterBoard,
    /// Conjugate the board's I/Q, mirroring the spectrum about the tuned
    /// frequency, on transmit as well as receive so the two directions cannot
    /// disagree about which sideband they are on.
    ///
    /// **On by default**: a Hermes-Lite 2 needs it — verified on air, where
    /// without it FT8 produces no decodes at all and SSB comes out on the wrong
    /// sideband. A board that turns out not to need it can turn it off.
    ///
    /// Deliberately *not* named `swap_iq`, which is what the one release that
    /// defaulted it to off called it. Ignoring that older key is the migration:
    /// whether an operator had found the setting and switched it on, or had it
    /// saved as off without ever knowing it existed, they all land on the value
    /// that works.
    #[serde(default = "HpsdrConfig::default_invert_spectrum")]
    pub invert_spectrum: bool,
    /// Switch on the Hermes-Lite 2's onboard power amplifier (register `0x09`
    /// bit 19). Ignored on every other board — the bit is a Hermes-Lite
    /// repurposing of an Apollo/Alex field.
    ///
    /// **On by default**, because with it off the board keys — the T/R relay
    /// throws, the PTT line and any accessory board follow — and puts out no
    /// power at all at the antenna jack. Turn it off only to drive an external
    /// amplifier from the low-power RF1 output, which also parks the T/R relay
    /// in receive (register `0x09` bit 18) so the antenna connector stays on
    /// the receiver.
    #[serde(default = "HpsdrConfig::default_pa_enable")]
    pub pa_enable: bool,
    /// Crystal/TCXO error in ppm, applied to RX/TX frequency before it's sent
    /// to the board's NCO.
    #[serde(default)]
    pub ppm: f64,
    /// Which of the board's DDCs (receivers) this radio runs, 0-based as the
    /// wire counts them. A Protocol 2 board carries several independently
    /// tunable DDCs on one connection, so two radios on the same address can
    /// each take one; the transmitter (DUC) belongs to DDC 0's radio, and
    /// Protocol 1 boards have only DDC 0 here. Defaults keep every existing
    /// `radio.json` on DDC 0, exactly as before.
    #[serde(default)]
    pub ddc: u8,
}

impl Default for HpsdrConfig {
    fn default() -> Self {
        HpsdrConfig {
            manual_ip: None,
            selected_ip: None,
            sample_rate_hz: 1_536_000.0,
            lna_gain_db: Self::default_lna_gain_db(),
            filter_board: HpsdrFilterBoard::None,
            invert_spectrum: Self::default_invert_spectrum(),
            pa_enable: Self::default_pa_enable(),
            ppm: 0.0,
            ddc: 0,
        }
    }
}

impl HpsdrConfig {
    /// Range of the Hermes-Lite 2 front-end gain, in dB.
    pub const LNA_GAIN_MIN_DB: f64 = -12.0;
    pub const LNA_GAIN_MAX_DB: f64 = 48.0;
    /// Name of the RX gain element the backend exposes for that gain. Lives here
    /// rather than in `sdroxide-hpsdr` so the (wasm-safe) settings UI can address
    /// the same element without depending on the native backend crate.
    pub const LNA_GAIN_ELEMENT: &'static str = "LNA";
    /// Ppm correction, riding `SetGain` like [`RtlSdrConfig::PPM_ELEMENT`].
    pub const PPM_ELEMENT: &'static str = "PPM";

    /// Mid-scale default: sensitive enough on a quiet band without clipping the
    /// ADC on a real antenna.
    pub fn default_lna_gain_db() -> f64 {
        20.0
    }

    /// Hermes-Lite 2 boards deliver a conjugated stream, so inversion is the
    /// working default. See [`HpsdrConfig::invert_spectrum`].
    pub fn default_invert_spectrum() -> bool {
        true
    }

    /// A Hermes-Lite 2 with its PA switched off transmits nothing at the
    /// antenna jack, so the amplifier is on unless the operator says otherwise.
    /// See [`HpsdrConfig::pa_enable`].
    pub fn default_pa_enable() -> bool {
        true
    }

    /// Supported DDC sample rates (Hz) for Protocol 2 boards.
    pub const SAMPLE_RATES: [f64; 6] =
        [48_000.0, 96_000.0, 192_000.0, 384_000.0, 768_000.0, 1_536_000.0];

    /// Protocol 1 (Metis) boards top out at 384 kHz.
    pub const P1_SAMPLE_RATES: [f64; 4] = [48_000.0, 96_000.0, 192_000.0, 384_000.0];

    /// The sample rates valid for a given protocol (1 or 2).
    pub fn rates_for(protocol: u8) -> &'static [f64] {
        if protocol == 1 { &Self::P1_SAMPLE_RATES } else { &Self::SAMPLE_RATES }
    }

    /// Resolve the IP to connect to: manual override, else the persisted pick.
    /// `None` means "discover and use the first responder".
    pub fn target_ip(&self) -> Option<&str> {
        self.manual_ip.as_deref().filter(|s| !s.trim().is_empty()).or(self.selected_ip.as_deref())
    }

    /// Scale `hz` by a ppm correction.
    pub fn apply_ppm(hz: f64, ppm: f64) -> f64 {
        hz * (1.0 + ppm / 1e6)
    }
}

/// One HPSDR device found by a discovery scan. Wasm-safe so it can cross the
/// `RadioController` trait to the settings UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HpsdrDevice {
    pub ip: String,
    pub mac: String,
    /// Board name, e.g. "Hermes", "Saturn", "Hermes-Lite 2".
    pub board: String,
    /// OpenHPSDR protocol the board speaks (1 or 2).
    pub protocol: u8,
    /// Whether the board reports it is already in use by another host.
    pub in_use: bool,
}

impl HpsdrDevice {
    /// One-line label for the selection UI.
    pub fn label(&self) -> String {
        let mut s = format!("{}  {}  (P{})", self.board, self.ip, self.protocol);
        if self.in_use {
            s.push_str("  [in use]");
        }
        if !self.supported() {
            s.push_str("  [unsupported protocol]");
        }
        s
    }

    /// Whether this device can be driven by the current implementation
    /// (Protocol 1 and Protocol 2 are both supported).
    pub fn supported(&self) -> bool {
        matches!(self.protocol, 1 | 2)
    }
}

/// TCI (Transceiver Control Interface, WebSocket) backend configuration.
/// Receive is wideband IQ (sdroxide demodulates); transmit is audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TciConfig {
    /// TCI server `host:port` (default `127.0.0.1:50001`, the ExpertSDR3 port).
    pub address: String,
    /// IQ stream sample rate in Hz (48k / 96k / 192k).
    pub iq_sample_rate_hz: f64,
    /// Which of the rig's receivers this radio runs, 0-based as the wire
    /// counts them (a SunSDR2DX has 0 and 1). Two radios on the same address
    /// share one connection, each with its own receiver; the transmitter
    /// belongs to receiver 0's radio. `#[serde(default)]` on the struct keeps
    /// every existing `radio.json` on receiver 0, exactly as before.
    pub rx: u32,
}

impl Default for TciConfig {
    fn default() -> Self {
        TciConfig { address: "127.0.0.1:50001".into(), iq_sample_rate_hz: 192_000.0, rx: 0 }
    }
}

impl TciConfig {
    /// IQ sample rates offered in the UI.
    pub const IQ_RATES: [f64; 3] = [48_000.0, 96_000.0, 192_000.0];
}

/// SmartSDR (FlexRadio) backend configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartSdrConfig {
    /// Radio address as `host[:port]`. Empty means "use the discovered radio",
    /// which is the normal case on a LAN — a FlexRadio announces itself.
    pub address: String,
    /// IP of the radio picked from a discovery scan (persisted selection).
    pub selected_ip: Option<String>,
    /// DAX IQ stream rate in Hz. 192 kHz is the radio's maximum, and so this
    /// backend's widest span.
    pub iq_sample_rate_hz: f64,
    /// Which of the radio's four DAX IQ channels to claim. Change it only when
    /// something else on the network is already using channel 1.
    pub iq_channel: u32,
    /// Station name reported to the radio, shown against our session in
    /// SmartSDR's client list and used to derive our stable GUI client id — so
    /// changing it makes the radio treat us as a new client.
    pub station: String,
}

impl Default for SmartSdrConfig {
    fn default() -> Self {
        SmartSdrConfig {
            address: String::new(),
            selected_ip: None,
            iq_sample_rate_hz: 192_000.0,
            iq_channel: 1,
            station: "sdroxide".into(),
        }
    }
}

impl SmartSdrConfig {
    /// IQ sample rates a FLEX will deliver over DAX.
    pub const IQ_RATES: [f64; 4] = [24_000.0, 48_000.0, 96_000.0, 192_000.0];
    /// DAX IQ channels the radio provides.
    pub const IQ_CHANNELS: [u32; 4] = [1, 2, 3, 4];

    /// The address to connect to: the manual entry, else the discovered
    /// selection, else nothing.
    pub fn target(&self) -> Option<&str> {
        let manual = self.address.trim();
        if !manual.is_empty() {
            return Some(manual);
        }
        self.selected_ip.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }
}

/// A FlexRadio found by a discovery scan, for the selection UI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SmartSdrDevice {
    pub ip: String,
    pub port: u16,
    pub model: String,
    pub serial: String,
    pub nickname: String,
    pub version: String,
    /// Whether a GUI client can join: nobody else has it, or multiFLEX is on.
    pub joinable: bool,
    /// Station names of GUI clients already connected.
    pub gui_clients: Vec<String>,
}

impl SmartSdrDevice {
    /// One-line label for the selection UI.
    pub fn label(&self) -> String {
        let name = match (self.nickname.is_empty(), self.model.is_empty()) {
            (false, false) => format!("{} ({})", self.nickname, self.model),
            (false, true) => self.nickname.clone(),
            (true, false) => self.model.clone(),
            (true, true) => "FlexRadio".to_string(),
        };
        let mut s = format!("{name}  {}", self.ip);
        if !self.version.is_empty() {
            s.push_str(&format!("  v{}", self.version));
        }
        if !self.gui_clients.is_empty() {
            s.push_str(&format!("  [in use: {}]", self.gui_clients.join(", ")));
        }
        if !self.joinable {
            s.push_str("  [multiFLEX off]");
        }
        s
    }
}

/// How an RTL-SDR reaches HF. The R82xx tuner itself starts at 24 MHz, so
/// anything below that needs help from the dongle's hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RtlSdrHfMode {
    /// Tuner only — nothing below 24 MHz.
    Off,
    /// Use whatever this dongle has: the V4's built-in upconverter, or
    /// direct sampling on a V3. Switched automatically at the crossover.
    #[default]
    Auto,
    /// Force direct sampling on the ADC's Q branch (the V3's HF port). Has no
    /// meaning on a Blog V4, which upconverts instead.
    DirectQ,
}

impl RtlSdrHfMode {
    pub const ALL: [RtlSdrHfMode; 3] =
        [RtlSdrHfMode::Auto, RtlSdrHfMode::Off, RtlSdrHfMode::DirectQ];

    /// Paired with [`RtlSdrHfMode::from_code`] so the mode can ride the
    /// `HFMODE` pseudo-element; keep the two in step.
    pub fn code(self) -> u8 {
        match self {
            RtlSdrHfMode::Off => 0,
            RtlSdrHfMode::Auto => 1,
            RtlSdrHfMode::DirectQ => 2,
        }
    }

    pub fn from_code(code: u8) -> RtlSdrHfMode {
        match code {
            0 => RtlSdrHfMode::Off,
            2 => RtlSdrHfMode::DirectQ,
            _ => RtlSdrHfMode::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RtlSdrHfMode::Off => "Off (tuner only, 24 MHz up)",
            RtlSdrHfMode::Auto => "Automatic",
            RtlSdrHfMode::DirectQ => "Direct sampling (Q branch)",
        }
    }
}

/// Which automatic gain loops to enable. The tuner AGC lives in the R82xx; the
/// RTL AGC is the demod's digital one. They are independent and can both run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RtlSdrAgc {
    /// Manual tuner gain, no automatic loops — the setting for measurement and
    /// for weak-signal digital modes.
    #[default]
    Manual,
    Tuner,
    Rtl,
    Both,
}

impl RtlSdrAgc {
    pub const ALL: [RtlSdrAgc; 4] =
        [RtlSdrAgc::Manual, RtlSdrAgc::Tuner, RtlSdrAgc::Rtl, RtlSdrAgc::Both];
    pub fn label(self) -> &'static str {
        match self {
            RtlSdrAgc::Manual => "Manual (no AGC)",
            RtlSdrAgc::Tuner => "Tuner AGC",
            RtlSdrAgc::Rtl => "RTL digital AGC",
            RtlSdrAgc::Both => "Tuner + RTL AGC",
        }
    }

    /// Whether the R82xx runs its own LNA/mixer gain loop.
    pub fn tuner_auto(self) -> bool {
        matches!(self, RtlSdrAgc::Tuner | RtlSdrAgc::Both)
    }

    /// Whether the demod's digital AGC runs.
    pub fn rtl_auto(self) -> bool {
        matches!(self, RtlSdrAgc::Rtl | RtlSdrAgc::Both)
    }

    /// AGC mode as a number, so it can ride the existing `SetGain` command on
    /// the `AGC` pseudo-element instead of needing a new `Command` variant.
    /// Paired with [`RtlSdrAgc::from_code`]; keep the two in step.
    pub fn code(self) -> u8 {
        match self {
            RtlSdrAgc::Manual => 0,
            RtlSdrAgc::Tuner => 1,
            RtlSdrAgc::Rtl => 2,
            RtlSdrAgc::Both => 3,
        }
    }

    pub fn from_code(code: u8) -> RtlSdrAgc {
        match code {
            1 => RtlSdrAgc::Tuner,
            2 => RtlSdrAgc::Rtl,
            3 => RtlSdrAgc::Both,
            _ => RtlSdrAgc::Manual,
        }
    }
}

/// RTL-SDR (RTL2832U over USB) backend configuration. Receive only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RtlSdrConfig {
    /// USB serial of the dongle to open. `None` = the first one found. Serial
    /// rather than an index because bus position changes on every replug, and
    /// a persisted index would attach to the wrong dongle.
    pub serial: Option<String>,
    /// Sample rate in Hz. The resampler only reaches 225–300 kHz and
    /// 900 kHz–3.2 MHz; everything between is rejected by the hardware.
    pub sample_rate_hz: f64,
    /// Crystal error in parts per million. Read it off the `clock error`
    /// line that `RUST_LOG=sdroxide_rtlsdr=debug` prints once the stream runs.
    pub ppm: i32,
    /// Tuner gain in dB when AGC is off. Snapped to the nearest step the
    /// hardware can actually produce.
    pub tuner_gain_db: f64,
    pub agc: RtlSdrAgc,
    pub hf_mode: RtlSdrHfMode,
    /// Bias tee: ~4.5 V DC on the antenna coax for a remote LNA. Off by
    /// default, and turned off again on a clean shutdown — it will damage a
    /// transceiver or anything DC-shorted on the other end of the cable.
    pub bias_tee: bool,
    /// Bulk transfers kept in flight (advanced). The default gives ~53 ms of
    /// hardware-side buffering at 2.4 Msps, twice the worst-case retune stall.
    pub transfers: u8,
    /// Size of each bulk transfer in KiB (advanced). Must stay a multiple of
    /// the endpoint's 512-byte packet.
    pub transfer_kib: u16,
}

impl Default for RtlSdrConfig {
    fn default() -> Self {
        RtlSdrConfig {
            serial: None,
            sample_rate_hz: 2_400_000.0,
            ppm: 0,
            tuner_gain_db: 30.0,
            agc: RtlSdrAgc::Manual,
            hf_mode: RtlSdrHfMode::Auto,
            bias_tee: false,
            transfers: 16,
            transfer_kib: 16,
        }
    }
}

impl RtlSdrConfig {
    /// Gain element names the backend exposes. They live here rather than in
    /// `sdroxide-rtlsdr` so the (wasm-safe) settings UI can address them
    /// without depending on the native backend crate — same reason as
    /// [`HpsdrConfig::LNA_GAIN_ELEMENT`].
    pub const TUNER_GAIN_ELEMENT: &'static str = "TUNER";
    pub const IF_GAIN_ELEMENT: &'static str = "IF";
    /// Pseudo-elements carrying settings that are not gains at all.
    ///
    /// These ride the existing `SetGain` command so that adding this backend
    /// needs no new `Command` variant, no `DeviceCaps` field and no engine
    /// change for four settings only one backend has. They are deliberately
    /// absent from `DeviceCaps::gains`, so nothing renders them as sliders —
    /// the RTL-SDR settings panel drives them directly. The encodings live
    /// beside the enums they carry ([`RtlSdrAgc::code`], `HfMode as u8`) so
    /// the two ends cannot drift.
    pub const AGC_ELEMENT: &'static str = "AGC";
    pub const PPM_ELEMENT: &'static str = "PPM";
    pub const HF_MODE_ELEMENT: &'static str = "HFMODE";
    pub const BIAS_TEE_ELEMENT: &'static str = "BIASTEE";

    /// Sample rates offered in the UI. All lie inside the resampler's upper
    /// window except 250 kHz, which is in the lower one. 3.2 Msps is offered
    /// but drops samples on most hosts.
    pub const SAMPLE_RATES: [f64; 9] = [
        250_000.0,
        960_000.0,
        1_024_000.0,
        1_200_000.0,
        1_536_000.0,
        1_800_000.0,
        2_048_000.0,
        2_400_000.0,
        3_200_000.0,
    ];

    /// Maximum R82xx tuner gain, in dB (the last entry of the gain table).
    pub const GAIN_MAX_DB: f64 = 49.6;

    /// Below this, HF handling kicks in: the Blog V4's upconverter reference
    /// frequency, and equally the bottom of the R82xx's own range.
    pub const HF_CROSSOVER_HZ: f64 = 28_800_000.0;
}

/// One RTL-SDR dongle found on the USB bus. Wasm-safe so it can cross the
/// `RadioController` trait to the settings UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RtlSdrDevice {
    /// USB serial string, when the dongle has one programmed.
    pub serial: Option<String>,
    /// Best available name: the USB product string, else the VID/PID table.
    pub name: String,
    pub vid: u16,
    pub pid: u16,
}

impl RtlSdrDevice {
    /// One-line label for the selection UI.
    pub fn label(&self) -> String {
        match &self.serial {
            Some(s) => format!("{}  (serial {s})", self.name),
            // Without a serial we can only ever open "the first one", so say so
            // rather than implying this entry can be pinned.
            None => format!("{}  [no serial — first match only]", self.name),
        }
    }
}

/// An RX-888 seen on the USB bus.
///
/// Wasm-safe so it can cross the `RadioController` trait to the settings UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rx888Device {
    /// USB serial. The boot ROM and the running firmware report *different*
    /// serials, so a pinned value only matches the state the device is in.
    pub serial: Option<String>,
    /// Product string, or a generic name while it is still in its boot ROM.
    pub name: String,
    /// True while the device is still in the Cypress boot ROM. Not a fault:
    /// every RX-888 looks like this until something programs it, and sdroxide
    /// does that on open.
    pub needs_firmware: bool,
    /// Whether the link negotiated SuperSpeed. Only meaningful once the device
    /// is programmed — the boot ROM always enumerates at USB 2.0, even on a
    /// perfectly good USB 3 cable and port.
    pub superspeed: bool,
}

impl Rx888Device {
    /// One-line label for the selection UI.
    pub fn label(&self) -> String {
        let mut s = self.name.clone();
        if let Some(serial) = &self.serial {
            s.push_str(&format!("  (serial {serial})"));
        }
        if self.needs_firmware {
            s.push_str("  [firmware will be uploaded]");
        }
        s
    }
}

/// RX-888 settings (`radio.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rx888Config {
    /// Pin a particular receiver; empty means "the first one found".
    pub serial: String,
    /// ADC clock in Hz, which is also the real-sample rate on the wire.
    pub adc_rate_hz: f64,
    /// LTC2208 dither: costs a little noise floor, buys spurious-free dynamic
    /// range.
    pub dither: bool,
    /// LTC2208 output randomiser. On by default — it stops the digital bus
    /// radiating into the front end, and undoing it costs one XOR per sample.
    pub randomize: bool,
    /// DC on the HF antenna port. Off by default: putting phantom power on
    /// someone's feedline uninvited is not a good default.
    pub bias_tee_hf: bool,
    /// Select the ADC's wider 2.25 Vp-p input range. Named for the GPIO bit,
    /// which is not actually a preamplifier — see the driver's `gpio::PGA_EN`.
    pub pga: bool,
    /// Step attenuator as a gain, i.e. -31.5..=0 dB.
    pub attenuator_db: f64,
    /// AD8370 VGA gain in dB.
    pub vga_db: f64,
    /// Reference trim, parts per million.
    pub ppm: f64,
    /// Override the bundled FX3 firmware image. Empty uses the built-in one.
    pub firmware_path: String,
}

impl Default for Rx888Config {
    fn default() -> Self {
        Rx888Config {
            serial: String::new(),
            adc_rate_hz: 64_800_000.0,
            dither: false,
            randomize: true,
            bias_tee_hf: false,
            pga: true,
            attenuator_db: 0.0,
            vga_db: 12.0,
            ppm: 0.0,
            firmware_path: String::new(),
        }
    }
}

impl Rx888Config {
    /// Pseudo gain-element names, riding `Command::SetGain` so this backend
    /// needs no new `Command` variant, no `DeviceCaps` field and no engine
    /// change for settings only it has. They live here rather than in
    /// `sdroxide-rx888` so the wasm-safe settings UI can address them without
    /// depending on the native backend crate.
    pub const VGA_ELEMENT: &'static str = "VGA";
    pub const ATT_ELEMENT: &'static str = "ATT";
    pub const DITHER_ELEMENT: &'static str = "DITHER";
    pub const BIAS_TEE_ELEMENT: &'static str = "BIASTEE";
    pub const PGA_ELEMENT: &'static str = "PGA";

    /// ADC clocks offered in the UI. The Si5351 will synthesise others, but
    /// these are the ones in common use on this board.
    pub const ADC_RATES: [f64; 4] = [16_200_000.0, 32_400_000.0, 64_800_000.0, 129_600_000.0];
}

/// AD9361 receive AGC mode. The names are the IIO `gain_control_mode` values,
/// which is what actually goes on the wire.
///
/// SoapySDR can only say "AGC on" or "AGC off"; the part itself has four modes
/// and they behave very differently on the air, which is one of the reasons
/// this backend is native rather than a SoapySDR device string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlutoAgc {
    /// The gain slider is in charge.
    Manual,
    /// Rides slowly over a signal — the right default for SSB and CW, where a
    /// fast AGC pumps on every syllable.
    #[default]
    SlowAttack,
    /// Reacts within a burst. Wanted where signals appear suddenly and at very
    /// different strengths.
    FastAttack,
    /// Digital AGC with an analog fast-attack safety net.
    Hybrid,
}

impl PlutoAgc {
    pub const ALL: [PlutoAgc; 4] =
        [PlutoAgc::Manual, PlutoAgc::SlowAttack, PlutoAgc::FastAttack, PlutoAgc::Hybrid];

    /// What the IIO attribute is set to.
    pub fn iio_name(self) -> &'static str {
        match self {
            PlutoAgc::Manual => "manual",
            PlutoAgc::SlowAttack => "slow_attack",
            PlutoAgc::FastAttack => "fast_attack",
            PlutoAgc::Hybrid => "hybrid",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PlutoAgc::Manual => "Manual",
            PlutoAgc::SlowAttack => "Slow attack",
            PlutoAgc::FastAttack => "Fast attack",
            PlutoAgc::Hybrid => "Hybrid",
        }
    }

    /// Numeric code carried on [`PlutoConfig::AGC_ELEMENT`], so the mode rides
    /// the existing `SetGain` command instead of needing one of its own.
    pub fn code(self) -> f64 {
        match self {
            PlutoAgc::Manual => 0.0,
            PlutoAgc::SlowAttack => 1.0,
            PlutoAgc::FastAttack => 2.0,
            PlutoAgc::Hybrid => 3.0,
        }
    }

    pub fn from_code(v: f64) -> PlutoAgc {
        match v.round() as i32 {
            0 => PlutoAgc::Manual,
            2 => PlutoAgc::FastAttack,
            3 => PlutoAgc::Hybrid,
            _ => PlutoAgc::SlowAttack,
        }
    }
}

/// ADALM-Pluto (PlutoSDR) backend configuration.
///
/// The device is reached over the network — which the USB cable already
/// provides, as an Ethernet gadget — so this is an address, not a serial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlutoConfig {
    /// `host[:port]`, defaulting to the USB gadget's device end. Blank falls
    /// back to [`Self::selected_ip`], then to the default address.
    pub address: String,
    /// IP of the Pluto picked from a discovery scan (persisted selection).
    pub selected_ip: Option<String>,
    /// Sample rate in Hz. The AD9361 reaches 61.44 Msps; a USB 2.0 Ethernet
    /// gadget does not, which is what [`Self::SAMPLE_RATES`] is scaled to.
    pub sample_rate_hz: f64,
    /// Analog filter bandwidth in Hz, or `0.0` for automatic (0.9 × the sample
    /// rate). Automatic is deliberately wide: the engine parks the LO a quarter
    /// of a span off the dial to keep the signal clear of a zero-IF part's DC
    /// spike, and a narrow analog filter is what makes it give that up.
    pub rf_bandwidth_hz: f64,
    /// Receive gain in dB when the AGC is in manual.
    pub rx_gain_db: f64,
    pub agc: PlutoAgc,
    /// Transmit gain in dB — negative, because the AD9361 expresses it as
    /// attenuation. `0` is full output. The default is well down: this is a
    /// transmitter, and a first key-up should not be a surprise.
    pub tx_gain_db: f64,
    /// `rf_port_select` for receive; empty leaves the device's own choice. A
    /// Pluto wires only `A_BALANCED`, but the AD9361 has nine and a custom
    /// board may use another.
    pub rx_port: String,
    /// `rf_port_select` for transmit; empty leaves the device's own choice.
    pub tx_port: String,
    /// Reference error in parts per million. Applied in software to every
    /// requested LO — the device's own `xo_correction` is persistent, and
    /// writing it would outlive the session.
    pub ppm: f64,
    /// Device-side buffer length in complex samples (advanced). 32768 is ~16 ms
    /// at 2 Msps: long enough that the per-buffer round trip is not the
    /// bottleneck, short enough that a retune is not visibly late.
    pub buffer_samples: usize,
    /// Which of the device's receive chains this radio runs, 0-based. A 2R2T
    /// firmware (a Pluto+) streams two; two radios on the same address share
    /// one connection, each with its own chain — **and the one LO**: the
    /// AD9361's chains share a synthesiser, so retuning either radio moves
    /// both, and the second chain is a second antenna, not a second
    /// frequency. The transmitter belongs to chain 0's radio. Defaults keep
    /// every existing `radio.json` on chain 0, exactly as before.
    #[serde(default)]
    pub rx: u8,
}

impl Default for PlutoConfig {
    fn default() -> Self {
        PlutoConfig {
            address: PlutoConfig::DEFAULT_ADDRESS.into(),
            selected_ip: None,
            // Above `NO_FIR_FLOOR_HZ`, so a stock Pluto can actually produce
            // it. The 2 Msps this used to be could not be, and an out-of-the-box
            // radio refused to open at its own default settings.
            sample_rate_hz: 2_500_000.0,
            rf_bandwidth_hz: 0.0,
            rx_gain_db: 40.0,
            agc: PlutoAgc::SlowAttack,
            tx_gain_db: -20.0,
            rx_port: String::new(),
            tx_port: String::new(),
            ppm: 0.0,
            buffer_samples: 32768,
            rx: 0,
        }
    }
}

impl PlutoConfig {
    /// Where an out-of-the-box Pluto lives: the device end of the USB Ethernet
    /// gadget (the host takes 192.168.2.10 on the same link).
    pub const DEFAULT_ADDRESS: &'static str = "192.168.2.1";

    /// Gain elements this backend exposes. They live here rather than in
    /// `sdroxide-pluto` so the (wasm-safe) settings UI can address them without
    /// depending on the native backend crate — same reason as
    /// [`HpsdrConfig::LNA_GAIN_ELEMENT`].
    pub const RF_GAIN_ELEMENT: &'static str = "RF";
    pub const TX_GAIN_ELEMENT: &'static str = "TXATT";
    /// Pseudo-elements carrying settings that are not gains at all. They ride
    /// the existing `SetGain` command so this backend needs no new `Command`
    /// variant, and are deliberately absent from `DeviceCaps::gains` so nothing
    /// renders them as sliders — the Pluto settings panel drives them directly.
    pub const AGC_ELEMENT: &'static str = "AGC";
    pub const PPM_ELEMENT: &'static str = "PPM";

    /// Sample rates offered in the UI.
    ///
    /// The floor is the AD9361's own (521 ksps, through its internal FIR
    /// decimator). The ceiling is not the part's 61.44 Msps but what a USB 2.0
    /// Ethernet gadget will actually carry: 2 Msps of 16-bit I/Q is 64 Mbit/s
    /// before framing, which is already most of the link.
    ///
    /// The entries below [`Self::NO_FIR_FLOOR_HZ`] need a FIR configuration
    /// loaded into the part, which sdroxide does not do — a stock Pluto rounds
    /// them all up to that floor and says so on connect. They are still offered
    /// because a board someone else has configured, or an IIO device that is
    /// not a Pluto at all, can honour them.
    pub const SAMPLE_RATES: [f64; 6] =
        [521_000.0, 1_000_000.0, 2_000_000.0, 2_500_000.0, 3_840_000.0, 5_000_000.0];

    /// The lowest rate an AD936x can produce with its FIR decimator bypassed,
    /// which is how a Pluto arrives and how sdroxide leaves it.
    ///
    /// The part's clock-chain solver accepts a rate only if `rate × 12` clears
    /// the ADC's 25 MHz minimum, so the true floor is 25 MHz / 12 = 2083333.33
    /// Hz — and the driver publishes that range through integer division, so it
    /// advertises 2083333 and then refuses it. This is the first integer that
    /// actually works.
    pub const NO_FIR_FLOOR_HZ: f64 = 2_083_334.0;

    /// The address to open: the typed one, else a discovered selection, else
    /// the USB gadget's default.
    pub fn target(&self) -> String {
        let typed = self.address.trim();
        if !typed.is_empty() {
            return typed.to_string();
        }
        match self.selected_ip.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(ip) => ip.to_string(),
            None => PlutoConfig::DEFAULT_ADDRESS.to_string(),
        }
    }

    /// Apply the reference trim to a frequency, the same way
    /// [`HpsdrConfig::apply_ppm`] does.
    pub fn apply_ppm(hz: f64, ppm: f64) -> f64 {
        hz * (1.0 + ppm / 1e6)
    }
}

/// A Pluto found on the network (or confirmed at a typed address).
///
/// Wasm-safe so it can cross the `RadioController` trait to the settings UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlutoDevice {
    pub ip: String,
    /// mDNS instance or host name, when discovery supplied one.
    pub hostname: String,
    /// The `hw_model` context attribute, e.g.
    /// "Analog Devices PlutoSDR Rev.B (Z7010-AD9364)".
    pub model: String,
    pub firmware: String,
    pub serial: String,
    /// libiio version the device's `iiod` reports.
    pub iiod_version: String,
}

impl PlutoDevice {
    /// One-line label for the selection UI.
    pub fn label(&self) -> String {
        let what = if self.model.is_empty() { "PlutoSDR" } else { self.model.as_str() };
        let mut s = format!("{what}  ({})", self.ip);
        if !self.firmware.is_empty() {
            s.push_str(&format!("  firmware {}", self.firmware));
        }
        s
    }

    /// Whether the model string names the AD9364 — the 70 MHz–6 GHz part an
    /// unlocked Pluto reports. Only a hint for the label; the real limits are
    /// read off the device when it is opened.
    pub fn is_ad9364(&self) -> bool {
        self.model.contains("AD9364")
    }
}

/// Which RSP the `sdrplay_api` service says a device is, from the `hwVer`
/// byte it reports. The numbering is the API's, not sequential — RSP1A is 255
/// because it was added after the RSP2 had already taken 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SdrPlayModel {
    Rsp1,
    Rsp1a,
    Rsp1b,
    Rsp2,
    RspDuo,
    RspDx,
    RspDxR2,
    Unknown,
}

impl SdrPlayModel {
    pub fn from_hw_ver(hw_ver: u8) -> SdrPlayModel {
        match hw_ver {
            1 => SdrPlayModel::Rsp1,
            2 => SdrPlayModel::Rsp2,
            3 => SdrPlayModel::RspDuo,
            4 => SdrPlayModel::RspDx,
            6 => SdrPlayModel::Rsp1b,
            7 => SdrPlayModel::RspDxR2,
            255 => SdrPlayModel::Rsp1a,
            _ => SdrPlayModel::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SdrPlayModel::Rsp1 => "RSP1",
            SdrPlayModel::Rsp1a => "RSP1A",
            SdrPlayModel::Rsp1b => "RSP1B",
            SdrPlayModel::Rsp2 => "RSP2",
            SdrPlayModel::RspDuo => "RSPduo",
            SdrPlayModel::RspDx => "RSPdx",
            SdrPlayModel::RspDxR2 => "RSPdx R2",
            SdrPlayModel::Unknown => "RSP (unknown model)",
        }
    }

    /// Highest LNA state the model has in *any* band — the settings slider's
    /// range. State 0 is maximum gain; each step up switches more attenuation
    /// in front of the tuner. Some bands have fewer states than this; the
    /// driver clamps per band and reports what it settled on, the same way the
    /// RTL-SDR backend snaps its tuner gain.
    pub fn max_lna_state(self) -> u8 {
        match self {
            SdrPlayModel::Rsp1 => 3,
            SdrPlayModel::Rsp1a | SdrPlayModel::Rsp1b => 9,
            SdrPlayModel::Rsp2 => 8,
            SdrPlayModel::RspDuo => 9,
            SdrPlayModel::RspDx | SdrPlayModel::RspDxR2 => 27,
            // An unknown model still has the API-guaranteed minimum.
            SdrPlayModel::Unknown => 3,
        }
    }

    /// Whether the model has a switchable bias tee. The original RSP1 is the
    /// only one without.
    pub fn has_bias_tee(self) -> bool {
        !matches!(self, SdrPlayModel::Rsp1 | SdrPlayModel::Unknown)
    }

    /// Whether the model has the FM-broadcast notch filter.
    pub fn has_rf_notch(self) -> bool {
        !matches!(self, SdrPlayModel::Rsp1 | SdrPlayModel::Unknown)
    }

    /// Whether the model has the separate DAB notch filter.
    pub fn has_dab_notch(self) -> bool {
        matches!(
            self,
            SdrPlayModel::Rsp1a
                | SdrPlayModel::Rsp1b
                | SdrPlayModel::RspDuo
                | SdrPlayModel::RspDx
                | SdrPlayModel::RspDxR2
        )
    }

    /// Whether the model has the RSPdx HDR mode (a second, higher-linearity
    /// signal path below 2 MHz).
    pub fn has_hdr(self) -> bool {
        matches!(self, SdrPlayModel::RspDx | SdrPlayModel::RspDxR2)
    }

    /// Antenna ports the operator can choose between, for `DeviceCaps`. Empty
    /// means one fixed port — the selector stays hidden, like every other
    /// single-port backend. The RSPduo's choice depends on which tuner is in
    /// use: tuner 1 has both a 50 Ω and a Hi-Z port, tuner 2 only its own.
    pub fn antennas(self, duo_tuner: SdrPlayDuoTuner) -> &'static [&'static str] {
        match self {
            SdrPlayModel::Rsp2 => &["Antenna A", "Antenna B", "Hi-Z"],
            SdrPlayModel::RspDx | SdrPlayModel::RspDxR2 => &["Antenna A", "Antenna B", "Antenna C"],
            SdrPlayModel::RspDuo => match duo_tuner {
                SdrPlayDuoTuner::Tuner1 => &["50 Ohm port", "Hi-Z port"],
                SdrPlayDuoTuner::Tuner2 => &[],
            },
            _ => &[],
        }
    }
}

/// RSP hardware AGC loop rate. The loop runs in the tuner's IF stage, driven
/// by the API service; `Off` hands the IF gain slider back to the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SdrPlayAgc {
    Off,
    Hz5,
    #[default]
    Hz50,
    Hz100,
}

impl SdrPlayAgc {
    pub const ALL: [SdrPlayAgc; 4] =
        [SdrPlayAgc::Off, SdrPlayAgc::Hz5, SdrPlayAgc::Hz50, SdrPlayAgc::Hz100];

    pub fn label(self) -> &'static str {
        match self {
            SdrPlayAgc::Off => "Off (manual IF gain)",
            SdrPlayAgc::Hz5 => "5 Hz",
            SdrPlayAgc::Hz50 => "50 Hz",
            SdrPlayAgc::Hz100 => "100 Hz",
        }
    }

    /// Numeric code carried on [`SdrPlayConfig::AGC_ELEMENT`], so the mode
    /// rides the existing `SetGain` command instead of needing one of its own.
    /// The values are the API's own `sdrplay_api_AgcControlT` numbers — note
    /// they are not in speed order — so the two ends cannot drift.
    pub fn code(self) -> f64 {
        match self {
            SdrPlayAgc::Off => 0.0,
            SdrPlayAgc::Hz100 => 1.0,
            SdrPlayAgc::Hz50 => 2.0,
            SdrPlayAgc::Hz5 => 3.0,
        }
    }

    pub fn from_code(v: f64) -> SdrPlayAgc {
        match v.round() as i32 {
            0 => SdrPlayAgc::Off,
            1 => SdrPlayAgc::Hz100,
            3 => SdrPlayAgc::Hz5,
            // Anything unrecognised lands on the safe default rather than
            // manual, which on an unknown band would be a deaf or overloaded
            // receiver.
            _ => SdrPlayAgc::Hz50,
        }
    }
}

/// Which RSPduo tuner to run (single-tuner mode; the second tuner idles).
/// Changing it reopens the device — the choice is fixed when the tuner is
/// selected, before streaming starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SdrPlayDuoTuner {
    #[default]
    Tuner1,
    Tuner2,
}

impl SdrPlayDuoTuner {
    pub const ALL: [SdrPlayDuoTuner; 2] = [SdrPlayDuoTuner::Tuner1, SdrPlayDuoTuner::Tuner2];

    pub fn label(self) -> &'static str {
        match self {
            SdrPlayDuoTuner::Tuner1 => "Tuner 1 (50 Ohm / Hi-Z)",
            SdrPlayDuoTuner::Tuner2 => "Tuner 2 (50 Ohm)",
        }
    }
}

/// SDRplay RSP settings (`radio.json`). Receive only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SdrPlayConfig {
    /// Pin a particular receiver by its API serial; empty means "the first one
    /// found".
    pub serial: String,
    /// Effective complex sample rate in Hz. At and above 2 Msps this is the
    /// ADC rate; below, the ADC runs at 2 Msps and the service decimates.
    pub sample_rate_hz: f64,
    /// Analog IF bandwidth in kHz, or 0 for automatic — the widest filter that
    /// fits inside the sample rate.
    pub bw_khz: u32,
    /// IF gain *reduction* in dB, 20..=59 — the RSP's native unit, where 20 is
    /// maximum gain. Only obeyed while the AGC is off.
    pub if_gr_db: i32,
    /// LNA state, 0..=model max. 0 is maximum gain; each step switches more
    /// front-end attenuation in. The default is deliberately mid-table, not
    /// 0: state 0 on a real antenna drives the ADC straight into overload,
    /// and the IF AGC cannot rescue that — its whole 20..59 dB range sits
    /// *after* the front end. 4 is also what SoapySDRPlay3 defaults to.
    pub lna_state: u8,
    pub agc: SdrPlayAgc,
    /// AGC target level in dBFS.
    pub agc_setpoint_dbfs: i32,
    /// Reference trim, parts per million, applied by the device itself.
    pub ppm: f64,
    /// Bias tee: ~4.7 V DC on the antenna port for a remote LNA. Off by
    /// default — putting phantom power on someone's feedline uninvited is not
    /// a good default.
    pub bias_tee: bool,
    /// FM broadcast-band notch filter.
    pub rf_notch: bool,
    /// DAB-band notch filter.
    pub dab_notch: bool,
    /// Chosen antenna port, by the names [`SdrPlayModel::antennas`] publishes.
    /// Empty leaves the device's default.
    pub antenna: String,
    /// RSPduo only: which tuner to run.
    pub duo_tuner: SdrPlayDuoTuner,
    /// RSPdx only: HDR mode below 2 MHz.
    pub hdr: bool,
}

impl Default for SdrPlayConfig {
    fn default() -> Self {
        SdrPlayConfig {
            serial: String::new(),
            sample_rate_hz: 2_000_000.0,
            bw_khz: 0,
            if_gr_db: 40,
            lna_state: 4,
            agc: SdrPlayAgc::Hz50,
            agc_setpoint_dbfs: -60,
            ppm: 0.0,
            bias_tee: false,
            rf_notch: false,
            dab_notch: false,
            antenna: String::new(),
            duo_tuner: SdrPlayDuoTuner::Tuner1,
            hdr: false,
        }
    }
}

impl SdrPlayConfig {
    /// Gain elements this backend exposes. They live here rather than in
    /// `sdroxide-sdrplay` so the (wasm-safe) settings UI can address them
    /// without depending on the native backend crate — same reason as
    /// [`HpsdrConfig::LNA_GAIN_ELEMENT`]. Both are carried as *negative*
    /// values (like the RX-888 attenuator) so more slider is more gain:
    /// `IF` is −(gain reduction dB), `LNA` is −(LNA state).
    pub const IF_GAIN_ELEMENT: &'static str = "IF";
    pub const LNA_ELEMENT: &'static str = "LNA";
    /// Pseudo-elements carrying settings that are not gains at all. They ride
    /// the existing `SetGain` command so this backend needs no new `Command`
    /// variant, and are deliberately absent from `DeviceCaps::gains` so
    /// nothing renders them as sliders — the SDRplay settings panel drives
    /// them directly. The AGC encoding lives beside the enum it carries
    /// ([`SdrPlayAgc::code`]) so the two ends cannot drift.
    pub const AGC_ELEMENT: &'static str = "AGC";
    pub const AGC_SETPOINT_ELEMENT: &'static str = "AGCSP";
    pub const PPM_ELEMENT: &'static str = "PPM";
    pub const BIAS_TEE_ELEMENT: &'static str = "BIASTEE";
    pub const RF_NOTCH_ELEMENT: &'static str = "RFNOTCH";
    pub const DAB_NOTCH_ELEMENT: &'static str = "DABNOTCH";
    pub const HDR_ELEMENT: &'static str = "HDR";

    /// IF gain reduction limits, in dB, from the API (`NORMAL_MIN_GR` and
    /// `MAX_BB_GR`).
    pub const IF_GR_MIN: i32 = 20;
    pub const IF_GR_MAX: i32 = 59;

    /// Sample rates offered in the UI. Below 2 Msps the ADC still runs at
    /// 2 Msps and the API decimates; above 6.048 Msps the ADC trades
    /// resolution for speed (12 bits up to 6.048, 10 to 8.064, 8 beyond).
    pub const SAMPLE_RATES: [f64; 10] = [
        250_000.0,
        500_000.0,
        1_000_000.0,
        2_000_000.0,
        3_000_000.0,
        4_000_000.0,
        5_000_000.0,
        6_000_000.0,
        8_000_000.0,
        10_000_000.0,
    ];

    /// Analog IF bandwidths the tuner has, in kHz — the values of
    /// `sdrplay_api_Bw_MHzT`.
    pub const BANDWIDTHS_KHZ: [u32; 8] = [200, 300, 600, 1536, 5000, 6000, 7000, 8000];
}

/// An RSP the `sdrplay_api` service reports. Wasm-safe so it can cross the
/// `RadioController` trait to the settings UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdrPlayDevice {
    /// The API's serial string — what [`SdrPlayConfig::serial`] pins.
    pub serial: String,
    /// The raw `hwVer` byte; [`Self::model`] decodes it.
    pub hw_ver: u8,
}

impl SdrPlayDevice {
    pub fn model(&self) -> SdrPlayModel {
        SdrPlayModel::from_hw_ver(self.hw_ver)
    }

    /// One-line label for the selection UI.
    pub fn label(&self) -> String {
        format!("{}  (serial {})", self.model().label(), self.serial)
    }

    /// Whether the service reported this receiver without a usable identity:
    /// no serial number, or a hardware version these bindings do not know.
    ///
    /// A missing serial is SDRplay's own documented signature of a USB
    /// communication problem — a brownout, a bad cable, or an API service
    /// holding a stale session after the device re-enumerated under it. Such
    /// a receiver still lists, selects and streams, but often deaf, and
    /// nothing else about the session looks wrong; hence a dedicated check
    /// every surface can warn from.
    pub fn identity_missing(&self) -> bool {
        Self::degraded_identity(&self.serial, self.model())
    }

    /// The predicate behind [`Self::identity_missing`], for callers that hold
    /// the serial and model without a device row (the running source).
    pub fn degraded_identity(serial: &str, model: SdrPlayModel) -> bool {
        serial.trim().is_empty() || model == SdrPlayModel::Unknown
    }

    /// Operator-facing warning for a degraded enumeration, `None` when
    /// healthy. One composer so the settings picker, the standing notice and
    /// the log all say the same thing.
    pub fn degraded_warning(serial: &str, model: SdrPlayModel) -> Option<String> {
        if !Self::degraded_identity(serial, model) {
            return None;
        }
        let what = if serial.trim().is_empty() {
            "no serial number"
        } else {
            "an unrecognised hardware version"
        };
        Some(format!(
            "The SDRplay service reports this RSP with {what} — usually a USB \
             communication problem, and such a receiver often runs deaf. Restart the \
             SDRplay API service, then unplug and replug the receiver."
        ))
    }

    /// [`Self::degraded_warning`] for a listed device.
    pub fn identity_warning(&self) -> Option<String> {
        Self::degraded_warning(&self.serial, self.model())
    }
}

/// Named converters for [`RadioConfig::converter_offset_hz`], with the offset
/// each one puts on the dial in Hz.
///
/// Signs follow the one rule the whole feature is built on: the hardware is
/// tuned to `dial + offset`. An *up*-converter therefore has a positive offset
/// — a Ham It Up presents 10.1 MHz to the receiver as 135.1 MHz — and a
/// *down*-converter a negative one: a universal Ku-band LNB with a 9750 MHz
/// local oscillator hands a 10.489 GHz downlink to the receiver at 739 MHz.
///
/// Anything else is typed in directly; the settings dialog calls that Manual,
/// and shows it whenever the offset matches nothing here.
pub const CONVERTER_PRESETS: [(&str, f64); 5] = [
    ("None", 0.0),
    ("Ham It Up (+125 MHz)", 125_000_000.0),
    ("SpyVerter (+120 MHz)", 120_000_000.0),
    ("LNB, Ku low (−9750 MHz)", -9_750_000_000.0),
    ("LNB, Ku high (−10600 MHz)", -10_600_000_000.0),
];

/// How far a converter offset may be set either way, in Hz.
///
/// Wide enough for a Ku-band LNB, which is the largest offset anyone puts in
/// front of a receiver; an HF upconverter is two orders of magnitude inside it.
pub const CONVERTER_OFFSET_MAX_HZ: f64 = 12_000_000_000.0;

/// The preset name for an offset, or `"Manual"` when it is not one of them.
pub fn converter_preset_name(offset_hz: f64) -> &'static str {
    CONVERTER_PRESETS
        .iter()
        .find(|(_, hz)| (hz - offset_hz).abs() < 0.5)
        .map(|(name, _)| *name)
        .unwrap_or("Manual")
}

/// The highest frequency an operator-supplied tuning range may name, in Hz.
///
/// 300 GHz is the top of the highest amateur allocation, and well past any
/// front end this program will meet — a number above it is a typo (a range
/// entered in Hz where megahertz was asked for, most likely) rather than a
/// microwave station.
pub const FREQ_RANGE_MAX_HZ: f64 = 300_000_000_000.0;

/// Parse an operator-typed list of tuning ranges — `"144-146, 430-440"` — into
/// (low, high) pairs in Hz.
///
/// The numbers are megahertz, because that is how an operator says which bands
/// a radio covers and how every band plan is written; the ranges themselves are
/// kept in Hz to match [`crate::DeviceCaps`]. Ranges are separated by commas,
/// semicolons or newlines and their edges by `-`, `–` or `..`, so a list
/// pasted back out of [`format_freq_ranges`] — or copied from a band plan —
/// reads straight in.
///
/// Empty input is not an error: it parses to no ranges, which is how an
/// operator says "use whatever the device publishes".
pub fn parse_freq_ranges(text: &str) -> Result<Vec<(f64, f64)>, String> {
    let mut out = Vec::new();
    for item in text.split([',', ';', '\n']) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let normalised = item.replace(['\u{2013}', '\u{2014}'], "-").replace("..", "-");
        let Some((lo, hi)) = normalised.split_once('-') else {
            return Err(format!("\"{item}\" is not a range — write it as low-high, e.g. 430-440"));
        };
        let lo = parse_range_edge(lo, item)?;
        let hi = parse_range_edge(hi, item)?;
        if hi <= lo {
            return Err(format!("\"{item}\" has its top at or below its bottom"));
        }
        out.push((lo, hi));
    }
    Ok(out)
}

/// One edge of a range, in MHz, to Hz. `whole` names the range it came from so
/// the message points at what was typed rather than at a bare number.
fn parse_range_edge(edge: &str, whole: &str) -> Result<f64, String> {
    let text = edge.trim();
    // A unit is optional and only ever the one the field is in; accepting it
    // costs nothing and refusing it would look like the number was wrong.
    let text = text.strip_suffix("MHz").or_else(|| text.strip_suffix("mhz")).unwrap_or(text).trim();
    let mhz: f64 = text
        .parse()
        .map_err(|_| format!("\"{text}\" in \"{whole}\" is not a number of megahertz"))?;
    if !mhz.is_finite() || mhz < 0.0 {
        return Err(format!("\"{text}\" in \"{whole}\" is not a frequency"));
    }
    let hz = mhz * 1e6;
    if hz > FREQ_RANGE_MAX_HZ {
        return Err(format!(
            "\"{text}\" in \"{whole}\" is above {} GHz — these are megahertz",
            FREQ_RANGE_MAX_HZ / 1e9
        ));
    }
    Ok(hz)
}

/// Ranges in Hz back to the megahertz list an operator typed, ready to be
/// parsed again by [`parse_freq_ranges`].
pub fn format_freq_ranges(ranges: &[(f64, f64)]) -> String {
    fn mhz(hz: f64) -> String {
        // Six decimals is one hertz, and trailing zeros are trimmed so a band
        // edge reads as "430" rather than "430.000000".
        let mut s = format!("{:.6}", hz / 1e6);
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
        s
    }
    ranges.iter().map(|&(lo, hi)| format!("{}-{}", mhz(lo), mhz(hi))).collect::<Vec<_>>().join(", ")
}

/// Persisted backend configuration (`radio.json`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RadioConfig {
    pub backend: Backend,
    /// Sound-card device (cpal name) carrying the radio's RX audio → PC.
    pub radio_audio_in: Option<String>,
    /// Sound-card device (cpal name) carrying the TX audio PC → radio.
    pub radio_audio_out: Option<String>,
    /// External frequency converter in the antenna line: the hardware is tuned
    /// this far from the operator's dial, in Hz. So `+125_000_000` is a Ham It
    /// Up HF upconverter and the dial reads the real on-air frequency, and a
    /// negative value is a down-converter such as a satellite LNB. `0.0` (the
    /// default) is no converter and leaves tuning exactly as it was.
    ///
    /// Hz rather than MHz because that is the unit every converter's
    /// documentation and every other SDR program states it in, and a number
    /// copied from one of those has to mean the same thing here.
    ///
    /// Receive only — a converter is not in the transmit path, so transmit is
    /// withdrawn while this is set.
    pub converter_offset_hz: f64,
    /// Tuning ranges the operator states for this radio, in Hz, replacing what
    /// the device publishes about itself. Empty (the default) leaves the
    /// device's own answer alone.
    ///
    /// Two things need this. A driver may publish no range at all — it is an
    /// optional call in SoapySDR, and SoapySX among others does not implement
    /// it — which leaves the program with nothing to check a frequency
    /// against. Or the range it publishes may be the silicon's rather than the
    /// radio's: a transceiver whose filters and PA cover one band still reports
    /// whatever its tuner chip can synthesise, and an operator who wants the
    /// dial and the transmit gate held to the real hardware has to say so.
    ///
    /// These describe the *device*, on the hardware side of any converter
    /// offset, which is where its own published ranges come from.
    pub freq_ranges_rx: Vec<(f64, f64)>,
    /// Transmit ranges the operator states, by the same rule as
    /// [`Self::freq_ranges_rx`] — and the licence gate still applies on top,
    /// so naming a range here is not a way around `tx_ham_only`.
    ///
    /// This cannot conjure a transmitter: a receive-only device has no TX
    /// channel and stays receive-only whatever is written here.
    pub freq_ranges_tx: Vec<(f64, f64)>,
    pub cat: CatConfig,
    pub hpsdr: HpsdrConfig,
    pub tci: TciConfig,
    pub smartsdr: SmartSdrConfig,
    pub rtlsdr: RtlSdrConfig,
    pub rx888: Rx888Config,
    pub pluto: PlutoConfig,
    pub sdrplay: SdrPlayConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every way an existing `radio.json` can arrive has to land on the working
    /// sideband. The one release that shipped this setting called it `swap_iq`
    /// and defaulted it to off, which is the broken value — so that key is
    /// deliberately not read any more, and neither an operator who found the
    /// checkbox nor one who never knew it existed ends up inverted the wrong way.
    #[test]
    fn spectrum_inversion_survives_every_old_config_shape() {
        let cases = [
            // Written before the setting existed at all.
            r#"{"sample_rate_hz": 384000.0}"#,
            // The old key, left at its (broken) default by someone who never
            // opened the HPSDR settings.
            r#"{"sample_rate_hz": 384000.0, "swap_iq": false}"#,
            // The old key, switched on by an operator who diagnosed it.
            r#"{"sample_rate_hz": 384000.0, "swap_iq": true}"#,
            // A completely empty object.
            r#"{}"#,
        ];
        for json in cases {
            let cfg: HpsdrConfig = serde_json::from_str(json).expect("parses");
            assert!(cfg.invert_spectrum, "inverted after loading {json}");
        }
        // A fresh install gets it too.
        assert!(HpsdrConfig::default().invert_spectrum);
        // And an operator who turns it off is still obeyed on the next load.
        let off: HpsdrConfig =
            serde_json::from_str(r#"{"invert_spectrum": false}"#).expect("parses");
        assert!(!off.invert_spectrum);
    }

    /// Every `radio.json` written before the converter existed has to keep
    /// tuning the radio exactly where it did — which means the offset must read
    /// back as zero, the one value that takes the whole feature out of circuit.
    #[test]
    fn converter_offset_defaults_to_none() {
        for json in [r#"{}"#, r#"{"backend": "RtlSdr"}"#, r#"{"rx888": {"ppm": 1.5}}"#] {
            let cfg: RadioConfig = serde_json::from_str(json).expect("parses");
            assert_eq!(cfg.converter_offset_hz, 0.0, "converter offset after loading {json}");
        }
        assert_eq!(RadioConfig::default().converter_offset_hz, 0.0);
        let up: RadioConfig =
            serde_json::from_str(r#"{"converter_offset_hz": 125000000.0}"#).expect("parses");
        assert_eq!(up.converter_offset_hz, 125_000_000.0);
    }

    /// Every `radio.json` written before this backend existed has to keep
    /// working, and has to land on a Pluto configuration that would actually
    /// open — the USB gadget's address, not an empty string.
    #[test]
    fn pluto_settings_default_for_every_older_config() {
        for json in [r#"{}"#, r#"{"backend": "Tci"}"#, r#"{"rx888": {"ppm": 1.5}}"#] {
            let cfg: RadioConfig = serde_json::from_str(json).expect("parses");
            assert_eq!(cfg.pluto.target(), PlutoConfig::DEFAULT_ADDRESS, "after loading {json}");
            assert_eq!(cfg.pluto.agc, PlutoAgc::SlowAttack);
        }
        // And the new variant round-trips by name, which is how `Backend` is
        // stored — appending it must not have renumbered anything.
        let pluto: RadioConfig = serde_json::from_str(r#"{"backend": "Pluto"}"#).expect("parses");
        assert_eq!(pluto.backend, Backend::Pluto);
        for b in Backend::ALL {
            let json = serde_json::to_string(&b).expect("serialises");
            assert_eq!(serde_json::from_str::<Backend>(&json).expect("round trip"), b);
        }
    }

    /// Every `radio.json` written before this backend existed has to keep
    /// working, and has to land on an SDRplay configuration that would
    /// actually open and hear something.
    #[test]
    fn sdrplay_settings_default_for_every_older_config() {
        for json in [r#"{}"#, r#"{"backend": "Pluto"}"#, r#"{"rx888": {"ppm": 1.5}}"#] {
            let cfg: RadioConfig = serde_json::from_str(json).expect("parses");
            assert_eq!(cfg.sdrplay.sample_rate_hz, 2_000_000.0, "after loading {json}");
            assert_eq!(cfg.sdrplay.agc, SdrPlayAgc::Hz50);
            assert!(!cfg.sdrplay.bias_tee, "no uninvited DC on the antenna after {json}");
        }
        // And the new variant round-trips by name, which is how `Backend` is
        // stored — appending it must not have renumbered anything.
        let sdrplay: RadioConfig =
            serde_json::from_str(r#"{"backend": "SdrPlay"}"#).expect("parses");
        assert_eq!(sdrplay.backend, Backend::SdrPlay);
        for b in Backend::ALL {
            let json = serde_json::to_string(&b).expect("serialises");
            assert_eq!(serde_json::from_str::<Backend>(&json).expect("round trip"), b);
        }
    }

    /// The AGC mode rides `SetGain` as the API's own numeric values, which are
    /// not in speed order — a hand-rolled "obvious" mapping here would set a
    /// different loop rate than the label says.
    #[test]
    fn sdrplay_agc_modes_survive_the_pseudo_gain_element_encoding() {
        for mode in SdrPlayAgc::ALL {
            assert_eq!(SdrPlayAgc::from_code(mode.code()), mode, "{}", mode.label());
        }
        // The API's numbering: 0 disable, 1 = 100 Hz, 2 = 50 Hz, 3 = 5 Hz.
        assert_eq!(SdrPlayAgc::Off.code(), 0.0);
        assert_eq!(SdrPlayAgc::Hz100.code(), 1.0);
        assert_eq!(SdrPlayAgc::Hz50.code(), 2.0);
        assert_eq!(SdrPlayAgc::Hz5.code(), 3.0);
        assert_eq!(SdrPlayAgc::from_code(99.0), SdrPlayAgc::Hz50);
    }

    /// The `hwVer` byte is the only thing that says which RSP is on the other
    /// end, and its numbering is historical rather than sequential — RSP1A is
    /// 255, RSP1B is 6, and 5 does not exist.
    #[test]
    fn sdrplay_models_decode_from_the_api_hw_ver() {
        assert_eq!(SdrPlayModel::from_hw_ver(1), SdrPlayModel::Rsp1);
        assert_eq!(SdrPlayModel::from_hw_ver(2), SdrPlayModel::Rsp2);
        assert_eq!(SdrPlayModel::from_hw_ver(3), SdrPlayModel::RspDuo);
        assert_eq!(SdrPlayModel::from_hw_ver(4), SdrPlayModel::RspDx);
        assert_eq!(SdrPlayModel::from_hw_ver(6), SdrPlayModel::Rsp1b);
        assert_eq!(SdrPlayModel::from_hw_ver(7), SdrPlayModel::RspDxR2);
        assert_eq!(SdrPlayModel::from_hw_ver(255), SdrPlayModel::Rsp1a);
        assert_eq!(SdrPlayModel::from_hw_ver(5), SdrPlayModel::Unknown);
        // Model-gated UI depends on these staying honest.
        assert!(!SdrPlayModel::Rsp1.has_bias_tee());
        assert!(SdrPlayModel::Rsp1b.has_dab_notch());
        assert!(!SdrPlayModel::Rsp1b.has_hdr());
        assert!(SdrPlayModel::RspDx.has_hdr());
        // Antenna lists: single-port models hide the selector entirely.
        assert!(SdrPlayModel::Rsp1b.antennas(SdrPlayDuoTuner::Tuner1).is_empty());
        assert_eq!(SdrPlayModel::Rsp2.antennas(SdrPlayDuoTuner::Tuner1).len(), 3);
        assert_eq!(SdrPlayModel::RspDuo.antennas(SdrPlayDuoTuner::Tuner1).len(), 2);
        assert!(SdrPlayModel::RspDuo.antennas(SdrPlayDuoTuner::Tuner2).is_empty());
    }

    /// An RSP enumerating with no serial (or a hwVer nothing decodes) is
    /// SDRplay's signature of a USB brownout or a wedged API service — a
    /// device that lists and streams but hears nothing. Field-reported on an
    /// RSP1B after broadband interference; every surface warns from this one
    /// predicate.
    #[test]
    fn an_rsp_without_an_identity_is_flagged_as_degraded() {
        let healthy = SdrPlayDevice { serial: "2405001234".into(), hw_ver: 6 };
        assert!(!healthy.identity_missing());
        assert!(healthy.identity_warning().is_none());

        let no_serial = SdrPlayDevice { serial: "  ".into(), hw_ver: 6 };
        assert!(no_serial.identity_missing());
        assert!(no_serial.identity_warning().unwrap().contains("no serial number"));

        let no_model = SdrPlayDevice { serial: "2405001234".into(), hw_ver: 0 };
        assert!(no_model.identity_missing());
        assert!(no_model.identity_warning().unwrap().contains("hardware version"));

        // Both point the operator at the same remedy.
        for d in [&no_serial, &no_model] {
            assert!(d.identity_warning().unwrap().contains("Restart the SDRplay API service"));
        }
    }

    /// SoapyAudio presents any sound card as an SDR that accepts every tune and
    /// ignores it. Field-reported: an RSP1A owner on a bundle install spent a
    /// session watching their line input, because "the first device found" was
    /// the sound card. Case folding is the part that is easy to get wrong — an
    /// enumeration says `audio` where the opened device says `Audio`.
    #[test]
    fn soapy_pseudo_drivers_are_recognised_whatever_their_case() {
        for d in ["audio", "Audio", "AUDIO", " audio ", "null", "Null"] {
            assert!(SoapyDeviceInfo::driver_is_pseudo(d), "{d} is not a radio");
            assert!(SoapyDeviceInfo::pseudo_warning(d, "Audio (Audio)").is_some());
        }
        for d in ["sdrplay", "rtlsdr", "hackrf", "lime", "uhd", "remote", ""] {
            assert!(!SoapyDeviceInfo::driver_is_pseudo(d), "{d} is real hardware");
            assert!(SoapyDeviceInfo::pseudo_warning(d, "x").is_none());
        }
        // The warning names the device and points somewhere useful.
        let w = SoapyDeviceInfo::pseudo_warning("Audio", "Audio (Audio)").unwrap();
        assert!(w.contains("Audio (Audio)") && w.contains("ignores the dial"));

        // Drivers with a native interface steer there; the rest stay on SoapySDR.
        assert_eq!(SoapyDeviceInfo::native_backend_for("sdrplay"), Some(Backend::SdrPlay));
        assert_eq!(SoapyDeviceInfo::native_backend_for("SDRplay"), Some(Backend::SdrPlay));
        assert_eq!(SoapyDeviceInfo::native_backend_for("rtlsdr"), Some(Backend::RtlSdr));
        assert_eq!(SoapyDeviceInfo::native_backend_for("plutosdr"), Some(Backend::Pluto));
        assert_eq!(SoapyDeviceInfo::native_backend_for("hackrf"), None);
        assert_eq!(SoapyDeviceInfo::native_backend_for("audio"), None);
    }

    /// A discovered radio and a typed address are two different things, and the
    /// typed one has to win — that is the whole reason both fields exist.
    #[test]
    fn a_typed_pluto_address_beats_a_discovered_one() {
        let mut cfg = PlutoConfig { address: String::new(), ..PlutoConfig::default() };
        assert_eq!(cfg.target(), PlutoConfig::DEFAULT_ADDRESS);
        cfg.selected_ip = Some("10.0.0.9".into());
        assert_eq!(cfg.target(), "10.0.0.9");
        cfg.address = "  pluto.local  ".into();
        assert_eq!(cfg.target(), "pluto.local");
    }

    /// The AGC mode rides `SetGain` as a number, so the encoding has to survive
    /// the round trip or the radio ends up in a mode nobody chose.
    #[test]
    fn agc_modes_survive_the_pseudo_gain_element_encoding() {
        for mode in PlutoAgc::ALL {
            assert_eq!(PlutoAgc::from_code(mode.code()), mode, "{}", mode.label());
        }
        // The IIO spellings are what goes on the wire; a typo here is a mode
        // the device rejects.
        assert_eq!(PlutoAgc::Manual.iio_name(), "manual");
        assert_eq!(PlutoAgc::SlowAttack.iio_name(), "slow_attack");
        assert_eq!(PlutoAgc::FastAttack.iio_name(), "fast_attack");
        assert_eq!(PlutoAgc::Hybrid.iio_name(), "hybrid");
        // Anything unrecognised lands on the safe default rather than manual,
        // which on an unknown band would be a deaf or overloaded receiver.
        assert_eq!(PlutoAgc::from_code(99.0), PlutoAgc::SlowAttack);
    }

    /// The sign is the whole feature. An upconverter moves the hardware *up*
    /// from the dial and a down-converter moves it down, and getting either
    /// backwards points the receiver twice the offset away from the signal.
    #[test]
    fn converter_presets_have_the_right_sign_and_size() {
        for (name, hz) in CONVERTER_PRESETS {
            assert!(hz.abs() <= CONVERTER_OFFSET_MAX_HZ, "{name} is outside the allowed range");
            assert_eq!(converter_preset_name(hz), name, "{name} should name itself");
        }
        // A Ham It Up presents 10.1008 MHz to the receiver as 135.1008 MHz.
        let ham = CONVERTER_PRESETS[1].1;
        assert_eq!(10_100_800.0 + ham, 135_100_800.0);
        // A universal LNB hands a 10.489 GHz downlink over at 739 MHz.
        let lnb = CONVERTER_PRESETS[3].1;
        assert_eq!(10_489_000_000.0 + lnb, 739_000_000.0);
        assert_eq!(converter_preset_name(0.0), "None");
        assert_eq!(converter_preset_name(28_000_000.0), "Manual");
    }

    /// The forms an operator will actually type, including one copied straight
    /// back out of the box below it.
    #[test]
    fn tuning_ranges_parse_the_way_they_are_written() {
        let two = parse_freq_ranges("144-146, 430-440").expect("parses");
        assert_eq!(two, vec![(144_000_000.0, 146_000_000.0), (430_000_000.0, 440_000_000.0)]);
        // Spaces, semicolons, en dashes, `..` and a unit are all the same list.
        for text in [
            " 144 - 146 ; 430 .. 440 ",
            "144\u{2013}146\n430-440",
            "144MHz-146MHz, 430 mhz - 440 mhz",
        ] {
            assert_eq!(parse_freq_ranges(text).expect(text), two, "parsing {text:?}");
        }
        // What the field shows is what the field accepts.
        assert_eq!(format_freq_ranges(&two), "144-146, 430-440");
        assert_eq!(parse_freq_ranges(&format_freq_ranges(&two)).unwrap(), two);
        // Down to the hertz, without trailing zeros on the round numbers.
        assert_eq!(format_freq_ranges(&[(10_100_805.0, 10_150_000.0)]), "10.100805-10.15");
        // Blank means "whatever the device says", not an error.
        assert_eq!(parse_freq_ranges("   ").unwrap(), vec![]);
        assert_eq!(format_freq_ranges(&[]), "");
    }

    /// Every rejection has to name what was typed: this is a field where a
    /// silent misreading would either hide bands or open ones the radio can't
    /// reach.
    #[test]
    fn nonsense_tuning_ranges_are_refused() {
        for bad in [
            "430",                   // not a range
            "430-",                  // half a range
            "440-430",               // backwards
            "430-430",               // empty
            "seven-eight",           // not numbers
            "430000000-44000000000", // Hz where megahertz was asked for
        ] {
            assert!(parse_freq_ranges(bad).is_err(), "{bad:?} should be refused");
        }
        // A good range in a bad list fails the whole list rather than being
        // quietly kept: half an entered limit is not a limit.
        assert!(parse_freq_ranges("144-146, oops").is_err());
    }

    /// A `radio.json` from before this setting existed has to keep behaving as
    /// it did, which means no ranges at all — the device's own answer stands.
    #[test]
    fn tuning_range_overrides_default_to_empty() {
        for json in [r#"{}"#, r#"{"backend": "Soapy"}"#, r#"{"converter_offset_hz": 0.0}"#] {
            let cfg: RadioConfig = serde_json::from_str(json).expect("parses");
            assert!(cfg.freq_ranges_rx.is_empty(), "rx ranges after loading {json}");
            assert!(cfg.freq_ranges_tx.is_empty(), "tx ranges after loading {json}");
        }
        let cfg: RadioConfig =
            serde_json::from_str(r#"{"freq_ranges_tx": [[430000000.0, 440000000.0]]}"#)
                .expect("parses");
        assert_eq!(cfg.freq_ranges_tx, vec![(430_000_000.0, 440_000_000.0)]);
    }

    #[test]
    fn hpsdr_defaults_round_trip() {
        let cfg = HpsdrConfig::default();
        let back: HpsdrConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn ppm_scales_frequency_proportionally() {
        assert_eq!(HpsdrConfig::apply_ppm(14_000_000.0, 0.0), 14_000_000.0);
        // +1 ppm at 14 MHz is +14 Hz.
        assert!((HpsdrConfig::apply_ppm(14_000_000.0, 1.0) - 14_000_014.0).abs() < 1e-6);
        assert!((HpsdrConfig::apply_ppm(14_000_000.0, -1.0) - 13_999_986.0).abs() < 1e-6);
    }
}
