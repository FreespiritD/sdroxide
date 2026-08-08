//! The single remote WebSocket session: Hello handshake, sign-in, codec
//! negotiation, three-lane sender, and the command/mic receive loop.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{info, warn};

use sdroxide_proto::{AudioCaps, AudioCodec, ClientMsg, PROTO_VERSION, ServerMsg, decode, encode};
use sdroxide_types::Command;

use crate::auth;
use crate::{SessionTx, Shared};

pub async fn ws_route(State(shared): State<Arc<Shared>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(|socket| session(socket, shared))
}

fn msg(m: &ServerMsg) -> Message {
    Message::Binary(encode(m).expect("encode").into())
}

async fn session(mut socket: WebSocket, shared: Arc<Shared>) {
    // Hello and the sign-in first, and only then the single-client slot. The
    // order matters: claiming the slot before knowing who this is would let
    // anyone who can open a socket lock the operator out of their own radio
    // without ever proving they may touch it.
    let Some(audio_caps) = handshake(&mut socket, &shared).await else {
        let _ = socket.close().await;
        return;
    };

    // Single-client rule: the loser gets Busy and is closed immediately.
    if shared.busy.swap(true, Ordering::SeqCst) {
        let _ = socket.send(msg(&ServerMsg::Busy)).await;
        let _ = socket.close().await;
        return;
    }
    run_session(&mut socket, &shared, audio_caps).await;

    // Cleanup — whatever happened, release the slot and drop the key.
    *shared.session.lock().unwrap() = None;
    shared.busy.store(false, Ordering::SeqCst);
    let _ = shared.cmd_tx.send(Command::SetPtt(false));
    let _ = shared.cmd_tx.send(Command::SetTune(false));
    info!("remote session ended");
}

/// `Hello`, then the sign-in challenge if this server has one. `None` means the
/// socket is finished with — the caller closes it and claims nothing.
///
/// The version check comes first so a client on the wrong protocol is told
/// exactly that, rather than being asked to sign in to a server it could not
/// have talked to anyway.
async fn handshake(socket: &mut WebSocket, shared: &Arc<Shared>) -> Option<AudioCaps> {
    // --- Hello (5 s budget) -------------------------------------------
    let hello = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await;
    let audio_caps = match hello {
        Ok(Some(Ok(Message::Binary(bytes)))) => match decode::<ClientMsg>(&bytes) {
            Ok(ClientMsg::Hello { proto, audio }) if proto == PROTO_VERSION => audio,
            Ok(ClientMsg::Hello { proto, .. }) => {
                let _ = socket
                    .send(msg(&ServerMsg::Error(format!(
                        "protocol mismatch: server {PROTO_VERSION}, client {proto}"
                    ))))
                    .await;
                return None;
            }
            _ => {
                let _ = socket.send(msg(&ServerMsg::Error("expected Hello".into()))).await;
                return None;
            }
        },
        _ => return None,
    };

    // --- Sign-in ------------------------------------------------------
    let signed_in = auth::challenge(
        socket,
        &shared.auth,
        auth::Frames {
            what: "/ws",
            required: encode(&ServerMsg::AuthRequired).expect("encode"),
            rejected: &|why| encode(&ServerMsg::AuthRejected(why.into())).expect("encode"),
            credentials: &|bytes| match decode::<ClientMsg>(bytes) {
                Ok(ClientMsg::Auth { username, password }) => Some((username, password)),
                _ => None,
            },
        },
    )
    .await;
    signed_in.then_some(audio_caps)
}

async fn run_session(socket: &mut WebSocket, shared: &Arc<Shared>, audio_caps: AudioCaps) {
    let rx_codec =
        if audio_caps.opus_decode { AudioCodec::Opus48kMono } else { AudioCodec::Pcm16_48k };
    let tx_codec =
        if audio_caps.opus_encode { AudioCodec::Opus48kMono } else { AudioCodec::Pcm16_48k };

    let (
        caps,
        state,
        memories,
        mem_folders,
        scanner,
        digi,
        voice,
        images,
        notice,
        station,
        tle_subs,
        sat_track,
    ) = {
        let latest = shared.latest.lock().unwrap();
        (
            latest.caps.clone(),
            latest.state.clone(),
            latest.memories.clone(),
            latest.mem_folders.clone(),
            latest.scanner.clone(),
            latest.digi.clone(),
            latest.voice.clone(),
            latest.images.clone(),
            latest.notice.clone(),
            latest.station.clone(),
            latest.tle_subs.clone(),
            latest.sat_track.clone(),
        )
    };
    let ack = ServerMsg::HelloAck { proto: PROTO_VERSION, caps, state, rx_codec, tx_codec };
    if socket.send(msg(&ack)).await.is_err() {
        return;
    }
    let _ = socket.send(msg(&ServerMsg::Memories(memories))).await;
    let _ = socket.send(msg(&ServerMsg::MemoryFolders(mem_folders))).await;
    let _ = socket.send(msg(&ServerMsg::Scanner(scanner))).await;
    // The operator config, which the engine announced once at startup. Without
    // this replay the client's callsign and grid come up empty and greyed out.
    if let Some(d) = digi {
        let _ = socket.send(msg(&ServerMsg::Ft8Status(d))).await;
    }
    // Likewise the voice keyer's slots, announced once at engine start.
    if let Some(v) = voice {
        let _ = socket.send(msg(&ServerMsg::VoiceStatus(v))).await;
    }
    // And the transmit-image presets, for the same reason. The received
    // galleries are not replayed: a panel lists its store when it opens, which
    // is both authoritative and the only view that can be paged.
    if let Some(p) = images {
        let _ = socket.send(msg(&ServerMsg::ImagePresets(p))).await;
    }
    // What the station is set up to do, announced at engine start like the
    // operator config. Without this replay the settings dialog here shows
    // defaults for every server-side tab — and applying them would write those
    // defaults over the operator's real configuration.
    if let Some(s) = station {
        let _ = socket.send(msg(&ServerMsg::StationConfig(s))).await;
        let _ = socket.send(msg(&ServerMsg::TleSubStatus(tle_subs))).await;
    }
    // The satellite lock is a condition too: a client attaching mid-pass has
    // to see it immediately, and must not offer to start one that is running.
    if sat_track.is_some() {
        let _ = socket.send(msg(&ServerMsg::SatTrack(sat_track))).await;
    }
    // A standing condition rather than an event: whoever attaches next has to
    // know the radio is refusing tunes or reconnecting, not just whoever
    // happened to be connected when it started.
    if notice.is_some() {
        let _ = socket.send(msg(&ServerMsg::Notice(notice))).await;
    }
    info!(?rx_codec, ?tx_codec, "remote client connected");

    // --- register lanes -----------------------------------------------
    let (rel_tx, mut rel_rx) = mpsc::channel::<ServerMsg>(256);
    let (aud_tx, mut aud_rx) = mpsc::channel::<ServerMsg>(8);
    *shared.session.lock().unwrap() = Some(SessionTx { reliable: rel_tx, audio: aud_tx, rx_codec });

    let (mut ws_tx, mut ws_rx) = futures_util::StreamExt::split(socket);

    // Sender: reliable first, then audio, then latest spectrum.
    let mut spectrum_rx = shared.spectrum_rx.clone();
    let mut wide_rx = shared.wide_spectrum_rx.clone();
    let sender = async {
        let mut last_spectrum_seq = 0u32;
        let mut last_wide_seq = 0u32;
        loop {
            tokio::select! {
                biased;
                m = rel_rx.recv() => {
                    let Some(m) = m else { break };
                    if ws_tx.send(msg(&m)).await.is_err() { break; }
                }
                m = aud_rx.recv() => {
                    let Some(m) = m else { break };
                    if ws_tx.send(msg(&m)).await.is_err() { break; }
                }
                changed = wide_rx.changed() => {
                    if changed.is_err() { break; }
                    let frame = wide_rx.borrow_and_update().clone();
                    if let Some(f) = frame {
                        if f.seq != last_wide_seq {
                            last_wide_seq = f.seq;
                            if ws_tx.send(msg(&ServerMsg::WideSpectrum(f))).await.is_err() { break; }
                        }
                    }
                }
                changed = spectrum_rx.changed() => {
                    if changed.is_err() { break; }
                    let frame = spectrum_rx.borrow_and_update().clone();
                    if let Some(f) = frame {
                        if f.seq != last_spectrum_seq {
                            last_spectrum_seq = f.seq;
                            if ws_tx.send(msg(&ServerMsg::Spectrum(f))).await.is_err() { break; }
                        }
                    }
                }
            }
        }
    };

    // Receiver: commands, mic frames, pings.
    let receiver = async {
        let mut opus_dec: Option<opus::Decoder> = None;
        let mut pcm = vec![0.0f32; 5760];
        while let Some(Ok(m)) = ws_rx.next().await {
            let Message::Binary(bytes) = m else {
                if matches!(m, Message::Close(_)) {
                    break;
                }
                continue;
            };
            match decode::<ClientMsg>(&bytes) {
                Ok(ClientMsg::Command(cmd)) => {
                    let _ = shared.cmd_tx.send(cmd);
                }
                Ok(ClientMsg::MicFrame { payload, .. }) => {
                    let n = match tx_codec {
                        AudioCodec::Opus48kMono => {
                            let dec = opus_dec.get_or_insert_with(|| {
                                opus::Decoder::new(48_000, opus::Channels::Mono)
                                    .expect("opus decoder")
                            });
                            match dec.decode_float(&payload, &mut pcm, false) {
                                Ok(n) => n,
                                Err(e) => {
                                    warn!("opus decode: {e}");
                                    continue;
                                }
                            }
                        }
                        AudioCodec::Pcm16_48k => {
                            let mut n = 0;
                            for (i, c) in payload.chunks_exact(2).enumerate() {
                                if i >= pcm.len() {
                                    break;
                                }
                                pcm[i] = i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0;
                                n += 1;
                            }
                            n
                        }
                    };
                    let mut mic = shared.mic_tx.lock().unwrap();
                    for &s in &pcm[..n] {
                        if mic.push(s).is_err() {
                            break; // ring full — engine will catch up
                        }
                    }
                }
                Ok(ClientMsg::Ping(t)) => {
                    if let Some(s) = shared.session.lock().unwrap().as_ref() {
                        let _ = s.reliable.try_send(ServerMsg::Pong(t));
                    }
                }
                Ok(ClientMsg::Hello { .. }) => {} // ignore late Hello
                // Likewise a late `Auth`: this socket is already signed in, so
                // there is nothing to re-check, and running it through the gate
                // would let an established client lock everybody else's sign-in
                // out for three seconds at a time.
                Ok(ClientMsg::Auth { .. }) => {}
                Err(e) => warn!("bad client message: {e}"),
            }
        }
    };

    tokio::select! {
        _ = sender => {}
        _ = receiver => {}
    }
}
