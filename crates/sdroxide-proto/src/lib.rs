//! WebSocket wire protocol between the sdroxide server and remote clients.
//!
//! Framing: every binary WS message is `[PROTO_VERSION_BYTE][postcard bytes]`.
//! The version byte is a fast sanity check; the real version negotiation
//! happens in `Hello`/`HelloAck`.
//!
//! Compiles for native and `wasm32-unknown-unknown`.

pub mod solar;

use serde::{Deserialize, Serialize};

use sdroxide_types::{
    CallsignInfo, Command, Decode, DeviceCaps, DigiStatus, ImageEntry, ImageKind, ImageListing,
    ImagePresets, MemoryChannel, MemoryFolder, Meters, QsoRecord, RadioState, RifpMeta, RifpStatus,
    SkimmerSpot, SpectrumFrame, Spot, SstvMode, SstvStatus, StationConfig, TleSubStatus,
    UploadResult, VoiceStatus,
};

/// Bump on any incompatible change to the message enums (this includes the
/// payload structs from `sdroxide-types` that ride the wire, e.g. `QsoRecord`).
/// v3: `QsoRecord` gained `id` + `comment` fields.
/// v4: added `ServerMsg::SkimmerSpots` + `Command::SetSkimmerEnabled` + a
/// `RadioState.skimmer_enabled` field.
/// v5: added SSTV — `Mode::Sstv`, `ServerMsg::Sstv*`, and
/// `Command::SstvTx`/`SstvSetMode`.
/// v6: added audio noise reduction + auto-notch — `Command::SetNoiseReduction`,
/// `Command::SetAutoNotch`, and `RxState.noise_reduction` / `RxState.auto_notch`.
/// v7: added keyboard modes Olivia/Thor/FSQ — new `Mode` variants, `DigiConfig`
/// submode fields (Olivia tones/bw, THOR submode, FSQ speed/call), `DigiStatus`
/// FSQ heard-list + directed-message fields, and a mode-agnostic digi image path
/// (`Command::DigiImageTx` / `RadioEvent::DigiImage` for the FSQ image sub-mode).
/// v8: added the audio recorder — `Command::SetRecording` and
/// `RadioState.recording` / `RadioState.recording_file`.
/// v9: FT8/FT4 QSO handling — `QsoStep::WaitCq` / `Confirming` and
/// `Command::DigiStartQso.wait_for_cq`.
/// v10: neural (RNNoise) noise reduction — new `NrLevel::Ai{Low,Med,High}`
/// variants can appear in `RxState.noise_reduction`.
/// v11: network cockpit — spot feeds, callsign lookup, and uploads. New
/// `Command::SetNetworkConfig`/`SpotDialHint`/`LookupCallsign`/`UploadQso`/
/// `SyncConfirmations` and `ServerMsg::Spots`/`NetStatus`/`CallsignResult`/
/// `Upload`/`Confirmations`, plus new `QsoRecord` fields.
/// v12: per-kind skimmer control — `RadioState.skimmer_enabled` became
/// `RadioState.skimmer: SkimmerSettings` (CW/PSK/RTTY enables + squelch) and
/// `Command::SetSkimmerEnabled` became `Command::SetSkimmerConfig`.
/// v13: built-in TCI server — `Command::SetTciServerConfig` and
/// `ServerMsg::TciServerStatus`.
/// v14: FreeDV Reporter — new `SpotKind::FreeDv` (extends the postcard
/// discriminant space of `ServerMsg::Spots`) and `NetworkConfig`'s new
/// `freedv_reporter` field. `NetworkConfig` also lost `my_call`/`my_grid`: the
/// operator identity is `DigiConfig`'s alone, so both ends must agree on the
/// shape `Command::SetNetworkConfig` carries.
/// v15: built-in Hamlib rigctld server — `Command::SetRigctldConfig` and
/// `ServerMsg::RigctldStatus` (both extend the postcard discriminant space).
/// v16: FT8 message handling and reporting. `Decode` gained `cq_dx` and
/// `free_text`, `TranscriptLine` gained `overheard`, and `PskConfig` gained the
/// upload fields — postcard is not self-describing, so every added field
/// changes the layout of the messages carrying them. Also new:
/// `Command::SetWsjtxConfig` (WSJT-X UDP broadcast).
/// v17: manual FT8 message control — `Command::DigiSetStep` and
/// `Command::DigiSendText` (both extend the postcard discriminant space).
/// v18: FT8 transmit watchdog — `DigiStatus.tx_watchdog` plus `DigiConfig`'s
/// `tx_watchdog_min` / `max_tx_repeats`, which both ends must agree on.
/// v19: voice keyer — `Command::VoiceRecord`/`VoicePlay`/`VoicePreview`/
/// `VoiceClear`/`VoiceRename` and `ServerMsg::VoiceStatus` (both extend the
/// postcard discriminant space).
/// v20: Hellschreiber — `ServerMsg::HellColumns` plus `DigiConfig`'s
/// `hell_variant` / `hell_rx_agc`, which both ends must agree on because
/// `DigiStatus` carries the config. (`Mode::Hell` alone would have been
/// compatible: it is appended to the enum, so no existing discriminant moves.)
/// v21: FT8 DXpedition mode — `DigiConfig`'s `dxped_mode` / `fox_slots`,
/// `DigiStatus.fox_queue`, and `Decode.rr73_to` (the RR73 half of a Fox
/// message, which is how a Hound learns its contact completed). Postcard is not
/// self-describing, so both ends must agree on every one of those fields.
/// v22: clock-offset monitoring — `DigiStatus.clock_offset_s`.
/// v23: directed CQs — `Decode.cq_dx` became `Decode.cq_to`, the modifier
/// itself (`DX`, `EU`, `JA`, `POTA`, …) rather than a single DX flag.
/// v24: the FT8/FT4 call queue — `Command::DigiQueueAdd`/`DigiQueueRemove` and
/// `DigiStatus.call_queue`.
/// v25: automatic transmit-frequency choice — `DigiConfig.auto_tx_freq`.
/// v26: RIFP (draft-dulaunoy-rifp-00) — `Mode::Rifp` and `Band::M70` (both
/// appended, so no existing discriminant moves), `Command::RifpTx` /
/// `RifpDropSession`, `ServerMsg::RifpRows` / `RifpImage` / `RifpStatus`, and
/// `DigiConfig`'s `rifp_*` fields, which both ends must agree on because
/// `DigiStatus` carries the config.
/// v27: WFM broadcast stereo — `RxState.wfm_stereo`, `Meters.stereo` and
/// `Command::SetWfmStereo`. The command is appended so no existing discriminant
/// moves, but postcard is not self-describing, so the two added struct fields
/// change the layout of every message carrying `RadioState` or `Meters`.
/// v28: JS8 — `Mode::Js8`, the `js8_*` fields on `DigiConfig`, and
/// `DigiStatus.js8` carrying the heard list, the reassembled conversation and
/// transmit-queue progress. No message enum gained a variant, but postcard is
/// not self-describing and the added struct fields change the layout of every
/// message carrying `DigiConfig` or `DigiStatus`.
/// v29: JS8 beaconing — `DigiConfig`'s `js8_hb_ack` (answer a heard heartbeat
/// with a signal report) and `js8_hb_anywhere` (beacon on the working frequency
/// instead of the 500–1000 Hz sub-band), plus `Js8Status.hb_hz`, the frequency
/// the last beacon actually went out on. `Js8Status.next_hb_in_s` is now
/// populated rather than always `None`, which is a behaviour change but not a
/// layout one. Both ends must agree on the three added fields, postcard being
/// what it is.
/// v30: broadcast station labels — new `SpotKind::Broadcast`, which extends the
/// postcard discriminant space of `ServerMsg::Spots` exactly as `FreeDv` did in
/// v14. The engine never emits it (the stations are synthesised client-side from
/// a bundled table), but the enum both ends decode has changed shape, so they
/// must agree on it.
/// v31: the full-band panadapter — a new `ServerMsg::WideSpectrum`, carrying an
/// ordinary `SpectrumFrame` on its own lane. Appended at the end of the enum so
/// no existing postcard discriminant moves, but an older client cannot decode
/// the new message, so the handshake has to reject it.
/// v32: engine notices reach remote clients — a new `ServerMsg::Notice`,
/// likewise appended at the end. What a notice says is the operator's business
/// wherever they are sitting: a radio refusing a tune, or an interface that has
/// dropped and is reconnecting, is not a local-console detail.
/// v33: the picture stores moved server-side — five new `ServerMsg`s
/// (`ImagePresets`, `ImageSlotSource`, `ImageListing`, `ImageFile`,
/// `ImageSaved`) and six new `Command`s (`ImageSetSlot`, `ImageClearSlot`,
/// `ImageSetMessage`, `ImageGetSlot`, `ImageList`, `ImageGet`), all appended so
/// no existing discriminant moves. The transmit slots, their overlay messages
/// and the received galleries used to be client state, which meant a browser
/// tab and the console attached to the same radio disagreed about both — and
/// the browser, having no filesystem, had neither. They belong to the radio:
/// the engine owns the files and hands out metadata, thumbnails and pixels on
/// request. Composition stays client-side, and transmit still rides the
/// existing `SstvTx` / `RifpTx`.
/// v34: the station configuration reaches remote clients — two new `ServerMsg`s
/// (`StationConfig`, `TleSubStatus`) and two new `Command`s (`SetSatConfig`,
/// `RefreshTleSubs`), all appended so no existing discriminant moves. The
/// network cockpit, the two built-in servers, the WSJT-X broadcast and the
/// satellite additions all describe the *station*, and all of them are files in
/// the engine host's config directory. A remote settings dialog used to read
/// its own machine's copy — nonexistent in a browser — so those tabs opened on
/// defaults, and pressing APPLY wrote the defaults back over the operator's
/// real configuration. The engine announces them instead, and the server caches
/// and replays them like the digi config.
/// v35: received pictures can be deleted — one new `ServerMsg` (`ImageDeleted`)
/// and one new `Command` (`ImageDelete`), both appended so no existing
/// discriminant moves. The store is on the engine host, so until now the only
/// way to clear out a season of half-decoded charts and noise-only frames was to
/// go to that machine with a file manager; a browser tab could not do it at all.
/// The deletion is broadcast rather than answered, because a picture that has
/// gone is gone from every gallery, not just the one that asked.
/// v36: sign-in — one new `ClientMsg` (`Auth`) and two new `ServerMsg`s
/// (`AuthRequired`, `AuthRejected`), all appended so no existing discriminant
/// moves. A server with credentials configured answers `Hello` with
/// `AuthRequired` instead of `HelloAck` and waits; everything that used to
/// happen next happens after the credentials are accepted. The version still
/// has to be bumped despite the appends: a v35 client cannot decode
/// `AuthRequired`, so it would report a protocol error rather than the truth,
/// which is that it needs a password and cannot ask for one.
/// v37: manual audio gain — `RxState.manual_gain_db` and a new
/// `Command::SetManualGain`, which is *not* appended: it sits with the other
/// receiver commands, so the discriminants after it move. Both ends must agree
/// on the added field anyway, postcard not being self-describing, so the
/// version has to be bumped either way. AGC off used to mean unity gain on the
/// demodulator's own output, which for an SSB signal 60 dB down is silence at
/// any volume setting; it now means this fixed gain instead.
/// v38: a finished FT8/FT4 contact says so — `TranscriptLine` gained `done`,
/// the flag on the line that marks the QSO complete and logged. Appended to the
/// struct, but postcard is not self-describing, so every message carrying a
/// `DigiStatus` changes layout and both ends have to agree on the field.
/// v39: `QsoStep::Done` is gone. Nothing ever set it — `Confirming` is the
/// state a finished contact sits in — so no engine has ever put it on the wire.
/// It was the last variant, so no surviving discriminant moves, but the enum
/// both ends decode has changed shape and this codebase bumps for that.
/// v40: sub-audible squelch signalling on NFM — `Meters.tone` carries the CTCSS
/// tone or DCS code being received, `RxState.tone_sql` the one the operator
/// requires before the audio gate opens, and `Command::SetToneSquelch` sets it.
/// The command is appended, so no surviving discriminant moves, but postcard is
/// not self-describing and the two added struct fields change the layout of
/// every message carrying `RadioState` or `Meters`.
/// v41: the frequency scanner — `RadioState.scan` says whether it is running
/// and whether it has stopped on something, `RadioEvent::Scanner` carries the
/// settings the way `Memories` carries the memory list, and four appended
/// commands (`SetScannerConfig`, `SetScanning`, `ScanNext`, `ScanSkip`) drive
/// it. The commands are appended, but the added `RadioState` field changes the
/// layout of every message carrying one, postcard not being self-describing.
/// v42: recording both sides of the QSO (RX left, TX right) instead of just
/// the receiver, plus an optional mono downmix — `Command::SetRecordingMono`
/// (appended) and `RadioState.recording_mono` (changes the layout of every
/// message carrying `RadioState`).
/// v43: two more noise-reduction engines. `NrLevel` gained
/// `Spec{Low,Med,High}` (a Rust port of libspecbleach's adaptive denoiser) and
/// `Df{Low,Med,High}` (DeepFilterNet3), both appended so no surviving
/// discriminant moves — but a v42 client cannot decode discriminants 7..12 at
/// all, so it would report a protocol error rather than the truth, which is
/// that the operator picked an engine it has never heard of. The `Ai*`
/// variants were also renamed `Rnn*`, which the wire cannot see (postcard is
/// positional and this enum is persisted nowhere else) but the labels can: the
/// chip reads "NR RNN Med" where it read "NR AI Med".
/// v44: WSPR. `Mode::Wspr` is appended, as is `SpotKind::Wspr`, so no surviving
/// discriminant moves — but `DigiConfig` gained five fields and `DigiStatus` a
/// `wspr: Option<WsprStatus>`, and postcard is not self-describing, so every
/// message carrying either changes layout and both ends have to agree. The new
/// `ServerMsg::WsprSpots` carries what a slot decoded: a WSPR reception is a
/// measurement of a path rather than a message somebody sent, so it travels as
/// its own event instead of being squeezed into `Decode`. `WsprStatus` also
/// carries `tx_blocked`, the engine's own answer to "can this station transmit
/// as configured" — the panel used to work that out for itself and got it
/// wrong, and one authority is the point.
/// v45: the CW skimmer's decoder is the operator's choice. `SkimmerSettings`
/// gained `cw_decoder` (DeepCW or the envelope-timing decoder) and `cw_slots`
/// (how many stations the neural one reads at once), so a receiver that cannot
/// spare the cores for a Conformer per station can still skim. Both are
/// appended, but `SkimmerSettings` is a field of `RadioState` as well as the
/// payload of `Command::SetSkimmerConfig`, and postcard is not self-describing:
/// the layout of every state broadcast changes and both ends have to agree.
/// v46: memory folders — `MemoryChannel` gained `folder`, which changes the
/// layout of every `Memories` message, postcard not being self-describing. The
/// folder list itself rides the new `ServerMsg::MemoryFolders` (appended last,
/// so no surviving discriminant moves), and four appended `Command`s
/// (`CreateMemoryFolder` / `RenameMemoryFolder` / `DeleteMemoryFolder` /
/// `MoveMemoryToFolder`) manage them.
/// v47: an RTTY memory carries its modem setup — `MemoryChannel` gained
/// `rtty: Option<RttyMemory>` (baud / shift / reverse / AFC), captured when a
/// memory is stored in RTTY mode and re-applied on recall. Changes the layout
/// of every `Memories` message, postcard not being self-describing.
/// v48: the satellite lock. Two appended `Command`s (`SetSatLock`,
/// `SetRotatorConfig`) and two appended `ServerMsg`s (`SatTrack`,
/// `RotatorStatus`), so no surviving discriminant moves — but `SatLink` gained
/// `inverting` and `StationConfig` gained `rotator`, and postcard is not
/// self-describing, so every message carrying either (`SetSatConfig`, the
/// `StationConfig` bundle) changes layout and both ends have to agree.
///
/// v49: `DeviceCaps` gained `shared_lo_rx` (a Pluto 2R2T's chains share one
/// LO). Appended field, but postcard is not self-describing, so every message
/// carrying capabilities changes layout and both ends have to agree.
pub const PROTO_VERSION: u16 = 49;
const VERSION_BYTE: u8 = 0x12;

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("empty message")]
    Empty,
    #[error("unsupported protocol version byte {0:#x}")]
    Version(u8),
    #[error("decode error: {0}")]
    Decode(#[from] postcard::Error),
}

/// Audio codec for one stream direction, negotiated at Hello time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    /// 20 ms Opus frames, 48 kHz mono.
    Opus48kMono,
    /// Little-endian PCM16, 48 kHz mono (fallback when WebCodecs is missing).
    Pcm16_48k,
}

/// What the client can encode/decode (browser WebCodecs availability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioCaps {
    pub opus_decode: bool,
    pub opus_encode: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello {
        proto: u16,
        audio: AudioCaps,
    },
    Command(Command),
    /// 20 ms mic frame in the codec negotiated at Hello.
    MicFrame {
        seq: u32,
        payload: Vec<u8>,
    },
    Ping(u64),
    /// Answer to [`ServerMsg::AuthRequired`]. Appended last on purpose:
    /// postcard encodes the variant as a positional discriminant, so inserting
    /// anywhere else would silently renumber every message after it.
    Auth {
        username: String,
        password: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    HelloAck {
        proto: u16,
        caps: DeviceCaps,
        state: RadioState,
        /// Codec of server→client RX audio.
        rx_codec: AudioCodec,
        /// Codec expected for client→server mic frames.
        tx_codec: AudioCodec,
    },
    State(RadioState),
    Spectrum(SpectrumFrame),
    Meters(Meters),
    Memories(Vec<MemoryChannel>),
    /// The scanner's settings, replayed on connect and re-sent on every change,
    /// exactly as `Memories` is.
    Scanner(sdroxide_types::ScannerConfig),
    RxAudio {
        seq: u32,
        payload: Vec<u8>,
    },
    Pong(u64),
    /// Another client already holds the (single) session.
    Busy,
    Error(String),
    // FT8/FT4 digital modes.
    Ft8Decodes(Vec<Decode>),
    Ft8Status(DigiStatus),
    Ft8QsoLogged(QsoRecord),
    // Skimmers (CW etc.).
    SkimmerSpots(Vec<SkimmerSpot>),
    // SSTV image mode.
    SstvLine {
        image_id: u32,
        y: u16,
        rgb: Vec<u8>,
    },
    SstvImage {
        image_id: u32,
        mode: SstvMode,
        w: u16,
        h: u16,
        png: Vec<u8>,
    },
    SstvStatus(SstvStatus),
    // Weather fax (receive only).
    WefaxLine {
        image_id: u32,
        y: u16,
        gray: Vec<u8>,
    },
    WefaxImage {
        image_id: u32,
        w: u16,
        h: u16,
        png: Vec<u8>,
    },
    WefaxStatus(sdroxide_types::WefaxStatus),
    // RIFP image mode.
    /// Reassembled raster rows of an incoming picture (grayscale, `w` per row).
    RifpRows {
        image_id: u32,
        y: u16,
        w: u16,
        h: u16,
        rows: Vec<u8>,
    },
    /// A completed, digest-verified picture (PNG bytes) and its manifest facts.
    RifpImage {
        image_id: u32,
        meta: RifpMeta,
        png: Vec<u8>,
    },
    RifpStatus(RifpStatus),
    /// FSQ image: a completed received picture (PNG bytes).
    DigiImage {
        png: Vec<u8>,
    },
    /// Hellschreiber: a batch of received dot columns, column-major, 0 = black.
    /// `seq` is the absolute column index so a client can detect a dropped
    /// batch — this lane drops rather than blocks when it backs up, and Hell has
    /// no framing of its own to resynchronise against.
    HellColumns {
        seq: u64,
        rows: u8,
        cols: Vec<u8>,
    },
    /// Voice keyer: slot contents plus what is being recorded or transmitted.
    VoiceStatus(VoiceStatus),
    // Network cockpit.
    Spots(Vec<Spot>),
    NetStatus(Option<String>),
    CallsignResult(CallsignInfo),
    Upload(UploadResult),
    Confirmations(Vec<QsoRecord>),
    /// Built-in TCI server status (listener up, bind address, client count).
    TciServerStatus {
        running: bool,
        addr: String,
        clients: usize,
        error: Option<String>,
    },
    /// Built-in rigctld server status, so the settings dialog on a remote
    /// client can show what the engine's listener is doing.
    RigctldStatus {
        running: bool,
        addr: String,
        clients: usize,
        error: Option<String>,
    },
    /// Full-band spectrum from a direct-sampling front end. Appended last on
    /// purpose: postcard encodes the variant as a positional discriminant, so
    /// inserting anywhere else would silently renumber every message after it.
    WideSpectrum(SpectrumFrame),
    /// A non-fatal operator notice from the engine, `None` to clear it — the
    /// radio refusing a tune, an interface reconnecting. Unlike
    /// [`ServerMsg::Error`] the session is intact and the client stays live.
    /// Appended last, for the reason above.
    Notice(Option<String>),
    /// The transmit-image presets, announced at startup and on every change.
    /// Cached by the server and replayed on connect, like the digi config and
    /// the voice keyer — without it a browser tab opens on five empty slots
    /// beside a console showing five full ones, and there is no second
    /// announcement to wait for.
    ImagePresets(ImagePresets),
    /// A preset's stored source picture, answering `Command::ImageGetSlot`.
    ImageSlotSource {
        slot: u8,
        version: u32,
        png: Vec<u8>,
    },
    /// One page of a received store, answering `Command::ImageList`.
    ImageListing(ImageListing),
    /// One received picture at full size, answering `Command::ImageGet`. An
    /// empty `png` means the store does not have it.
    ImageFile {
        kind: ImageKind,
        name: String,
        png: Vec<u8>,
    },
    /// A freshly received picture has been stored, as a gallery would list it.
    ImageSaved(ImageEntry),
    /// What the station is set up to do: the network cockpit, the two built-in
    /// servers, the WSJT-X broadcast and the satellite additions. Cached by the
    /// server and replayed on connect, like the digi config — these are files
    /// in the engine host's config directory, and a client on another machine
    /// has no other way to learn them.
    StationConfig(Box<StationConfig>),
    /// What each TLE subscription's cached listing holds. Replayed on connect
    /// beside the config it annotates.
    TleSubStatus(Vec<TleSubStatus>),
    /// A received picture has been deleted from the store, answering
    /// `Command::ImageDelete`. Sent to whichever client is attached, whether or
    /// not it is the one that asked.
    ImageDeleted {
        kind: ImageKind,
        name: String,
    },
    /// This server wants a username and password. Sent in place of
    /// [`ServerMsg::HelloAck`] once `Hello` has been read and its version
    /// accepted — after, so a client on the wrong protocol is told *that*
    /// rather than being asked to sign in to a server it could not talk to
    /// anyway. The client answers with [`ClientMsg::Auth`].
    ///
    /// Nothing else is sent, read or acted on until the credentials are
    /// accepted: not the capabilities, not the state, and above all not the
    /// single-client slot, which is claimed only afterwards so that a stranger
    /// cannot lock the operator out of their own radio by connecting to it.
    AuthRequired,
    /// Those were not the credentials, and why not. The socket stays open so
    /// the operator can correct a typo without redialling — but the server
    /// takes its time before it will judge another attempt.
    AuthRejected(String),
    /// What a WSPR slot decoded.
    ///
    /// Not `Ft8Decodes`: a WSPR reception is a measurement of a path, not a
    /// message addressed to anyone, and it carries the transmit power and drift
    /// that make it one. Squeezing it into `Decode` would have meant throwing
    /// both away. Appended last, so no surviving discriminant moves.
    WsprSpots(Vec<sdroxide_types::WsprSpot>),
    /// The memory folders, replayed on connect and re-sent on every change,
    /// exactly as `Memories` is. Appended last, for the usual reason.
    MemoryFolders(Vec<MemoryFolder>),
    /// What the satellite lock is doing — look angles, range, the Doppler
    /// corrections as applied. The latest one is cached by the server and
    /// replayed on connect, so a client arriving mid-pass sees the lock
    /// immediately rather than at the next tick. `None` when the lock is
    /// released. Appended last, for the usual reason.
    SatTrack(Option<Box<sdroxide_types::SatTrackStatus>>),
    /// The rotctld client's health, mirrored from the engine's
    /// `RadioEvent::RotatorStatus`. Appended last, for the usual reason.
    RotatorStatus {
        connected: bool,
        az_deg: f64,
        el_deg: f64,
        error: Option<String>,
    },
}

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, ProtoError> {
    Ok(postcard::to_extend(msg, vec![VERSION_BYTE])?)
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, ProtoError> {
    match bytes {
        [] => Err(ProtoError::Empty),
        [VERSION_BYTE, rest @ ..] => Ok(postcard::from_bytes(rest)?),
        [v, ..] => Err(ProtoError::Version(*v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdroxide_types::ImageSlotInfo;

    #[test]
    fn roundtrip_client_and_server_msgs() {
        let msgs = [
            ClientMsg::Hello {
                proto: PROTO_VERSION,
                audio: AudioCaps { opus_decode: true, opus_encode: false },
            },
            ClientMsg::Command(Command::SetPtt(true)),
            ClientMsg::MicFrame { seq: 7, payload: vec![1, 2, 3] },
        ];
        for m in &msgs {
            let bytes = encode(m).unwrap();
            let back: ClientMsg = decode(&bytes).unwrap();
            assert_eq!(&back, m);
        }

        let m = ServerMsg::State(RadioState::default());
        let bytes = encode(&m).unwrap();
        let back: ServerMsg = decode(&bytes).unwrap();
        assert_eq!(back, m);

        // SSTV image/status messages round-trip (binary pixel payloads).
        let sstv = [
            ServerMsg::SstvLine { image_id: 3, y: 7, rgb: vec![1, 2, 3, 4, 5, 6] },
            ServerMsg::SstvImage {
                image_id: 3,
                mode: SstvMode::Martin1,
                w: 320,
                h: 256,
                png: vec![0x89, 0x50, 0x4e, 0x47],
            },
            ServerMsg::SstvStatus(SstvStatus {
                tx_mode: SstvMode::Robot36,
                detected: Some(SstvMode::Scottie2),
                ..SstvStatus::default()
            }),
        ];
        for m in &sstv {
            let bytes = encode(m).unwrap();
            let back: ServerMsg = decode(&bytes).unwrap();
            assert_eq!(&back, m);
        }

        // RIFP carries pixels, a manifest summary, and a per-chunk map.
        let rifp = [
            ServerMsg::RifpRows { image_id: 2, y: 11, w: 4, h: 20, rows: vec![9, 8, 7, 6] },
            ServerMsg::RifpImage {
                image_id: 2,
                meta: RifpMeta {
                    session: "0123456789abcdef".into(),
                    filename: "oe1test.png".into(),
                    sender: Some("OE1TEST".into()),
                    hint: None,
                    media_type: "image/png".into(),
                    content_encoding: "identity".into(),
                    width: 320,
                    height: 240,
                    bits_per_pixel: 4,
                    encoded_size: 9_000,
                    chunk_count: 47,
                    chunks_first_pass: 45,
                },
                png: vec![0x89, 0x50, 0x4e, 0x47],
            },
            ServerMsg::RifpStatus(RifpStatus {
                tx_active: true,
                tx_progress: 0.25,
                sessions: vec![sdroxide_types::RifpSession {
                    session: "0123456789abcdef".into(),
                    sender: None,
                    have_manifest: true,
                    have: 3,
                    total: 47,
                    map: vec![0b0000_0111],
                    idle_s: 2,
                }],
                ..RifpStatus::default()
            }),
        ];
        for m in &rifp {
            let bytes = encode(m).unwrap();
            let back: ServerMsg = decode(&bytes).unwrap();
            assert_eq!(&back, m);
        }

        // The picture stores: metadata one way, thumbnails and whole pictures
        // the other. Every one of these carries a binary payload, which is
        // exactly what a length-prefixed non-self-describing encoding gets
        // wrong when a field is added in the wrong place.
        let pictures = [
            ServerMsg::ImagePresets(ImagePresets {
                slots: vec![
                    ImageSlotInfo {
                        message: "CQ SSTV de OE1TEST".into(),
                        width: 1024,
                        height: 768,
                        version: 0xdead_beef,
                        thumb: vec![0x89, 0x50, 0x4e, 0x47, 0x0d],
                    },
                    ImageSlotInfo::default(),
                ],
            }),
            ServerMsg::ImageSlotSource {
                slot: 3,
                version: 0x1234_5678,
                png: vec![0x89, 0x50, 0x4e, 0x47, 1, 2, 3],
            },
            ServerMsg::ImageListing(ImageListing {
                kind: ImageKind::Wefax,
                offset: 48,
                total: 312,
                entries: vec![ImageEntry {
                    kind: ImageKind::Wefax,
                    name: "wefax-20260729-141530Z-7878.1kHz-DWD.png".into(),
                    unix: 1_785_075_330,
                    width: 1809,
                    height: 1200,
                    bytes: 1_234_567,
                    thumb: vec![0x89, 0x50, 0x4e, 0x47],
                    rifp: None,
                }],
                dir: "/home/op/Pictures/sdroxide/wefax".into(),
            }),
            ServerMsg::ImageFile {
                kind: ImageKind::Sstv,
                name: "sstv-1753795200000.png".into(),
                png: vec![0x89, 0x50, 0x4e, 0x47, 9, 9],
            },
            ServerMsg::ImageSaved(ImageEntry {
                kind: ImageKind::Sstv,
                name: "sstv-1753795200000.png".into(),
                unix: 1_753_795_200,
                width: 320,
                height: 256,
                bytes: 40_000,
                thumb: vec![0x89, 0x50],
                rifp: None,
            }),
            ServerMsg::ImageDeleted {
                kind: ImageKind::Wefax,
                name: "wefax-20260729-141530Z-7878.1kHz-DWD.png".into(),
            },
        ];
        for m in &pictures {
            let bytes = encode(m).unwrap();
            let back: ServerMsg = decode(&bytes).unwrap();
            assert_eq!(&back, m);
        }

        let cmds = [
            ClientMsg::Command(Command::ImageSetSlot { slot: 2, bytes: vec![0xff, 0xd8, 0xff] }),
            ClientMsg::Command(Command::ImageSetMessage { slot: 2, message: "73".into() }),
            ClientMsg::Command(Command::ImageGetSlot(4)),
            ClientMsg::Command(Command::ImageClearSlot(0)),
            ClientMsg::Command(Command::ImageList { kind: ImageKind::Wefax, offset: 0, count: 48 }),
            ClientMsg::Command(Command::ImageGet {
                kind: ImageKind::Sstv,
                name: "sstv-1753795200000.png".into(),
            }),
            ClientMsg::Command(Command::ImageDelete {
                kind: ImageKind::Sstv,
                name: "sstv-1753795200000.png".into(),
            }),
        ];
        for m in &cmds {
            let bytes = encode(m).unwrap();
            let back: ClientMsg = decode(&bytes).unwrap();
            assert_eq!(&back, m);
        }
    }

    /// The station configuration, both ways.
    ///
    /// Worth its own test because `SatConfig` reaches types that were only ever
    /// written to JSON before: `OrbitRings` deserialises tolerantly from a
    /// config file (`untagged`, so `deserialize_any`), which postcard refuses
    /// outright. It has to take a second, non-self-describing form here, and a
    /// round trip is the only thing that says so.
    #[test]
    fn roundtrip_station_config() {
        use sdroxide_types::{
            CustomTle, OrbitRings, Passband, SatConfig, SatFreqs, SatLink, StationConfig,
            TleSubStatus, TleSubscription,
        };

        let sat = SatConfig {
            tles: vec![CustomTle {
                name: "NOAA 19".into(),
                line1: "1 33591U 09005A   26031.51268519  .00000271  00000-0  16472-3 0  9992"
                    .into(),
                line2: "2 33591  99.0361 121.3384 0013431 262.5195  97.4595 14.13096410877269"
                    .into(),
                enabled: true,
            }],
            subs: vec![TleSubscription {
                name: "Weather".into(),
                url: "https://celestrak.org/NORAD/elements/gp.php?GROUP=weather&FORMAT=tle".into(),
                enabled: true,
                orbits: OrbitRings::All,
                only: vec![33_591],
            }],
            freqs: vec![SatFreqs::new(
                43_017,
                "NOAA 19",
                vec![SatLink::down("APT", "FM", Passband::at(137.1))],
            )],
            seeded: true,
        };
        let msgs = [
            ServerMsg::StationConfig(Box::new(StationConfig { sat: sat.clone(), ..no_station() })),
            ServerMsg::TleSubStatus(vec![TleSubStatus {
                url: "https://celestrak.org/NORAD/elements/gp.php?GROUP=weather&FORMAT=tle".into(),
                fetched_unix: 1_785_075_330,
                count: 8,
                curated: 0,
                error: Some("connection reset".into()),
            }]),
        ];
        for m in &msgs {
            let bytes = encode(m).unwrap();
            let back: ServerMsg = decode(&bytes).unwrap();
            assert_eq!(&back, m);
        }

        let cmd = ClientMsg::Command(Command::SetSatConfig(sat));
        let back: ClientMsg = decode(&encode(&cmd).unwrap()).unwrap();
        assert_eq!(back, cmd);
        let cmd = ClientMsg::Command(Command::RefreshTleSubs);
        let back: ClientMsg = decode(&encode(&cmd).unwrap()).unwrap();
        assert_eq!(back, cmd);
    }

    /// Every orbit-ring position survives the wire, including the one a bare
    /// index would land on by accident if the mapping ever slipped.
    #[test]
    fn orbit_rings_survive_the_wire() {
        use sdroxide_types::{OrbitRings, SatConfig, StationConfig, TleSubscription};

        for orbits in OrbitRings::ALL {
            let sat = SatConfig {
                subs: vec![TleSubscription {
                    name: "g".into(),
                    url: "https://example.invalid/tle.txt".into(),
                    enabled: true,
                    orbits,
                    only: Vec::new(),
                }],
                ..SatConfig::default()
            };
            let m = ServerMsg::StationConfig(Box::new(StationConfig { sat, ..no_station() }));
            let back: ServerMsg = decode(&encode(&m).unwrap()).unwrap();
            assert_eq!(back, m, "orbit rings {orbits:?} did not survive");
        }
    }

    fn no_station() -> sdroxide_types::StationConfig {
        sdroxide_types::StationConfig::default()
    }

    /// The sign-in exchange, both ways.
    ///
    /// These three are the only messages that cross before the handshake has
    /// finished, so a client and server that disagree about their encoding
    /// cannot recover — there is no established session to report the fault on.
    #[test]
    fn roundtrip_sign_in() {
        let ask = ServerMsg::AuthRequired;
        assert_eq!(decode::<ServerMsg>(&encode(&ask).unwrap()).unwrap(), ask);

        let no = ServerMsg::AuthRejected("username or password not accepted".into());
        assert_eq!(decode::<ServerMsg>(&encode(&no).unwrap()).unwrap(), no);

        // Non-ASCII in either field: passwords are whatever the operator typed.
        let answer =
            ClientMsg::Auth { username: "oe1test".into(), password: "pässwörd ✓".into() };
        assert_eq!(decode::<ClientMsg>(&encode(&answer).unwrap()).unwrap(), answer);
    }

    #[test]
    fn rejects_wrong_version_byte() {
        assert!(matches!(decode::<ClientMsg>(&[0x7f, 0, 0]), Err(ProtoError::Version(0x7f))));
        assert!(matches!(decode::<ClientMsg>(&[]), Err(ProtoError::Empty)));
    }
}
