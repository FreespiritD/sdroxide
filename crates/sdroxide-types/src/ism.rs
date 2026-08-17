//! ISM-band decoder domain types, shared by the native engine, the wire
//! protocol, and the UI (native + WASM). Pure data + serde — the decoding lives
//! in the native `sdroxide-ism` crate.
//!
//! # Why a device table rather than a decode log
//!
//! The 868 MHz band is a room full of unattended transmitters repeating
//! themselves: a weather sensor every 48 s, a heat meter every few minutes, a
//! door contact whenever somebody walks through it. A chronological log of every
//! burst is mostly the same twenty devices over and over, and the question an
//! operator actually has is "what is around me, and what is it reading".
//!
//! So the engine keeps one [`IsmReport`] per device and re-sends the whole table
//! a couple of times a second, exactly as the skimmer re-sends its whole spot
//! set. The stable [`IsmReport::id`] lets the panel update a row in place, and
//! [`IsmReport::count`] plus [`IsmReport::last_at`] carry the history that a log
//! would otherwise have had to hold.

use serde::{Deserialize, Serialize};

/// The families of device the decoder can be asked to listen for.
///
/// This is the granularity the *operator* thinks in, and the granularity
/// [`IsmSettings::families`] switches. [`IsmProtocol`] is finer — it is what a
/// decode reports having found.
///
/// The discriminants are bit positions in [`IsmSettings::families`], so
/// variants may be **appended** but never reordered or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsmFamily {
    /// Weather and environment sensors: Fine Offset, LaCrosse, Bresser, TFA.
    Weather,
    /// Utility metering: wireless M-Bus water, heat, gas and electricity meters.
    Meter,
    /// Home automation: Z-Wave, Homematic, KNX-RF, EnOcean, FS20.
    HomeAuto,
    /// LoRa, recognised and labelled but never demodulated.
    LoRa,
}

impl IsmFamily {
    /// Every family, in UI order.
    pub const ALL: [IsmFamily; 4] =
        [IsmFamily::Weather, IsmFamily::Meter, IsmFamily::HomeAuto, IsmFamily::LoRa];

    /// The chip's label.
    pub fn label(self) -> &'static str {
        match self {
            IsmFamily::Weather => "WEATHER",
            IsmFamily::Meter => "METERS",
            IsmFamily::HomeAuto => "HOME",
            IsmFamily::LoRa => "LORA",
        }
    }

    /// What the hover text says the family covers.
    pub fn hint(self) -> &'static str {
        match self {
            IsmFamily::Weather => "Weather and soil sensors — Fine Offset, LaCrosse IT+",
            IsmFamily::Meter => "Wireless M-Bus water, heat, gas and electricity meters",
            IsmFamily::HomeAuto => "Z-Wave, Homematic, KNX-RF, EnOcean, FS20",
            IsmFamily::LoRa => "Recognise LoRa chirps and report their parameters",
        }
    }

    /// Bit position in [`IsmSettings::families`].
    pub fn bit(self) -> u32 {
        1 << (self as u32)
    }

    /// Whether any protocol in this family is implemented yet.
    ///
    /// The chips for the rest are drawn disabled rather than hidden: the band is
    /// full of these devices whether or not sdroxide can read them, and an
    /// operator who can see that meters are "not yet" is better informed than
    /// one who sees nothing and concludes there are none.
    pub fn implemented(self) -> bool {
        matches!(self, IsmFamily::Weather)
    }
}

/// Which protocol a report was decoded with.
///
/// Variants may be **appended** but never reordered — postcard encodes the
/// discriminant positionally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsmProtocol {
    /// LaCrosse "IT+" TX29 / TX35 and the Conrad and TFA units that are the same
    /// radio in another case.
    LaCrosseIt,
    /// Fine Offset Electronics sensors, sold as Ecowitt, Froggit, Ambient
    /// Weather, SwitchDoc and Misol among others.
    FineOffset,
    /// Bresser weather centres. Same channel and sync word as the Fine Offset
    /// family, at less than half the symbol rate.
    Bresser,
}

impl IsmProtocol {
    /// Every protocol, in UI order.
    pub const ALL: [IsmProtocol; 3] =
        [IsmProtocol::LaCrosseIt, IsmProtocol::FineOffset, IsmProtocol::Bresser];

    pub fn label(self) -> &'static str {
        match self {
            IsmProtocol::LaCrosseIt => "LaCrosse IT+",
            IsmProtocol::FineOffset => "Fine Offset",
            IsmProtocol::Bresser => "Bresser",
        }
    }

    /// Which family switch turns this protocol on.
    pub fn family(self) -> IsmFamily {
        match self {
            IsmProtocol::LaCrosseIt | IsmProtocol::FineOffset | IsmProtocol::Bresser => {
                IsmFamily::Weather
            }
        }
    }
}

/// A physical quantity a decoded frame can carry.
///
/// A closed enum rather than a free-text name so that the unit, the sensible
/// number of decimals, and the ordering in a row are decided once, here, instead
/// of in every protocol module and again in the UI. Variants may be
/// **appended**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsmQuantity {
    TempC,
    HumidityPct,
    WindAvgMs,
    WindGustMs,
    WindDirDeg,
    RainMm,
    PressureHpa,
    UvIndex,
    LuxLx,
    SoilMoisturePct,
    BatteryPct,
    BatteryVolts,
    /// Distance to the nearest detected lightning strike. These sensors are
    /// AS3935 Franklin front ends: the figure is a storm-energy estimate binned
    /// into a few kilometres, not a range measurement.
    LightningKm,
    /// Strikes counted since the sensor was last reset. A running total, so what
    /// matters is that it went up.
    StrikeCount,
}

impl IsmQuantity {
    /// Short name for a panel cell.
    pub fn label(self) -> &'static str {
        match self {
            IsmQuantity::TempC => "temp",
            IsmQuantity::HumidityPct => "hum",
            IsmQuantity::WindAvgMs => "wind",
            IsmQuantity::WindGustMs => "gust",
            IsmQuantity::WindDirDeg => "dir",
            IsmQuantity::RainMm => "rain",
            IsmQuantity::PressureHpa => "press",
            IsmQuantity::UvIndex => "uv",
            IsmQuantity::LuxLx => "light",
            IsmQuantity::SoilMoisturePct => "soil",
            IsmQuantity::BatteryPct => "batt",
            IsmQuantity::BatteryVolts => "batt",
            IsmQuantity::LightningKm => "strike",
            IsmQuantity::StrikeCount => "strikes",
        }
    }

    /// The unit the value is in. Empty for dimensionless quantities.
    pub fn unit(self) -> &'static str {
        match self {
            IsmQuantity::TempC => "°C",
            IsmQuantity::HumidityPct | IsmQuantity::SoilMoisturePct | IsmQuantity::BatteryPct => {
                "%"
            }
            IsmQuantity::WindAvgMs | IsmQuantity::WindGustMs => "m/s",
            IsmQuantity::WindDirDeg => "°",
            IsmQuantity::RainMm => "mm",
            IsmQuantity::PressureHpa => "hPa",
            IsmQuantity::UvIndex | IsmQuantity::StrikeCount => "",
            IsmQuantity::LuxLx => "lx",
            IsmQuantity::BatteryVolts => "V",
            IsmQuantity::LightningKm => "km",
        }
    }

    /// Decimals worth showing. More than the sensor resolves is noise dressed as
    /// precision — these devices report tenths of a degree and whole percent.
    pub fn decimals(self) -> usize {
        match self {
            IsmQuantity::TempC
            | IsmQuantity::WindAvgMs
            | IsmQuantity::WindGustMs
            | IsmQuantity::UvIndex => 1,
            IsmQuantity::RainMm | IsmQuantity::BatteryVolts => 2,
            IsmQuantity::HumidityPct
            | IsmQuantity::WindDirDeg
            | IsmQuantity::PressureHpa
            | IsmQuantity::LuxLx
            | IsmQuantity::SoilMoisturePct
            | IsmQuantity::BatteryPct
            | IsmQuantity::LightningKm
            | IsmQuantity::StrikeCount => 0,
        }
    }
}

/// One measurement out of a decoded frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IsmReading {
    pub quantity: IsmQuantity,
    pub value: f64,
}

impl IsmReading {
    pub fn new(quantity: IsmQuantity, value: f64) -> IsmReading {
        IsmReading { quantity, value }
    }

    /// `"21.3 °C"` — the value at its quantity's precision, with the unit.
    pub fn fmt_value(&self) -> String {
        let unit = self.quantity.unit();
        let n = format!("{:.*}", self.quantity.decimals(), self.value);
        if unit.is_empty() { n } else { format!("{n} {unit}") }
    }

    /// `"temp 21.3 °C"` — as it appears in a panel row.
    pub fn fmt_labelled(&self) -> String {
        format!("{} {}", self.quantity.label(), self.fmt_value())
    }
}

/// The latest report from one device, plus how often it has been heard.
///
/// One of these per device, not per burst — see the module note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IsmReport {
    /// Stable for the life of a session, derived from the protocol and the
    /// device's own identity, so a panel row keeps its place as the device
    /// repeats.
    pub id: u64,
    pub protocol: IsmProtocol,
    /// Model or variant where the protocol distinguishes them — `"WH51"`,
    /// `"TX35DTH-IT"`. `None` when the frame does not say.
    pub model: Option<String>,
    /// The device's identity as the protocol expresses it: a sensor ID, a meter
    /// serial, a node address. Already formatted for display.
    pub device: String,
    /// Where the burst was, measured from the signal rather than assumed from
    /// the channel it was found on.
    pub freq_hz: f64,
    /// Signal-to-noise of the burst that produced this report, dB.
    pub snr_db: i16,
    /// Unix seconds when this device was first heard this session.
    pub first_at: i64,
    /// Unix seconds of the report the values come from.
    pub last_at: i64,
    /// Frames accepted from this device this session. A device heard once may
    /// be a lucky CRC pass; one heard fifty times is really there.
    pub count: u32,
    pub readings: Vec<IsmReading>,
    /// Anything that is not a numbered quantity: battery state, message type,
    /// status flags. Pre-formatted `(name, value)` pairs.
    pub extra: Vec<(String, String)>,
    /// The frame validated but its payload is encrypted, so `readings` is empty
    /// and only the identity is known. sdroxide holds no keys.
    pub encrypted: bool,
    /// The validated frame as hex, for identifying a device the decoder only
    /// half understands.
    pub raw_hex: String,
}

impl IsmReport {
    /// The readings joined for a single-line cell.
    pub fn fmt_readings(&self) -> String {
        if self.encrypted && self.readings.is_empty() {
            return "encrypted".to_string();
        }
        let mut parts: Vec<String> = self.readings.iter().map(|r| r.fmt_labelled()).collect();
        parts.extend(self.extra.iter().map(|(k, v)| format!("{k} {v}")));
        parts.join("  ")
    }

    /// What the panel calls this device: the model if the frame named one, else
    /// the protocol.
    pub fn fmt_kind(&self) -> String {
        match &self.model {
            Some(m) => format!("{} {m}", self.protocol.label()),
            None => self.protocol.label().to_string(),
        }
    }
}

/// Default for [`IsmSettings::threshold_db`].
///
/// A burst has to stand this far above the channel's own noise floor before the
/// decoder will spend anything on it. 12 dB is below where any of these
/// protocols still decode, so the threshold is not what limits sensitivity — it
/// is what stops the gate opening on noise and handing the slicers garbage.
pub const ISM_THRESHOLD_DB_DEFAULT: i16 = 12;

/// Default for [`IsmSettings::max_devices`].
pub const ISM_MAX_DEVICES_DEFAULT: u16 = 200;

/// Which ISM decoders run, and how hard they squelch. Owned by the engine (it
/// lives in [`crate::RadioState`]), edited from the ISM window, and persisted
/// across restarts (`ism.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IsmSettings {
    /// Whether the ISM decoder runs at all. Off costs no DSP and no thread.
    pub enabled: bool,
    /// Minimum SNR (dB) a burst must reach before it is decoded.
    pub threshold_db: i16,
    /// Which families listen, as [`IsmFamily::bit`] flags.
    ///
    /// A bitmask rather than the `[bool; N]` that [`crate::SkimmerSettings`]
    /// uses: this struct is a field of [`crate::RadioState`], so it is in every
    /// state broadcast, and postcard is not self-describing. A fixed-length
    /// array would change the wire layout of all of them every time a family was
    /// added — which is exactly what the later protocol phases do.
    pub families: u32,
    /// Most devices remembered; the least recently heard are dropped past this.
    pub max_devices: u16,
}

impl Default for IsmSettings {
    fn default() -> Self {
        IsmSettings {
            enabled: false,
            threshold_db: ISM_THRESHOLD_DB_DEFAULT,
            families: IsmFamily::Weather.bit(),
            max_devices: ISM_MAX_DEVICES_DEFAULT,
        }
    }
}

impl IsmSettings {
    /// Nothing running — what the engine substitutes for a front end with no
    /// wideband IQ to work from. The rest of the settings keep their values so
    /// an operator swapping to such a radio and back finds them where they were.
    pub const OFF: IsmSettings = IsmSettings {
        enabled: false,
        threshold_db: ISM_THRESHOLD_DB_DEFAULT,
        families: 0,
        max_devices: ISM_MAX_DEVICES_DEFAULT,
    };

    pub fn family_enabled(&self, f: IsmFamily) -> bool {
        self.families & f.bit() != 0
    }

    pub fn set_family(&mut self, f: IsmFamily, on: bool) {
        if on {
            self.families |= f.bit();
        } else {
            self.families &= !f.bit();
        }
    }

    pub fn protocol_enabled(&self, p: IsmProtocol) -> bool {
        self.family_enabled(p.family())
    }

    /// True while the decoder should be running: switched on, and with at least
    /// one family to listen for.
    pub fn any_enabled(&self) -> bool {
        self.enabled && self.families != 0
    }
}

/// Where the decoder is listening, for the panel to show.
///
/// The ISM channels are fixed but the receiver's window is not: it is as wide as
/// the front end gives and wherever the operator has tuned. A channel outside
/// that window cannot be decoded, and saying which ones are live is the
/// difference between "no meters here" and "you are not listening to the meter
/// channel".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IsmChannelStatus {
    pub freq_hz: f64,
    /// What sits on this channel, for the tooltip.
    pub label: String,
    /// Whether a decoder is actually running on this channel.
    pub live: bool,
    /// Why it is not, when it is not — "outside the receiver's window" and
    /// "no family enabled listens here" are very different problems and the
    /// operator can only fix them by being told which one it is.
    pub reason: Option<String>,
}

/// What the engine tells the panel about the decoder's own state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IsmStatus {
    /// Every channel in the plan, and whether it is reachable right now.
    pub channels: Vec<IsmChannelStatus>,
    /// Why nothing is running, when nothing is running.
    pub unavailable: Option<String>,
    /// Where the operator would have to tune for the whole plan to fit. `None`
    /// when it already does.
    pub suggest_center_hz: Option<f64>,
    /// Bursts the gate opened on since the decoder started, and how many of
    /// those produced a valid frame. A high count with no decodes is the band
    /// being busy with something unsupported — worth showing rather than
    /// leaving the panel looking broken.
    pub bursts: u64,
    pub decodes: u64,
}
