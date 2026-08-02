use serde::{Deserialize, Serialize};

use crate::{
    AgcMode, Band, DigiConfig, Direction, ImageKind, Mode, NetworkConfig, NrLevel, QsoStep,
    RigctldConfig, RxId, SatConfig, SkimmerSettings, SpectrumConfig, SstvMode, TciServerConfig,
    UploadTarget, Vfo, WsjtxConfig,
};

/// The single control vocabulary. The GUI, the WebSocket protocol, and the
/// future TCI server all speak `Command`; the DSP engine is its only consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    // VFO / tuning
    SetVfo {
        vfo: Vfo,
        hz: f64,
    },
    SelectVfo(Vfo),
    SwapVfos,
    CopyAtoB,
    SetSplit(bool),
    SetCenter(f64),
    SetSampleRate(f64),
    /// Engine applies band-stack recall (or the band default entry).
    SetBand(Band),

    // Receiver settings
    SetMode {
        rx: RxId,
        mode: Mode,
    },
    SetFilter {
        rx: RxId,
        lo: f32,
        hi: f32,
    },
    SetAgc {
        rx: RxId,
        agc: AgcMode,
    },
    SetAgcMaxGain {
        rx: RxId,
        db: f32,
    },
    /// Fixed audio gain used while the AGC is off.
    SetManualGain {
        rx: RxId,
        db: f32,
    },
    SetVolume {
        rx: RxId,
        v: f32,
    },
    SetMute {
        rx: RxId,
        muted: bool,
    },
    /// Squelch threshold in dBFS ([`crate::SQUELCH_OPEN_DB`] = open).
    SetSquelch {
        rx: RxId,
        db: f32,
    },
    SetNoiseBlanker(bool),
    /// Spectral audio noise-reduction intensity for a receiver.
    SetNoiseReduction {
        rx: RxId,
        level: NrLevel,
    },
    /// Adaptive auto-notch (constant-tone canceller) for a receiver.
    SetAutoNotch {
        rx: RxId,
        on: bool,
    },
    SetSubRx(bool),
    /// Park the sub receiver on an absolute frequency. The engine clamps it to
    /// the device passband — the sub is a DDC on the same IQ stream as the main
    /// receiver, so it can only reach what the hardware is already receiving.
    SetSubRxFreq(f64),
    SetRit {
        enabled: bool,
        hz: i32,
    },
    SetXit {
        enabled: bool,
        hz: i32,
    },
    /// Start (`true`) or stop (`false`) recording the receiver audio to an MP3
    /// file. The engine names the file (date/time/frequency/mode) and stores it
    /// in the user's music directory (or the config dir as a fallback).
    SetRecording(bool),

    // Transmit
    SetPtt(bool),
    SetTune(bool),
    SetTxDrive(f32),
    SetTuneDrive(f32),
    SetMicGain(f32),

    // Voice keyer (10 recorded messages; see [`crate::VoiceStatus`])
    /// Start recording the microphone into a slot (`Some`), or stop and store
    /// what has been recorded (`None`). Refused while transmitting.
    VoiceRecord(Option<u8>),
    /// Transmit a recorded message (`Some`), or stop one in progress (`None`).
    /// The engine keys the transmitter itself and unkeys at the end of the
    /// message. In RADE the recording is fed to the codec in place of the
    /// microphone; in the other digital modes the keyer is refused.
    VoicePlay(Option<u8>),
    /// Listen to a recorded message through the speakers without transmitting
    /// (`Some`), or stop the monitor (`None`). Refused while transmitting.
    VoicePreview(Option<u8>),
    /// Erase a slot's recording.
    VoiceClear(u8),
    /// Rename a slot (the label the UI and the bindings editor show).
    VoiceRename {
        slot: u8,
        name: String,
    },

    // Hardware
    SetGain {
        dir: Direction,
        element: String,
        db: f64,
    },
    SetAntenna {
        dir: Direction,
        name: String,
    },

    // Memories
    StoreMemory {
        name: String,
    },
    RecallMemory(u32),
    DeleteMemory(u32),

    // Display
    SetSpectrumCfg(SpectrumConfig),

    // Digital modes (FT8/FT4)
    SetDigiConfig(DigiConfig),
    /// Set our transmit tone offset within the passband (Hz).
    SetDigiAudioFreq(f32),
    /// Start calling CQ.
    DigiCallCq,
    /// Begin a QSO with a decoded station. `wait_for_cq` holds transmission
    /// until the station calls CQ (or calls us) — set when replying to a decode
    /// that is neither a CQ nor addressed to us, so we don't jump into an
    /// exchange already in progress.
    DigiStartQso {
        from: String,
        grid: Option<String>,
        snr: i16,
        audio_hz: f32,
        #[serde(default)]
        wait_for_cq: bool,
    },
    /// FT8/FT4: jump the exchange to this step, choosing by hand which message
    /// goes out next (WSJT-X's Tx1–Tx6). Steps that address a station are
    /// ignored when none is being worked.
    DigiSetStep(QsoStep),
    /// FT8/FT4: send this message verbatim in the next transmit slot, then
    /// carry on with the exchange. Empty text cancels one queued but unsent.
    DigiSendText(String),
    /// FT8/FT4: mark a station to work. Queued stations are taken in order, the
    /// next one starting as soon as the sequencer is free — so a run of callers
    /// can be marked in one pass over a busy slot and then worked hands-off.
    /// Adding a station already queued moves it to the end.
    DigiQueueAdd {
        from: String,
        grid: Option<String>,
        snr: i16,
        audio_hz: f32,
        /// Hold until they call CQ, as [`Command::DigiStartQso`] does.
        #[serde(default)]
        wait_for_cq: bool,
    },
    /// FT8/FT4: drop a station from the call queue. An empty callsign clears it.
    DigiQueueRemove(String),
    /// Gracefully stop the QSO sequence (finish the current burst, then idle).
    DigiStopQso,
    /// Abort any in-progress transmission immediately.
    DigiAbortTx,
    /// Continuous keyboard modes (PSK/RTTY): set the full outgoing text buffer.
    /// The engine keeps already-sent characters and streams the rest.
    DigiTxText(String),
    /// Continuous keyboard modes: enter (true) or leave (false) transmit.
    DigiTxActive(bool),
    /// SSTV: select the mode (also sizes the TX image). `None` = Auto — the RX
    /// auto-detects the mode and TX defaults to Martin 1.
    SstvSetMode(Option<SstvMode>),
    /// SSTV: transmit a composed image (PNG bytes) in the given mode. Keying
    /// starts immediately; `DigiAbortTx` stops it.
    SstvTx {
        mode: SstvMode,
        png: Vec<u8>,
    },
    /// Weather fax: begin a picture now, without waiting for a start tone.
    /// The usual way to catch a chart that was already running when you tuned.
    WefaxStart,
    /// Weather fax: end the picture in progress, keeping what has arrived.
    WefaxStop,
    /// Weather fax: shift the line alignment by whole pixels. Positive moves
    /// the picture right.
    WefaxNudge(i32),
    /// FSQ image: transmit a picture (PNG bytes; the engine grayscales/scales it).
    DigiImageTx {
        png: Vec<u8>,
    },
    /// RIFP: transmit a composed image (PNG bytes). The engine quantises it to
    /// the configured grayscale depth, encodes it as the configured
    /// content-encoding, and sends manifest + data + end frames. Keying starts
    /// immediately; `DigiAbortTx` stops it.
    RifpTx {
        png: Vec<u8>,
    },
    /// RIFP: drop an incomplete incoming session by its 16-hex-digit ID, or
    /// every session when the string is empty.
    RifpDropSession(String),

    // Skimmers
    /// Set which skimmers (CW / PSK / RTTY) run and how hard each squelches.
    SetSkimmerConfig(SkimmerSettings),

    // Network cockpit: spot feeds, lookups, uploads.
    /// Apply (and persist) the network-feature configuration: (re)connect the
    /// DX cluster, (dis)arm the POTA/SOTA/PSK feeds, and store credentials.
    SetNetworkConfig(NetworkConfig),
    /// The operator's current dial frequency, so band-scoped feeds (PSK
    /// Reporter) can query the right slice. Sent by the engine on VFO change.
    SpotDialHint(f64),
    /// Look up a callsign via the configured provider; the result comes back as
    /// [`crate::RadioEvent::CallsignResult`].
    LookupCallsign {
        call: String,
    },
    /// Upload one QSO's ADIF to the given targets; each result comes back as
    /// [`crate::RadioEvent::Upload`].
    UploadQso {
        qso_id: u64,
        adif: String,
        targets: Vec<UploadTarget>,
    },
    /// Download QSL confirmations from LoTW/eQSL and return the parsed
    /// confirmation records as [`crate::RadioEvent::Confirmations`].
    SyncConfirmations,

    /// Apply (and persist) the built-in TCI server configuration: bind, rebind
    /// or stop the listener that third-party TCI clients connect to. The result
    /// comes back as [`crate::RadioEvent::TciServerStatus`].
    SetTciServerConfig(TciServerConfig),

    /// Apply (and persist) the built-in Hamlib rigctld server configuration:
    /// bind, rebind or stop the listener that "NET rigctl" clients (WSJT-X,
    /// fldigi, N1MM, GPredict, …) connect to. The result comes back as
    /// [`crate::RadioEvent::RigctldStatus`].
    SetRigctldConfig(RigctldConfig),
    /// Apply (and persist) the WSJT-X UDP broadcast configuration: start, retarget
    /// or stop the datagram stream that GridTracker, JTAlert, N1MM+ and Log4OM
    /// listen to. Output only — nothing arrives on that socket.
    SetWsjtxConfig(WsjtxConfig),

    /// Allow WFM broadcast stereo on a receiver. `false` forces mono even when
    /// the 19 kHz pilot is locked — worth having for a noisy station, since the
    /// difference channel carries far more noise than the sum. No effect on any
    /// other mode. Appended rather than filed next to the other per-RX audio
    /// commands: postcard numbers variants by position.
    SetWfmStereo {
        rx: RxId,
        on: bool,
    },

    // Transmit-image presets and the received-picture stores. Appended for the
    // usual reason: postcard numbers variants by position.
    /// Store a picture in transmit preset `slot`, from the raw bytes of a
    /// picked file (PNG or JPEG). The engine scales the long edge down to
    /// [`crate::IMAGE_SOURCE_MAX_EDGE`] and keeps a PNG of that, so the store
    /// never holds a phone camera's forty megapixels and every client composes
    /// from identical pixels. The result comes back as
    /// [`crate::RadioEvent::ImagePresets`]; an upload that is too big or is not
    /// a picture is refused with a [`crate::RadioEvent::Notice`].
    ImageSetSlot {
        slot: u8,
        bytes: Vec<u8>,
    },
    /// Empty a preset's picture. The overlay message is the operator's text and
    /// stays — clearing a picture is replacing it, not forgetting what it said.
    ImageClearSlot(u8),
    /// Set the text composited over a preset's picture.
    ImageSetMessage {
        slot: u8,
        message: String,
    },
    /// Ask for a preset's stored source picture; the answer is
    /// [`crate::RadioEvent::ImageSlotSource`]. Requested lazily and cached
    /// against the slot's version: composition happens client-side, so the
    /// pixels only have to cross once per picture, not once per keystroke.
    ImageGetSlot(u8),
    /// List a received store, newest first: `count` entries (capped at
    /// [`crate::IMAGE_PAGE_MAX`]) starting at `offset`. Answered with
    /// [`crate::RadioEvent::ImageListing`].
    ImageList {
        kind: ImageKind,
        offset: u32,
        count: u32,
    },
    /// Fetch one received picture at full size, by the name a listing gave.
    /// Answered with [`crate::RadioEvent::ImageFile`].
    ImageGet {
        kind: ImageKind,
        name: String,
    },

    // The operator's satellite additions. Appended for the usual reason:
    // postcard numbers variants by position.
    /// Apply (and persist) the satellite configuration: pasted element sets,
    /// subscribed listings and frequency overrides. Answered with a fresh
    /// [`crate::RadioEvent::StationConfig`] and, since the subscription list
    /// may have changed, a fresh [`crate::RadioEvent::TleSubStatus`].
    ///
    /// It rides the engine rather than being written client-side because the
    /// tracker's listings are fetched — and cached — on the machine the engine
    /// runs on, which is the only one a browser client can reach.
    SetSatConfig(SatConfig),
    /// Re-fetch every enabled TLE subscription now, rather than waiting for the
    /// six-hourly cadence (the settings dialog's UPDATE NOW). One HTTPS round
    /// trip per subscription, off the engine thread; the outcome comes back as
    /// [`crate::RadioEvent::TleSubStatus`].
    RefreshTleSubs,

    // Culling a received store. Appended for the usual reason: postcard numbers
    // variants by position.
    /// Delete one received picture, by the name a listing gave it.
    ///
    /// The file goes, on the machine the radio is plugged into — a store fills
    /// up with noise-only frames and half-decoded charts, and the alternative is
    /// walking over to that machine with a file manager. The name is sanitised
    /// and resolved inside the store exactly as [`Command::ImageGet`]'s is: it
    /// arrives over a socket nothing authenticates and ends up at
    /// `std::fs::remove_file`, which is the one place in this program where
    /// getting that wrong costs something that cannot be got back.
    ///
    /// Answered with [`crate::RadioEvent::ImageDeleted`] to *every* attached
    /// client, since a picture that has gone is gone from all of their galleries;
    /// a delete that fails becomes a [`crate::RadioEvent::Notice`] instead.
    ImageDelete {
        kind: ImageKind,
        name: String,
    },

    /// Require a CTCSS tone or DCS code before the audio gate opens on this
    /// receiver, or `None` for plain carrier squelch. NFM only. Appended for the
    /// usual reason: postcard numbers variants by position.
    SetToneSquelch {
        rx: RxId,
        tone: Option<crate::SubTone>,
    },
}
