//! An [`IqSource`] for an RX-888 Mk2 driven over USB by the native driver in
//! `sdroxide-rx888` — no SoapySDR, no libusb, and no vendor driver package.
//!
//! Receive only: the trait's transmit methods already default to errors, which
//! is the correct answer for a receiver.
//!
//! # What this source is doing that the others are not
//!
//! The hardware has no downconverter. It sends 16-bit *real* samples at the full
//! ADC rate — 64.8 Msps covering 0–32.4 MHz — and an FFT downconverter in the
//! backend turns that into ~2 Msps of complex baseband. Two consequences leak
//! out here:
//!
//! * [`IqSource::sample_rate`] is the *downconverter's* output rate, not the
//!   ADC clock. The engine's own DDC chain then works from that as usual.
//! * Retuning is free. There is no LO to move and no I2C to wait for — a retune
//!   is a change of FFT bin — so dragging the panadapter across the whole of HF
//!   costs nothing.

use std::time::Duration;

use sdroxide_radio::{Complex32, IqSource, Result};
use sdroxide_rx888::{Rx888Handle, Settings};
use sdroxide_types::Rx888Config;

/// How long the receiver may deliver nothing before the link counts as dead.
/// Matches the RTL-SDR backend: this is a local USB device, so there is no
/// network to be briefly slow.
const SILENCE_BEFORE_REOPEN: Duration = Duration::from_secs(3);

/// Translate the persisted config into what the driver wants.
fn settings_from(cfg: &Rx888Config) -> Settings {
    Settings {
        serial: Some(cfg.serial.trim().to_string()).filter(|s| !s.is_empty()),
        adc_rate_hz: cfg.adc_rate_hz,
        dither: cfg.dither,
        randomize: cfg.randomize,
        bias_tee_hf: cfg.bias_tee_hf,
        pga: cfg.pga,
        attenuator_db: cfg.attenuator_db,
        vga_db: cfg.vga_db,
        ppm: cfg.ppm,
    }
}

pub struct Rx888Source {
    handle: Rx888Handle,
    center: f64,
    rx_scratch: Vec<f32>,
    label: String,
    vga_db: f64,
    att_db: f64,
    bias_tee: bool,
}

impl Rx888Source {
    pub fn open(cfg: &Rx888Config, center_hz: f64) -> anyhow::Result<Self> {
        let firmware = cfg.firmware_path.trim();
        if !firmware.is_empty() {
            tracing::info!("RX-888: using firmware image {firmware}");
        }
        let handle = sdroxide_rx888::spawn(&settings_from(cfg), center_hz)?;
        let label = format!(
            "{} @ {:.1} Msps ADC → {:.3} Msps",
            handle.label(),
            handle.adc_rate_hz() / 1e6,
            handle.out_rate_hz() / 1e6,
        );
        tracing::info!("RX-888 source ready: {label}, center {center_hz:.0} Hz");
        Ok(Rx888Source {
            center: center_hz,
            rx_scratch: Vec::new(),
            label,
            vga_db: cfg.vga_db,
            att_db: cfg.attenuator_db,
            bias_tee: cfg.bias_tee_hf,
            handle,
        })
    }

    /// The complex baseband rate the downconverter produces.
    pub fn sample_rate_hz(&self) -> f64 {
        self.handle.out_rate_hz()
    }

    /// The ADC clock, for the settings UI to report.
    pub fn adc_rate_hz(&self) -> f64 {
        self.handle.adc_rate_hz()
    }
}

impl IqSource for Rx888Source {
    fn sample_rate(&self) -> f64 {
        self.handle.out_rate_hz()
    }

    fn center_hz(&self) -> f64 {
        self.center
    }

    fn set_center_hz(&mut self, hz: f64) -> Result<()> {
        self.center = hz;
        self.handle.set_center_hz(hz);
        Ok(())
    }

    fn read(&mut self, buf: &mut [Complex32]) -> Result<usize> {
        let need = buf.len() * 2;
        if self.rx_scratch.len() < need {
            self.rx_scratch.resize(need, 0.0);
        }
        let n = self.handle.read(&mut self.rx_scratch[..need]);
        let pairs = n / 2;
        if pairs == 0 {
            // Nothing yet — brief nap so the DSP loop doesn't spin hot.
            std::thread::sleep(Duration::from_millis(2));
            return Ok(0);
        }
        for p in 0..pairs {
            buf[p] = Complex32::new(self.rx_scratch[2 * p], self.rx_scratch[2 * p + 1]);
        }
        Ok(pairs)
    }

    fn describe(&self) -> String {
        self.label.clone()
    }

    /// The two analogue gain stages, plus pseudo-elements for the switches.
    ///
    /// Routing the switches through `SetGain` rather than adding `Command`
    /// variants keeps `Command`, `DeviceCaps` and the engine untouched for
    /// settings only this backend has — the same trick the RTL-SDR backend uses
    /// for its AGC and bias tee.
    fn set_gain_element(&mut self, name: &str, db: f64) -> Result<()> {
        match name {
            Rx888Config::VGA_ELEMENT => {
                self.vga_db = db;
                self.handle.set_vga_db(db);
            }
            Rx888Config::ATT_ELEMENT => {
                self.att_db = db;
                self.handle.set_att_db(db);
            }
            Rx888Config::DITHER_ELEMENT => self.handle.set_dither(db >= 0.5),
            Rx888Config::BIAS_TEE_ELEMENT => {
                self.bias_tee = db >= 0.5;
                self.handle.set_bias_tee(self.bias_tee);
            }
            Rx888Config::PGA_ELEMENT => self.handle.set_pga(db >= 0.5),
            _ => {}
        }
        Ok(())
    }

    fn current_gains(&self) -> Vec<(String, f64)> {
        vec![
            // Report what the hardware settled on where it has said so, since
            // both stages quantise: the VGA onto a linear voltage vernier and
            // the attenuator onto a 0.5 dB grid.
            (
                Rx888Config::VGA_ELEMENT.to_string(),
                self.handle.effective_vga_db().unwrap_or(self.vga_db),
            ),
            (
                Rx888Config::ATT_ELEMENT.to_string(),
                self.handle.effective_att_db().unwrap_or(self.att_db),
            ),
        ]
    }

    /// The whole of HF at once, at about twenty frames a second.
    ///
    /// The backend analyses roughly 2 % of the samples going past to build this,
    /// so it costs about 1 % of what the downconverter costs — see
    /// `sdroxide_dsp::WideSpectrum`.
    fn wide_spectrum_db(&mut self, out: &mut Vec<f32>) -> Option<(f64, f64)> {
        let frame = self.handle.take_wide_spectrum()?;
        out.clear();
        out.extend_from_slice(&frame);
        Some(self.handle.wide_span_hz())
    }

    /// A receiver that has been unplugged, or whose threads have died, is
    /// reported as needing a reopen so the engine reconnects on its own.
    fn needs_reopen(&self) -> bool {
        !self.handle.is_alive() || self.handle.silent_for() >= SILENCE_BEFORE_REOPEN
    }

    /// Hand the device back before the engine opens its replacement — without
    /// this, pressing Apply in Settings → Radio fails with "held by another
    /// program", the other program being us.
    fn release(&mut self) {
        self.handle.release();
    }

    /// Surface what an operator needs to know but cannot see: a link too slow
    /// for the requested sample rate, or DC sitting on the antenna feedline.
    fn open_status(&self) -> Option<String> {
        let mut notes: Vec<String> = Vec::new();
        if let Some(w) = self.handle.warning() {
            notes.push(w.to_string());
        }
        if self.bias_tee {
            notes.push(format!("{}: bias tee is ON — DC on the antenna coax", self.label));
        }
        (!notes.is_empty()).then(|| notes.join("  •  "))
    }
}
