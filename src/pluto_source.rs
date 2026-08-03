//! An [`IqSource`] for an ADALM-Pluto driven over IIOD by the native driver in
//! `sdroxide-pluto` — no libiio, no libSoapySDR.
//!
//! The AD9361 delivers wideband complex I/Q, so this drives the engine's normal
//! DDC/demod path exactly like a SoapySDR device (`audio_mode = false`), and
//! transmit is modulated I/Q rather than audio the rig modulates.
//!
//! # Zero IF
//!
//! Unlike every other native backend here, the Pluto's front end is zero-IF:
//! LO leakage, DC offset and flicker noise all pile up exactly where the
//! operator's VFO would otherwise sit. So this source does the two things the
//! SoapySDR path does for the same reason — it asks the engine to park the LO a
//! quarter-span away ([`IqSource::lo_offset_hz`]) and DC-blocks the stream
//! before anything downstream sees it.

use std::time::Duration;

use sdroxide_dsp::ComplexDcBlock;
use sdroxide_pluto::PlutoHandle;
use sdroxide_radio::{Complex32, DC_BLOCK_HZ, IqSource, Result, lo_offset_for};
use sdroxide_types::{PlutoAgc, PlutoConfig};

/// How long the device may deliver nothing before the connection counts as
/// dead and the engine starts reconnecting. The same five seconds the HPSDR
/// backend allows: this is a network rig, and a Pluto that has just been
/// re-plugged takes a while to bring its interface back up.
const SILENCE_BEFORE_REOPEN: Duration = Duration::from_secs(5);

pub struct PlutoSource {
    handle: PlutoHandle,
    center: f64,
    rx_scratch: Vec<f32>,
    tx_scratch: Vec<f32>,
    dc: ComplexDcBlock,
    lo_offset: f64,
    label: String,
}

impl PlutoSource {
    /// Open `address` and start receiving at `center_hz`.
    pub fn open(address: &str, cfg: &PlutoConfig, center_hz: f64) -> anyhow::Result<Self> {
        let handle = PlutoHandle::open(address, cfg, center_hz)?;
        let rate = handle.sample_rate_hz;
        // Decided against the analog filter we set ourselves — see
        // `sdroxide_radio::lo_offset_for` for why that filter is opened up
        // rather than left at the AD9361's default.
        let lo_offset = lo_offset_for(rate, handle.rf_bandwidth_hz);
        let label = handle.label();
        tracing::info!(
            "PlutoSDR source ready: {label}, centre {center_hz:.0} Hz, \
             LO offset {lo_offset:.0} Hz (0 = LO on the VFO)"
        );
        Ok(PlutoSource {
            center: center_hz,
            rx_scratch: Vec::new(),
            tx_scratch: Vec::new(),
            dc: ComplexDcBlock::new(DC_BLOCK_HZ, rate),
            lo_offset,
            label,
            handle,
        })
    }

    /// What the device says it can do — the source of every figure in
    /// `pluto_caps`.
    pub fn limits(&self) -> &sdroxide_pluto::PlutoLimits {
        &self.handle.limits
    }
}

impl IqSource for PlutoSource {
    fn sample_rate(&self) -> f64 {
        self.handle.sample_rate_hz
    }

    fn center_hz(&self) -> f64 {
        self.center
    }

    fn set_center_hz(&mut self, hz: f64) -> Result<()> {
        self.center = hz;
        self.handle.set_rx_freq(hz);
        Ok(())
    }

    fn lo_offset_hz(&self) -> f64 {
        self.lo_offset
    }

    fn read(&mut self, buf: &mut [Complex32]) -> Result<usize> {
        let need = buf.len() * 2;
        if self.rx_scratch.len() < need {
            self.rx_scratch.resize(need, 0.0);
        }
        let n = self.handle.rx_read(&mut self.rx_scratch[..need]);
        let pairs = n / 2;
        if pairs == 0 {
            // Nothing yet — brief nap so the DSP loop doesn't spin hot.
            std::thread::sleep(Duration::from_millis(2));
            return Ok(0);
        }
        for p in 0..pairs {
            buf[p] = Complex32::new(self.rx_scratch[2 * p], self.rx_scratch[2 * p + 1]);
        }
        // Deliberately not reset across an over: the offset is a property of
        // the hardware, not of the stream, so carrying the estimate avoids a
        // re-convergence transient every time receive resumes.
        self.dc.process(&mut buf[..pairs]);
        Ok(pairs)
    }

    fn describe(&self) -> String {
        self.label.clone()
    }

    /// The AD9361's receive gain, plus the two pseudo-elements this backend
    /// carries on the same command — the AGC mode and the reference trim. See
    /// [`PlutoConfig::AGC_ELEMENT`] for why they ride `SetGain` rather than
    /// having `Command` variants of their own.
    fn set_gain_element(&mut self, name: &str, db: f64) -> Result<()> {
        match name {
            PlutoConfig::RF_GAIN_ELEMENT => self.handle.set_rx_gain_db(db),
            PlutoConfig::AGC_ELEMENT => {
                self.handle.set_agc_mode(PlutoAgc::from_code(db).iio_name())
            }
            PlutoConfig::PPM_ELEMENT => self.handle.set_ppm(db),
            _ => {}
        }
        Ok(())
    }

    fn current_gains(&self) -> Vec<(String, f64)> {
        vec![(PlutoConfig::RF_GAIN_ELEMENT.to_string(), self.handle.rx_gain_db())]
    }

    fn set_tx_gain_element(&mut self, name: &str, db: f64) -> Result<()> {
        if name == PlutoConfig::TX_GAIN_ELEMENT {
            self.handle.set_tx_gain_db(db);
        }
        Ok(())
    }

    fn current_tx_gains(&self) -> Vec<(String, f64)> {
        vec![(PlutoConfig::TX_GAIN_ELEMENT.to_string(), self.handle.tx_gain_db())]
    }

    /// `rf_port_select`. A stock Pluto wires only `A_BALANCED` and `A`, but the
    /// AD9361 has nine receive ports and a board built around one may use
    /// another, so whatever the device published is offered.
    fn set_antenna(&mut self, name: &str) -> Result<()> {
        self.handle.set_rx_port(name);
        Ok(())
    }

    fn current_antenna(&self) -> String {
        self.handle.rx_port().to_string()
    }

    fn set_tx_antenna(&mut self, name: &str) -> Result<()> {
        self.handle.set_tx_port(name);
        Ok(())
    }

    fn current_tx_antenna(&self) -> String {
        self.handle.tx_port().to_string()
    }

    fn tx_begin(&mut self, center_hz: f64, _rate: f64) -> Result<f64> {
        Ok(self.handle.tx_begin(center_hz))
    }

    fn tx_write(&mut self, samples: &[Complex32]) -> Result<()> {
        self.tx_scratch.clear();
        self.tx_scratch.reserve(samples.len() * 2);
        for s in samples {
            self.tx_scratch.push(s.re);
            self.tx_scratch.push(s.im);
        }
        self.handle.tx_write(&self.tx_scratch);
        Ok(())
    }

    /// Let the queued samples reach the device before PTT drops. The engine
    /// hands us a burst faster than real time and the hardware drains it one
    /// buffer at a time, so unkeying immediately would cut the tail — which for
    /// FT8 is the difference between a decode and nothing.
    fn tx_drain(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while self.handle.tx_pending() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn tx_end(&mut self) -> Result<()> {
        self.handle.tx_end();
        Ok(())
    }

    /// Receive is torn down for the length of an over, but a partial buffer can
    /// still be sitting in the ring when it resumes.
    fn discard_pending_rx(&mut self) {
        self.handle.discard_pending_rx();
    }

    fn open_status(&self) -> Option<String> {
        self.handle.open_status()
    }

    /// A Pluto that has stopped delivering samples — unplugged, rebooted, its
    /// interface reconfigured, or its buffer taken by another program — is
    /// reported as needing a reopen so the engine reconnects on its own.
    fn needs_reopen(&self) -> bool {
        !self.handle.is_alive() || self.handle.silent_for() >= SILENCE_BEFORE_REOPEN
    }

    /// Close the three IIOD connections before the engine builds this front
    /// end's replacement. `iiod` will not hand the same buffer to a second
    /// connection, so opening the replacement while this one still holds it
    /// fails as "device busy" — the other program being us.
    fn release(&mut self) {
        self.handle.release();
    }
}
