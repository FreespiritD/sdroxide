//! Headless server mode: engine + WebSocket/HTTP frontend.

use std::path::PathBuf;

use anyhow::Result;
use sdroxide_config::Settings;
use sdroxide_radio::rtrb;
use sdroxide_radio::{AudioParams, EngineConfig, IqSource, MicParams, ReopenFn, start_engine};
use sdroxide_server::ServerParams;
use sdroxide_types::{DeviceCaps, Mode};

pub fn run(
    source: Box<dyn IqSource>,
    caps: DeviceCaps,
    settings: &Settings,
    // Whether the engine refuses to key outside the amateur bands. Resolved in
    // `main` from `config.toml` and the `--oob-tx` flag.
    tx_ham_only: bool,
    initial_mode: Option<Mode>,
    // Antenna ports named on the command line (RX, TX); the remembered session
    // fills in whichever the operator left out. The main reason this flag
    // exists: nobody is sitting at a headless server to pick a port by hand.
    initial_antenna: (Option<String>, Option<String>),
    port: u16,
    web_root: Option<PathBuf>,
    reopen: Option<ReopenFn>,
) -> Result<()> {
    // Demod audio ring (engine → server, interleaved stereo @48 k) and mic
    // ring (server → engine, mono @48 k).
    let (audio_producer, audio_consumer) = rtrb::RingBuffer::<f32>::new(48_000 * 2);
    let (mic_producer, mic_consumer) = rtrb::RingBuffer::<f32>::new(48_000);

    let handles = start_engine(
        source,
        caps,
        EngineConfig {
            audio: Some(AudioParams { producer: audio_producer, out_rate: 48_000.0 }),
            mic: Some(MicParams { consumer: mic_consumer, rate: 48_000.0 }),
            cal_offset_db: settings.cal_offset_db as f32,
            initial_mode,
            initial_antenna,
            tx_ham_only,
            // A headless server is typically started before the rig it talks
            // to; the engine uses this to attach as soon as the radio is there.
            reopen,
            // The server *is* the radio for everyone connected to it, so it is
            // the side that remembers where the last session was left.
            remember_session: true,
            // Server mode is single-radio: the station scope, no interlock,
            // no peer engines to sync stores with.
            ..Default::default()
        },
    );

    sdroxide_server::run_blocking(ServerParams {
        cmd_tx: handles.cmd_tx,
        event_rx: handles.event_rx,
        spectrum_out: handles.spectrum_out,
        wide_spectrum_out: handles.wide_spectrum_out,
        audio_rx: audio_consumer,
        mic_tx: mic_producer,
        bind: settings.server_bind.clone(),
        port,
        web_root,
        // Re-read per connection rather than captured from `settings`: the
        // credentials are a file on this machine, and an operator who changes
        // their password — by hand, or from the settings dialog of the GUI
        // running beside this server — should not have to restart the server
        // and drop whoever is on it for the change to hold.
        access: Some(Box::new(sdroxide_config::load_remote_access)),
        // The same enumeration the local settings dialog uses, offered to
        // whoever is connected. Without it the Rescan / Discover / Test buttons
        // on a remote or browser client have nothing to answer them, and a
        // headless station's radio could only be changed by editing
        // `radio.json` on this machine and restarting.
        probe: Some(Box::new(crate::devices::probe)),
    })?;
    Ok(())
}
