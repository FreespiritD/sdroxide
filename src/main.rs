mod audio_cat_source;
mod console;
mod device_registry;
mod gui_main;
mod hpsdr_source;
mod local_controller;
mod null_source;
mod pluto_source;
mod rtlsdr_source;
mod rx888_source;
mod sdrplay_source;
mod server_main;
mod smartsdr_source;
mod tci_source;

use anyhow::{Context, bail};
use clap::Parser;
use sdroxide_config::Settings;
use sdroxide_radio::{
    ConvertedSource, FileSource, IqSource, SigGenSource, override_caps_ranges, shift_caps,
};
#[cfg(feature = "soapy")]
use sdroxide_radio::{DeviceInfo, SoapyDevice, enumerate_devices};
use sdroxide_types::{Backend, DeviceCaps, RadioConfig};

#[derive(Parser, Debug, Clone)]
#[command(version, about)]
struct Cli {
    /// SoapySDR device args, e.g. "driver=hackrf" (default: config, then first device)
    #[arg(long)]
    device: Option<String>,

    /// List devices and their probed capabilities, then exit
    #[arg(long)]
    probe: bool,

    /// Terminal waterfall mode
    #[arg(long)]
    console: bool,

    /// Use the built-in signal generator instead of hardware
    #[arg(long)]
    siggen: bool,

    /// Play a raw interleaved CF32 IQ file instead of hardware
    #[arg(long)]
    file: Option<std::path::PathBuf>,

    /// Center frequency in Hz (default: where the last session was left)
    #[arg(long)]
    freq: Option<f64>,

    /// Sample rate in Hz (default: from config)
    #[arg(long)]
    rate: Option<f64>,

    /// Overall RX gain in dB (default: hardware AGC or a moderate value)
    #[arg(long)]
    gain: Option<f64>,

    /// Initial mode (USB, LSB, CW, AM, SAM, NFM, WFM, DIGU, DIGL, DSB, SPEC, FT8,
    /// FT4, FT2, PSK, RTTY, SSTV, RIFP, OLIVIA, THOR, FSQ)
    ///
    /// Default: the mode the last session was left in.
    #[arg(long)]
    mode: Option<sdroxide_types::Mode>,

    /// RX antenna port, as the device names it ("LNAH", "TX/RX"). Run --probe
    /// to list what the front end offers.
    ///
    /// Default: the port the last session was left on, and failing that
    /// whatever the driver selects when it opens the device. Worth setting on a
    /// headless server, where nobody is at the machine to pick one.
    #[arg(long, value_name = "NAME")]
    antenna: Option<String>,

    /// TX antenna port, likewise ("BAND1", "BAND2")
    #[arg(long, value_name = "NAME")]
    tx_antenna: Option<String>,

    /// Headless TX smoke test: key a tune carrier for SECS seconds at the
    /// configured (minimal) drive and gains, then exit
    #[arg(long, value_name = "SECS")]
    tx_tune: Option<f64>,

    /// Headless FT8 smoke test: call CQ (with a test callsign) at minimal
    /// power for ~SECS seconds, report whether a slot-aligned burst keyed
    #[arg(long, value_name = "SECS")]
    ft8_cq: Option<f64>,

    /// Headless RADE smoke test: run the digital-voice receiver for ~SECS
    /// seconds (pair with --file) and report whether the modem reached sync
    #[arg(long, value_name = "SECS")]
    rade_rx: Option<f64>,

    /// Connect to FreeDV Reporter read-only for ~SECS seconds and print what
    /// arrives. Uses the server's "view" role: nothing is reported and this
    /// station does not appear on qso.freedv.org. Needs no radio.
    #[arg(long, value_name = "SECS")]
    freedv_reporter_probe: Option<f64>,

    /// FreeDV Reporter host for --freedv-reporter-probe
    #[arg(long, value_name = "HOST[:PORT]", default_value = "qso.freedv.org")]
    freedv_reporter_host: String,

    /// Run as a server: HTTP web client + WebSocket streaming backend
    #[arg(long)]
    server: bool,

    /// Connect as a native remote client to a running sdroxide server
    /// (e.g. "host:4950" or a full ws:// URL)
    #[arg(long, value_name = "HOST[:PORT]")]
    connect: Option<String>,

    /// Server port (default: from config)
    #[arg(long)]
    port: Option<u16>,

    /// Directory with the trunk-built web client (default: embedded assets
    /// if compiled with --features embed-web)
    #[arg(long)]
    web_root: Option<std::path::PathBuf>,

    /// Spectrum FFT size
    #[arg(long, default_value_t = 4096)]
    fft: usize,

    /// Console waterfall lines per second
    #[arg(long, default_value_t = 15)]
    fps: u32,

    /// Display floor in dBFS
    #[arg(long, default_value_t = -110.0, allow_negative_numbers = true)]
    db_floor: f32,

    /// Display ceiling in dBFS
    #[arg(long, default_value_t = -10.0, allow_negative_numbers = true)]
    db_ceil: f32,

    /// Console spectrum width in characters
    #[arg(long, default_value_t = 100)]
    width: usize,

    /// Allow transmit on any frequency the hardware supports, not just the
    /// amateur bands
    ///
    /// Overrides `tx_ham_only` in config.toml for this run only, and puts a
    /// warning on screen that has to be dismissed by hand. For licensed
    /// out-of-band use — MARS/CAP, a commercial or experimental licence, a
    /// dummy load — where transmitting outside the amateur allocations is
    /// something you are authorised to do. Everywhere else it is an offence,
    /// and the band edges are the last thing standing between a mistyped
    /// frequency and an interference complaint.
    #[arg(long)]
    oob_tx: bool,
}

impl Cli {
    /// The dial to open the front end on. Always resolved by
    /// [`Cli::restore_session`] before anything reads it; the fallback is only
    /// so this can never panic on a code path that skipped that.
    fn center_hz(&self) -> f64 {
        self.freq.unwrap_or_else(|| sdroxide_config::Session::default().freq_hz)
    }

    /// Fill in the frequency and mode the operator did not ask for from where
    /// the last session was left, so the program comes back up where it was.
    /// Returns the mode the engine should start in.
    fn restore_session(&mut self) -> Option<sdroxide_types::Mode> {
        self.apply_session(sdroxide_config::load_session())
    }

    /// The command line always wins; whatever it left out comes from the
    /// remembered session.
    fn apply_session(&mut self, session: sdroxide_config::Session) -> Option<sdroxide_types::Mode> {
        self.freq = Some(self.freq.unwrap_or(session.freq_hz));
        self.mode = Some(self.mode.unwrap_or(session.mode));
        self.mode
    }

    /// The antenna ports named on the command line, RX then TX.
    ///
    /// Not merged with the session here, the way the dial and mode are: the
    /// front end has to be open before a port name means anything, so the engine
    /// does that merge once it can check the names against what the device
    /// actually offers.
    fn initial_antenna(&self) -> (Option<String>, Option<String>) {
        (self.antenna.clone(), self.tx_antenna.clone())
    }

    /// Whether the engine should refuse to key outside the amateur bands.
    ///
    /// The flag can only ever *loosen* the config, never tighten it: a build
    /// without it behaves exactly as before.
    fn tx_ham_only(&self, settings: &Settings) -> bool {
        settings.tx_ham_only && !self.oob_tx
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut cli = Cli::parse();
    let settings = Settings::load();

    if cli.probe {
        return probe(&cli, &settings);
    }
    if cli.console {
        cli.restore_session();
        let (source, _caps) = open_source(&cli, &settings)?;
        return console::run(
            source,
            console::Options {
                fft_size: cli.fft,
                fps: cli.fps.max(1),
                db_floor: cli.db_floor,
                db_ceil: cli.db_ceil,
                width: cli.width.clamp(16, 400),
            },
        );
    }

    // The headless smoke tests below deliberately do *not* restore the
    // remembered dial. They are diagnostics — one of them keys a carrier — and
    // they have to run somewhere predictable rather than wherever the last
    // session happened to end. `--freq` still moves them.
    if let Some(secs) = cli.tx_tune {
        let (source, caps) = open_source(&cli, &settings)?;
        return tx_tune_test(source, caps, cli.tx_ham_only(&settings), secs.clamp(0.2, 10.0));
    }
    if let Some(secs) = cli.ft8_cq {
        let (source, caps) = open_source(&cli, &settings)?;
        return ft8_cq_test(source, caps, cli.tx_ham_only(&settings), secs.clamp(16.0, 60.0));
    }
    if let Some(secs) = cli.rade_rx {
        let (source, caps) = open_source(&cli, &settings)?;
        return rade_rx_test(source, caps, cli.tx_ham_only(&settings), secs.clamp(2.0, 120.0));
    }
    // Before any radio setup: the probe talks to the network and nothing else.
    if let Some(secs) = cli.freedv_reporter_probe {
        return freedv_reporter_probe(&cli.freedv_reporter_host, secs.clamp(2.0, 300.0));
    }
    if cli.server {
        let initial_mode = cli.restore_session();
        let (source, caps) = open_source(&cli, &settings)?;
        let port = cli.port.unwrap_or(settings.server_port);
        return server_main::run(
            source,
            caps,
            &settings,
            cli.tx_ham_only(&settings),
            initial_mode,
            cli.initial_antenna(),
            port,
            cli.web_root.clone(),
            Some(reopen_factory(&cli)),
        );
    }
    // A remote client drives somebody else's engine; that engine is the one
    // that remembers where its radio was left.
    if let Some(target) = &cli.connect {
        let url = if target.contains("://") { target.clone() } else { format!("ws://{target}/ws") };
        return gui_main::run_remote(&url);
    }

    // The station's radio roster: the legacy single radio unless more have
    // been added. Radio 0 keeps every command-line override and the legacy
    // config paths; the others boot purely from their own scope, and a device
    // that isn't there right now costs a "reconnecting" tab, not the launch.
    let roster = sdroxide_config::load_radios();
    let mut radios = Vec::new();
    for (i, slot) in roster.radios.iter().enumerate() {
        let store = sdroxide_config::Store::radio(slot.id);
        let boot = if i == 0 {
            let initial_mode = cli.restore_session();
            let (source, caps) = open_source(&cli, &settings)?;
            gui_main::RadioBoot {
                id: slot.id,
                name: slot.name.clone(),
                source,
                caps,
                initial_mode,
                initial_antenna: cli.initial_antenna(),
                reopen: Some(reopen_factory(&cli)),
                store,
            }
        } else {
            let mut c = secondary_cli(&cli);
            let session = store.load_session();
            let initial_mode = c.apply_session(session);
            let (source, caps) =
                match open_converted_source(&store.load_radio_config(), &c, &settings) {
                    Ok(pair) => pair,
                    Err(e) => {
                        // Names are often empty now (the tab derives one from
                        // the interface); the id is what the engine's own
                        // messages use, so log that.
                        tracing::warn!("radio {} unavailable: {e:#}", slot.id + 1);
                        let msg = format!(
                            "{e}. Retrying — or open Settings → Radio to choose another interface."
                        );
                        (
                            Box::new(null_source::NullSource::new(c.center_hz(), msg))
                                as Box<dyn IqSource>,
                            synthetic_caps("No radio"),
                        )
                    }
                };
            gui_main::RadioBoot {
                id: slot.id,
                name: slot.name.clone(),
                source,
                caps,
                initial_mode,
                initial_antenna: (None, None),
                reopen: Some(reopen_factory_for(&c, store.clone())),
                store,
            }
        };
        radios.push(boot);
    }
    let factory_cli = secondary_cli(&cli);
    gui_main::run_multi(radios, &settings, cli.tx_ham_only(&settings), factory_cli)
}

/// Factory the engine calls to rebuild the interface at runtime: when the
/// operator switches interface (Settings → Radio → Apply), so that never needs a
/// restart, and when the engine reconnects an interface that wasn't there yet.
/// Re-reads the persisted radio config + settings each call and opens at the
/// current dial. Fallible so a bad new config leaves the current interface
/// running.
fn reopen_factory(cli: &Cli) -> sdroxide_radio::ReopenFn {
    reopen_factory_for(cli, sdroxide_config::Store::station())
}

/// [`reopen_factory`], reading `radio.json` from one radio's scope. The
/// `--siggen`/`--file` overrides only ever apply to the station scope (radio
/// 0); every caller building a factory for another radio passes a
/// [`secondary_cli`], which has them stripped.
fn reopen_factory_for(cli: &Cli, store: sdroxide_config::Store) -> sdroxide_radio::ReopenFn {
    let cli = cli.clone();
    Box::new(move |center: f64| {
        let mut c = cli.clone();
        c.freq = Some(center);
        let settings = Settings::load();
        if c.siggen || c.file.is_some() {
            return open_source(&c, &settings).map_err(|e| format!("{e:#}"));
        }
        let radio = store.load_radio_config();
        open_converted_source(&radio, &c, &settings).map_err(|e| format!("{e:#}"))
    })
}

/// The command line, minus everything that belongs to radio 0 alone: the
/// synthetic sources (`--siggen`, `--file`) and the tuning/antenna/device
/// overrides. A secondary radio boots purely from its own scope — the flags
/// were typed for the radio the operator has always had.
fn secondary_cli(cli: &Cli) -> Cli {
    let mut c = cli.clone();
    c.siggen = false;
    c.file = None;
    c.freq = None;
    c.mode = None;
    c.rate = None;
    c.gain = None;
    c.antenna = None;
    c.tx_antenna = None;
    c
}

/// Headless tune-carrier smoke test. Relies on the engine safety rails:
/// TX hardware gains at minimum, tune drive default 5%, ham-band lockout.
fn tx_tune_test(
    source: Box<dyn IqSource>,
    caps: sdroxide_types::DeviceCaps,
    tx_ham_only: bool,
    secs: f64,
) -> anyhow::Result<()> {
    use sdroxide_types::{Command, RadioEvent};
    use std::time::Duration;

    let mut handles = sdroxide_radio::start_engine(
        source,
        caps,
        sdroxide_radio::EngineConfig { tx_ham_only, ..Default::default() },
    );
    let engine_thread = handles.thread.take();
    std::thread::sleep(Duration::from_millis(400));
    handles.cmd_tx.send(Command::SetTune(true))?;
    std::thread::sleep(Duration::from_secs_f64(secs));
    handles.cmd_tx.send(Command::SetTune(false))?;
    std::thread::sleep(Duration::from_millis(400));

    let mut keyed = false;
    let mut failure = None;
    while let Ok(ev) = handles.event_rx.try_recv() {
        match ev {
            RadioEvent::State(s) => keyed |= s.tx.tune,
            RadioEvent::ConnectionLost(e) => failure = Some(e),
            _ => {}
        }
    }
    let outcome = match (keyed, failure) {
        (_, Some(e)) => Err(anyhow::anyhow!("TX test failed: {e}")),
        (false, None) => {
            Err(anyhow::anyhow!("TX was refused (safety rails or device limits) — see log"))
        }
        (true, None) => {
            println!("TX tune test OK: carrier keyed for {secs:.1} s and released.");
            Ok(())
        }
    };
    drop(handles);
    if let Some(t) = engine_thread {
        let _ = t.join();
    }
    outcome
}

/// Headless FT8 smoke test: configure a test callsign, enter FT8, call CQ,
/// and confirm the engine keys a slot-aligned burst. Minimal drive / min TX
/// gain (same emission level as `--tx-tune`).
fn ft8_cq_test(
    source: Box<dyn IqSource>,
    caps: sdroxide_types::DeviceCaps,
    tx_ham_only: bool,
    secs: f64,
) -> anyhow::Result<()> {
    use sdroxide_types::{Command, DigiConfig, Mode, RadioEvent, RxId};
    use std::time::Duration;

    let mut handles = sdroxide_radio::start_engine(
        source,
        caps,
        sdroxide_radio::EngineConfig {
            tx_ham_only,
            initial_mode: Some(Mode::Ft8),
            ..Default::default()
        },
    );
    let engine_thread = handles.thread.take();
    std::thread::sleep(Duration::from_millis(400));

    let cfg = DigiConfig { my_call: "AB1CD".into(), my_grid: "FN42".into(), ..Default::default() };
    handles.cmd_tx.send(Command::SetMode { rx: RxId::Main, mode: Mode::Ft8 })?;
    handles.cmd_tx.send(Command::SetDigiConfig(cfg))?;
    handles.cmd_tx.send(Command::DigiCallCq)?;

    let mut keyed = false;
    let mut failure = None;
    let deadline = std::time::Instant::now() + Duration::from_secs_f64(secs);
    while std::time::Instant::now() < deadline {
        while let Ok(ev) = handles.event_rx.try_recv() {
            match ev {
                RadioEvent::State(s) => keyed |= s.tx.ptt,
                RadioEvent::Ft8Status(s) if s.transmitting => keyed = true,
                RadioEvent::ConnectionLost(e) => failure = Some(e),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    handles.cmd_tx.send(Command::DigiStopQso)?;
    handles.cmd_tx.send(Command::DigiAbortTx)?;
    std::thread::sleep(Duration::from_millis(300));

    let outcome = match (keyed, failure) {
        (_, Some(e)) => Err(anyhow::anyhow!("FT8 CQ test failed: {e}")),
        (false, None) => Err(anyhow::anyhow!(
            "no FT8 burst keyed in {secs:.0}s — check UTC clock / safety rails (see log)"
        )),
        (true, None) => {
            println!("FT8 CQ test OK: a slot-aligned burst keyed and released.");
            Ok(())
        }
    };
    drop(handles);
    if let Some(t) = engine_thread {
        let _ = t.join();
    }
    outcome
}

/// Read-only FreeDV Reporter check: connect, listen, and print what the server
/// sends.
///
/// This uses the reporter's `"view"` role, which receives every broadcast but
/// never joins the public roster — so it is safe to point at qso.freedv.org.
/// Going *visible* requires an operator enabling the feature in Settings with a
/// real callsign; there is deliberately no flag for it here.
fn freedv_reporter_probe(host_arg: &str, secs: f64) -> anyhow::Result<()> {
    let (host, port) = match host_arg.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().unwrap_or(80)),
        None => (host_arg, 80),
    };
    println!("FreeDV Reporter probe (view role — not visible to others): {host}:{port}");
    println!("listening for {secs:.0} s…\n");

    let s = sdroxide_net::freedv_reporter_probe(host, port, secs);

    if s.event_counts.is_empty() {
        println!("events: (none)");
    } else {
        let counts: Vec<String> =
            s.event_counts.iter().map(|(name, n)| format!("{name} {n}")).collect();
        println!("events: {}", counts.join("  "));
    }
    let with_freq = s.stations.iter().filter(|st| st.freq_hz > 0).count();
    println!("stations: {} ({with_freq} with a frequency)\n", s.stations.len());
    for st in &s.stations {
        println!(
            "  {:<10} {:<8} {:>10.3} kHz  {:<8} {:<3} {}",
            st.call,
            st.grid,
            st.freq_hz as f64 / 1000.0,
            st.mode,
            if st.tx {
                "TX"
            } else if st.rx_only {
                "RX"
            } else {
                ""
            },
            st.version,
        );
    }
    println!(
        "\nhandshake: open={} connect_ack={} connection_successful={}",
        s.got_open, s.got_connect_ack, s.got_connection_successful
    );
    if let Some(status) = &s.last_status {
        println!("status: {status}");
    }
    if s.ok() {
        println!("\nPASS");
        Ok(())
    } else {
        bail!("FreeDV Reporter probe did not complete a session with at least one station")
    }
}

/// Headless RADE receive check: bring the engine up in RADE mode and report
/// what the modem made of whatever the source is feeding it.
///
/// Pair with `--file` and an IQ file from `rade-harness iq` for a repeatable
/// end-to-end run with no radio and no sound card.
fn rade_rx_test(
    source: Box<dyn IqSource>,
    caps: sdroxide_types::DeviceCaps,
    tx_ham_only: bool,
    secs: f64,
) -> anyhow::Result<()> {
    use sdroxide_types::{Command, Mode, RadioEvent, RxId};
    use std::time::Duration;

    // The receive chain — and with it the decoder's audio tap — only exists
    // when the engine has somewhere to play audio. Give it a ring buffer we
    // drain and throw away, so the test needs no sound card.
    let (producer, mut sink) = rtrb::RingBuffer::<f32>::new(96_000);
    let mut handles = sdroxide_radio::start_engine(
        source,
        caps,
        sdroxide_radio::EngineConfig {
            tx_ham_only,
            initial_mode: Some(Mode::Rade),
            audio: Some(sdroxide_radio::AudioParams { producer, out_rate: 48_000.0 }),
            ..Default::default()
        },
    );
    let engine_thread = handles.thread.take();
    handles.cmd_tx.send(Command::SetMode { rx: RxId::Main, mode: Mode::Rade })?;

    let (mut synced, mut best_snr, mut eoo, mut dropped) = (false, f32::MIN, 0u64, 0u64);
    let mut failure = None;
    let deadline = std::time::Instant::now() + Duration::from_secs_f64(secs);
    while std::time::Instant::now() < deadline {
        while let Ok(ev) = handles.event_rx.try_recv() {
            match ev {
                RadioEvent::Ft8Status(s) => {
                    if let Some(r) = s.rade {
                        if r.sync {
                            synced = true;
                            best_snr = best_snr.max(r.snr_db);
                        }
                        eoo = eoo.max(r.eoo_count);
                        dropped = dropped.max(r.dropped);
                    }
                }
                RadioEvent::ConnectionLost(e) => failure = Some(e),
                _ => {}
            }
        }
        while sink.pop().is_ok() {}
        std::thread::sleep(Duration::from_millis(50));
    }

    let outcome = match (synced, failure) {
        (_, Some(e)) => Err(anyhow::anyhow!("RADE test failed: {e}")),
        (false, None) => Err(anyhow::anyhow!(
            "RADE never reached sync in {secs:.0}s — is the source carrying a RADE signal?"
        )),
        (true, None) => {
            println!(
                "RADE RX test OK: sync reached, best SNR {best_snr:.1} dB, \
                 {eoo} end-of-over frame(s), {dropped} samples dropped."
            );
            Ok(())
        }
    };
    drop(handles);
    if let Some(t) = engine_thread {
        let _ = t.join();
    }
    outcome
}

#[cfg(feature = "soapy")]
fn device_filter(cli: &Cli, settings: &Settings) -> String {
    cli.device.clone().unwrap_or_else(|| settings.device_args.clone())
}

fn probe(cli: &Cli, settings: &Settings) -> anyhow::Result<()> {
    // RTL-SDR first, and in every build: the native driver needs no system
    // library, so this half of `--probe` works even in the non-SoapySDR
    // variant. It is the field-diagnosis tool for "does this machine see my
    // dongle, and may this user have it?".
    probe_rtlsdr();
    probe_rx888();
    probe_sdrplay();
    probe_soapy(cli, settings)
}

fn probe_rtlsdr() {
    let devices = sdroxide_rtlsdr::list();
    if devices.is_empty() {
        println!("No RTL-SDR dongles found on USB.");
    } else {
        println!("=== RTL-SDR (native USB driver) ===");
        for (i, d) in devices.iter().enumerate() {
            println!("  {}: {}  [usb {:04x}:{:04x}]", i, d.label(), d.vid, d.pid);
        }
    }
    println!();
}

fn probe_rx888() {
    let devices = sdroxide_rx888::list();
    if devices.is_empty() {
        println!("No RX-888 receivers found on USB.");
    } else {
        println!("=== RX-888 (native USB driver) ===");
        for (i, d) in devices.iter().enumerate() {
            // A receiver in its boot ROM always reports USB 2.0, so saying
            // "not SuperSpeed" there would be alarming and wrong.
            let link = if d.needs_firmware {
                "link speed unknown until programmed"
            } else if d.superspeed {
                "SuperSpeed"
            } else {
                "USB 2.0 — use a USB 3 cable and port for the full rate"
            };
            println!("  {}: {}  [{}]", i, d.label(), link);
        }
    }
    println!();
}

fn probe_sdrplay() {
    // Unlike the USB probes this one can fail three different ways, each with
    // a different fix, so the failure text is the whole point of printing it.
    match sdroxide_sdrplay::try_list() {
        Ok(devices) if devices.is_empty() => {
            println!("No SDRplay RSPs reported by the SDRplay API service.");
        }
        Ok(devices) => {
            println!("=== SDRplay RSP (vendor API service) ===");
            for (i, d) in devices.iter().enumerate() {
                println!("  {}: {}", i, d.label());
                if let Some(w) = d.identity_warning() {
                    println!("     ! {w}");
                }
            }
        }
        Err(e) => println!("SDRplay: {e}"),
    }
    println!();
}

#[cfg(feature = "soapy")]
fn probe_soapy(cli: &Cli, settings: &Settings) -> anyhow::Result<()> {
    let filter = device_filter(cli, settings);
    let devices = enumerate_devices(&filter).context("SoapySDR enumeration failed")?;
    if devices.is_empty() {
        println!("No SoapySDR devices found (filter: {:?}).", filter);
        return Ok(());
    }
    for (i, d) in devices.iter().enumerate() {
        println!("=== Device {}: {} [{}] ===", i, d.label, d.driver);
        match SoapyDevice::open(&d.args) {
            Ok(dev) => print_caps(dev.caps()),
            Err(e) => println!("  failed to open: {e}"),
        }
    }
    Ok(())
}

#[cfg(not(feature = "soapy"))]
fn probe_soapy(_cli: &Cli, _settings: &Settings) -> anyhow::Result<()> {
    println!("This build has no SoapySDR support (built with --no-default-features).");
    Ok(())
}

#[cfg(feature = "soapy")]
fn print_caps(caps: &sdroxide_types::DeviceCaps) {
    let fmt_mhz = |hz: f64| format!("{:.3} MHz", hz / 1e6);
    println!("  driver        : {}", caps.driver);
    println!("  label         : {}", caps.label);
    println!(
        "  channels      : {} RX, {} TX{}",
        caps.rx_channels,
        caps.tx_channels,
        if caps.tx_channels > 0 {
            if caps.full_duplex { " (full duplex)" } else { " (half duplex)" }
        } else {
            " (receive only)"
        }
    );
    // Both directions are always printed, including the empty case: a driver
    // that implements no frequency-range call is a thing an operator needs to
    // be told, not a line that quietly goes missing. This is the device's own
    // answer — any range stated in radio.json is applied when the radio is
    // opened for use, not here.
    for (name, ranges, chan) in [
        ("RX freq", &caps.freq_ranges_rx, caps.rx_channels),
        ("TX freq", &caps.freq_ranges_tx, caps.tx_channels),
    ] {
        if chan == 0 {
            continue;
        }
        if ranges.is_empty() {
            println!("  {name:<13} : not published by this driver (set one in Settings → Radio)");
        } else {
            let list: Vec<String> = ranges
                .iter()
                .map(|&(lo, hi)| format!("{} – {}", fmt_mhz(lo), fmt_mhz(hi)))
                .collect();
            println!("  {:<13} : {}", name, list.join(", "));
        }
    }
    if !caps.sample_rates.is_empty() {
        let list: Vec<String> =
            caps.sample_rates.iter().map(|r| format!("{:.3}", r / 1e6)).collect();
        println!("  rates (Msps)  : {}", list.join(", "));
    }
    for &(lo, hi) in &caps.rate_ranges {
        println!("  rate range    : {:.3} – {:.3} Msps", lo / 1e6, hi / 1e6);
    }
    for g in &caps.gains {
        println!(
            "  gain {:<8} : {:?} {} to {} dB (step {})",
            g.name, g.direction, g.min_db, g.max_db, g.step_db
        );
    }
    if !caps.antennas_rx.is_empty() {
        println!("  RX antennas   : {}", caps.antennas_rx.join(", "));
    }
    if !caps.antennas_tx.is_empty() {
        println!("  TX antennas   : {}", caps.antennas_tx.join(", "));
    }
    if !caps.sensors.is_empty() {
        println!("  sensors       : {}", caps.sensors.join(", "));
    }
}

fn open_source(cli: &Cli, settings: &Settings) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let rate = cli.rate.unwrap_or(settings.sample_rate);

    if cli.siggen {
        return Ok((
            Box::new(SigGenSource::demo(rate, cli.center_hz())),
            synthetic_caps("Signal generator"),
        ));
    }
    if let Some(path) = &cli.file {
        let label = format!("IQ file {}", path.display());
        return Ok((
            Box::new(
                FileSource::open(path, rate, cli.center_hz())
                    .with_context(|| format!("opening IQ file {}", path.display()))?,
            ),
            synthetic_caps(&label),
        ));
    }

    // Try the configured radio interface. If it can't be opened (no SoapySDR
    // device, HPSDR unreachable, CAT port missing, TCI server not up yet, …)
    // fall back to a null source so the GUI — and the Settings dialog — still
    // come up, instead of the program refusing to launch. The engine keeps
    // retrying this same interface in the background (`IqSource::needs_reopen`),
    // so a rig that simply wasn't ready attaches by itself; Settings → Radio is
    // only needed to choose a *different* one.
    let radio = sdroxide_config::load_radio_config();
    match open_converted_source(&radio, cli, settings) {
        Ok(pair) => Ok(pair),
        Err(e) => {
            tracing::warn!("radio interface unavailable: {e:#}");
            let msg =
                format!("{e}. Retrying — or open Settings → Radio to choose another interface.");
            Ok((
                Box::new(null_source::NullSource::new(cli.center_hz(), msg)),
                synthetic_caps("No radio"),
            ))
        }
    }
}

/// [`open_configured_source`], with the operator's own tuning ranges and any
/// external frequency converter folded in.
///
/// The distinction that makes this a separate function: the centre
/// `open_configured_source` passes to each back end is a *hardware* frequency,
/// while the one this is called with — from `--freq`, from the restored session,
/// or from the engine's current dial on a reopen — is the operator's. So the
/// offset goes on here, once, and [`ConvertedSource`] takes it off again for
/// everything the source reports back.
///
/// Stated ranges go on before the offset, for the same reason: they describe
/// the hardware, and `shift_caps` moves the hardware's ranges into the
/// operator's domain whether the hardware or its owner named them.
fn open_converted_source(
    radio: &RadioConfig,
    cli: &Cli,
    settings: &Settings,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let offset = radio.converter_offset_hz;
    if offset == 0.0 || !offset.is_finite() {
        let (source, caps) = open_configured_source(radio, cli, settings)?;
        return Ok((source, stated_ranges(caps, radio)));
    }
    let mut c = cli.clone();
    c.freq = Some(cli.center_hz() + offset);
    let (source, caps) = open_configured_source(radio, &c, settings)?;
    let caps = stated_ranges(caps, radio);
    tracing::info!(
        "frequency converter: hardware tuned {:.6} MHz above the dial; transmit withdrawn",
        offset / 1e6
    );
    Ok((Box::new(ConvertedSource::new(source, offset)), shift_caps(caps, offset)))
}

/// Apply the tuning ranges stated in `radio.json`, and say so in the log —
/// a range the operator set months ago is otherwise invisible when a band later
/// refuses to come up.
fn stated_ranges(caps: DeviceCaps, radio: &RadioConfig) -> DeviceCaps {
    let out = override_caps_ranges(caps, &radio.freq_ranges_rx, &radio.freq_ranges_tx);
    for (dir, stated, applied) in [
        ("RX", &radio.freq_ranges_rx, &out.freq_ranges_rx),
        ("TX", &radio.freq_ranges_tx, &out.freq_ranges_tx),
    ] {
        if !stated.is_empty() {
            tracing::info!(
                "{dir} tuning range set in the configuration: {} MHz",
                sdroxide_types::format_freq_ranges(applied)
            );
        }
    }
    out
}

/// Open the interface selected in `radio.json`. `Auto` prefers a SoapySDR device
/// and falls back to CAT when none is present (or the binary has no soapy).
fn open_configured_source(
    radio: &RadioConfig,
    cli: &Cli,
    settings: &Settings,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    // `--rate` reaches only the backends that have somewhere to put it. Saying
    // so is the point: it used to be accepted and silently dropped, which reads
    // as "the flag works and the radio ignored it". Once per run, not once per
    // reconnect — this is called again every time the engine reopens.
    if let Some(rate) = cli.rate {
        if !matches!(radio.backend, Backend::Soapy | Backend::Pluto | Backend::Auto) {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| {
                tracing::warn!(
                    "--rate {:.3} Msps does not apply to the {} interface — set its sample \
                     rate in radio.json instead",
                    rate / 1e6,
                    radio.backend.label()
                );
            });
        }
    }
    match radio.backend {
        // A freshly created radio tab: nothing is opened — and nothing may be,
        // or this tab would grab a device another radio is already running —
        // until the operator picks an interface.
        Backend::None => bail!("no radio interface selected — choose one in Settings → Radio"),
        Backend::Cat => open_cat_source(radio),
        Backend::Hpsdr => open_hpsdr_source(radio, cli.center_hz()),
        Backend::Tci => open_tci_source(radio, cli.center_hz()),
        Backend::SmartSdr => open_smartsdr_source(radio, cli.center_hz()),
        Backend::Pluto => open_pluto_source(radio, cli.center_hz(), cli.rate),
        Backend::RtlSdr => open_rtlsdr_source(radio, cli.center_hz()),
        Backend::Rx888 => open_rx888_source(radio, cli.center_hz()),
        Backend::SdrPlay => open_sdrplay_source(radio, cli.center_hz()),
        Backend::Soapy => open_soapy_source(cli, settings),
        Backend::Auto => {
            #[cfg(feature = "soapy")]
            {
                // "Is a SoapySDR radio present?" — a sound card must not answer
                // yes here either, or auto-detect lands on it instead of falling
                // through to the CAT rig.
                let filter = device_filter(cli, settings);
                if selectable_soapy_devices(&filter)
                    .map(|(real, _)| real.is_empty())
                    .unwrap_or(true)
                {
                    open_cat_source(radio)
                } else {
                    open_soapy_source(cli, settings)
                }
            }
            #[cfg(not(feature = "soapy"))]
            {
                open_cat_source(radio)
            }
        }
    }
}

/// Whether the operator's filter names a driver at all. A filter that says
/// `driver=audio` is an explicit request for the sound card and is obeyed; an
/// empty one, or one that only narrows by serial, is not.
#[cfg(feature = "soapy")]
fn filter_names_driver(filter: &str) -> bool {
    filter.to_ascii_lowercase().contains("driver=")
}

/// The devices an automatic pick may choose from, and the ones held back:
/// everything, minus the modules that are not radios, unless the filter asked
/// for one by name.
///
/// Without this the sound card wins on any bundle install — SoapyAudio
/// enumerates ahead of the real hardware, opens happily, and produces a
/// plausible-looking spectrum of the machine's line input on whatever
/// frequency the dial claims. See [`SoapyDeviceInfo::driver_is_pseudo`].
///
/// The held-back list comes back rather than being dropped so the caller can
/// tell "nothing is plugged in" from "the only thing here is a sound card",
/// which need entirely different advice.
#[cfg(feature = "soapy")]
fn selectable_soapy_devices(filter: &str) -> anyhow::Result<(Vec<DeviceInfo>, Vec<DeviceInfo>)> {
    use sdroxide_types::SoapyDeviceInfo;
    let all = enumerate_devices(filter).context("SoapySDR enumeration failed")?;
    if filter_names_driver(filter) {
        return Ok((all, Vec::new()));
    }
    let (real, pseudo): (Vec<_>, Vec<_>) =
        all.into_iter().partition(|d| !SoapyDeviceInfo::driver_is_pseudo(&d.driver));
    for d in &pseudo {
        tracing::info!(
            driver = %d.driver,
            label = %d.label,
            "skipping a SoapySDR module that is not a radio — name it with \
             --device driver={} to open it anyway",
            d.driver.to_ascii_lowercase(),
        );
    }
    Ok((real, pseudo))
}

/// Open the first available SoapySDR device (feature-gated).
#[cfg(feature = "soapy")]
fn open_soapy_source(
    cli: &Cli,
    settings: &Settings,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let rate = cli.rate.unwrap_or(settings.sample_rate);
    let filter = device_filter(cli, settings);
    let (devices, skipped) = selectable_soapy_devices(&filter)?;
    let Some(info) = devices.first() else {
        // Two different situations, two different fixes: nothing plugged in at
        // all, versus a machine whose only "SDR" is its sound card.
        if skipped.is_empty() {
            bail!("no SoapySDR devices found (filter: {:?})", filter);
        }
        bail!(
            "no SoapySDR radios found (filter: {:?}) — the only devices SoapySDR reports \
             are sound cards ({}), which are not radios and are never opened automatically. \
             Pick a native interface in Settings → Radio, or ask for the sound card by name \
             with --device driver=audio.",
            filter,
            skipped.iter().map(|d| d.label.as_str()).collect::<Vec<_>>().join(", "),
        );
    };
    // Which one, not just that there was one: on a bundle install this is the
    // only line that says whether the radio being opened is the radio meant.
    tracing::info!(driver = %info.driver, label = %info.label, "SoapySDR device selected");
    let dev =
        SoapyDevice::open(&info.args).with_context(|| format!("opening device {}", info.label))?;
    let caps = dev.caps().clone();
    Ok((Box::new(dev.rx_source(rate, cli.center_hz(), cli.gain)?), caps))
}

#[cfg(not(feature = "soapy"))]
fn open_soapy_source(
    _cli: &Cli,
    _settings: &Settings,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    bail!("SoapySDR support is not compiled into this build")
}

/// Build the CAT + sound-card source and its capabilities from radio.json.
fn open_cat_source(radio: &RadioConfig) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let src = audio_cat_source::AudioCatSource::open(
        radio.cat.clone(),
        radio.radio_audio_in.as_deref(),
        radio.radio_audio_out.as_deref(),
    )
    .context("opening CAT rig")?;
    let caps = cat_caps(radio);
    Ok((Box::new(src), caps))
}

/// Build the HPSDR (ethernet SDR) source from radio.json. The target IP is the
/// manual override, else the persisted selection, else the first device found by
/// a discovery scan; the protocol is detected when the connection opens.
fn open_hpsdr_source(
    radio: &RadioConfig,
    center_hz: f64,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let ip: std::net::Ipv4Addr = if let Some(s) = radio.hpsdr.target_ip() {
        s.trim().parse().with_context(|| format!("invalid HPSDR IP address {s:?}"))?
    } else {
        let found = sdroxide_hpsdr::discover_default();
        let dev = found.iter().find(|d| d.supported()).ok_or_else(|| {
            anyhow::anyhow!("no HPSDR device found on the network — enter a target IP in Settings")
        })?;
        dev.ip.parse().with_context(|| format!("discovered HPSDR IP {:?}", dev.ip))?
    };

    let src = hpsdr_source::HpsdrSource::open(ip, &radio.hpsdr, center_hz)
        .context("opening HPSDR device")?;
    let caps = hpsdr_caps(
        src.board(),
        src.sample_rate_hz(),
        src.protocol(),
        src.has_lna_gain(),
        radio.hpsdr.ddc,
    );
    Ok((Box::new(src), caps))
}

/// Capabilities for an HPSDR board: wideband IQ (not `audio_mode`), TX-capable,
/// half-duplex. The board enforces its own limits. Protocol 1 boards top out at
/// 384 kHz, and a Hermes-Lite 2 samples at 76.8 MHz, so its Nyquist limit is
/// 38.4 MHz — tuning past that on one only aliases. A secondary DDC (`ddc >
/// 0`) has no transmitter: the board has one DUC, and it belongs to the DDC-0
/// radio.
fn hpsdr_caps(board: &str, sample_rate: f64, protocol: u8, has_lna: bool, ddc: u8) -> DeviceCaps {
    let hermes_lite = sdroxide_hpsdr::board_has_lna_gain(board);
    let nyquist = if hermes_lite { 38_400_000.0 } else { 61_440_000.0 };
    let gains = if has_lna {
        vec![sdroxide_types::GainElement {
            name: sdroxide_hpsdr::LNA_GAIN_ELEMENT.into(),
            direction: sdroxide_types::Direction::Rx,
            min_db: sdroxide_hpsdr::LNA_GAIN_MIN_DB,
            max_db: sdroxide_hpsdr::LNA_GAIN_MAX_DB,
            step_db: 1.0,
        }]
    } else {
        Vec::new()
    };
    let (label, tx_channels) = if ddc == 0 {
        (format!("{board} (HPSDR P{protocol}, {:.3} Msps)", sample_rate / 1e6), 1)
    } else {
        (format!("{board} DDC{} (HPSDR P{protocol}, {:.3} Msps)", ddc + 1, sample_rate / 1e6), 0)
    };
    DeviceCaps {
        driver: "hpsdr".into(),
        label,
        rx_channels: 1,
        tx_channels,
        audio_mode: false,
        freq_ranges_rx: vec![(0.0, nyquist)],
        freq_ranges_tx: if tx_channels > 0 {
            vec![(1_800_000.0, if hermes_lite { 30_000_000.0 } else { 54_000_000.0 })]
        } else {
            Vec::new()
        },
        sample_rates: sdroxide_types::HpsdrConfig::rates_for(protocol).to_vec(),
        gains,
        ..DeviceCaps::default()
    }
}

/// Build the RTL-SDR source from radio.json. The dongle is picked by USB
/// serial, or the first one found when none is configured.
fn open_rtlsdr_source(
    radio: &RadioConfig,
    center_hz: f64,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let src = rtlsdr_source::RtlSdrSource::open(&radio.rtlsdr, center_hz)
        .context("opening RTL-SDR dongle")?;
    let caps = rtlsdr_caps(&src);
    Ok((Box::new(src), caps))
}

/// Build the RX-888 source from radio.json. Uploads the FX3 firmware first if
/// the receiver is still sitting in its boot ROM, which it will be on every
/// fresh plug-in.
fn open_rx888_source(
    radio: &RadioConfig,
    center_hz: f64,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let src = rx888_source::Rx888Source::open(&radio.rx888, center_hz)
        .context("opening RX-888 receiver")?;
    let caps = rx888_caps(&src);
    Ok((Box::new(src), caps))
}

/// Capabilities for an RX-888: wideband IQ, receive only, HF.
///
/// Two things differ from every other backend here. The sample rate advertised
/// is the *downconverter's* output, not the ADC clock — the hardware has no DDC,
/// so the conversion from 64.8 Msps of real samples to complex baseband happens
/// on the host. And the frequency range stops at the ADC's Nyquist limit rather
/// than at some tuner's ceiling, because direct sampling is all this receiver
/// does: there is no VHF path in this driver.
fn rx888_caps(src: &rx888_source::Rx888Source) -> DeviceCaps {
    use sdroxide_types::{Direction, GainElement, Rx888Config};
    let rate = src.sample_rate_hz();
    let nyquist = src.adc_rate_hz() / 2.0;
    DeviceCaps {
        driver: "rx888".into(),
        label: src.describe(),
        rx_channels: 1,
        tx_channels: 0,
        audio_mode: false,
        freq_ranges_rx: vec![(0.0, nyquist)],
        sample_rates: vec![rate],
        gains: vec![
            GainElement {
                name: Rx888Config::VGA_ELEMENT.into(),
                direction: Direction::Rx,
                min_db: -6.0,
                max_db: 34.0,
                // The AD8370's vernier is linear in voltage, so the dB step
                // varies; a request is snapped to the nearest code and reported
                // back, which makes a fine slider honest enough.
                step_db: 0.5,
            },
            GainElement {
                name: Rx888Config::ATT_ELEMENT.into(),
                direction: Direction::Rx,
                min_db: -31.5,
                max_db: 0.0,
                step_db: 0.5,
            },
        ],
        ..DeviceCaps::default()
    }
}

/// Build the SDRplay RSP source from radio.json. The device is picked by the
/// API's serial, or the first one found when none is configured.
fn open_sdrplay_source(
    radio: &RadioConfig,
    center_hz: f64,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let src = sdrplay_source::SdrPlaySource::open(&radio.sdrplay, center_hz)
        .context("opening SDRplay RSP")?;
    let caps = sdrplay_caps(&src);
    Ok((Box::new(src), caps))
}

/// Capabilities for an SDRplay RSP: wideband IQ, receive only, 1 kHz–2 GHz on
/// every model.
///
/// The two real gain elements are both *negated reductions* — `IF` is −(IF
/// gain reduction), `LNA` is −(LNA state) — so the sliders read the usual way
/// round: right is louder. The LNA range is the model's best band; bands with
/// fewer states get clamped by the driver, which reports the state it kept.
/// The switches (AGC, notches, bias tee, HDR) ride pseudo-elements that are
/// deliberately not listed here, so only the SDRplay settings panel renders
/// them.
///
/// The LNA is listed first — the first element is the main window's Gain
/// slider, and the LNA is the only gain the operator always owns: with the
/// hardware AGC on (the default) the service holds the IF gain, and a slider
/// the AGC snaps back is worse than none. It is also the control that
/// actually clears an overloaded front end, which the IF gain never can.
fn sdrplay_caps(src: &sdrplay_source::SdrPlaySource) -> DeviceCaps {
    use sdroxide_types::{Direction, GainElement, SdrPlayConfig};
    let model = src.model();
    DeviceCaps {
        driver: "sdrplay".into(),
        label: src.describe(),
        rx_channels: 1,
        tx_channels: 0,
        audio_mode: false,
        freq_ranges_rx: vec![(1_000.0, 2_000_000_000.0)],
        sample_rates: SdrPlayConfig::SAMPLE_RATES.to_vec(),
        gains: vec![
            GainElement {
                name: SdrPlayConfig::LNA_ELEMENT.into(),
                direction: Direction::Rx,
                min_db: -(model.max_lna_state() as f64),
                max_db: 0.0,
                step_db: 1.0,
            },
            GainElement {
                name: SdrPlayConfig::IF_GAIN_ELEMENT.into(),
                direction: Direction::Rx,
                min_db: -(SdrPlayConfig::IF_GR_MAX as f64),
                max_db: -(SdrPlayConfig::IF_GR_MIN as f64),
                step_db: 1.0,
            },
        ],
        antennas_rx: src.antennas().to_vec(),
        ..DeviceCaps::default()
    }
}

/// Build the PlutoSDR source from radio.json. The address is the operator's
/// typed one, else a persisted discovery selection, else the USB gadget's
/// default — see [`sdroxide_types::PlutoConfig::target`].
///
/// `rate_override` is `--rate`, which for this backend takes precedence over
/// the configured rate: on a headless install the command line is the part of
/// the configuration that lives in the unit file, and a flag that is accepted
/// and then quietly ignored is worse than one that does not exist.
fn open_pluto_source(
    radio: &RadioConfig,
    center_hz: f64,
    rate_override: Option<f64>,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let mut cfg = radio.pluto.clone();
    if let Some(rate) = rate_override.filter(|r| r.is_finite() && *r > 0.0) {
        if (rate - cfg.sample_rate_hz).abs() > 1.0 {
            tracing::info!(
                "PlutoSDR: --rate {:.3} Msps overrides the {:.3} Msps in radio.json",
                rate / 1e6,
                cfg.sample_rate_hz / 1e6
            );
        }
        cfg.sample_rate_hz = rate;
    }
    let address = cfg.target();
    let src = pluto_source::PlutoSource::open(&address, &cfg, center_hz)
        .with_context(|| format!("opening PlutoSDR at {address}"))?;
    let caps = pluto_caps(&src, cfg.rx);
    Ok((Box::new(src), caps))
}

/// Capabilities for a PlutoSDR: wideband IQ, transmit-capable, half duplex.
///
/// Everything here except the duplex flag is read off the device rather than
/// written down, because the two boards this backend serves genuinely differ: a
/// stock AD9363 covers 325 MHz–3.8 GHz and one unlocked to AD9364 covers
/// 70 MHz–6 GHz, and the receive gain range moves with frequency. Quoting
/// either set of numbers as a constant would leave half the Plutos in
/// circulation refusing frequencies they can reach.
///
/// The AD9361 *is* a full-duplex part, and `full_duplex` is still left false: a
/// Pluto is normally reached over a USB 2.0 Ethernet gadget, which will not
/// carry a megasample-per-second stream both ways at once. Receive is torn down
/// for the length of an over so the whole link is available to transmit.
fn pluto_caps(src: &pluto_source::PlutoSource, rx: u8) -> DeviceCaps {
    use sdroxide_types::{Direction, GainElement, PlutoConfig};
    let Some(limits) = src.limits() else {
        return DeviceCaps { driver: "pluto".into(), label: src.describe(), ..Default::default() };
    };
    // The transmitter belongs to the chain-0 radio — the device has one DUC
    // path wired here.
    let tx_capable = rx == 0;
    let mut gains = vec![GainElement {
        name: PlutoConfig::RF_GAIN_ELEMENT.into(),
        direction: Direction::Rx,
        min_db: limits.rx_gain_db.0,
        max_db: limits.rx_gain_db.1,
        step_db: limits.rx_gain_db.2,
    }];
    if tx_capable {
        // Transmit "gain" is the AD9361's attenuator, so this range is
        // negative: 0 dB is full output.
        gains.push(GainElement {
            name: PlutoConfig::TX_GAIN_ELEMENT.into(),
            direction: Direction::Tx,
            min_db: limits.tx_gain_db.0,
            max_db: limits.tx_gain_db.1,
            step_db: limits.tx_gain_db.2,
        });
    }
    DeviceCaps {
        driver: "pluto".into(),
        label: src.describe(),
        rx_channels: 1,
        tx_channels: if tx_capable { 1 } else { 0 },
        full_duplex: false,
        audio_mode: false,
        freq_ranges_rx: vec![limits.rx_lo_hz],
        freq_ranges_tx: if tx_capable { vec![limits.tx_lo_hz] } else { Vec::new() },
        sample_rates: PlutoConfig::SAMPLE_RATES.to_vec(),
        rate_ranges: vec![limits.sample_rate_hz],
        gains,
        antennas_rx: limits.rx_ports.clone(),
        antennas_tx: if tx_capable { limits.tx_ports.clone() } else { Vec::new() },
        // On a 2R2T firmware the chains share the one synthesiser, so a
        // sibling radio's retune moves this radio's centre too (and vice
        // versa). Stated only when a sibling is possible.
        shared_lo_rx: src.rx_chains() > 1,
        ..DeviceCaps::default()
    }
}

/// Capabilities for an RTL-SDR: wideband IQ, receive only.
///
/// The frequency ranges are the interesting part. A Blog V4 upconverts HF in
/// hardware, so it is continuous from DC. Anything else reaches HF only through
/// direct sampling, which tops out at the ADC's Nyquist limit — leaving a gap
/// between there and the tuner's 24 MHz floor. Overlapping or disjoint ranges
/// are both fine: `DeviceCaps::can_rx_hz` is an `any` over the list.
fn rtlsdr_caps(src: &rtlsdr_source::RtlSdrSource) -> DeviceCaps {
    let rate = src.sample_rate_hz();
    let freq_ranges_rx = if src.is_blog_v4() {
        vec![(0.0, 1_766_000_000.0)]
    } else if src.hf_capable() {
        vec![(0.0, 14_400_000.0), (24_000_000.0, 1_766_000_000.0)]
    } else {
        vec![(24_000_000.0, 1_766_000_000.0)]
    };
    DeviceCaps {
        driver: "rtlsdr".into(),
        label: format!("{} ({}, {:.3} Msps)", src.describe(), src.tuner(), rate / 1e6),
        rx_channels: 1,
        tx_channels: 0,
        audio_mode: false,
        freq_ranges_rx,
        sample_rates: sdroxide_types::RtlSdrConfig::SAMPLE_RATES.to_vec(),
        gains: vec![sdroxide_types::GainElement {
            name: sdroxide_types::RtlSdrConfig::TUNER_GAIN_ELEMENT.into(),
            direction: sdroxide_types::Direction::Rx,
            min_db: 0.0,
            max_db: sdroxide_types::RtlSdrConfig::GAIN_MAX_DB,
            // The hardware only has 29 discrete steps; a request is snapped to
            // the nearest and reported back, so a fine slider is honest enough.
            step_db: 0.1,
        }],
        ..DeviceCaps::default()
    }
}

/// Build the TCI (WebSocket) source from radio.json: wideband IQ receive +
/// audio transmit.
fn open_tci_source(
    radio: &RadioConfig,
    center_hz: f64,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let src = tci_source::TciSource::open(
        &radio.tci.address,
        radio.tci.iq_sample_rate_hz,
        center_hz,
        radio.tci.rx,
    )
    .context("connecting to TCI server")?;
    let caps = tci_caps(&radio.tci.address, src.sample_rate_hz(), radio.tci.rx);
    Ok((Box::new(src), caps))
}

/// Capabilities for a TCI rig: wideband IQ RX (not `audio_mode`), TX via raw
/// audio (`tx_audio`) which the rig modulates. The rig enforces its own limits.
/// A secondary receiver (`rx > 0`) has no transmitter: TCI rigs have one, and
/// it belongs to the receiver-0 radio.
fn tci_caps(address: &str, iq_rate: f64, rx: u32) -> DeviceCaps {
    let (label, tx_channels, tx_audio) = if rx == 0 {
        (format!("TCI {address} ({:.0} kHz IQ)", iq_rate / 1000.0), 1, true)
    } else {
        (format!("TCI RX{} {address} ({:.0} kHz IQ)", rx + 1, iq_rate / 1000.0), 0, false)
    };
    DeviceCaps {
        driver: "tci".into(),
        label,
        rx_channels: 1,
        tx_channels,
        audio_mode: false,
        tx_audio,
        freq_ranges_rx: vec![(0.0, 160_000_000.0)],
        freq_ranges_tx: if tx_channels > 0 {
            vec![(1_800_000.0, 54_000_000.0)]
        } else {
            Vec::new()
        },
        sample_rates: sdroxide_types::TciConfig::IQ_RATES.to_vec(),
        // No RX gains: the SunSDR2DX ATT/Preamp is not reachable over TCI
        // (verified against ExpertSDR3 — no command spelling drives it, and
        // toggling it in the GUI emits nothing on the wire). TCI gain control
        // is deferred until a controllable path is found.
        ..DeviceCaps::default()
    }
}

/// Build the SmartSDR (FlexRadio) source from radio.json: DAX IQ receive +
/// DAX audio transmit over the LAN.
fn open_smartsdr_source(
    radio: &RadioConfig,
    center_hz: f64,
) -> anyhow::Result<(Box<dyn IqSource>, DeviceCaps)> {
    let src = smartsdr_source::SmartSdrSource::open(&radio.smartsdr, center_hz)
        .context("connecting to the FlexRadio")?;
    let caps = smartsdr_caps(src.model(), src.describe());
    Ok((Box::new(src), caps))
}

/// Capabilities for a FlexRadio: wideband IQ RX over DAX (not `audio_mode`), TX
/// via DAX audio (`tx_audio`) which the radio modulates.
///
/// The frequency ranges are the radio's own published coverage rather than
/// anything this backend imposes: a FLEX receives from 30 kHz to 54 MHz (the
/// 8000 series and the 6600/6700 add 2 m), and the radio declines anything it
/// cannot do. The sample rates are the four DAX IQ stream rates — 192 kHz is
/// the ceiling, which makes it this backend's widest span.
fn smartsdr_caps(model: &str, label: String) -> DeviceCaps {
    // The 6400/6600 and the whole 8000 family have a 2 m receiver; the rest stop
    // at 6 m. Getting this wrong only costs a refused tune, so infer it.
    let vhf = matches!(model, "FLEX-6600" | "FLEX-6600M" | "FLEX-6700" | "FLEX-6700R")
        || model.starts_with("FLEX-8");
    let rx_top = if vhf { 165_000_000.0 } else { 54_000_000.0 };
    DeviceCaps {
        driver: "smartsdr".into(),
        label,
        rx_channels: 1,
        tx_channels: 1,
        audio_mode: false,
        tx_audio: true,
        freq_ranges_rx: vec![(30_000.0, rx_top)],
        freq_ranges_tx: vec![(1_800_000.0, 54_000_000.0)],
        sample_rates: sdroxide_types::SmartSdrConfig::IQ_RATES.to_vec(),
        // No RX gain elements: a FLEX has no user-settable front-end gain in the
        // sense this list means. Its `display pan rfgain` is a per-panadapter
        // preamp/attenuator whose steps differ by model and are only discoverable
        // by asking the radio (`display pan rfgain_info`), so it is left out
        // until that query can be verified against hardware.
        ..DeviceCaps::default()
    }
}

/// Capabilities for a CAT rig. TX-capable unless PTT is VOX-only-with-no-audio;
/// we advertise TX so the UI shows PTT and the safety rails apply. Frequency
/// range covers HF+6m (the rig enforces its own limits over CAT).
///
/// The RX floor is deliberately below any rig's: the range is what the engine
/// refuses to tune past, and a general-coverage receiver that reaches the LF
/// time signals must not be held back by a figure invented here. A rig asked
/// for something it cannot do simply declines over CAT, which costs nothing.
fn cat_caps(radio: &RadioConfig) -> DeviceCaps {
    let demod = matches!(radio.cat.format, sdroxide_types::SoundFormat::DemodAudio);
    DeviceCaps {
        driver: "cat".into(),
        label: format!("{} (CAT)", radio.cat.family.label()),
        rx_channels: 1,
        tx_channels: 1,
        audio_mode: demod,
        freq_ranges_rx: vec![(10_000.0, 148_000_000.0)],
        freq_ranges_tx: vec![(1_800_000.0, 54_000_000.0)],
        ..DeviceCaps::default()
    }
}

/// Capabilities for non-hardware sources (RX-only, unlimited tuning).
fn synthetic_caps(label: &str) -> DeviceCaps {
    DeviceCaps {
        driver: "none".into(),
        label: label.into(),
        rx_channels: 1,
        tx_channels: 0,
        freq_ranges_rx: vec![(0.0, 6e9)],
        ..DeviceCaps::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(oob: bool) -> Cli {
        let mut c = Cli::parse_from(["sdroxide"]);
        c.oob_tx = oob;
        c
    }

    /// `--oob-tx` may only ever *loosen* the lockout. A build that never passes
    /// it has to behave exactly as it did before the flag existed, and no
    /// combination of flags may turn the lockout on when the config has turned
    /// it off — that would be a surprise in the dangerous direction.
    #[test]
    fn the_flag_only_ever_loosens_the_band_lockout() {
        let locked = Settings { tx_ham_only: true, ..Settings::default() };
        let open = Settings { tx_ham_only: false, ..Settings::default() };

        assert!(cli(false).tx_ham_only(&locked), "the default must keep the lockout");
        assert!(!cli(true).tx_ham_only(&locked), "--oob-tx must lift it");
        // Already unlocked in the config: the flag changes nothing either way.
        assert!(!cli(false).tx_ham_only(&open));
        assert!(!cli(true).tx_ham_only(&open));
    }

    /// The flag is opt-in on the command line and nowhere else.
    #[test]
    fn the_flag_is_off_unless_asked_for() {
        assert!(!Cli::parse_from(["sdroxide"]).oob_tx);
        assert!(Cli::parse_from(["sdroxide", "--oob-tx"]).oob_tx);
        assert!(Settings::default().tx_ham_only, "the shipped default is locked");
    }

    fn session(freq_hz: f64, mode: sdroxide_types::Mode) -> sdroxide_config::Session {
        sdroxide_config::Session { freq_hz, mode, ..Default::default() }
    }

    /// Starting with no arguments is the case this exists for: come back up on
    /// the frequency and mode the radio was left on, not on a fixed default.
    #[test]
    fn a_bare_start_comes_up_where_the_last_session_ended() {
        let mut c = Cli::parse_from(["sdroxide"]);
        let mode = c.apply_session(session(7_074_000.0, sdroxide_types::Mode::Ft8));
        assert_eq!(c.center_hz(), 7_074_000.0);
        assert_eq!(mode, Some(sdroxide_types::Mode::Ft8));
        assert_eq!(c.mode, mode, "the engine and the source agree on the mode");
    }

    /// The command line is an instruction, not a suggestion: what it names must
    /// survive the restore, and only what it left out may be filled in.
    #[test]
    fn the_command_line_outranks_the_remembered_session() {
        let mut both = Cli::parse_from(["sdroxide", "--freq", "50150000", "--mode", "cw"]);
        both.apply_session(session(7_074_000.0, sdroxide_types::Mode::Ft8));
        assert_eq!(both.center_hz(), 50_150_000.0);
        assert_eq!(both.mode, Some(sdroxide_types::Mode::Cw));

        // Each half is independent — naming one must not discard the other.
        let mut freq_only = Cli::parse_from(["sdroxide", "--freq", "50150000"]);
        freq_only.apply_session(session(7_074_000.0, sdroxide_types::Mode::Ft8));
        assert_eq!(freq_only.center_hz(), 50_150_000.0);
        assert_eq!(freq_only.mode, Some(sdroxide_types::Mode::Ft8), "the mode is still restored");

        let mut mode_only = Cli::parse_from(["sdroxide", "--mode", "cw"]);
        mode_only.apply_session(session(7_074_000.0, sdroxide_types::Mode::Ft8));
        assert_eq!(mode_only.center_hz(), 7_074_000.0, "the frequency is still restored");
        assert_eq!(mode_only.mode, Some(sdroxide_types::Mode::Cw));
    }

    /// A run that never restores anything — the headless smoke tests — still
    /// has to open a front end somewhere, on the frequency this program has
    /// always defaulted to.
    #[test]
    fn a_run_that_skips_the_restore_keeps_the_old_default() {
        assert_eq!(Cli::parse_from(["sdroxide"]).center_hz(), 14_200_000.0);
        assert_eq!(Cli::parse_from(["sdroxide", "--freq", "50150000"]).center_hz(), 50_150_000.0);
    }
}
