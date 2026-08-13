use crate::{
    CallsignInfo, Command, Decode, DeviceCaps, DigiStatus, MemoryChannel, MemoryFolder, Meters,
    QsoRecord, RadioState, RifpMeta, RifpStatus, SkimmerSpot, SpectrumFrame, Spot, SstvMode,
    SstvStatus, UploadResult, VoiceStatus,
};

/// Events flowing engine → UI.
#[derive(Debug, Clone, PartialEq)]
pub enum RadioEvent {
    Capabilities(DeviceCaps),
    /// Full state snapshot on any change (latest-wins).
    State(RadioState),
    Spectrum(SpectrumFrame),
    /// Full-band spectrum from a front end that sees far more than the IQ it
    /// delivers — the RX-888's whole 0–32 MHz. Only ever sent by sources that
    /// have one; every other backend leaves this lane silent.
    WideSpectrum(SpectrumFrame),
    Meters(Meters),
    Memories(Vec<MemoryChannel>),
    /// The memory folders, announced beside [`RadioEvent::Memories`] at startup
    /// and after every change. A separate event rather than a field of it:
    /// which folder a memory sits in rides in the [`MemoryChannel`] itself, so
    /// the two lists change independently.
    MemoryFolders(Vec<MemoryFolder>),
    ConnectionLost(String),
    /// A non-fatal, persistent status/warning for the operator (e.g. the radio
    /// audio input was unavailable or a mono card was selected for IQ). `None`
    /// clears it. Native-local only — not forwarded to remote clients.
    Notice(Option<String>),
    /// FT8/FT4 decodes from one receive slot.
    Ft8Decodes(Vec<Decode>),
    /// FT8/FT4 engine status change.
    Ft8Status(DigiStatus),
    /// A completed QSO, appended to the session log.
    Ft8QsoLogged(QsoRecord),
    /// Latest set of skimmer spots (CW etc.).
    SkimmerSpots(Vec<SkimmerSpot>),
    /// SSTV: one freshly decoded scanline `rgb` (3·width bytes) at row `y` of the
    /// image identified by `image_id`. Paints progressively.
    SstvLine {
        image_id: u32,
        y: u16,
        rgb: Vec<u8>,
    },
    /// SSTV: a completed image (PNG bytes) plus its identity and size.
    SstvImage {
        image_id: u32,
        mode: SstvMode,
        w: u16,
        h: u16,
        png: Vec<u8>,
    },
    /// SSTV: engine status change (tx/rx active, detected mode, progress).
    SstvStatus(SstvStatus),
    /// Weather fax: one freshly decoded scan line, one byte per pixel.
    WefaxLine {
        image_id: u32,
        y: u16,
        gray: Vec<u8>,
    },
    /// Weather fax: a completed chart (PNG bytes). The height is however many
    /// lines arrived before the stop tone, so it is carried rather than implied.
    WefaxImage {
        image_id: u32,
        w: u16,
        h: u16,
        png: Vec<u8>,
    },
    /// Weather fax: receiver status (tuning, phasing, line count).
    WefaxStatus(crate::WefaxStatus),
    /// RIFP: freshly reassembled raster rows of an incoming picture — `rows`
    /// grayscale bytes starting at row `y`, `w` bytes per row. Only the raster
    /// content-encodings can be painted before the object completes; a PNG,
    /// JPEG or facsimile object arrives as one [`RadioEvent::RifpImage`].
    RifpRows {
        image_id: u32,
        y: u16,
        /// Picture size, so a panel can size its canvas from the first row it
        /// sees rather than waiting for the manifest to reach it too.
        w: u16,
        h: u16,
        rows: Vec<u8>,
    },
    /// RIFP: a completed, digest-verified image (PNG bytes) plus what the
    /// manifest said about it.
    RifpImage {
        image_id: u32,
        meta: RifpMeta,
        png: Vec<u8>,
    },
    /// RIFP: engine status change (transfer progress, sessions, counters).
    RifpStatus(RifpStatus),
    /// FSQ image: a completed received picture (PNG bytes).
    DigiImage {
        png: Vec<u8>,
    },
    /// Hellschreiber: a run of freshly received dot columns, batched since the
    /// last poll. `cols` is column-major (`cols[c * rows + r]`, row 0 at the
    /// top), 0 = black … 255 = white.
    ///
    /// `seq` is the absolute index of the first column since the receiver
    /// started. Hell has no framing to resynchronise against, so the panel uses
    /// it to tell a dropped batch (leave a gap of the right width) from a
    /// restarted receiver (`seq` going backwards — clear the raster).
    HellColumns {
        seq: u64,
        rows: u8,
        cols: Vec<u8>,
    },
    /// Voice keyer: slot contents plus what is being recorded or transmitted.
    /// Emitted on every change and, while one of the two runs, often enough for
    /// the UI to animate a position.
    VoiceStatus(VoiceStatus),
    /// Latest set of network spots (DX cluster / POTA / SOTA / PSK Reporter),
    /// merged and de-duplicated by the spot manager.
    Spots(Vec<Spot>),
    /// A human-readable status line for the spot feeds (cluster connection
    /// state, feed errors). `None` clears it.
    NetStatus(Option<String>),
    /// Result of a [`Command::LookupCallsign`].
    CallsignResult(CallsignInfo),
    /// Result of one QSO/target upload from a [`Command::UploadQso`].
    Upload(UploadResult),
    /// Parsed QSL-confirmation records from a [`Command::SyncConfirmations`];
    /// the UI matches these against the log to set the `*_rcvd` flags.
    Confirmations(Vec<QsoRecord>),
    /// Built-in TCI server status: whether the listener is up, where it is
    /// bound, how many third-party clients are connected, and any bind error.
    /// Emitted on every transition, so the settings dialog never polls.
    TciServerStatus {
        running: bool,
        addr: String,
        clients: usize,
        error: Option<String>,
    },
    /// Built-in rigctld server status: whether the listener is up, where it is
    /// bound, how many clients are connected, and any bind error. Emitted on
    /// every transition, so the settings dialog never polls.
    RigctldStatus {
        running: bool,
        addr: String,
        clients: usize,
        error: Option<String>,
    },
    /// The transmit-image presets: what is in each slot, and the message
    /// composited over it. Emitted once at startup and on every change, like
    /// the voice keyer, because the pictures a station sends belong to the
    /// radio rather than to whichever screen is attached to it.
    ImagePresets(crate::ImagePresets),
    /// A preset's stored source picture, answering [`Command::ImageGetSlot`].
    /// `png` is empty for an empty slot. `version` is the fingerprint the
    /// picture had when it was read, so a client that asked during a change
    /// knows which one it got.
    ImageSlotSource {
        slot: u8,
        version: u32,
        png: Vec<u8>,
    },
    /// One page of a received store, answering [`Command::ImageList`].
    ImageListing(crate::ImageListing),
    /// One received picture at full size, answering [`Command::ImageGet`].
    /// An empty `png` means the store does not have it — a gallery that is
    /// waiting has to be able to stop waiting.
    ImageFile {
        kind: crate::ImageKind,
        name: String,
        png: Vec<u8>,
    },
    /// A freshly received picture has been stored. Carries the same entry a
    /// listing would, so a gallery prepends it without asking for the page
    /// again — and, unlike a listing, it is the only place a RIFP manifest
    /// survives.
    ImageSaved(crate::ImageEntry),
    /// Everything the engine host persists on the station's behalf — the
    /// network cockpit, the two built-in servers, the WSJT-X broadcast and the
    /// satellite additions. Emitted once at startup and after every change, so
    /// a settings dialog is populated from what the *station* is actually set
    /// to rather than from whatever file happens to sit beside the screen.
    ///
    /// Boxed: the bundle is by far the largest event here, and every other
    /// variant would otherwise pay for it.
    StationConfig(Box<crate::StationConfig>),
    /// What each TLE subscription's last fetch did. Emitted with the config it
    /// describes and again after a [`crate::Command::RefreshTleSubs`].
    TleSubStatus(Vec<crate::TleSubStatus>),
    /// A received picture is no longer in the store, answering
    /// [`Command::ImageDelete`].
    ///
    /// Broadcast rather than returned to whoever asked: the store belongs to the
    /// radio, and a gallery on another screen showing a thumbnail of a file that
    /// no longer exists would only find out by opening it.
    ImageDeleted {
        kind: crate::ImageKind,
        name: String,
    },
    /// What a WSPR slot decoded — or, from the WSPRnet feed, who decoded us.
    ///
    /// Its own event rather than [`RadioEvent::Ft8Decodes`]: WSPR reports a
    /// path, not a message, and the transmit power and drift that make it a
    /// measurement have nowhere to live in a [`Decode`].
    WsprSpots(Vec<crate::WsprSpot>),
    /// The scanner's settings, after a change or on connect — the same contract
    /// as [`RadioEvent::Memories`]. What the scanner is *doing* rides in
    /// [`crate::RadioState::scan`] instead, being small and constantly changing.
    Scanner(crate::ScannerConfig),
    /// Paths a spotting network demonstrated between two other stations —
    /// the Reverse Beacon Network's skimmers.
    ///
    /// The one event here that is deliberately **not** forwarded to a remote
    /// client. A few hundred of these arrive every five seconds, which is two
    /// orders of magnitude more traffic than the spot list, and the useful form
    /// of them is not the paths but the field they add up to. The engine host
    /// folds them into its own store and the solar relay ships the *result* —
    /// a handful of `BandPlane`s — to every viewer, which is both smaller and
    /// the thing a map actually draws.
    PropPaths(Vec<crate::PropPath>),
    /// What the satellite lock is doing: look angles, range, the Doppler
    /// corrections as applied. `Some` a couple of times a second while a lock
    /// is active, `None` once when it is released. Appended last, for the
    /// usual reason.
    SatTrack(Option<Box<crate::SatTrackStatus>>),
    /// The rotctld client's health: whether the daemon is reachable and where
    /// it reports the antenna pointing. Emitted on transitions and slowly
    /// while connected — the commanded target rides
    /// [`RadioEvent::SatTrack`] instead, this is what the hardware answered.
    RotatorStatus {
        connected: bool,
        az_deg: f64,
        el_deg: f64,
        error: Option<String>,
    },
}

/// Snapshot of the frontend's switchable sound devices (native clients).
/// `selected_*` of `None` means "system default".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioDevices {
    pub outputs: Vec<String>,
    pub inputs: Vec<String>,
    pub selected_output: Option<String>,
    pub selected_input: Option<String>,
}

/// The seam that lets the same UI run against an in-process radio engine
/// (native GUI) or a WebSocket session (WASM remote client).
pub trait RadioController {
    fn send(&mut self, cmd: Command);

    /// Non-blocking; the UI drains this each frame until `None`.
    fn poll_event(&mut self) -> Option<RadioEvent>;

    /// Microphone audio, 48 kHz mono. The local implementation is a no-op
    /// because cpal feeds the engine directly.
    fn send_mic(&mut self, _pcm_48k_mono: &[f32]) {}

    /// Whether the UI should schedule a repaint soon (fresh data pending).
    fn wants_repaint_soon(&self) -> bool {
        false
    }

    /// Whether this controller can be re-established after the link to the
    /// radio dropped, i.e. whether the UI should offer a "Reconnect" button on
    /// [`RadioEvent::ConnectionLost`]. False for an in-process engine: there is
    /// no link to redial, and an engine that has stopped needs the app
    /// restarted.
    fn can_reconnect(&self) -> bool {
        false
    }

    /// Whether the engine is on another machine, so the paths it reports —
    /// where received pictures are being saved, where a recording went — name
    /// *its* filesystem rather than the one the operator is sitting at.
    ///
    /// Wording only, and false by default: an in-process engine writes to the
    /// disk the UI reads.
    fn engine_is_remote(&self) -> bool {
        false
    }

    /// Dial the radio again after a lost connection, discarding whatever was
    /// left of the old session. `Ok` means the attempt is under way — the
    /// handshake completes asynchronously, so the UI learns the outcome from
    /// the events that follow, exactly as at startup.
    fn reconnect(&mut self) -> Result<(), String> {
        Err("this connection cannot be re-established".into())
    }

    /// Where this connection stands with the server's sign-in challenge, so
    /// the UI knows whether to put a dialog up instead of the radio.
    ///
    /// Always [`AuthPhase::Open`] for an in-process engine: there is no
    /// network between the UI and the radio, so there is nothing to prove.
    fn auth_phase(&self) -> crate::AuthPhase {
        crate::AuthPhase::Open
    }

    /// Answer the challenge. No-op where there is none.
    fn send_auth(&mut self, username: String, password: String) {
        let _ = (username, password);
    }

    /// Silence (or restore) this radio's speaker path — the tab-strip mute.
    /// Distinct from [`Command::SetMute`], which is a per-receiver setting the
    /// operator owns: this one belongs to the tab, and everything (DSP,
    /// decoders, the recorder) keeps running underneath it. No-op by default —
    /// a remote session has no tab strip of its own yet.
    fn set_muted(&mut self, muted: bool) {
        let _ = muted;
    }

    /// Tell this radio's engine to re-read the station-shared stores
    /// (memories, band stacks, digi operator config) from disk, because
    /// another radio in the same process just saved one of them. No-op for
    /// remote controllers — there is no second local engine to have written.
    fn nudge_shared_stores(&mut self) {}

    /// Detach from the radio and reclaim what this controller holds: for an
    /// in-process engine, disconnect it (it stops), drop the audio streams and
    /// join the DSP thread. Called when a radio tab closes and on app exit; a
    /// controller that owns nothing needs nothing here.
    fn shutdown(&mut self) {}

    /// The frontend's switchable audio devices, or `None` when the platform
    /// has none to offer (e.g. the browser client, where the browser owns
    /// device routing). Enumeration may be slow — call on demand, not per
    /// frame.
    fn audio_devices(&self) -> Option<AudioDevices> {
        None
    }

    /// Switch the audio output (`output = true`) or input device by name;
    /// `None` selects the system default. No-op on platforms without
    /// switchable audio.
    fn set_audio_device(&mut self, output: bool, name: Option<String>) {
        let _ = (output, name);
    }

    /// Whether this build can drive SoapySDR devices (compiled with the `soapy`
    /// feature). The settings UI offers the SoapySDR interface only when true.
    /// Default false (remote clients don't own the server's hardware).
    fn soapy_supported(&self) -> bool {
        false
    }

    /// Serial ports available for CAT control (native local client only).
    fn serial_ports(&self) -> Vec<String> {
        Vec::new()
    }

    /// Scan the LAN for OpenHPSDR devices (native local client only). Blocking —
    /// the settings UI calls this on demand (a "Discover" button), not per frame.
    /// Default empty: the browser/remote client can't scan the server's network.
    fn discover_hpsdr(&self) -> Vec<crate::HpsdrDevice> {
        Vec::new()
    }

    /// List RTL-SDR dongles on the USB bus (native local client only). Fast
    /// and non-invasive — no device is opened — so the settings UI may call it
    /// on demand as often as the operator presses Rescan, including while a
    /// dongle is streaming. Default empty: a browser client cannot see the
    /// server's USB bus.
    fn list_rtlsdr(&self) -> Vec<crate::RtlSdrDevice> {
        Vec::new()
    }

    /// List RX-888 receivers on the USB bus (native local client only). Same
    /// contract as [`RadioController::list_rtlsdr`]: nothing is opened, so it is
    /// safe to call while one is streaming. Devices still sitting in their boot
    /// ROM are included and flagged, because that is the normal state of an
    /// RX-888 that has just been plugged in.
    fn list_rx888(&self) -> Vec<crate::Rx888Device> {
        Vec::new()
    }

    /// List Airspy HF+ receivers on the USB bus (native local client only).
    /// Same contract as [`RadioController::list_rtlsdr`]: nothing is opened, so
    /// it is safe to call while one is streaming. Which model each one is
    /// cannot be told from the bus — every HF+ shares a product id — so the
    /// entries name the family and not the model.
    fn list_airspyhf(&self) -> Vec<crate::AirspyHfDevice> {
        Vec::new()
    }

    /// List the RSPs the SDRplay API service reports (native local client
    /// only). Blocking but brief — the service answers from its own device
    /// table — and safe while one is streaming: enumeration takes the API's
    /// device lock only long enough to read the list. Default empty: a browser
    /// client cannot reach the server's SDRplay service, and neither can a
    /// machine without the vendor API installed.
    fn list_sdrplay(&self) -> Vec<crate::SdrPlayDevice> {
        Vec::new()
    }

    /// Enumerate the SoapySDR devices this machine can see (native local client
    /// only, and only in a build with the `soapy` feature).
    ///
    /// Unlike the lists above this one is *not* free: enumeration loads every
    /// installed SoapySDR module and asks each to scan, which on a bundle
    /// install means probing several buses. The settings UI therefore calls it
    /// on dialog-open and on Rescan, never per frame.
    fn list_soapy(&self) -> Vec<crate::SoapyDeviceInfo> {
        Vec::new()
    }

    /// Test a TCI server connection at `address` (`host:port`). Blocking — the
    /// settings UI calls this on demand (a "Test connection" button). Returns a
    /// success summary or an error message. Default: unsupported (remote client).
    fn test_tci(&self, _address: &str) -> Result<String, String> {
        Err("not supported on this client".into())
    }

    /// Test an Icom LAN connection. Blocking, on demand.
    ///
    /// There is no discovery counterpart: an Icom does not announce itself on
    /// the network, so the address is always typed in and the only question
    /// worth answering is whether what is at that address is a radio that will
    /// let us in. Default: unsupported (remote client).
    fn test_icomnet(&self, _cfg: &crate::IcomNetConfig) -> Result<String, String> {
        Err("not supported on this client".into())
    }

    /// The most recent Icom LAN session trace, for a bug report.
    ///
    /// This backend has not been verified against hardware, so the settings UI
    /// offers the trace as copyable text rather than expecting a user to
    /// reproduce a fault with the right `RUST_LOG` filter set. `None` when no
    /// session has run. Default: nothing to report.
    fn icomnet_diagnostics(&self) -> Option<String> {
        None
    }

    /// Listen for FlexRadio discovery broadcasts (native local client only).
    /// Blocking for a couple of seconds — the settings UI calls it on demand
    /// from a "Discover" button, not per frame. Default empty: a browser client
    /// cannot see the server's network.
    fn discover_smartsdr(&self) -> Vec<crate::SmartSdrDevice> {
        Vec::new()
    }

    /// Test a SmartSDR radio at `address`. Blocking, on demand.
    ///
    /// Implementations must stop short of registering as a GUI client: on a
    /// radio without multiFLEX that would evict whatever client the operator is
    /// actually using, which is a poor thing for a "Test connection" button to
    /// do. Default: unsupported (remote client).
    fn test_smartsdr(&self, _address: &str) -> Result<String, String> {
        Err("not supported on this client".into())
    }

    /// Scan for PlutoSDRs (native local client only). Blocking for a couple of
    /// seconds — the settings UI calls it on demand from a "Discover" button,
    /// not per frame.
    ///
    /// Asks mDNS *and* opens the USB gadget's default address, because a Pluto
    /// on the end of a USB cable often has no reachable mDNS responder. Default
    /// empty: a browser client cannot see the server's network.
    fn discover_pluto(&self) -> Vec<crate::PlutoDevice> {
        Vec::new()
    }

    /// Test a PlutoSDR at `address` (`host[:port]`). Blocking, on demand.
    ///
    /// Reads the front-end limits as well as the identity, so the answer states
    /// the tuning range *this* board has — a stock AD9363 and one unlocked to
    /// AD9364 differ by an octave and a half, and only the device knows which
    /// it is. Default: unsupported (remote client).
    fn test_pluto(&self, _address: &str) -> Result<String, String> {
        Err("not supported on this client".into())
    }

    /// The most recent PlutoSDR session trace, for a bug report.
    ///
    /// This backend has not been verified against hardware, so the settings UI
    /// offers the trace as copyable text rather than expecting a user to
    /// reproduce a fault with the right `RUST_LOG` filter set. `None` when no
    /// session has run.
    fn pluto_diagnostics(&self) -> Option<String> {
        None
    }

    /// The most recent SmartSDR session trace, for a bug report.
    ///
    /// This backend has not been verified against hardware, so the settings UI
    /// offers the trace as copyable text rather than expecting a user to
    /// reproduce a fault with the right `RUST_LOG` filter set. `None` when no
    /// session has run. Default: nothing to report.
    fn smartsdr_diagnostics(&self) -> Option<String> {
        None
    }

    /// The most recent Airspy HF+ session trace, for a bug report.
    ///
    /// This backend has not been verified against hardware, so the settings UI
    /// offers the trace as copyable text rather than expecting a user to
    /// reproduce a fault with the right `RUST_LOG` filter set. `None` when no
    /// session has run. Default: nothing to report.
    fn airspyhf_diagnostics(&self) -> Option<String> {
        None
    }

    /// The persisted radio-backend config (SoapySDR vs CAT), or `None` when the
    /// client can't own it (the browser remote client).
    fn radio_config(&self) -> Option<crate::RadioConfig> {
        None
    }

    /// Persist an updated radio-backend config. This only stores it; call
    /// [`RadioController::reopen_source`] to apply it to the live engine. No-op
    /// where unsupported.
    fn set_radio_config(&mut self, cfg: crate::RadioConfig) {
        let _ = cfg;
    }

    /// Rebuild the IQ source from the persisted radio config at runtime, so a
    /// backend / CAT-audio / HPSDR-TCI-address change takes effect without a
    /// restart. Call after [`RadioController::set_radio_config`]. No-op on the
    /// remote client (the server owns its hardware).
    fn reopen_source(&mut self) {}
}
