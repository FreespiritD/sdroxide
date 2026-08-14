//! The engine host's interface configuration, over a real socket.
//!
//! This is the whole of what makes an SDR's own settings reachable from another
//! machine: the engine announces `radio.json`, the server keeps it and replays
//! it to whoever connects, and an edit sent back is written where the radio is.
//! An RTL-SDR's AGC mode, its ppm correction and its bias tee are not gain
//! stages and have no `DeviceCaps` entry to ride — this lane is the only route
//! they have.
//!
//! Its own test binary because it redirects the config directory through the
//! environment, which is process-global: the tests in `session.rs` read that
//! directory, and must not find it moved out from under them.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use sdroxide_proto::{AudioCaps, ClientMsg, PROTO_VERSION, ServerMsg, decode, encode};
use sdroxide_radio::{EngineConfig, SigGenSource, start_engine};
use sdroxide_server::{ServerParams, serve};
use sdroxide_types::{
    Backend, Command, DeviceCaps, RadioConfig, RtlSdrAgc, RtlSdrConfig, RtlSdrHfMode,
};

const PORT: u16 = 39473;

async fn recv_msg(
    ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
) -> ServerMsg {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(15), ws.next())
            .await
            .expect("timeout waiting for server message")
            .expect("stream ended")
            .expect("ws error");
        if let Message::Binary(bytes) = m {
            return decode::<ServerMsg>(&bytes).expect("decode");
        }
    }
}

/// Wait for the next `RadioConfig`, ignoring the streams running underneath it.
async fn next_radio_config(
    ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
) -> RadioConfig {
    loop {
        if let ServerMsg::RadioConfig(c) = recv_msg(ws).await {
            return *c;
        }
    }
}

async fn send(
    ws: &mut (impl SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin),
    m: &ClientMsg,
) {
    ws.send(Message::Binary(encode(m).unwrap().into())).await.unwrap();
}

/// What a dongle on the far end is configured to do. Deliberately not the
/// defaults: a message that arrived empty, or one whose fields had shifted,
/// would still look plausible against `RadioConfig::default()`.
fn far_end_dongle() -> RadioConfig {
    RadioConfig {
        backend: Backend::RtlSdr,
        rtlsdr: RtlSdrConfig {
            serial: Some("00000042".into()),
            sample_rate_hz: 1_536_000.0,
            ppm: -11,
            tuner_gain_db: 22.5,
            agc: RtlSdrAgc::Manual,
            hf_mode: RtlSdrHfMode::Auto,
            bias_tee: false,
            iq_correction: true,
            ..RtlSdrConfig::default()
        },
        ..RadioConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_servers_interface_configuration_is_replayed_and_can_be_edited() {
    let root = std::env::temp_dir().join(format!("sdroxide-radiocfg-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch config dir");
    // SAFETY: set before anything reads it, and this binary holds one test.
    unsafe { std::env::set_var("SDROXIDE_CONFIG_DIR", &root) };

    // Seeded through the same store the engine reads it with: a hand-rolled
    // file could pass this test against a shape the engine cannot load.
    let store = sdroxide_config::Store::station();
    let stored = far_end_dongle();
    store.save_radio_config(&stored).expect("seed radio.json");

    // The source is a signal generator rather than a dongle — this lane carries
    // the *configuration*, and does not care what the front end turned out to
    // be. What matters is that the engine's store is the scratch directory.
    let handles = start_engine(
        Box::new(SigGenSource::demo(1_536_000.0, 14_200_000.0)),
        DeviceCaps {
            driver: "siggen".into(),
            label: "Test signal generator".into(),
            rx_channels: 1,
            freq_ranges_rx: vec![(0.0, 6e9)],
            ..DeviceCaps::default()
        },
        EngineConfig::default(),
    );
    tokio::spawn(serve(ServerParams {
        cmd_tx: handles.cmd_tx,
        event_rx: handles.event_rx,
        spectrum_out: handles.spectrum_out,
        wide_spectrum_out: handles.wide_spectrum_out,
        audio_rx: sdroxide_radio::rtrb::RingBuffer::<f32>::new(96_000).1,
        mic_tx: sdroxide_radio::rtrb::RingBuffer::<f32>::new(48_000).0,
        bind: "127.0.0.1".into(),
        port: PORT,
        web_root: None,
        access: None,
    }));
    // Long enough that the engine's one startup announcement is already behind
    // us when the client arrives — which is the case the replay exists for.
    tokio::time::sleep(Duration::from_millis(800)).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{PORT}/ws"))
        .await
        .expect("connect");
    send(
        &mut ws,
        &ClientMsg::Hello {
            proto: PROTO_VERSION,
            audio: AudioCaps { opus_decode: false, opus_encode: false },
        },
    )
    .await;

    // On connect: what the far end is set to, not what this machine's defaults
    // are. Without the replay the Radio tab would open on those defaults and
    // write them back over the operator's the moment anything was touched.
    let announced = next_radio_config(&mut ws).await;
    assert_eq!(announced, stored, "the connect-time replay did not carry the stored config");

    // ...and the settings that have no `DeviceCaps` entry survived the trip.
    // These are the ones a remote operator could not reach at all before.
    assert_eq!(announced.rtlsdr.agc, RtlSdrAgc::Manual);
    assert_eq!(announced.rtlsdr.ppm, -11);
    assert_eq!(announced.rtlsdr.tuner_gain_db, 22.5);

    // An edit from the client is written where the radio is, and echoed back.
    // `reopen: false` — this is the half that persists; the knob itself has
    // already reached the hardware through `SetGain`.
    let mut edited = stored.clone();
    edited.rtlsdr.tuner_gain_db = 41.2;
    edited.rtlsdr.agc = RtlSdrAgc::Tuner;
    edited.rtlsdr.bias_tee = true;
    send(
        &mut ws,
        &ClientMsg::Command(Command::SetRadioConfig {
            cfg: Box::new(edited.clone()),
            reopen: false,
        }),
    )
    .await;

    let echoed = next_radio_config(&mut ws).await;
    assert_eq!(echoed, edited, "the echo did not carry the edit");

    // The echo is read back from the store rather than bounced off the command,
    // so this is most of the assertion that the file was written — but read the
    // file too, because "announced" and "persisted" are the two halves that
    // have to agree for a restart to come up on the same settings.
    assert!(root.join("radio.json").exists(), "nothing was written where the radio is");
    assert_eq!(
        store.load_radio_config(),
        edited,
        "radio.json on the engine's machine was not updated"
    );

    let _ = std::fs::remove_dir_all(&root);
}
