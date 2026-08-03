use sdroxide_dsp::ComplexDcBlock;
use soapysdr::Direction;
use tracing::{info, warn};

use sdroxide_types::{DeviceCaps, GainElement};

use crate::{Complex32, DC_BLOCK_HZ, IqSource, RadioError, Result, lo_offset_for};

/// How many times a failing RX stream is rebuilt in place before the source
/// gives up and asks to be reopened. One rebuild covers a front end knocked
/// over by a call it refused; a stream still failing after that is a device
/// problem a fresh stream cannot fix.
const STREAM_REBUILD_LIMIT: u32 = 2;

/// One enumerated device: its label plus the args string that opens it.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub label: String,
    pub driver: String,
    pub args: String,
}

/// Enumerate devices matching a SoapySDR args filter ("" for all).
pub fn enumerate_devices(filter: &str) -> Result<Vec<DeviceInfo>> {
    let found = soapysdr::enumerate(filter)?;
    Ok(found
        .iter()
        .map(|args| DeviceInfo {
            label: args.get("label").unwrap_or_default().to_string(),
            driver: args.get("driver").unwrap_or_default().to_string(),
            args: args.to_string(),
        })
        .collect())
}

/// An open SoapySDR device plus its probed capabilities.
pub struct SoapyDevice {
    dev: soapysdr::Device,
    caps: DeviceCaps,
}

impl SoapyDevice {
    pub fn open(args: &str) -> Result<Self> {
        let dev = soapysdr::Device::new(args)?;
        let caps = probe_caps(&dev)?;
        info!(driver = %caps.driver, label = %caps.label, "opened SoapySDR device");
        Ok(SoapyDevice { dev, caps })
    }

    pub fn caps(&self) -> &DeviceCaps {
        &self.caps
    }

    pub fn device(&self) -> &soapysdr::Device {
        &self.dev
    }

    /// Pick a working sample rate: the requested one if supported, otherwise
    /// the closest supported value, preferring 48 kHz power-of-two multiples.
    pub fn choose_sample_rate(&self, requested: f64) -> f64 {
        let ok = |r: f64| {
            self.caps.rate_ranges.iter().any(|&(lo, hi)| r >= lo && r <= hi)
                || self.caps.sample_rates.iter().any(|&s| (s - r).abs() < 1.0)
        };
        if ok(requested) {
            return requested;
        }
        // Preferred ladder: 48k * 2^k
        let mut candidates: Vec<f64> = (0..12).map(|k| 48_000.0 * (1u64 << k) as f64).collect();
        candidates.extend(self.caps.sample_rates.iter().copied());
        candidates
            .into_iter()
            .filter(|&r| ok(r))
            .min_by(|a, b| (a - requested).abs().total_cmp(&(b - requested).abs()))
            .unwrap_or(requested)
    }

    /// Open, configure, and activate an RX stream on channel 0.
    ///
    /// `gain_db`: explicit overall RX gain; `None` enables hardware AGC when
    /// available, else a moderate manual gain.
    pub fn rx_source(
        self,
        sample_rate: f64,
        center_hz: f64,
        gain_db: Option<f64>,
    ) -> Result<SoapyRxSource> {
        let channel = 0;
        let rate = self.choose_sample_rate(sample_rate);
        self.dev.set_sample_rate(Direction::Rx, channel, rate)?;
        self.dev.set_frequency(Direction::Rx, channel, center_hz, ())?;

        // Hardware AGC owns the RX stages where it is running, so those must not
        // be re-asserted behind its back (see `rx_gains`).
        let mut agc = false;
        if let Some(g) = gain_db {
            let _ = self.dev.set_gain_mode(Direction::Rx, channel, false);
            self.dev.set_gain(Direction::Rx, channel, g)?;
        } else if self.dev.has_gain_mode(Direction::Rx, channel).unwrap_or(false) {
            let _ = self.dev.set_gain_mode(Direction::Rx, channel, true);
            agc = true;
        } else if let Ok(range) = self.dev.gain_range(Direction::Rx, channel) {
            let g = range.minimum + 0.4 * (range.maximum - range.minimum);
            let _ = self.dev.set_gain(Direction::Rx, channel, g);
        }

        let actual_rate = self.dev.sample_rate(Direction::Rx, channel)?;

        // SAFETY RAIL: force every TX gain stage to its minimum at startup
        // so keying up can never emit unexpected power.
        if self.caps.tx_channels > 0 {
            let _ = self.dev.set_gain(Direction::Tx, channel, 0.0);
            for name in self.dev.list_gains(Direction::Tx, channel).unwrap_or_default() {
                if let Ok(r) = self.dev.gain_element_range(Direction::Tx, channel, name.as_str()) {
                    let _ =
                        self.dev.set_gain_element(Direction::Tx, channel, name.as_str(), r.minimum);
                }
            }
        }

        // What the stages are set to now, which is what every later stream
        // restart and direction change has to reproduce.
        let read_back = |dir: Direction| -> Vec<(String, f64)> {
            self.dev
                .list_gains(dir, channel)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|name| {
                    let db = self.dev.gain_element(dir, channel, name.as_str()).ok()?;
                    Some((name, db))
                })
                .collect()
        };
        let rx_gains = if agc { Vec::new() } else { read_back(Direction::Rx) };
        let tx_gains =
            if self.caps.tx_channels > 0 { read_back(Direction::Tx) } else { Vec::new() };

        let analog_bw = self.dev.bandwidth(Direction::Rx, channel).unwrap_or(0.0);
        let lo_offset = lo_offset_for(actual_rate, analog_bw);
        info!(
            rate = actual_rate,
            analog_bw, lo_offset, "RX front end configured (LO offset 0 = LO on the VFO)"
        );

        let mut source = SoapyRxSource {
            dev: Some(self.dev),
            caps: self.caps,
            rx_stream: None,
            tx_stream: None,
            channel,
            sample_rate: actual_rate,
            center_hz,
            lo_offset,
            dc: ComplexDcBlock::new(DC_BLOCK_HZ, actual_rate),
            overflows: 0,
            tx_underflows: 0,
            stream_failures: 0,
            lost: false,
            rx_gains,
            tx_gains,
            tx_setup_needs_idle: false,
        };
        source.open_rx_stream()?;
        Ok(source)
    }
}

/// A live SoapySDR device: an RX stream (closed while transmitting on
/// half-duplex hardware), plus a TX stream while keyed.
pub struct SoapyRxSource {
    /// `None` once the source has been stood down ([`IqSource::release`]).
    ///
    /// The handle is dropped rather than merely flagged because SoapySDR keeps
    /// a table of open devices keyed by their arguments: a second `make` with
    /// the same arguments hands back the *same* device object, refcounted. So a
    /// replacement opened while this one is still held is not a fresh device at
    /// all — it is this device, carrying whatever state broke it.
    dev: Option<soapysdr::Device>,
    caps: DeviceCaps,
    rx_stream: Option<soapysdr::RxStream<Complex32>>,
    tx_stream: Option<soapysdr::TxStream<Complex32>>,
    channel: usize,
    sample_rate: f64,
    center_hz: f64,
    lo_offset: f64,
    /// Front-end DC/LO-leakage removal, applied to every block as it arrives so
    /// the spectrum display, both DDCs, the skimmer and the TCI IQ feed all see
    /// a clean stream. See [`ComplexDcBlock`].
    dc: ComplexDcBlock,
    overflows: u64,
    tx_underflows: u64,
    /// Consecutive read failures since the last block of samples arrived; reset
    /// as soon as the stream delivers again. Bounds the rebuild-in-place
    /// attempts at [`STREAM_REBUILD_LIMIT`].
    stream_failures: u32,
    /// The front end stopped working and could not be brought back in place, so
    /// it asks to be reopened ([`IqSource::needs_reopen`]) and the engine's
    /// background reconnect owns it from here.
    lost: bool,
    /// What each RX gain stage is supposed to be set to, so a stream restart
    /// cannot quietly hand the front end back to whatever the hardware happens
    /// to be left in — see [`SoapyRxSource::reassert_gains`]. Empty while
    /// hardware AGC is running, which owns those stages itself.
    rx_gains: Vec<(String, f64)>,
    /// The same for the transmit side, re-asserted on every key-up.
    tx_gains: Vec<(String, f64)>,
    /// This driver will not build a transmit stream while the receiver is
    /// running, so key-down stands the receiver down first — see
    /// [`SoapyRxSource::open_tx_stream`]. Latched on the first refusal rather
    /// than assumed, because most drivers are happy either way.
    tx_setup_needs_idle: bool,
}

/// Record a gain element's requested value, replacing any earlier one.
fn remember_gain(list: &mut Vec<(String, f64)>, name: &str, db: f64) {
    match list.iter_mut().find(|(n, _)| n == name) {
        Some(entry) => entry.1 = db,
        None => list.push((name.to_string(), db)),
    }
}

impl SoapyRxSource {
    pub fn caps(&self) -> &DeviceCaps {
        &self.caps
    }

    /// The open device, or an error once it has been stood down.
    fn dev(&self) -> Result<&soapysdr::Device> {
        self.dev.as_ref().ok_or_else(|| RadioError::Msg("the device was released".into()))
    }

    /// Create and activate a fresh RX stream, reasserting the RX rate and
    /// frequency first (half-duplex hardware shares the LO/clock with TX, so
    /// a TX cycle can leave them on TX values).
    fn open_rx_stream(&mut self) -> Result<()> {
        let dev = self.dev()?;
        dev.set_sample_rate(Direction::Rx, self.channel, self.sample_rate)?;
        dev.set_frequency(Direction::Rx, self.channel, self.center_hz, ())?;
        let mut stream = dev.rx_stream::<Complex32>(&[self.channel])?;
        stream.activate(None)?;
        self.rx_stream = Some(stream);
        self.reassert_gains(Direction::Rx);
        info!(rate = self.sample_rate, center = self.center_hz, "RX stream active");
        Ok(())
    }

    /// Write the gain stages for `dir` back to what they are supposed to be.
    ///
    /// # Why a stream restart loses them
    ///
    /// Stopping a stream puts the transceiver in its idle state, and that state
    /// is not neutral. On a HackRF, `hackrf_stop_rx`/`hackrf_stop_tx` sets
    /// transceiver mode OFF, and the firmware's `rf_path_set_direction(OFF)`
    /// bypasses the 14 dB RF amplifier and *records* the bypass; starting RX or
    /// TX again only powers the amp back up if the bypass is already clear, so
    /// the amp stays dead until something sends `hackrf_set_amp_enable` again.
    /// SoapyHackRF does not: it caches the amp state and re-applies it only on a
    /// direct RX↔TX handover, and closing the stream (which is what half-duplex
    /// keying does here) goes through mode OFF instead. The cached value still
    /// reads 14 dB, so nothing anywhere notices that the hardware disagrees.
    ///
    /// The result an operator sees is the receive AMP going dead after the first
    /// over and the transmit AMP never working at all — the level is set while
    /// receiving, then thrown away by the very transition that was supposed to
    /// use it.
    fn reassert_gains(&self, dir: Direction) {
        let Some(dev) = self.dev.as_ref() else { return };
        let wanted = match dir {
            Direction::Rx => &self.rx_gains,
            Direction::Tx => &self.tx_gains,
        };
        for (name, db) in wanted {
            if let Err(e) = dev.set_gain_element(dir, self.channel, name.as_str(), *db) {
                warn!("could not re-apply the {name} gain: {e}");
            }
        }
    }

    /// Create the transmit stream, standing the receiver down first if this
    /// driver only lets streams be set up while none of them are running.
    ///
    /// # Why the receiver may have to go away first
    ///
    /// Setting up a stream is not necessarily independent of the streams that
    /// already exist, even on hardware that transmits and receives at the same
    /// time. SoapySX (SXceiver) refuses outright — "Streams can be setup only
    /// if none of the streams are running" — because setting one up configures
    /// its two ALSA PCMs and links them together, which ALSA only permits while
    /// they are stopped. LimeSuite is quieter about it and no better: it stops
    /// the running data threads to make room ("Stopping data stream to set up a
    /// new stream") and restarts them underneath the receive stream. Others
    /// (HackRF, PlutoSDR, bladeRF) only object to a second stream in the *same*
    /// direction and do not care about this at all.
    ///
    /// So the cheap call is tried first, and on a refusal the receive stream is
    /// closed, the transmit stream built while the device is idle, and the
    /// receiver opened again before the transmitter is keyed — only *setup* is
    /// what those drivers guard, so activating both afterwards is fine. Later
    /// overs go straight to the idle path: repeating a call that has already
    /// been refused costs a round trip and, over SoapyRemote, a socket pair and
    /// a stream window per key-down.
    fn open_tx_stream(&mut self) -> Result<soapysdr::TxStream<Complex32>> {
        if !self.tx_setup_needs_idle {
            match self.dev()?.tx_stream::<Complex32>(&[self.channel]) {
                Ok(tx) => return Ok(tx),
                Err(e) => {
                    warn!(
                        "the TX stream could not be set up while the receiver was running ({e}); \
                         standing the receiver down for this and every later key-down"
                    );
                    self.tx_setup_needs_idle = true;
                }
            }
        }
        // Closing the receive stream is what leaves the device idle. On
        // half-duplex hardware `tx_begin` has already closed it and it stays
        // closed for the whole over, so there is nothing to put back.
        let restore_rx = self.rx_stream.is_some();
        self.rx_stream = None;
        let tx = self.dev()?.tx_stream::<Complex32>(&[self.channel]);
        if restore_rx {
            // Back before the transmitter is keyed, and back even when the
            // transmit stream could not be built either: a refused key-up must
            // not cost the operator the receiver as well.
            if let Err(e) = self.open_rx_stream() {
                warn!("the receiver did not come back around the TX stream setup: {e}");
            }
        }
        Ok(tx?)
    }

    /// Put the front end back the way it was after a call the driver refused,
    /// and give up on it if that fails too.
    ///
    /// A driver does not necessarily fail cleanly: SoapyLMS7 asked for a
    /// frequency below the LMS7002M's range reconfigures the interface clock,
    /// fails half-way ("Cannot achieve desired sample rate: rate too low"), and
    /// leaves the device delivering nothing at all — a LimeSDR tuned out of
    /// range that way stays dead until the process is restarted. Reasserting
    /// the rate and the last frequency that worked, on a fresh stream, is what
    /// brings it back.
    fn recover(&mut self, what: &str) {
        if self.tx_stream.is_some() {
            // Mid-over: the receive path belongs to `tx_end`, which reasserts
            // the rate and this same centre when it hands RX back. Opening an
            // RX stream now would fight it — and on half-duplex hardware would
            // pull the front end off transmit while the operator is keyed.
            info!("front end left alone until the over ends ({what})");
            return;
        }
        // The stream is rebuilt, not reused: a half-configured front end can
        // leave it delivering stale or aliased buffers (see `tx_begin`).
        self.rx_stream = None;
        match self.open_rx_stream() {
            Ok(()) => info!(center = self.center_hz, "front end restarted after {what}"),
            Err(e) => {
                warn!("front end could not be restarted after {what}: {e}");
                self.lost = true;
            }
        }
    }
}

impl IqSource for SoapyRxSource {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn center_hz(&self) -> f64 {
        self.center_hz
    }

    fn set_center_hz(&mut self, hz: f64) -> Result<()> {
        let Some(dev) = self.dev.as_ref() else {
            // Stood down and waiting to be replaced. The replacement is opened
            // on whatever the dial says by then, so the operator keeps tuning
            // through the gap; refusing here would only snap the dial back for
            // a front end that is on its way out anyway.
            self.center_hz = hz;
            return Ok(());
        };
        let tuned = dev.set_frequency(Direction::Rx, self.channel, hz, ());
        match tuned {
            Ok(()) => {
                self.center_hz = hz;
                Ok(())
            }
            Err(e) => {
                warn!(request_hz = hz, "the front end refused the tune: {e}");
                // The old centre is still what `self.center_hz` holds, so this
                // puts the hardware back on the last frequency that worked.
                self.recover("a refused tune");
                Err(RadioError::Soapy(e))
            }
        }
    }

    fn read(&mut self, buf: &mut [Complex32]) -> Result<usize> {
        // No RX stream: transmitting on half-duplex hardware, or the front end
        // has been given up on / stood down. Neither has samples, and the
        // second must not spin the engine's loop while it waits to be reopened.
        let lost = self.lost;
        let Some(stream) = self.rx_stream.as_mut() else {
            if lost {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            return Ok(0);
        };
        let n = match stream.read(&mut [buf], 200_000) {
            Ok(n) => n,
            Err(e) if e.code == soapysdr::ErrorCode::Timeout => 0,
            // Overflow = samples dropped because we read too slowly.
            // Recoverable: log and keep streaming.
            Err(e) if e.code == soapysdr::ErrorCode::Overflow => {
                self.overflows += 1;
                if self.overflows.is_power_of_two() {
                    tracing::warn!(count = self.overflows, "RX overflow (samples dropped)");
                }
                0
            }
            // Anything else is the stream itself failing — a dongle pulled out
            // of its socket, or a front end left broken by a call it refused.
            // Rebuilding it in place is usually enough; a stream that keeps
            // failing anyway is given up on rather than rebuilt in a loop, and
            // asks to be reopened instead of taking the engine down with it.
            Err(e) => {
                warn!(attempt = self.stream_failures + 1, "RX stream failed: {e}");
                self.stream_failures += 1;
                if self.stream_failures > STREAM_REBUILD_LIMIT {
                    self.rx_stream = None;
                    self.lost = true;
                } else {
                    self.recover("a stream failure");
                }
                return Ok(0);
            }
        };
        if n > 0 {
            self.stream_failures = 0; // it is delivering again
        }
        // Deliberately not reset across a dropped block or a TX cycle: the
        // offset is a property of the hardware, not of the stream, so carrying
        // the estimate avoids a re-convergence transient on every resume.
        self.dc.process(&mut buf[..n]);
        Ok(n)
    }

    fn lo_offset_hz(&self) -> f64 {
        self.lo_offset
    }

    fn describe(&self) -> String {
        format!("{} ({:.3} Msps)", self.caps.label, self.sample_rate / 1e6)
    }

    fn set_gain_element(&mut self, name: &str, db: f64) -> Result<()> {
        let dev = self.dev()?;
        let _ = dev.set_gain_mode(Direction::Rx, self.channel, false);
        dev.set_gain_element(Direction::Rx, self.channel, name, db)?;
        // Remembered as well as applied: this is what `reassert_gains` puts back
        // after a stream restart, and setting it by hand also ends any AGC that
        // was running (the `set_gain_mode` above), so the stage is ours now.
        remember_gain(&mut self.rx_gains, name, db);
        Ok(())
    }

    fn set_antenna(&mut self, name: &str) -> Result<()> {
        self.dev()?.set_antenna(Direction::Rx, self.channel, name)?;
        Ok(())
    }

    fn current_gains(&self) -> Vec<(String, f64)> {
        let Some(dev) = self.dev.as_ref() else { return Vec::new() };
        dev.list_gains(Direction::Rx, self.channel)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|name| {
                let db = dev.gain_element(Direction::Rx, self.channel, name.as_str()).ok()?;
                Some((name, db))
            })
            .collect()
    }

    fn current_antenna(&self) -> String {
        self.dev
            .as_ref()
            .and_then(|d| d.antenna(Direction::Rx, self.channel).ok())
            .unwrap_or_default()
    }

    fn tx_begin(&mut self, center_hz: f64, rate: f64) -> Result<f64> {
        if self.caps.tx_channels == 0 {
            return Err(RadioError::Msg("device is not transmit capable".into()));
        }
        // Half duplex (HackRF): fully CLOSE the RX stream before switching
        // direction. Merely deactivating it and reactivating after TX leaves
        // the SoapyHackRF RX path corrupted (repeated/aliased buffers) until
        // the device is reopened; a fresh stream avoids that.
        if !self.caps.full_duplex {
            self.rx_stream = None;
        }
        let dev = self.dev()?;
        // Only reprogram the rate when the device is not already on it. The
        // engine keys up at the rate the receiver is running at, so this is
        // normally a no-op request — but not a no-op call: hardware that clocks
        // both directions from one divider (the SX1255 does) reconfigures the
        // whole front end here, and SoapySX warns that doing so while a stream
        // runs can corrupt the I2S channel order.
        if (dev.sample_rate(Direction::Tx, self.channel).unwrap_or(0.0) - rate).abs() > 1.0 {
            dev.set_sample_rate(Direction::Tx, self.channel, rate)?;
        }
        dev.set_frequency(Direction::Tx, self.channel, center_hz, ())?;
        let actual = dev.sample_rate(Direction::Tx, self.channel)?;

        let mut tx = self.open_tx_stream()?;
        tx.activate(None)?;
        self.tx_stream = Some(tx);
        // After activation, so the direction change cannot undo it: switching
        // the RF path is what drops the amplifier state in the first place.
        self.reassert_gains(Direction::Tx);
        tracing::info!(center_hz, rate = actual, "TX active");
        Ok(actual)
    }

    fn tx_write(&mut self, samples: &[Complex32]) -> Result<()> {
        let Some(tx) = self.tx_stream.as_mut() else {
            return Err(RadioError::Msg("TX not active".into()));
        };
        match tx.write_all(&[samples], None, false, 500_000) {
            Ok(()) => Ok(()),
            Err(e) if e.code == soapysdr::ErrorCode::Underflow => {
                self.tx_underflows += 1;
                if self.tx_underflows.is_power_of_two() {
                    tracing::warn!(count = self.tx_underflows, "TX underflow");
                }
                Ok(())
            }
            Err(e) => Err(RadioError::Soapy(e)),
        }
    }

    fn tx_end(&mut self) -> Result<()> {
        // Dropping the TX stream deactivates and closes it.
        self.tx_stream = None;
        // Whatever the duplex, the receiver is whatever is missing here: half
        // duplex handed its stream over at key-down (see `tx_begin`), and on a
        // full-duplex device whose driver made us close it (`open_tx_stream`)
        // it may not have come back. A device that has been stood down or given
        // up on is not restarted from here — that is the reconnect's job.
        if self.rx_stream.is_none() && self.dev.is_some() && !self.lost {
            // Reopen a fresh RX stream (see tx_begin).
            self.open_rx_stream()?;
        }
        tracing::info!("TX ended, RX restored");
        Ok(())
    }

    /// On full-duplex hardware the RX stream stays active for the whole over,
    /// so the driver keeps buffering internally; flush it with zero-timeout
    /// reads so the first post-TX block isn't a stale backlog. Half-duplex
    /// hardware already gets a fresh, empty stream from `tx_end` above.
    fn discard_pending_rx(&mut self) {
        if !self.caps.full_duplex {
            return;
        }
        let Some(stream) = self.rx_stream.as_mut() else { return };
        let mut scratch = [Complex32::new(0.0, 0.0); 4096];
        // Bounded: a driver that never reports empty must not spin forever.
        for _ in 0..64 {
            match stream.read(&mut [&mut scratch], 0) {
                Ok(0) => break,
                Ok(_) => continue,
                // An overflow is what a whole over of unread samples looks
                // like, so it is the expected *first* result here rather than
                // a reason to stop: the samples it dropped are the stale ones
                // being discarded anyway. Breaking on it would abandon the
                // flush on exactly the case it exists for.
                Err(e) if e.code == soapysdr::ErrorCode::Overflow => continue,
                // Timeout is the ring reporting itself empty — the drain's
                // normal exit. Anything else is the stream itself failing,
                // which `read` is the one equipped to handle.
                Err(_) => break,
            }
        }
    }

    fn set_tx_gain_element(&mut self, name: &str, db: f64) -> Result<()> {
        self.dev()?.set_gain_element(Direction::Tx, self.channel, name, db)?;
        remember_gain(&mut self.tx_gains, name, db);
        // A stage shared between the two directions — the HackRF's 14 dB amp is
        // one switch, not two — has just been moved by a transmit setting while
        // the receiver is the one using it. Put the receive side back; the
        // transmit value is re-asserted at key-up.
        if self.tx_stream.is_none() {
            self.reassert_gains(Direction::Rx);
        }
        Ok(())
    }

    fn current_tx_gains(&self) -> Vec<(String, f64)> {
        let Some(dev) = self.dev.as_ref() else { return Vec::new() };
        dev.list_gains(Direction::Tx, self.channel)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|name| {
                let db = dev.gain_element(Direction::Tx, self.channel, name.as_str()).ok()?;
                Some((name, db))
            })
            .collect()
    }

    fn set_tx_antenna(&mut self, name: &str) -> Result<()> {
        if self.caps.tx_channels == 0 {
            return Err(RadioError::Msg("device is not transmit capable".into()));
        }
        self.dev()?.set_antenna(Direction::Tx, self.channel, name)?;
        Ok(())
    }

    fn current_tx_antenna(&self) -> String {
        if self.caps.tx_channels == 0 {
            return String::new();
        }
        self.dev
            .as_ref()
            .and_then(|d| d.antenna(Direction::Tx, self.channel).ok())
            .unwrap_or_default()
    }

    fn needs_reopen(&self) -> bool {
        self.lost
    }

    fn release(&mut self) {
        // Streams first: a stream outliving its device is a use-after-free in
        // the driver, not merely an orphan.
        self.rx_stream = None;
        self.tx_stream = None;
        self.dev = None;
        // Inert but callable, and asking for a replacement — see the trait doc.
        self.lost = true;
    }
}

fn probe_caps(dev: &soapysdr::Device) -> Result<DeviceCaps> {
    let driver = dev.driver_key().unwrap_or_default();
    let hw_info = dev.hardware_info().unwrap_or_default();
    let label = hw_info
        .get("label")
        .map(str::to_string)
        .unwrap_or_else(|| format!("{} ({})", driver, dev.hardware_key().unwrap_or_default()));

    let rx_channels = dev.num_channels(Direction::Rx).unwrap_or(0);
    let tx_channels = dev.num_channels(Direction::Tx).unwrap_or(0);
    let full_duplex = rx_channels > 0 && dev.full_duplex(Direction::Rx, 0).unwrap_or(false);

    let ranges = |dir: Direction, has: bool| -> Vec<(f64, f64)> {
        if !has {
            return Vec::new();
        }
        dev.frequency_range(dir, 0)
            .map(|v| v.into_iter().map(|r| (r.minimum, r.maximum)).collect())
            .unwrap_or_default()
    };
    let freq_ranges_rx = ranges(Direction::Rx, rx_channels > 0);
    let freq_ranges_tx = ranges(Direction::Tx, tx_channels > 0);

    let mut sample_rates = Vec::new();
    let mut rate_ranges = Vec::new();
    if rx_channels > 0 {
        for r in dev.get_sample_rate_range(Direction::Rx, 0).unwrap_or_default() {
            if (r.maximum - r.minimum).abs() < 1.0 {
                sample_rates.push(r.minimum);
            } else {
                rate_ranges.push((r.minimum, r.maximum));
            }
        }
    }

    let mut gains = Vec::new();
    for (dir, typed, n) in [
        (Direction::Rx, sdroxide_types::Direction::Rx, rx_channels),
        (Direction::Tx, sdroxide_types::Direction::Tx, tx_channels),
    ] {
        if n == 0 {
            continue;
        }
        for name in dev.list_gains(dir, 0).unwrap_or_default() {
            if let Ok(r) = dev.gain_element_range(dir, 0, name.as_str()) {
                gains.push(GainElement {
                    name,
                    direction: typed,
                    min_db: r.minimum,
                    max_db: r.maximum,
                    step_db: r.step,
                });
            }
        }
    }

    let antennas_rx = if rx_channels > 0 {
        dev.antennas(Direction::Rx, 0).unwrap_or_default()
    } else {
        Vec::new()
    };
    let antennas_tx = if tx_channels > 0 {
        dev.antennas(Direction::Tx, 0).unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut sensors: Vec<String> = dev.list_sensors().unwrap_or_default();
    for (dir, n) in [(Direction::Rx, rx_channels), (Direction::Tx, tx_channels)] {
        if n > 0 {
            sensors.extend(dev.list_channel_sensors(dir, 0).unwrap_or_default());
        }
    }
    let lower: Vec<String> = sensors.iter().map(|s| s.to_lowercase()).collect();
    let has_swr_sensor = lower.iter().any(|s| s.contains("swr") || s.contains("vswr"));
    let has_fwd_power_sensor =
        lower.iter().any(|s| s.contains("forward") || s.contains("fwd") || s.contains("tx_power"));

    Ok(DeviceCaps {
        driver,
        label,
        rx_channels,
        tx_channels,
        full_duplex,
        audio_mode: false,
        tx_audio: false,
        freq_ranges_rx,
        freq_ranges_tx,
        sample_rates,
        rate_ranges,
        gains,
        antennas_rx,
        antennas_tx,
        sensors,
        has_swr_sensor,
        has_fwd_power_sensor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gain_element_is_remembered_once_and_updated_in_place() {
        let mut gains = Vec::new();
        remember_gain(&mut gains, "AMP", 14.0);
        remember_gain(&mut gains, "LNA", 24.0);
        // A second value for a stage replaces the first rather than queuing
        // behind it — otherwise a stream restart would re-apply the old one.
        remember_gain(&mut gains, "AMP", 0.0);
        assert_eq!(gains, vec![("AMP".to_string(), 0.0), ("LNA".to_string(), 24.0)]);
    }
}
