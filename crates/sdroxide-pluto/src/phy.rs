//! The AD9361/AD9363 front end, addressed through IIO attributes.
//!
//! Three IIO devices make up a Pluto. `ad9361-phy` is the transceiver itself —
//! local oscillators, sample rate, analog filter, gains, AGC mode, port
//! selection. `cf-ad9361-lpc` and `cf-ad9361-dds-core-lpc` are the receive and
//! transmit DMA buffers, which carry samples and nothing else.
//!
//! # Read the limits, do not assume them
//!
//! Every limit this backend reports to the UI is read off the device through an
//! `_available` attribute. That is not fastidiousness: the same firmware runs on
//! a stock AD9363 board (325 MHz–3.8 GHz) and on one unlocked to AD9364
//! (70 MHz–6 GHz), and the receive gain range moves with frequency. Hardcoding
//! either would mean half the Plutos in circulation refusing frequencies they
//! can reach, or accepting ones they cannot.
//!
//! Where a firmware publishes no `_available`, a fallback is used **and
//! recorded** in [`PlutoLimits::assumed`], which the source adapter surfaces
//! through `IqSource::open_status` — a limit that was guessed should say so
//! rather than look like a measurement.
//!
//! # The two channels called `voltage0`
//!
//! In IIO the AD9361's receive chain is `INPUT voltage0` and its transmit chain
//! `OUTPUT voltage0`. They share a name and several attribute names
//! (`hardwaregain`, `rf_bandwidth`, `sampling_frequency`) and are entirely
//! different things — receive `hardwaregain` is gain in dB, transmit
//! `hardwaregain` is *attenuation* expressed as negative dB.

use crate::context::{Context, Device, ScanFormat};
use crate::error::{Error, Result};
use crate::iiod::Connection;

/// The transceiver's control device.
pub const PHY: &str = "ad9361-phy";
/// The receive DMA buffer.
pub const RX_BUFFER: &str = "cf-ad9361-lpc";
/// The transmit DMA buffer.
pub const TX_BUFFER: &str = "cf-ad9361-dds-core-lpc";

/// Receive chain: `INPUT voltage0` on the phy.
const RX_CHAN: (bool, &str) = (false, "voltage0");
/// Transmit chain: `OUTPUT voltage0` on the phy.
const TX_CHAN: (bool, &str) = (true, "voltage0");
/// Receive local oscillator.
const RX_LO: (bool, &str) = (true, "altvoltage0");
/// Transmit local oscillator.
const TX_LO: (bool, &str) = (true, "altvoltage1");

/// AD9363 tuning range, the conservative fallback when the device publishes no
/// `frequency_available`.
const AD9363_LO_HZ: (f64, f64) = (325_000_000.0, 3_800_000_000.0);
/// AD9364 tuning range — what an unlocked Pluto reaches.
const AD9364_LO_HZ: (f64, f64) = (70_000_000.0, 6_000_000_000.0);
/// AD9361 sample-rate range. The floor is the internal FIR decimator's output;
/// the ceiling is the part's, not the link's — see `PlutoConfig::SAMPLE_RATES`
/// for what a USB Ethernet gadget can actually carry.
const RATE_HZ: (f64, f64) = (521_000.0, 61_440_000.0);
/// AD9361 analog filter range.
const BANDWIDTH_HZ: (f64, f64) = (200_000.0, 56_000_000.0);
/// Receive gain, dB: (min, max, step).
const RX_GAIN_DB: (f64, f64, f64) = (0.0, 71.0, 1.0);
/// Transmit attenuation expressed as gain, dB: (min, max, step).
const TX_GAIN_DB: (f64, f64, f64) = (-89.75, 0.0, 0.25);

/// What the device says it can do.
#[derive(Debug, Clone)]
pub struct PlutoLimits {
    pub rx_lo_hz: (f64, f64),
    pub tx_lo_hz: (f64, f64),
    pub sample_rate_hz: (f64, f64),
    pub rf_bandwidth_hz: (f64, f64),
    /// (min, max, step)
    pub rx_gain_db: (f64, f64, f64),
    /// (min, max, step), as gain — so the numbers are negative.
    pub tx_gain_db: (f64, f64, f64),
    pub rx_ports: Vec<String>,
    pub tx_ports: Vec<String>,
    pub agc_modes: Vec<String>,
    /// Limits that came from the fallback table rather than the device, named
    /// so the operator can be told which figures are guesses.
    pub assumed: Vec<String>,
}

impl PlutoLimits {
    /// The fallback set, before anything has been read.
    fn fallback(ad9364: bool) -> PlutoLimits {
        let lo = if ad9364 { AD9364_LO_HZ } else { AD9363_LO_HZ };
        PlutoLimits {
            rx_lo_hz: lo,
            tx_lo_hz: lo,
            sample_rate_hz: RATE_HZ,
            rf_bandwidth_hz: BANDWIDTH_HZ,
            rx_gain_db: RX_GAIN_DB,
            tx_gain_db: TX_GAIN_DB,
            rx_ports: Vec::new(),
            tx_ports: Vec::new(),
            agc_modes: vec![
                "manual".into(),
                "slow_attack".into(),
                "fast_attack".into(),
                "hybrid".into(),
            ],
            assumed: Vec::new(),
        }
    }

    /// A sentence for `IqSource::open_status`, or `None` when everything came
    /// from the device.
    pub fn assumption_notice(&self) -> Option<String> {
        if self.assumed.is_empty() {
            return None;
        }
        Some(format!(
            "PlutoSDR: this firmware does not publish {} — sdroxide is assuming the \
             standard AD936x figures, which may not match your board",
            self.assumed.join(", ")
        ))
    }
}

/// Everything resolved once at open time: the wire ids of the devices and
/// channels, the sample layouts, and the device's own limits.
#[derive(Debug, Clone)]
pub struct Phy {
    pub phy_id: String,
    pub rx_buffer_id: String,
    pub tx_buffer_id: String,
    /// Wire layouts of the two *enabled* scan elements — I then Q — on the
    /// receive buffer. A 2R2T device's second pair is never enabled, so it
    /// never appears here.
    pub rx_scan: Vec<ScanFormat>,
    /// … and for the transmit buffer.
    pub tx_scan: Vec<ScanFormat>,
    /// DDS tone channels on the transmit buffer, which have to be silenced
    /// before DMA transmit will be heard.
    pub dds_channels: Vec<String>,
    pub limits: PlutoLimits,
    pub model: String,
    pub firmware: String,
    pub serial: String,
}

impl Phy {
    /// Resolve the devices and read the limits. `addr` only decorates errors.
    pub fn probe(conn: &mut Connection, ctx: &Context, addr: &str) -> Result<Phy> {
        let phy = ctx.require(PHY, addr)?;
        let rx = ctx.require(RX_BUFFER, addr)?;
        let tx = ctx.require(TX_BUFFER, addr)?;

        let (rx_scan, rx_note) = iq_pair(RX_BUFFER, rx)?;
        let (tx_scan, tx_note) = iq_pair(TX_BUFFER, tx)?;
        for note in [&rx_note, &tx_note].into_iter().flatten() {
            tracing::info!("PlutoSDR: {note}");
            conn.note(note);
        }
        let dds_channels: Vec<String> = tx
            .channels
            .iter()
            .filter(|c| c.output && c.scan.is_none() && c.has_attr("raw"))
            .map(|c| c.id.clone())
            .collect();

        let mut limits = PlutoLimits::fallback(ctx.is_ad9364());
        let phy_id = phy.id.clone();

        // Each of these only issues a READ when the context says the attribute
        // is there — an -ENOENT per missing attribute would fill the trace with
        // noise on exactly the firmwares where the trace matters most.
        let mut missing = Vec::new();
        if let Some(r) = read_range(conn, &phy_id, phy, RX_LO, "frequency_available")? {
            limits.rx_lo_hz = (r.0, r.2);
            limits.tx_lo_hz = (r.0, r.2);
        } else {
            missing.push("a tuning range");
        }
        if let Some(r) = read_range(conn, &phy_id, phy, TX_LO, "frequency_available")? {
            limits.tx_lo_hz = (r.0, r.2);
        }
        // Not counted as an assumption worth telling the operator about: no
        // AD9361 driver publishes this, and the range is a property of the
        // silicon rather than of the board, so the fallback is simply the right
        // answer. Flagging it would put a warning on every Pluto ever made.
        if let Some(r) = read_range(conn, &phy_id, phy, RX_CHAN, "sampling_frequency_available")? {
            limits.sample_rate_hz = (r.0, r.2);
        }
        if let Some(r) = read_range(conn, &phy_id, phy, RX_CHAN, "rf_bandwidth_available")? {
            limits.rf_bandwidth_hz = (r.0, r.2);
        }
        if let Some(r) = read_range(conn, &phy_id, phy, RX_CHAN, "hardwaregain_available")? {
            limits.rx_gain_db = (r.0, r.2, r.1);
        } else {
            missing.push("a receive gain range");
        }
        if let Some(r) = read_range(conn, &phy_id, phy, TX_CHAN, "hardwaregain_available")? {
            limits.tx_gain_db = (r.0, r.2, r.1);
        } else {
            missing.push("a transmit attenuation range");
        }
        limits.rx_ports = read_list(conn, &phy_id, phy, RX_CHAN, "rf_port_select_available")?;
        limits.tx_ports = read_list(conn, &phy_id, phy, TX_CHAN, "rf_port_select_available")?;
        let modes = read_list(conn, &phy_id, phy, RX_CHAN, "gain_control_mode_available")?;
        if !modes.is_empty() {
            limits.agc_modes = modes;
        }
        limits.assumed = missing.into_iter().map(str::to_string).collect();

        tracing::info!(
            "PlutoSDR: {} firmware {} — RX LO {:.3}–{:.3} MHz, rate {:.0}–{:.0} ksps, \
             RX gain {:.1}–{:.1} dB, TX gain {:.2}–{:.2} dB",
            ctx.hw_model(),
            ctx.fw_version(),
            limits.rx_lo_hz.0 / 1e6,
            limits.rx_lo_hz.1 / 1e6,
            limits.sample_rate_hz.0 / 1e3,
            limits.sample_rate_hz.1 / 1e3,
            limits.rx_gain_db.0,
            limits.rx_gain_db.1,
            limits.tx_gain_db.0,
            limits.tx_gain_db.1,
        );
        if let Some(notice) = limits.assumption_notice() {
            tracing::warn!("{notice}");
        }

        Ok(Phy {
            phy_id,
            rx_buffer_id: rx.id.clone(),
            tx_buffer_id: tx.id.clone(),
            rx_scan,
            tx_scan,
            dds_channels,
            limits,
            model: ctx.hw_model().to_string(),
            firmware: ctx.fw_version().to_string(),
            serial: ctx.hw_serial().to_string(),
        })
    }

    /// Bytes one complex sample occupies on the receive buffer.
    pub fn rx_sample_bytes(&self) -> usize {
        self.rx_scan.iter().map(|f| f.bytes()).sum()
    }

    /// … and on the transmit buffer.
    pub fn tx_sample_bytes(&self) -> usize {
        self.tx_scan.iter().map(|f| f.bytes()).sum()
    }

    // ---- control --------------------------------------------------------

    /// Set the receive sample rate, returning what the hardware settled on.
    ///
    /// On the AD9361 the receive and transmit paths hang off the same clock
    /// tree, so this moves both; the transmit rate is read back rather than
    /// assumed.
    pub fn set_sample_rate(&self, conn: &mut Connection, hz: f64) -> Result<f64> {
        let (min, max) = self.limits.sample_rate_hz;
        let want = hz.clamp(min, max).round() as i64;
        self.write_on_a_floor(conn, RX_CHAN, "sampling_frequency", want, min)?;
        self.sample_rate(conn)
    }

    /// Write an integer attribute that may be sitting exactly on the floor the
    /// device published, retrying one step up when the device then rejects its
    /// own stated minimum as out of range.
    ///
    /// IIO prints an `_available` range through integer division, so a floor
    /// that is really a fraction arrives a hair low — and the driver that
    /// printed it still enforces the true value. An AD9361 with its FIR
    /// bypassed publishes `[2083333 1 61440000]` for a real minimum of
    /// 25 MHz / 12 = 2083333.33: its clock-chain solver accepts a rate only if
    /// `rate × 12` clears the ADC's 25 MHz floor, and 2083333 × 12 is four
    /// hertz short. So a stock Pluto refuses the rate it just asked for, and
    /// every sdroxide sample rate at or below 2.083 Msps — which was all three
    /// of the lowest offered, including the default — died here with EINVAL
    /// before a single buffer was opened.
    ///
    /// One retry is enough and no more is justified: truncation cannot lose as
    /// much as a whole unit, so the next integer up is necessarily inside the
    /// real range. It is only ever attempted when the value *is* the published
    /// floor and the device called it invalid.
    fn write_on_a_floor(
        &self,
        conn: &mut Connection,
        chan: (bool, &str),
        attr: &str,
        value: i64,
        floor: f64,
    ) -> Result<()> {
        /// The errno a driver answers when a value is outside what it accepts.
        const EINVAL: i64 = -22;
        let attempt = conn.write_attr(&self.phy_id, Some(chan), attr, &value.to_string());
        // Only the floor itself is a candidate: anywhere else in the range an
        // EINVAL means what it says.
        let on_the_floor = (value as f64 - floor).abs() < 1.0;
        match attempt {
            Err(Error::Remote { code: EINVAL, .. }) if on_the_floor => {
                tracing::info!(
                    "PlutoSDR: {attr} {value} was refused as out of range even though the \
                     device published it as the minimum — its true floor is a fraction above \
                     that, so trying {}",
                    value + 1
                );
                conn.write_attr(&self.phy_id, Some(chan), attr, &(value + 1).to_string())
            }
            other => other,
        }
    }

    pub fn sample_rate(&self, conn: &mut Connection) -> Result<f64> {
        read_f64(conn, &self.phy_id, RX_CHAN, "sampling_frequency")
    }

    /// The transmit chain's rate. Normally identical to the receive one — both
    /// hang off the same clock tree — but read rather than assumed, because the
    /// engine sizes its transmit interpolator from this number and a wrong one
    /// puts the signal on the wrong frequency.
    pub fn tx_sample_rate(&self, conn: &mut Connection) -> Result<f64> {
        read_f64(conn, &self.phy_id, TX_CHAN, "sampling_frequency")
    }

    /// Set the analog filter bandwidth on both chains, returning the receive
    /// side's actual value.
    ///
    /// This is load-bearing for more than selectivity: the engine parks the LO
    /// a quarter of a span away from the VFO to keep the signal off a zero-IF
    /// part's DC spike, and that offset is abandoned if the analog filter is
    /// too narrow to pass it. Leaving the AD9361's default here is what makes
    /// the offset silently do nothing.
    pub fn set_bandwidth(&self, conn: &mut Connection, hz: f64) -> Result<f64> {
        let hz = hz.clamp(self.limits.rf_bandwidth_hz.0, self.limits.rf_bandwidth_hz.1);
        let value = format!("{}", hz.round() as i64);
        conn.write_attr(&self.phy_id, Some(RX_CHAN), "rf_bandwidth", &value)?;
        // The transmit filter is a separate register; a failure there must not
        // stop receive from coming up.
        if let Err(e) = conn.write_attr(&self.phy_id, Some(TX_CHAN), "rf_bandwidth", &value) {
            tracing::warn!("PlutoSDR: could not set the transmit filter bandwidth: {e}");
        }
        read_f64(conn, &self.phy_id, RX_CHAN, "rf_bandwidth")
    }

    pub fn set_rx_lo(&self, conn: &mut Connection, hz: f64) -> Result<()> {
        let hz = hz.clamp(self.limits.rx_lo_hz.0, self.limits.rx_lo_hz.1);
        conn.write_attr(&self.phy_id, Some(RX_LO), "frequency", &format!("{}", hz.round() as i64))
    }

    pub fn set_tx_lo(&self, conn: &mut Connection, hz: f64) -> Result<()> {
        let hz = hz.clamp(self.limits.tx_lo_hz.0, self.limits.tx_lo_hz.1);
        conn.write_attr(&self.phy_id, Some(TX_LO), "frequency", &format!("{}", hz.round() as i64))
    }

    /// The one gain-control mode in which the operator, rather than the
    /// AD9361, owns the receive gain register.
    pub const MANUAL_AGC: &'static str = "manual";

    /// `manual`, `slow_attack`, `fast_attack` or `hybrid`. An unknown mode is
    /// refused here rather than on the wire so the message names the modes the
    /// device actually offers.
    pub fn set_agc_mode(&self, conn: &mut Connection, mode: &str) -> Result<()> {
        if !self.limits.agc_modes.iter().any(|m| m == mode) {
            return Err(Error::Unsupported(format!(
                "this Pluto has no AGC mode {mode:?} — it offers {}",
                self.limits.agc_modes.join(", ")
            )));
        }
        conn.write_attr(&self.phy_id, Some(RX_CHAN), "gain_control_mode", mode)
    }

    /// Receive gain in dB — **only writable while the gain-control mode is
    /// `manual`**, which is why `mode` is a parameter rather than something the
    /// caller is trusted to have checked.
    ///
    /// In any attack mode the AD9361 owns this register and the driver answers
    /// `-EOPNOTSUPP` (-95) rather than ignoring the write. Since the default
    /// AGC here is slow attack, sending it unconditionally aborted the open on
    /// every radio that had not been switched to manual — and the workaround,
    /// pinning the AGC to manual, left the receiver at a fixed 40 dB of an
    /// available 71 and looking deaf.
    ///
    /// Skipping is not the same as losing the value: the caller keeps it and
    /// replays it on the way back into manual (see `control_thread`).
    pub fn set_rx_gain(&self, conn: &mut Connection, mode: &str, db: f64) -> Result<()> {
        if mode != Phy::MANUAL_AGC {
            tracing::debug!(
                "PlutoSDR: holding the {db:.1} dB receive gain — the AGC is in {mode}, where \
                 the AD9361 owns that register"
            );
            return Ok(());
        }
        let db = db.clamp(self.limits.rx_gain_db.0, self.limits.rx_gain_db.1);
        conn.write_attr(&self.phy_id, Some(RX_CHAN), "hardwaregain", &format!("{db:.6}"))
    }

    /// Transmit gain in dB — negative, because on the AD9361 this is
    /// attenuation. `0` is full output and `-89.75` is as close to off as the
    /// part gets.
    pub fn set_tx_gain(&self, conn: &mut Connection, db: f64) -> Result<()> {
        let db = db.clamp(self.limits.tx_gain_db.0, self.limits.tx_gain_db.1);
        conn.write_attr(&self.phy_id, Some(TX_CHAN), "hardwaregain", &format!("{db:.6}"))
    }

    /// Put the transmitter at its quietest setting.
    ///
    /// Called before the source is handed to the engine, so that whatever the
    /// configuration says, a first key-up cannot emit unexpected power. The
    /// SoapySDR backend does the same thing for the same reason.
    pub fn silence_transmitter(&self, conn: &mut Connection) -> Result<()> {
        self.set_tx_gain(conn, self.limits.tx_gain_db.0)
    }

    pub fn set_rx_port(&self, conn: &mut Connection, port: &str) -> Result<()> {
        conn.write_attr(&self.phy_id, Some(RX_CHAN), "rf_port_select", port)
    }

    pub fn set_tx_port(&self, conn: &mut Connection, port: &str) -> Result<()> {
        conn.write_attr(&self.phy_id, Some(TX_CHAN), "rf_port_select", port)
    }

    pub fn rx_port(&self, conn: &mut Connection) -> Result<String> {
        conn.read_attr(&self.phy_id, Some(RX_CHAN), "rf_port_select")
    }

    pub fn tx_port(&self, conn: &mut Connection) -> Result<String> {
        conn.read_attr(&self.phy_id, Some(TX_CHAN), "rf_port_select")
    }

    /// Silence the transmit DDS.
    ///
    /// The transmit path can be fed either by a DMA buffer or by four on-chip
    /// tone generators, and the tone generators win by default. Skip this and
    /// the Pluto transmits a carrier pair instead of the modulated signal —
    /// on frequency, at full power, and completely wrong.
    pub fn silence_dds(&self, conn: &mut Connection) -> Result<()> {
        for chan in &self.dds_channels {
            conn.write_attr(&self.tx_buffer_id, Some((true, chan)), "raw", "0")?;
        }
        if self.dds_channels.is_empty() {
            tracing::warn!(
                "PlutoSDR: {TX_BUFFER} published no DDS tone channels to silence — if \
                 transmit puts out a steady carrier instead of the modulation, this is why"
            );
        }
        Ok(())
    }
}

/// The scan elements a buffer is actually opened with: the I/Q pair at scan
/// indices 0 and 1.
///
/// A stock Pluto's buffers carry exactly those two. A 2R2T firmware — a Pluto+,
/// or any AD936x build with both chains compiled in — publishes four, two
/// chains' worth of I and Q, and refusing such a device would be wrong twice
/// over: the first pair is a perfectly good radio, and `OPEN`'s channel mask
/// exists precisely to enable a subset. This driver always opens buffers with
/// mask bits 0 and 1 set, so only the first pair travels the wire and the
/// decoders see exactly the layout returned here; the extra elements stay
/// disabled. The second value is a sentence for the session trace when
/// elements were left disabled, so a report from such a device says which
/// chain it was using.
///
/// What cannot be accepted is a device with fewer than two elements — that
/// cannot carry complex baseband at all — or one whose first pair does not sit
/// at indices 0 and 1, because the mask would then enable something other than
/// what is decoded.
fn iq_pair(name: &str, dev: &Device) -> Result<(Vec<ScanFormat>, Option<String>)> {
    let scans = dev.scan_channels();
    if scans.len() < 2 {
        return Err(Error::Unsupported(format!(
            "{name} has {} scan element(s), not the I and Q an I/Q stream needs",
            scans.len()
        )));
    }
    let indices = (
        scans[0].scan.expect("scan_channels keeps only scan elements").0,
        scans[1].scan.expect("scan_channels keeps only scan elements").0,
    );
    if indices != (0, 1) {
        return Err(Error::Unsupported(format!(
            "{name}'s first scan elements sit at indices {} and {}, not 0 and 1 — the \
             channel mask this driver opens buffers with (bits 0 and 1) would not select \
             them",
            indices.0, indices.1
        )));
    }
    let note = (scans.len() > 2).then(|| {
        let disabled: Vec<&str> = scans[2..].iter().map(|c| c.id.as_str()).collect();
        format!(
            "{name} has {} scan elements — a dual-channel (2R2T) firmware such as a \
             Pluto+. Enabling only the first I/Q pair ({} and {}) and leaving {} disabled",
            scans.len(),
            scans[0].id,
            scans[1].id,
            disabled.join(", ")
        )
    });
    Ok((scans.iter().take(2).map(|c| c.scan.expect("checked above").1).collect(), note))
}

// ---- attribute helpers --------------------------------------------------

fn read_f64(conn: &mut Connection, dev: &str, chan: (bool, &str), attr: &str) -> Result<f64> {
    let text = conn.read_attr(dev, Some(chan), attr)?;
    // Values arrive as bare numbers, sometimes with a unit ("71.000000 dB").
    let head = text.split_whitespace().next().unwrap_or("");
    head.parse()
        .map_err(|_| Error::Msg(format!("{dev} {} {attr} answered {text:?}, not a number", chan.1)))
}

/// Read an IIO `[min step max]` range, or `None` when the device does not
/// publish that attribute at all.
fn read_range(
    conn: &mut Connection,
    dev_id: &str,
    dev: &crate::context::Device,
    chan: (bool, &str),
    attr: &str,
) -> Result<Option<(f64, f64, f64)>> {
    if !dev.channel(chan.1, chan.0).is_some_and(|c| c.has_attr(attr)) {
        return Ok(None);
    }
    let text = conn.read_attr(dev_id, Some(chan), attr)?;
    Ok(parse_range(&text))
}

/// A space-separated list attribute (`gain_control_mode_available`,
/// `rf_port_select_available`), or empty when absent.
fn read_list(
    conn: &mut Connection,
    dev_id: &str,
    dev: &crate::context::Device,
    chan: (bool, &str),
    attr: &str,
) -> Result<Vec<String>> {
    if !dev.channel(chan.1, chan.0).is_some_and(|c| c.has_attr(attr)) {
        return Ok(Vec::new());
    }
    let text = conn.read_attr(dev_id, Some(chan), attr)?;
    Ok(text.split_whitespace().map(str::to_string).collect())
}

/// Parse IIO's `[min step max]` range spelling into `(min, step, max)`.
///
/// The brackets are part of the value, and the fields are in that order — not
/// min/max/step, which is the natural reading and the wrong one.
fn parse_range(text: &str) -> Option<(f64, f64, f64)> {
    let inner = text.trim().trim_start_matches('[').trim_end_matches(']');
    let mut it = inner.split_whitespace();
    let min: f64 = it.next()?.parse().ok()?;
    let step: f64 = it.next()?.parse().ok()?;
    let max: f64 = it.next()?.parse().ok()?;
    if !min.is_finite() || !step.is_finite() || !max.is_finite() || max < min {
        return None;
    }
    Some((min, step, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Channel;

    /// A capture/DMA buffer device whose scan elements sit at `indices`.
    fn buffer(indices: &[u32]) -> Device {
        Device {
            id: "iio:device4".into(),
            name: "cf-ad9361-lpc".into(),
            attrs: Vec::new(),
            channels: indices
                .iter()
                .map(|&i| Channel {
                    id: format!("voltage{i}"),
                    name: None,
                    output: false,
                    attrs: Vec::new(),
                    scan: Some((i, ScanFormat::parse("le:s12/16>>0").expect("format"))),
                })
                .collect(),
        }
    }

    #[test]
    fn a_stock_pluto_buffer_is_its_own_iq_pair() {
        let (scan, note) = iq_pair("cf-ad9361-lpc", &buffer(&[0, 1])).expect("pair");
        assert_eq!(scan.len(), 2);
        assert!(note.is_none(), "a stock buffer needs no explaining: {note:?}");
    }

    /// A Pluto+ runs 2R2T firmware: four scan elements, two chains' worth of
    /// I/Q. Refusing it was the bug — the first pair is a perfectly good radio,
    /// and the `OPEN` mask (bits 0 and 1) enables exactly that pair.
    #[test]
    fn a_2r2t_buffer_uses_the_first_pair_and_notes_the_rest() {
        let (scan, note) = iq_pair("cf-ad9361-lpc", &buffer(&[0, 1, 2, 3])).expect("pair");
        assert_eq!(scan.len(), 2);
        let note = note.expect("a disabled pair is worth a line in the report");
        assert!(note.contains("4 scan elements"), "{note}");
        assert!(note.contains("voltage0 and voltage1"), "{note}");
        assert!(note.contains("voltage2, voltage3"), "{note}");
    }

    #[test]
    fn too_few_scan_elements_are_refused() {
        let err = iq_pair("cf-ad9361-lpc", &buffer(&[0])).expect_err("one element");
        assert!(err.to_string().contains("1 scan element"), "{err}");
    }

    /// The mask `open_buffer` sends is literally bits 0 and 1; a device whose
    /// first pair sits elsewhere would stream elements we would mis-decode.
    #[test]
    fn a_pair_off_indices_zero_and_one_is_refused() {
        let err = iq_pair("cf-ad9361-lpc", &buffer(&[2, 3])).expect_err("wrong indices");
        assert!(err.to_string().contains("2 and 3"), "{err}");
    }

    #[test]
    fn ranges_are_min_step_max_in_that_order() {
        // An AD9364-unlocked Pluto's tuning range.
        assert_eq!(parse_range("[70000000 1 6000000000]"), Some((70e6, 1.0, 6e9)));
        // Transmit attenuation, as gain.
        let (min, step, max) = parse_range("[-89.750000 0.250000 0.000000]").expect("tx range");
        assert_eq!((min, step, max), (-89.75, 0.25, 0.0));
        // Whitespace and missing brackets are both survivable.
        assert_eq!(parse_range(" 0 1 71 "), Some((0.0, 1.0, 71.0)));
        assert_eq!(parse_range("[0 1 71]"), Some((0.0, 1.0, 71.0)));
        // Anything else is "the device did not say", not a panic.
        assert_eq!(parse_range(""), None);
        assert_eq!(parse_range("[0 1]"), None);
        assert_eq!(parse_range("slow_attack fast_attack"), None);
        // A backwards range is a firmware bug, not a limit to obey.
        assert_eq!(parse_range("[100 1 10]"), None);
    }

    #[test]
    fn the_fallback_follows_the_part_the_model_string_names() {
        assert_eq!(PlutoLimits::fallback(false).rx_lo_hz, AD9363_LO_HZ);
        assert_eq!(PlutoLimits::fallback(true).rx_lo_hz, AD9364_LO_HZ);
    }

    /// A guessed limit has to announce itself: the operator otherwise reads
    /// "325–3800 MHz" as something the radio said about itself.
    #[test]
    fn assumed_limits_produce_a_notice_and_known_ones_do_not() {
        let mut l = PlutoLimits::fallback(false);
        assert!(l.assumption_notice().is_none());
        l.assumed = vec!["a tuning range".into()];
        let notice = l.assumption_notice().expect("notice");
        assert!(notice.contains("a tuning range"), "{notice}");
        assert!(notice.contains("assuming"), "{notice}");
    }
}
