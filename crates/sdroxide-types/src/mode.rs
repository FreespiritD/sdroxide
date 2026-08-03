use serde::{Deserialize, Serialize};

/// Demodulation / modulation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    Lsb,
    Usb,
    Cw,
    Am,
    Sam,
    Nfm,
    Wfm,
    Digu,
    Digl,
    Dsb,
    Spec,
    /// FT8 digital mode — USB underneath, decoded/encoded by the digi engine.
    Ft8,
    /// FT4 digital mode — USB underneath, decoded/encoded by the digi engine.
    Ft4,
    /// PSK31 keyboard mode — USB underneath, streaming BPSK31 decode/encode.
    Psk,
    /// RTTY keyboard mode — USB underneath, streaming FSK/Baudot decode/encode.
    Rtty,
    /// SSTV image mode — USB underneath, image decode/encode by the digi engine.
    Sstv,
    /// Olivia MFSK keyboard mode — USB underneath, tones/bandwidth chosen in setup.
    Olivia,
    /// THOR (DominoEX-family MFSK+FEC) keyboard mode — submode chosen in setup.
    Thor,
    /// FSQ (Fast Simple QSO) IFK keyboard mode — undirected/directed/image.
    Fsq,
    /// RF Paint (Spectrum Painting) — USB underneath; paints text/images
    /// directly onto the receiver's waterfall. Transmit-only (no decode).
    RfPaint,
    /// FreeDV RADE V1 (Radio Autoencoder) digital voice — USB underneath, a
    /// neural codec over an OFDM waveform occupying ~1000–1900 Hz of audio.
    Rade,
    /// Hellschreiber — USB underneath, a facsimile mode that paints a 7×14 dot
    /// matrix per character straight onto the channel. No sync, no framing, no
    /// decoder: the receiver free-runs and the operator's eye reads the raster.
    ///
    /// Appended last on purpose. `Mode` is postcard-encoded by declaration
    /// index and serde-serialised into stored configs, so a new variant may only
    /// go at the end. Where it *appears* is set by [`Mode::ALL`] instead.
    Hell,
    /// RIFP (Radio Image Framing Protocol, draft-dulaunoy-rifp-00) — a
    /// packetised image mode. Unlike every other digital mode here it is not
    /// USB underneath: the `rifp-cpfsk-4800` profile is continuous-phase FSK
    /// straight on the carrier, ±4 kHz at 4800 baud, so the dial *is* the
    /// signal's centre and the channel is ~25 kHz wide. Appended for the same
    /// reason as [`Mode::Hell`].
    Rifp,
    /// HF weather facsimile (WEFAX / radiofax) — USB underneath, an FM
    /// subcarrier carrying a continuous raster. Receive only: the charts are
    /// broadcast by meteorological services, and an amateur station has nothing
    /// to send back. Appended for the same reason as [`Mode::Hell`].
    Wefax,
    /// JS8 — the keyboard/messaging mode built on FT8's 8-FSK waveform. Slotted
    /// like FT8 but conversational rather than a contest exchange: free text,
    /// directed commands and heartbeats, at one of four speeds chosen in setup.
    /// Appended for the same reason as [`Mode::Hell`].
    Js8,
}

impl Mode {
    /// Every mode, in the order they cycle and appear in the picker — which is
    /// deliberately *not* the enum's declaration order (see [`Mode::Hell`]).
    pub const ALL: [Mode; 25] = [
        Mode::Lsb,
        Mode::Usb,
        Mode::Cw,
        Mode::Am,
        Mode::Sam,
        Mode::Nfm,
        Mode::Wfm,
        Mode::Digu,
        Mode::Digl,
        Mode::Dsb,
        Mode::Spec,
        Mode::Ft8,
        Mode::Ft4,
        Mode::Js8,
        Mode::Psk,
        Mode::Rtty,
        Mode::Sstv,
        Mode::Rifp,
        Mode::Wefax,
        Mode::Olivia,
        Mode::Thor,
        Mode::Fsq,
        Mode::Hell,
        Mode::RfPaint,
        Mode::Rade,
    ];

    /// The digital modes handled by a dedicated decode/encode engine (the
    /// slotted FT8/FT4 modes, the continuous keyboard modes, Hell, SSTV, RIFP,
    /// RF Paint). All are USB underneath except RIFP, which is FSK on the
    /// carrier.
    pub const DIGITAL: [Mode; 14] = [
        Mode::Ft8,
        Mode::Ft4,
        Mode::Js8,
        Mode::Psk,
        Mode::Rtty,
        Mode::Olivia,
        Mode::Thor,
        Mode::Fsq,
        Mode::Hell,
        Mode::Sstv,
        Mode::Rifp,
        Mode::Wefax,
        Mode::RfPaint,
        Mode::Rade,
    ];

    /// True for modes that use a dedicated decode/QSO layer over USB.
    pub fn is_digital(self) -> bool {
        matches!(
            self,
            Mode::Ft8
                | Mode::Ft4
                | Mode::Js8
                | Mode::Psk
                | Mode::Rtty
                | Mode::Sstv
                | Mode::Rifp
                | Mode::Olivia
                | Mode::Thor
                | Mode::Fsq
                | Mode::Hell
                | Mode::RfPaint
                | Mode::Rade
                | Mode::Wefax
        )
    }

    /// True for the modes whose transmit waveform is not single-sideband audio
    /// on the carrier, so the dial is the signal's centre rather than its lower
    /// edge. Only RIFP so far: its CPFSK profile keys the carrier itself.
    pub fn is_carrier_centered(self) -> bool {
        matches!(self, Mode::Rifp)
    }

    /// True for the continuous keyboard text modes (PSK31 / RTTY / Olivia / Thor
    /// / FSQ), as opposed to the slotted FT8/FT4 modes. Drives which decode
    /// engine + panel is used.
    pub fn is_text_modem(self) -> bool {
        matches!(self, Mode::Psk | Mode::Rtty | Mode::Olivia | Mode::Thor | Mode::Fsq)
    }

    /// True for the slotted FT8/FT4 modes, as opposed to the continuous
    /// keyboard modems and the image modes. Drives the decode-list / callsign
    /// overlays that only make sense for a slot-based decoder.
    pub fn is_slotted(self) -> bool {
        matches!(self, Mode::Ft8 | Mode::Ft4 | Mode::Js8)
    }

    /// True for JS8. Forks the digi panel to the conversation UI and uses its
    /// own controller: it is slotted like FT8 but carries a chat rather than a
    /// contest exchange, so the Tx1–Tx6 sequencer has nothing to say about it.
    pub fn is_js8(self) -> bool {
        matches!(self, Mode::Js8)
    }

    /// True for the FSQ mode (adds a directed-message / contacts / image layer
    /// on top of the plain keyboard-modem panel).
    pub fn is_fsq(self) -> bool {
        matches!(self, Mode::Fsq)
    }

    /// True for the SSTV image mode. Forks the digi panel to the image UI and
    /// skips the FT8/text-modem overlays.
    pub fn is_sstv(self) -> bool {
        matches!(self, Mode::Sstv)
    }

    /// True for the RIFP image mode. Shares SSTV's image panel (compose,
    /// transmit, gallery) over a packetised protocol and its own modem.
    pub fn is_rifp(self) -> bool {
        matches!(self, Mode::Rifp)
    }

    /// True for the modes that drive the image panel — a picture compositor on
    /// transmit, a live picture and a gallery on receive.
    pub fn is_image(self) -> bool {
        matches!(self, Mode::Sstv | Mode::Rifp)
    }

    /// True for HF weather fax. Its own panel rather than the image one: there
    /// is nothing to compose and nothing to transmit, and what it needs instead
    /// — line rate, index of cooperation, phasing and slant — has no counterpart
    /// in SSTV.
    pub fn is_wefax(self) -> bool {
        matches!(self, Mode::Wefax)
    }

    /// True for the receive-only modes, so the UI can leave the transmit
    /// controls out rather than showing ones that refuse.
    pub fn is_rx_only(self) -> bool {
        matches!(self, Mode::Wefax)
    }

    /// True for Hellschreiber. Forks the digi panel to the scrolling raster UI:
    /// unlike the keyboard modems there is nothing to decode into text, so it
    /// gets its own controller and panel rather than joining `is_text_modem`.
    pub fn is_hell(self) -> bool {
        matches!(self, Mode::Hell)
    }

    /// True for the RF Paint (Spectrum Painting) mode. Forks the digi panel to
    /// the text/image painting UI and uses its own transmit-only controller.
    pub fn is_rf_paint(self) -> bool {
        matches!(self, Mode::RfPaint)
    }

    /// True for FreeDV RADE V1 digital voice. Unlike the other digital modes it
    /// carries speech rather than text or images, so it both replaces the
    /// receive audio and consumes the microphone on transmit.
    pub fn is_rade(self) -> bool {
        matches!(self, Mode::Rade)
    }

    /// Whether the voice keyer may transmit in this mode.
    ///
    /// The digital modes synthesise their own transmit audio, so a recorded
    /// message has nowhere to go — RADE excepted: it carries speech, and takes
    /// the playback as its microphone input exactly like a live over.
    pub fn allows_voice_keyer(self) -> bool {
        !self.is_digital() || self.is_rade()
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Lsb => "LSB",
            Mode::Usb => "USB",
            Mode::Cw => "CW",
            Mode::Am => "AM",
            Mode::Sam => "SAM",
            Mode::Nfm => "NFM",
            Mode::Wfm => "WFM",
            Mode::Digu => "DIGU",
            Mode::Digl => "DIGL",
            Mode::Dsb => "DSB",
            Mode::Spec => "SPEC",
            Mode::Ft8 => "FT8",
            Mode::Ft4 => "FT4",
            Mode::Psk => "PSK",
            Mode::Rtty => "RTTY",
            Mode::Sstv => "SSTV",
            Mode::Olivia => "OLIVIA",
            Mode::Thor => "THOR",
            Mode::Fsq => "FSQ",
            Mode::Hell => "HELL",
            Mode::RfPaint => "RFPAINT",
            Mode::Rade => "RADE",
            Mode::Rifp => "RIFP",
            Mode::Wefax => "WEFAX",
            Mode::Js8 => "JS8",
        }
    }

    /// Default audio passband edges in Hz relative to the carrier/VFO.
    /// Negative frequencies are below the carrier (LSB side).
    pub fn default_filter(self) -> (f32, f32) {
        match self {
            Mode::Lsb => (-2850.0, -150.0),
            Mode::Usb => (150.0, 2850.0),
            // CW passband is centered on the sidetone pitch (default 700 Hz).
            Mode::Cw => (450.0, 950.0),
            Mode::Am | Mode::Sam => (-5000.0, 5000.0),
            Mode::Nfm => (-8000.0, 8000.0),
            Mode::Wfm => (-96_000.0, 96_000.0),
            Mode::Digu => (200.0, 3200.0),
            Mode::Digl => (-3200.0, -200.0),
            Mode::Dsb => (-2850.0, 2850.0),
            Mode::Spec => (-5000.0, 5000.0),
            // FT8/FT4 occupy the whole USB audio passband (tones 0..~3500 Hz).
            // PSK/RTTY/Olivia/Thor/FSQ/Hell do the same (the modem filters
            // narrowly around audio_hz — and Hell X9 needs nearly all of it).
            // SSTV occupies the full USB audio passband.
            Mode::Ft8
            | Mode::Ft4
            | Mode::Js8
            | Mode::Psk
            | Mode::Rtty
            | Mode::Sstv
            | Mode::Olivia
            | Mode::Thor
            | Mode::Fsq
            | Mode::Hell
            | Mode::RfPaint => (100.0, 3300.0),
            // The fax subcarrier is 1900 Hz ± 400; the wider passband leaves
            // room for a receiver tuned a few hundred hertz off, which is the
            // normal state of affairs on a chart found by ear.
            Mode::Wefax => (500.0, 3300.0),
            // RIFP is not a sideband mode: the CPFSK carrier sits *on* the
            // dial and swings ±4 kHz, so the passband straddles it. 25 kHz is
            // the profile's recommended occupied bandwidth.
            Mode::Rifp => (-12_500.0, 12_500.0),
            // RADE V1's OFDM carriers sit between roughly 1060 and 1880 Hz;
            // the wider passband leaves room for the acquisition search to
            // track a signal that is off frequency.
            Mode::Rade => (300.0, 2700.0),
        }
    }

    /// True for modes that place the displayed carrier below the passband.
    pub fn is_lower_sideband(self) -> bool {
        matches!(self, Mode::Lsb | Mode::Digl)
    }

    /// Furthest a filter edge may be dragged from the carrier — bounded by
    /// the mode's DSP channel bandwidth.
    pub fn max_filter_hz(self) -> f32 {
        match self {
            Mode::Wfm => 120_000.0,
            _ => 24_000.0,
        }
    }

    /// Filter width presets: (label, lo, hi) relative to the carrier.
    pub fn filter_presets(self) -> &'static [(&'static str, f32, f32)] {
        match self {
            Mode::Usb | Mode::Digu => &[
                ("1.8k", 200.0, 2000.0),
                ("2.4k", 200.0, 2600.0),
                ("2.7k", 150.0, 2850.0),
                ("3.3k", 100.0, 3400.0),
            ],
            Mode::Lsb | Mode::Digl => &[
                ("1.8k", -2000.0, -200.0),
                ("2.4k", -2600.0, -200.0),
                ("2.7k", -2850.0, -150.0),
                ("3.3k", -3400.0, -100.0),
            ],
            Mode::Cw => &[
                ("100", 650.0, 750.0),
                ("250", 575.0, 825.0),
                ("500", 450.0, 950.0),
                ("1k", 200.0, 1200.0),
            ],
            Mode::Am | Mode::Sam => {
                &[("6k", -3000.0, 3000.0), ("10k", -5000.0, 5000.0), ("16k", -8000.0, 8000.0)]
            }
            Mode::Nfm => &[("8k", -4000.0, 4000.0), ("16k", -8000.0, 8000.0)],
            Mode::Dsb => &[("5k", -2500.0, 2500.0), ("6k", -3000.0, 3000.0)],
            // Digital modes have a fixed wide passband; no presets.
            Mode::Wfm
            | Mode::Spec
            | Mode::Ft8
            | Mode::Ft4
            | Mode::Js8
            | Mode::Psk
            | Mode::Rtty
            | Mode::Sstv
            | Mode::Olivia
            | Mode::Thor
            | Mode::Fsq
            | Mode::Hell
            | Mode::RfPaint
            | Mode::Rifp
            | Mode::Wefax
            | Mode::Rade => &[],
        }
    }
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Mode::ALL
            .into_iter()
            .find(|m| m.label().eq_ignore_ascii_case(s))
            .ok_or_else(|| format!("unknown mode {s:?} (try USB, LSB, CW, AM, SAM, NFM, WFM…)"))
    }
}

/// Which denoiser is running behind the NR chip.
///
/// Derived from [`NrLevel`] rather than stored: the wire carries the level, so a
/// fifth engine would cost three appended `NrLevel` variants and nothing else.
/// This type is never serialised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NrEngine {
    /// RNNoise (`nnnoiseless`) — a recurrent per-band gain estimator.
    Rnn,
    /// DeepFilterNet3 (`deep_filter` / tract) — ERB gains plus a deep filter.
    DeepFilter,
    /// The spectral-bleach algorithm, ported to Rust in `sdroxide-dsp`.
    SpecBleach,
    /// The hand-written MCRA + log-MMSE spectral NR this program started with.
    Spectral,
}

impl NrEngine {
    /// Engine-row order in the NR picker: neural first, classical last.
    pub const ALL: [NrEngine; 4] =
        [NrEngine::Rnn, NrEngine::DeepFilter, NrEngine::SpecBleach, NrEngine::Spectral];

    /// The tag the chips wear. The original spectral NR keeps the bare "NR" it
    /// has always had, so an operator who never opens the picker sees exactly
    /// the chip they saw before.
    pub fn tag(self) -> &'static str {
        match self {
            NrEngine::Rnn => "RNN",
            NrEngine::DeepFilter => "DFNR",
            NrEngine::SpecBleach => "SPEC",
            NrEngine::Spectral => "NR",
        }
    }

    /// What the hover text calls it.
    pub fn name(self) -> &'static str {
        match self {
            NrEngine::Rnn => "RNNoise — neural, speech-trained, cheap",
            NrEngine::DeepFilter => "DeepFilterNet3 — neural, strongest, costliest",
            NrEngine::SpecBleach => "Spectral bleach — adaptive spectral, masked",
            NrEngine::Spectral => "Spectral NR — MCRA + log-MMSE",
        }
    }

    /// This engine at `strength`.
    pub fn at(self, s: NrStrength) -> NrLevel {
        use NrEngine::*;
        use NrStrength::*;
        match (self, s) {
            (Spectral, Low) => NrLevel::Low,
            (Spectral, Med) => NrLevel::Medium,
            (Spectral, High) => NrLevel::High,
            (Rnn, Low) => NrLevel::RnnLow,
            (Rnn, Med) => NrLevel::RnnMed,
            (Rnn, High) => NrLevel::RnnHigh,
            (SpecBleach, Low) => NrLevel::SpecLow,
            (SpecBleach, Med) => NrLevel::SpecMed,
            (SpecBleach, High) => NrLevel::SpecHigh,
            (DeepFilter, Low) => NrLevel::DfLow,
            (DeepFilter, Med) => NrLevel::DfMed,
            (DeepFilter, High) => NrLevel::DfHigh,
        }
    }

    /// The next engine in picker order, wrapping.
    pub fn next(self) -> NrEngine {
        let i = NrEngine::ALL.iter().position(|e| *e == self).unwrap_or(0);
        NrEngine::ALL[(i + 1) % NrEngine::ALL.len()]
    }
}

/// How hard whichever engine is selected is pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NrStrength {
    Low,
    Med,
    High,
}

impl NrStrength {
    pub const ALL: [NrStrength; 3] = [NrStrength::Low, NrStrength::Med, NrStrength::High];

    pub fn label(self) -> &'static str {
        match self {
            NrStrength::Low => "Low",
            NrStrength::Med => "Med",
            NrStrength::High => "High",
        }
    }
}

/// Audio noise-reduction setting for the demodulated audio: one of four engines
/// at one of three intensities, or off. See [`NrEngine`].
///
/// **The declaration order is the wire format.** postcard encodes the
/// discriminant positionally, so variants are only ever appended — the spectral
/// group sits where it always did (1..3), the RNNoise group where proto v10 put
/// it (4..6), and the two engines added in v43 follow. Nothing reads the
/// declaration order but the wire: [`NrLevel::ALL`] and the picker impose the
/// display order instead.
///
/// The RNNoise variants were called `Ai*` until v43. Renaming them cost nothing:
/// this enum reaches disk through no name-based format — it rides postcard and
/// is persisted nowhere else — so only the positions matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum NrLevel {
    #[default]
    Off,
    // Spectral NR (`SpectralNr`) — the original three, discriminants 1..3.
    Low,
    Medium,
    High,
    // RNNoise (`NeuralNr`) — appended in proto v10, discriminants 4..6.
    RnnLow,
    RnnMed,
    RnnHigh,
    // Spectral bleach (`SpecBleachNr`) — appended in v43, discriminants 7..9.
    SpecLow,
    SpecMed,
    SpecHigh,
    // DeepFilterNet3 (`DeepFilterNr`) — appended in v43, discriminants 10..12.
    DfLow,
    DfMed,
    DfHigh,
}

impl NrLevel {
    /// Every setting, in the order the picker lists them.
    pub const ALL: [NrLevel; 13] = [
        NrLevel::Off,
        NrLevel::RnnLow,
        NrLevel::RnnMed,
        NrLevel::RnnHigh,
        NrLevel::DfLow,
        NrLevel::DfMed,
        NrLevel::DfHigh,
        NrLevel::SpecLow,
        NrLevel::SpecMed,
        NrLevel::SpecHigh,
        NrLevel::Low,
        NrLevel::Medium,
        NrLevel::High,
    ];

    /// Suffix shown after "NR" on the chip (Off shows just "NR"). The original
    /// spectral NR keeps its bare "Low"/"Mid"/"High" — it is the one an operator
    /// may already have muscle memory for.
    pub fn label(self) -> &'static str {
        match self {
            NrLevel::Off => "Off",
            NrLevel::Low => "Low",
            NrLevel::Medium => "Mid",
            NrLevel::High => "High",
            NrLevel::RnnLow => "RNN Low",
            NrLevel::RnnMed => "RNN Med",
            NrLevel::RnnHigh => "RNN High",
            NrLevel::SpecLow => "SPEC Low",
            NrLevel::SpecMed => "SPEC Med",
            NrLevel::SpecHigh => "SPEC High",
            NrLevel::DfLow => "DFNR Low",
            NrLevel::DfMed => "DFNR Med",
            NrLevel::DfHigh => "DFNR High",
        }
    }

    pub fn is_on(self) -> bool {
        !matches!(self, NrLevel::Off)
    }

    /// Which denoiser this runs, or `None` when NR is off.
    pub fn engine(self) -> Option<NrEngine> {
        Some(match self {
            NrLevel::Off => return None,
            NrLevel::Low | NrLevel::Medium | NrLevel::High => NrEngine::Spectral,
            NrLevel::RnnLow | NrLevel::RnnMed | NrLevel::RnnHigh => NrEngine::Rnn,
            NrLevel::SpecLow | NrLevel::SpecMed | NrLevel::SpecHigh => NrEngine::SpecBleach,
            NrLevel::DfLow | NrLevel::DfMed | NrLevel::DfHigh => NrEngine::DeepFilter,
        })
    }

    /// How hard it is pushed, or `None` when NR is off.
    pub fn strength(self) -> Option<NrStrength> {
        Some(match self {
            NrLevel::Off => return None,
            NrLevel::Low | NrLevel::RnnLow | NrLevel::SpecLow | NrLevel::DfLow => NrStrength::Low,
            NrLevel::Medium | NrLevel::RnnMed | NrLevel::SpecMed | NrLevel::DfMed => {
                NrStrength::Med
            }
            NrLevel::High | NrLevel::RnnHigh | NrLevel::SpecHigh | NrLevel::DfHigh => {
                NrStrength::High
            }
        })
    }

    /// The same strength on a different engine. From `Off`, starts at Med — the
    /// level worth trying first on every one of them.
    pub fn with_engine(self, e: NrEngine) -> NrLevel {
        e.at(self.strength().unwrap_or(NrStrength::Med))
    }

    /// The same engine at a different strength. From `Off`, picks RNNoise: the
    /// cheapest engine that works on voice, and the one that needs no model.
    pub fn with_strength(self, s: NrStrength) -> NrLevel {
        self.engine().unwrap_or(NrEngine::Rnn).at(s)
    }

    /// One step for a button or a knob: Off → Low → Med → High → Off, *within
    /// the engine already selected*. A single control cannot usefully walk
    /// thirteen states, and the engine is a considered choice — something set
    /// once from the picker, not something a footswitch changes underfoot.
    ///
    /// From Off this starts on RNNoise; the level is the whole state, so
    /// switching off forgets which engine was on rather than adding a second
    /// field to the wire to remember it.
    pub fn next(self) -> NrLevel {
        match self.strength() {
            None => NrLevel::RnnLow,
            Some(NrStrength::Low) => self.with_strength(NrStrength::Med),
            Some(NrStrength::Med) => self.with_strength(NrStrength::High),
            Some(NrStrength::High) => NrLevel::Off,
        }
    }

    /// The next engine at the same strength, wrapping — the other half of what
    /// the picker offers, for a control surface with a button to spare.
    pub fn next_engine(self) -> NrLevel {
        match self.engine() {
            None => NrEngine::Rnn.at(NrStrength::Med),
            Some(e) => self.with_engine(e.next()),
        }
    }

    /// Spectral-NR tuning: `(noise over-estimation factor, minimum gain floor)`.
    /// A larger over-estimate removes more of the noise; a lower floor lets weak
    /// bins be attenuated further — more aggressive, at more risk of artefacts.
    /// The over-factors are modest because the MCRA estimator is unbiased (it
    /// tracks the noise mean, not an under-estimated minimum), so ~1.0 already
    /// removes stationary noise; higher values are pure over-subtraction.
    /// Neutral (unused) for Off and for every other engine.
    pub fn params(self) -> (f32, f32) {
        match self {
            NrLevel::Low => (1.0, 0.30),
            NrLevel::Medium => (1.4, 0.14),
            NrLevel::High => (2.0, 0.07),
            _ => (1.0, 1.0),
        }
    }

    /// Spectral-bleach tuning: `(reduction in dB, residue whitening 0..=1)`.
    /// Whitening flattens what is left so the residue reads as even hiss rather
    /// than as musical noise, which matters more the harder the reduction is
    /// pushed. 20 dB is the top: the algorithm will take 40, and on a radio
    /// signal that sounds like a swimming pool.
    pub fn spec_params(self) -> (f32, f32) {
        match self {
            NrLevel::SpecLow => (6.0, 0.00),
            NrLevel::SpecMed => (12.0, 0.15),
            NrLevel::SpecHigh => (20.0, 0.30),
            _ => (0.0, 0.0),
        }
    }

    /// RNNoise wet/dry depth (0 = bypass, 1 = full RNNoise). Only meaningful for
    /// the `Rnn*` variants.
    pub fn rnn_mix(self) -> f32 {
        match self {
            NrLevel::RnnLow => 0.55,
            NrLevel::RnnMed => 0.8,
            NrLevel::RnnHigh => 1.0,
            _ => 0.0,
        }
    }

    /// DeepFilterNet's attenuation limit, in dB — the most it may take out of
    /// any band. A limit rather than a wet/dry blend because it is the knob the
    /// network itself exposes: a capped mask still tracks the speech, where a
    /// dry blend at 45 % puts 45 % of the noise back with it.
    pub fn df_atten_db(self) -> f32 {
        match self {
            NrLevel::DfLow => 6.0,
            NrLevel::DfMed => 12.0,
            NrLevel::DfHigh => 24.0,
            _ => 0.0,
        }
    }

    /// Make-up gain applied to the listener audio after noise reduction:
    /// suppression lowers the overall level (more so at higher settings), so a
    /// progressively larger boost keeps the perceived loudness roughly constant.
    /// The neural engines preserve speech level far better than spectral
    /// subtraction, so their make-up is gentle — DeepFilterNet is trained to
    /// leave the speech where it found it and needs least of all.
    pub fn makeup_gain(self) -> f32 {
        match self {
            NrLevel::Off => 1.0,
            NrLevel::RnnLow => 1.0,
            NrLevel::RnnMed => 1.1,
            NrLevel::RnnHigh => 1.2,
            NrLevel::DfLow => 1.0,
            NrLevel::DfMed => 1.05,
            NrLevel::DfHigh => 1.15,
            NrLevel::SpecLow => 1.15,
            NrLevel::SpecMed => 1.4,
            NrLevel::SpecHigh => 1.7,
            NrLevel::Low => 1.3,
            NrLevel::Medium => 1.7,
            NrLevel::High => 2.1,
        }
    }
}

/// AGC behavior for a receiver channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgcMode {
    Off,
    Slow,
    Med,
    Fast,
}

impl AgcMode {
    pub const ALL: [AgcMode; 4] = [AgcMode::Off, AgcMode::Slow, AgcMode::Med, AgcMode::Fast];

    pub fn label(self) -> &'static str {
        match self {
            AgcMode::Off => "Off",
            AgcMode::Slow => "Slow",
            AgcMode::Med => "Med",
            AgcMode::Fast => "Fast",
        }
    }

    /// Cycle to the next setting: Off → Slow → Med → Fast → Off.
    pub fn next(self) -> AgcMode {
        match self {
            AgcMode::Off => AgcMode::Slow,
            AgcMode::Slow => AgcMode::Med,
            AgcMode::Med => AgcMode::Fast,
            AgcMode::Fast => AgcMode::Off,
        }
    }

    /// Hang time in milliseconds; `None` means AGC disabled.
    pub fn hang_ms(self) -> Option<f32> {
        match self {
            AgcMode::Off => None,
            AgcMode::Slow => Some(1000.0),
            AgcMode::Med => Some(500.0),
            AgcMode::Fast => Some(100.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire is the declaration order. Every discriminant that has ever been
    /// on the wire is pinned here, so a variant inserted rather than appended
    /// fails the build instead of silently renaming everyone's noise reduction.
    #[test]
    fn nr_discriminants_are_stable() {
        assert_eq!(NrLevel::Off as u8, 0);
        assert_eq!(NrLevel::Low as u8, 1);
        assert_eq!(NrLevel::Medium as u8, 2);
        assert_eq!(NrLevel::High as u8, 3);
        // Were AiLow/AiMed/AiHigh before proto v43; the rename is invisible to
        // postcard, the positions are not.
        assert_eq!(NrLevel::RnnLow as u8, 4);
        assert_eq!(NrLevel::RnnMed as u8, 5);
        assert_eq!(NrLevel::RnnHigh as u8, 6);
        assert_eq!(NrLevel::SpecLow as u8, 7);
        assert_eq!(NrLevel::SpecMed as u8, 8);
        assert_eq!(NrLevel::SpecHigh as u8, 9);
        assert_eq!(NrLevel::DfLow as u8, 10);
        assert_eq!(NrLevel::DfMed as u8, 11);
        assert_eq!(NrLevel::DfHigh as u8, 12);
    }

    #[test]
    fn nr_engine_and_strength_round_trip() {
        for l in NrLevel::ALL {
            let (Some(e), Some(s)) = (l.engine(), l.strength()) else {
                assert_eq!(l, NrLevel::Off);
                continue;
            };
            assert_eq!(e.at(s), l, "{l:?} did not round-trip through {e:?}/{s:?}");
        }
    }

    #[test]
    fn nr_all_lists_every_engine_at_every_strength_exactly_once() {
        assert_eq!(NrLevel::ALL.len(), 1 + NrEngine::ALL.len() * NrStrength::ALL.len());
        for e in NrEngine::ALL {
            for s in NrStrength::ALL {
                let want = e.at(s);
                assert_eq!(
                    NrLevel::ALL.iter().filter(|l| **l == want).count(),
                    1,
                    "{want:?} is not in ALL exactly once"
                );
            }
        }
    }

    /// A button walks four states and comes back, without leaving its engine.
    #[test]
    fn nr_next_stays_on_its_engine() {
        for e in NrEngine::ALL {
            let mut l = e.at(NrStrength::Low);
            for _ in 0..2 {
                l = l.next();
                assert_eq!(l.engine(), Some(e));
            }
            assert_eq!(l.next(), NrLevel::Off);
        }
    }

    #[test]
    fn nr_next_engine_holds_the_strength_and_wraps() {
        let mut l = NrEngine::ALL[0].at(NrStrength::High);
        for _ in 0..NrEngine::ALL.len() {
            assert_eq!(l.strength(), Some(NrStrength::High));
            l = l.next_engine();
        }
        assert_eq!(l, NrEngine::ALL[0].at(NrStrength::High), "next_engine did not wrap");
    }

    /// Reaching for a strength with NR off switches it on rather than doing
    /// nothing, and reaching for an engine keeps the strength you had.
    #[test]
    fn nr_from_off_picks_sensible_defaults() {
        assert_eq!(NrLevel::Off.with_strength(NrStrength::High), NrLevel::RnnHigh);
        assert_eq!(NrLevel::Off.with_engine(NrEngine::DeepFilter), NrLevel::DfMed);
        assert_eq!(NrLevel::SpecHigh.with_engine(NrEngine::Rnn), NrLevel::RnnHigh);
    }
}
