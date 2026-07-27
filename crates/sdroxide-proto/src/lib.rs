//! WebSocket wire protocol between the sdroxide server and remote clients.
//!
//! Framing: every binary WS message is `[PROTO_VERSION_BYTE][postcard bytes]`.
//! The version byte is a fast sanity check; the real version negotiation
//! happens in `Hello`/`HelloAck`.
//!
//! Compiles for native and `wasm32-unknown-unknown`.

use serde::{Deserialize, Serialize};

use sdroxide_types::{
    CallsignInfo, Command, Decode, DeviceCaps, DigiStatus, MemoryChannel, Meters, QsoRecord,
    RadioState, SkimmerSpot, Spot, SpectrumFrame, SstvMode, SstvStatus, UploadResult,
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
pub const PROTO_VERSION: u16 = 15;
const VERSION_BYTE: u8 = 0x0F;

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
    Hello { proto: u16, audio: AudioCaps },
    Command(Command),
    /// 20 ms mic frame in the codec negotiated at Hello.
    MicFrame { seq: u32, payload: Vec<u8> },
    Ping(u64),
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
    RxAudio { seq: u32, payload: Vec<u8> },
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
    SstvLine { image_id: u32, y: u16, rgb: Vec<u8> },
    SstvImage { image_id: u32, mode: SstvMode, w: u16, h: u16, png: Vec<u8> },
    SstvStatus(SstvStatus),
    /// FSQ image: a completed received picture (PNG bytes).
    DigiImage { png: Vec<u8> },
    // Network cockpit.
    Spots(Vec<Spot>),
    NetStatus(Option<String>),
    CallsignResult(CallsignInfo),
    Upload(UploadResult),
    Confirmations(Vec<QsoRecord>),
    /// Built-in TCI server status (listener up, bind address, client count).
    TciServerStatus { running: bool, addr: String, clients: usize, error: Option<String> },
    /// Built-in rigctld server status, so the settings dialog on a remote
    /// client can show what the engine's listener is doing.
    RigctldStatus { running: bool, addr: String, clients: usize, error: Option<String> },
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
    }

    #[test]
    fn rejects_wrong_version_byte() {
        assert!(matches!(decode::<ClientMsg>(&[0x7f, 0, 0]), Err(ProtoError::Version(0x7f))));
        assert!(matches!(decode::<ClientMsg>(&[]), Err(ProtoError::Empty)));
    }
}
