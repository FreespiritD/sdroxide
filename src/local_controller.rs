//! In-process [`RadioController`]: wraps the engine's channel endpoints, and
//! owns the cpal stream handles so audio devices can be swapped at runtime
//! (the engine only ever holds the ring endpoints).

use sdroxide_radio::crossbeam_channel::{Receiver, Sender};
use sdroxide_radio::{AudioParams, EngineHandles, EngineSwap, MicParams, triple_buffer};
use sdroxide_types::{
    AudioDevices, Command, RadioConfig, RadioController, RadioEvent, SpectrumFrame,
};
use tracing::warn;

pub struct LocalController {
    cmd_tx: Sender<Command>,
    event_rx: Receiver<RadioEvent>,
    spectrum: triple_buffer::Output<SpectrumFrame>,
    wide_spectrum: triple_buffer::Output<SpectrumFrame>,
    swap_tx: Sender<EngineSwap>,
    /// The engine thread, joined in [`RadioController::shutdown`] so device
    /// teardown (SoapySDR/libusb) can't race the C libraries' exit handlers.
    /// `None` after shutdown, or when the frontend chose to join it itself.
    thread: Option<std::thread::JoinHandle<()>>,
    /// Live cpal streams (they must outlive their ring endpoints in the engine).
    audio_out: Option<sdroxide_audio::AudioOutput>,
    mic_in: Option<sdroxide_audio::AudioInput>,
    /// Currently selected device names; `None` = system default.
    out_name: Option<String>,
    in_name: Option<String>,
    /// Where this radio's `radio.json` lives — radio 0 is the legacy root.
    store: sdroxide_config::Store,
}

impl LocalController {
    pub fn new(
        mut handles: EngineHandles,
        audio_out: Option<sdroxide_audio::AudioOutput>,
        mic_in: Option<sdroxide_audio::AudioInput>,
        out_name: Option<String>,
        in_name: Option<String>,
        store: sdroxide_config::Store,
    ) -> Self {
        LocalController {
            cmd_tx: handles.cmd_tx,
            event_rx: handles.event_rx,
            spectrum: handles.spectrum_out,
            wide_spectrum: handles.wide_spectrum_out,
            swap_tx: handles.swap_tx,
            thread: handles.thread.take(),
            audio_out,
            mic_in,
            out_name,
            in_name,
            store,
        }
    }

    fn persist_selection(&self) {
        let mut s = sdroxide_config::Settings::load();
        s.audio_output = self.out_name.clone();
        s.audio_input = self.in_name.clone();
        if let Err(e) = s.save() {
            warn!("saving audio device selection: {e}");
        }
    }
}

impl RadioController for LocalController {
    fn send(&mut self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
    }

    fn poll_event(&mut self) -> Option<RadioEvent> {
        if let Ok(ev) = self.event_rx.try_recv() {
            return Some(ev);
        }
        if self.spectrum.update() {
            let f = self.spectrum.output_buffer();
            if !f.bins.is_empty() {
                return Some(RadioEvent::Spectrum(f.clone()));
            }
        }
        if self.wide_spectrum.update() {
            let f = self.wide_spectrum.output_buffer();
            if !f.bins.is_empty() {
                return Some(RadioEvent::WideSpectrum(f.clone()));
            }
        }
        None
    }

    fn wants_repaint_soon(&self) -> bool {
        !self.event_rx.is_empty() || self.spectrum.updated() || self.wide_spectrum.updated()
    }

    fn audio_devices(&self) -> Option<AudioDevices> {
        Some(AudioDevices {
            outputs: sdroxide_audio::output_device_names(),
            inputs: sdroxide_audio::input_device_names(),
            selected_output: self.out_name.clone(),
            selected_input: self.in_name.clone(),
        })
    }

    fn set_audio_device(&mut self, output: bool, name: Option<String>) {
        if output {
            // Drop the old stream first so an exclusive device is released.
            self.audio_out = None;
            match sdroxide_audio::start_output(name.as_deref(), 48_000) {
                Ok((out, producer)) => {
                    let out_rate = out.sample_rate;
                    self.audio_out = Some(out);
                    let _ = self
                        .swap_tx
                        .send(EngineSwap::Output(Some(AudioParams { producer, out_rate })));
                }
                Err(e) => {
                    warn!("audio output {name:?}: {e}; running silent");
                    let _ = self.swap_tx.send(EngineSwap::Output(None));
                }
            }
            self.out_name = name;
        } else {
            self.mic_in = None;
            match sdroxide_audio::start_input(name.as_deref(), 48_000) {
                Ok((input, consumer)) => {
                    let rate = input.sample_rate;
                    self.mic_in = Some(input);
                    let _ =
                        self.swap_tx.send(EngineSwap::Input(Some(MicParams { consumer, rate })));
                }
                Err(e) => {
                    warn!("audio input {name:?}: {e}; TX carries silence");
                    let _ = self.swap_tx.send(EngineSwap::Input(None));
                }
            }
            self.in_name = name;
        }
        self.persist_selection();
    }

    fn soapy_supported(&self) -> bool {
        cfg!(feature = "soapy")
    }

    fn serial_ports(&self) -> Vec<String> {
        sdroxide_cat::available_ports()
    }

    fn discover_hpsdr(&self) -> Vec<sdroxide_types::HpsdrDevice> {
        sdroxide_hpsdr::discover_default()
    }

    fn list_rx888(&self) -> Vec<sdroxide_types::Rx888Device> {
        sdroxide_rx888::list()
    }

    fn list_rtlsdr(&self) -> Vec<sdroxide_types::RtlSdrDevice> {
        sdroxide_rtlsdr::list()
    }

    fn list_sdrplay(&self) -> Vec<sdroxide_types::SdrPlayDevice> {
        sdroxide_sdrplay::list()
    }

    #[cfg(feature = "soapy")]
    fn list_soapy(&self) -> Vec<sdroxide_types::SoapyDeviceInfo> {
        // The whole enumeration, pseudo-drivers included: this feeds a list the
        // operator reads, and a sound card that is being skipped is exactly what
        // they need to see named. The *automatic* pick filters it (see
        // `selectable_soapy_devices` in main.rs).
        sdroxide_radio::enumerate_devices("")
            .unwrap_or_else(|e| {
                warn!("SoapySDR enumeration failed: {e}");
                Vec::new()
            })
            .into_iter()
            .map(|d| sdroxide_types::SoapyDeviceInfo {
                driver: d.driver,
                label: d.label,
                args: d.args,
            })
            .collect()
    }

    fn test_tci(&self, address: &str) -> Result<String, String> {
        sdroxide_tci::test_connection(address, std::time::Duration::from_secs(3))
    }

    fn discover_smartsdr(&self) -> Vec<sdroxide_types::SmartSdrDevice> {
        crate::smartsdr_source::discover()
    }

    fn test_smartsdr(&self, address: &str) -> Result<String, String> {
        sdroxide_smartsdr::test_connection(address, std::time::Duration::from_secs(3))
    }

    fn smartsdr_diagnostics(&self) -> Option<String> {
        Some(crate::smartsdr_source::diagnostics_or_hint())
    }

    fn discover_pluto(&self) -> Vec<sdroxide_types::PlutoDevice> {
        sdroxide_pluto::discover_default()
    }

    fn test_pluto(&self, address: &str) -> Result<String, String> {
        sdroxide_pluto::test_connection(address, std::time::Duration::from_secs(3))
    }

    fn pluto_diagnostics(&self) -> Option<String> {
        Some(match sdroxide_pluto::diagnostics() {
            Some(t) => t,
            None => "No PlutoSDR session has run yet — press Test connection or \
                     Apply / reconnect first."
                .to_string(),
        })
    }

    fn radio_config(&self) -> Option<RadioConfig> {
        Some(self.store.load_radio_config())
    }

    fn set_radio_config(&mut self, cfg: RadioConfig) {
        // Persist now; `reopen_source` applies it to the running engine.
        if let Err(e) = self.store.save_radio_config(&cfg) {
            warn!("saving radio config: {e}");
        }
    }

    fn reopen_source(&mut self) {
        // The engine rebuilds the source from the freshly persisted radio config.
        let _ = self.swap_tx.send(EngineSwap::ReopenSource);
    }

    fn set_muted(&mut self, muted: bool) {
        let _ = self.swap_tx.send(EngineSwap::MuteOutput(muted));
    }

    fn nudge_shared_stores(&mut self) {
        let _ = self.swap_tx.send(EngineSwap::ReloadSharedStores);
    }

    fn shutdown(&mut self) {
        // The engine runs until its last command sender drops, so disconnect
        // it by swapping the senders for endpoints wired to nothing. The audio
        // streams go too — an exclusive device has to be free before another
        // radio (or another program) can have it.
        let (dead_cmd, _) = sdroxide_radio::crossbeam_channel::unbounded();
        self.cmd_tx = dead_cmd;
        let (dead_swap, _) = sdroxide_radio::crossbeam_channel::unbounded();
        self.swap_tx = dead_swap;
        self.audio_out = None;
        self.mic_in = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
