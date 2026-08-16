//! Headless server mode: one engine per radio in the station roster, behind a
//! single WebSocket/HTTP frontend.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use sdroxide_config::Settings;
use sdroxide_radio::rtrb;
use sdroxide_radio::{AudioParams, EngineConfig, MicParams, StoreSync, TxGate, start_engine};
use sdroxide_server::{RadioParams, ServerParams};

use crate::RadioBoot;

pub fn run(
    // The station's radios, in roster order, already opened by `main` — the
    // same list the GUI would have put in tabs. The first is what a client
    // that names no radio gets.
    radios: Vec<RadioBoot>,
    settings: &Settings,
    // Whether the engines refuse to key outside the amateur bands. Resolved in
    // `main` from `config.toml` and the `--oob-tx` flag.
    tx_ham_only: bool,
    port: u16,
    web_root: Option<PathBuf>,
) -> Result<()> {
    // Shared by every engine in this process, exactly as in the GUI: one
    // transmitter on the air at a time, and a store change made through one
    // radio seen by the rest. A server needs the interlock more than the shack
    // does, not less — nobody is sitting here to notice two radios keying.
    let gate = Arc::new(TxGate::new());
    let sync = Arc::new(StoreSync::new());

    let mut params = Vec::with_capacity(radios.len());
    for (i, boot) in radios.into_iter().enumerate() {
        // Demod audio ring (engine → server, interleaved stereo @48 k) and mic
        // ring (server → engine, mono @48 k), one pair per radio.
        let (audio_producer, audio_consumer) = rtrb::RingBuffer::<f32>::new(48_000 * 2);
        let (mic_producer, mic_consumer) = rtrb::RingBuffer::<f32>::new(48_000);

        let handles = start_engine(
            boot.source,
            boot.caps,
            EngineConfig {
                audio: Some(AudioParams { producer: audio_producer, out_rate: 48_000.0 }),
                mic: Some(MicParams { consumer: mic_consumer, rate: 48_000.0 }),
                cal_offset_db: settings.cal_offset_db as f32,
                initial_mode: boot.initial_mode,
                initial_antenna: boot.initial_antenna,
                tx_ham_only,
                // A headless server is typically started before the rig it
                // talks to; the engine uses this to attach as soon as the
                // radio is there.
                reopen: boot.reopen,
                // The server *is* the radio for everyone connected to it, so it
                // is the side that remembers where the last session was left —
                // each radio in its own scope.
                remember_session: true,
                store: boot.store,
                instance: boot.id,
                // Exactly one engine runs the station-wide network services,
                // for the same reason as in the GUI: they hold logins and
                // sockets that must not be opened once per radio.
                primary: i == 0,
                tx_gate: Some(gate.clone()),
                store_sync: Some(sync.clone()),
            },
        );

        params.push(RadioParams {
            id: boot.id,
            name: boot.name,
            cmd_tx: handles.cmd_tx,
            event_rx: handles.event_rx,
            spectrum_out: handles.spectrum_out,
            wide_spectrum_out: handles.wide_spectrum_out,
            audio_rx: audio_consumer,
            mic_tx: mic_producer,
        });
    }

    sdroxide_server::run_blocking(ServerParams {
        radios: params,
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
