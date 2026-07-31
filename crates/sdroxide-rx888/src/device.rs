//! Bringing a programmed receiver up: sample clock, front-end GPIO, and the two
//! analogue gain stages.
//!
//! Everything here runs on the thread that owns the [`UsbDev`] — see the
//! invariant in [`crate::usb`].

use std::time::Duration;

use nusb::transfer::TransferError;

use crate::error::{Error, Result};
use crate::protocol::{Cmd, STATS_LEN, Stats, Version, arg, gpio};
use crate::usb::{self, UsbDev};

/// Default ADC clock. 64.8 Msps covers 0–32.4 MHz, needs 129.6 MB/s, and leaves
/// enough SuperSpeed headroom to be reliable on a shared bus. 129.6 Msps is
/// offered but is not the default, for the same reason the UI labels the
/// RTL-SDR's 3.2 Msps as the rate that drops samples.
pub const DEFAULT_ADC_HZ: f64 = 64_800_000.0;

/// Sample rates offered in the UI. The Si5351 will synthesise others, but these
/// are the ones in common use on this board.
pub const ADC_RATES: &[f64] = &[16_200_000.0, 32_400_000.0, 64_800_000.0, 129_600_000.0];

/// The LTC2208's specified ceiling.
const MAX_ADC_HZ: f64 = 130_000_000.0;
/// Below this the Si5351 and the ADC both misbehave, and the coverage is not
/// worth having anyway.
const MIN_ADC_HZ: f64 = 4_000_000.0;

/// Gain element names, shared with the settings UI.
pub const ATT_ELEMENT: &str = "ATT";
pub const VGA_ELEMENT: &str = "VGA";

/// PE4312 step attenuator: 0..=63 in 0.5 dB steps.
const ATT_MAX_CODE: u8 = 63;
pub const ATT_MIN_DB: f64 = -31.5;
pub const ATT_MAX_DB: f64 = 0.0;
pub const ATT_STEP_DB: f64 = 0.5;

/// How the receiver should be set up.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub serial: Option<String>,
    pub adc_rate_hz: f64,
    /// LTC2208 dither: costs a little noise floor, buys spurious-free dynamic
    /// range. Off by default, matching the upstream host software.
    pub dither: bool,
    /// LTC2208 output randomizer. On by default — it keeps the digital bus from
    /// radiating into the front end, and undoing it costs one XOR per sample.
    pub randomize: bool,
    /// Power the HF antenna port. Off by default: switching phantom power onto
    /// someone's antenna without being asked is not a good default.
    pub bias_tee_hf: bool,
    /// Select the ADC's wider 2.25 Vp-p input range. Despite the GPIO bit's
    /// name this is a range select, not a preamp — see [`gpio::PGA_EN`]. On by
    /// default for the headroom.
    pub pga: bool,
    /// Attenuation as a gain, i.e. -31.5..=0 dB.
    pub attenuator_db: f64,
    /// AD8370 VGA gain in dB.
    pub vga_db: f64,
    /// Reference trim, parts per million.
    pub ppm: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            serial: None,
            adc_rate_hz: DEFAULT_ADC_HZ,
            dither: false,
            randomize: true,
            bias_tee_hf: false,
            pga: true,
            attenuator_db: 0.0,
            vga_db: 12.0,
            ppm: 0.0,
        }
    }
}

/// An opened, configured receiver.
pub struct Device {
    usb: UsbDev,
    /// The authoritative GPIO word. The firmware keeps no shadow of its own and
    /// applies whatever it is sent, so every bit must be re-stated on every
    /// write — which means exactly one place may own this value.
    gpio_word: u32,
    adc_rate_hz: f64,
    randomize: bool,
    version: Option<Version>,
    /// Set when the link is too slow for the requested rate, for
    /// `IqSource::open_status`.
    warning: Option<String>,
    streaming: bool,
}

impl Device {
    /// Open, upload firmware if needed, and apply `settings`.
    pub fn open(settings: &Settings, firmware_override: Option<&[u8]>) -> Result<Device> {
        let usb = usb::open(settings.serial.as_deref(), firmware_override)?;
        let version = usb.vendor_in(Cmd::TestFx3, 0, 0, 4).ok().and_then(|b| Version::parse(&b));
        if let Some(v) = version {
            tracing::info!("RX-888 firmware {v}");
        }

        let (adc_rate_hz, warning) = clamp_rate_to_link(settings.adc_rate_hz, &usb);

        let mut dev = Device {
            usb,
            gpio_word: 0,
            adc_rate_hz,
            randomize: settings.randomize,
            version,
            warning,
            streaming: false,
        };

        dev.apply_gpio(settings)?;
        dev.set_attenuator_db(settings.attenuator_db)?;
        dev.set_vga_db(settings.vga_db)?;
        dev.set_adc_rate(adc_rate_hz, settings.ppm)?;
        Ok(dev)
    }

    pub fn label(&self) -> &str {
        self.usb.label()
    }

    pub fn serial(&self) -> Option<&str> {
        self.usb.serial()
    }

    pub fn version(&self) -> Option<Version> {
        self.version
    }

    /// The ADC clock, which is also the real-sample rate.
    pub fn adc_rate_hz(&self) -> f64 {
        self.adc_rate_hz
    }

    /// Whether the ADC is scrambling its output, so the converter knows whether
    /// to undo it.
    pub fn randomized(&self) -> bool {
        self.randomize
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn usb(&self) -> &UsbDev {
        &self.usb
    }

    /// Program the Si5351. `ppm` trims the reference.
    pub fn set_adc_rate(&mut self, hz: f64, ppm: f64) -> Result<()> {
        let hz = hz.clamp(MIN_ADC_HZ, MAX_ADC_HZ);
        let trimmed = hz * (1.0 + ppm * 1e-6);
        let word = trimmed.round() as u32;
        tracing::info!("RX-888: ADC clock {:.6} MHz", f64::from(word) / 1e6);
        self.usb.vendor_out(Cmd::StartAdc, 0, 0, &word.to_le_bytes())?;
        self.adc_rate_hz = trimmed;
        Ok(())
    }

    /// Rebuild and send the whole GPIO word from `settings`.
    pub fn apply_gpio(&mut self, settings: &Settings) -> Result<()> {
        let mut w = 0u32;
        // SHDWN stays clear: setting it powers the front end down.
        if settings.dither {
            w |= gpio::DITH;
        }
        if settings.randomize {
            w |= gpio::RANDO;
        }
        if settings.bias_tee_hf {
            w |= gpio::BIAS_HF;
        }
        if settings.pga {
            w |= gpio::PGA_EN;
        }
        // HF-only for now, so the VHF front end stays out of circuit.
        w &= !gpio::VHF_EN;
        self.randomize = settings.randomize;
        self.write_gpio(w)
    }

    /// Turn the front-panel LED on or off, leaving every other bit alone.
    pub fn set_led(&mut self, on: bool) -> Result<()> {
        let w = if on { self.gpio_word | gpio::LED_BLUE } else { self.gpio_word & !gpio::LED_BLUE };
        self.write_gpio(w)
    }

    pub fn set_bias_tee(&mut self, on: bool) -> Result<()> {
        let w = if on { self.gpio_word | gpio::BIAS_HF } else { self.gpio_word & !gpio::BIAS_HF };
        self.write_gpio(w)
    }

    pub fn set_dither(&mut self, on: bool) -> Result<()> {
        let w = if on { self.gpio_word | gpio::DITH } else { self.gpio_word & !gpio::DITH };
        self.write_gpio(w)
    }

    /// Changing randomization mid-stream would desynchronise the converter from
    /// the ADC, so this reports the new state for the caller to act on.
    pub fn set_randomize(&mut self, on: bool) -> Result<()> {
        let w = if on { self.gpio_word | gpio::RANDO } else { self.gpio_word & !gpio::RANDO };
        self.write_gpio(w)?;
        self.randomize = on;
        Ok(())
    }

    pub fn set_pga(&mut self, on: bool) -> Result<()> {
        let w = if on { self.gpio_word | gpio::PGA_EN } else { self.gpio_word & !gpio::PGA_EN };
        self.write_gpio(w)
    }

    fn write_gpio(&mut self, word: u32) -> Result<()> {
        tracing::debug!("RX-888: GPIO <- 0x{word:08x}");
        self.usb.vendor_out(Cmd::GpioFx3, 0, 0, &word.to_le_bytes())?;
        self.gpio_word = word;
        Ok(())
    }

    /// The live GPIO word as this driver believes it to be.
    pub fn gpio_word(&self) -> u32 {
        self.gpio_word
    }

    /// Set the step attenuator. `db` is a gain, so -31.5..=0.
    ///
    /// Returns the attenuation actually programmed, which is `db` rounded to the
    /// hardware's 0.5 dB grid.
    pub fn set_attenuator_db(&mut self, db: f64) -> Result<f64> {
        let code = att_code_for(db);
        self.set_arg(arg::DAT31_ATT, code)?;
        let achieved = att_db_for(code);
        tracing::debug!("RX-888: attenuator {achieved:+.1} dB (code {code})");
        Ok(achieved)
    }

    /// Set the AD8370 VGA. Returns the gain actually programmed.
    pub fn set_vga_db(&mut self, db: f64) -> Result<f64> {
        let code = ad8370_code_for(db);
        self.set_arg(arg::AD8370_VGA, code)?;
        let achieved = ad8370_db_for(code);
        tracing::debug!("RX-888: VGA {achieved:+.1} dB (code 0x{code:02x})");
        Ok(achieved)
    }

    /// Send one `SETARGFX3` parameter.
    ///
    /// # The value travels in `wValue`, not in the data phase
    ///
    /// The published API reference describes this command as "OUT, n bytes",
    /// which reads as though the parameter is the payload. It is not: the
    /// firmware's handler is `rx888r2_SetGain(wValue)`, and the payload it reads
    /// with `CyU3PUsbGetEP0Data` is discarded. Putting the value in the payload
    /// therefore sets the parameter to **zero** — and since code 0 mutes the
    /// AD8370 outright, the symptom is a receiver that streams perfectly and
    /// hears nothing, at every gain setting, with no error anywhere.
    ///
    /// A one-byte payload is still sent so the data phase the firmware expects
    /// is well formed.
    fn set_arg(&self, index: u16, value: u8) -> Result<()> {
        self.usb.vendor_out(Cmd::SetArgFx3, u16::from(value), index, &[value])
    }

    /// Start the GPIF engine. Samples begin arriving on the bulk endpoint.
    ///
    /// # Why the status stage is allowed to go missing
    ///
    /// `STARTFX3` does not poke a register — it rebuilds the FX3's DMA channels
    /// and restarts the GPIF state machine, and firmware 2.4 does not answer the
    /// control transfer while that is happening. On the hardware this was
    /// developed against the request never completes at all: nusb gives up after
    /// its timeout and reports `Cancelled` (usbfs maps `ETIMEDOUT` onto it), and
    /// yet the receiver is streaming perfectly by the time the call returns.
    ///
    /// Treating that as a failure means refusing to stream from a device that is
    /// already streaming, so a timeout here is logged and accepted. Whether the
    /// command worked is decided by the only thing that actually settles it —
    /// samples arriving on the bulk endpoint.
    pub fn start(&mut self) -> Result<()> {
        // Deliberately short. Since the status stage is never coming, every
        // millisecond here is dead air before the first transfer is submitted —
        // and it is charged to the operator as a slow start, or, if they happen
        // to be measuring throughput over a short window, as lost samples. Two
        // seconds of it made a perfectly healthy receiver look like it was
        // running at 67 % of its sample rate.
        const START_TIMEOUT: Duration = Duration::from_millis(150);
        match self.usb.vendor_out_timeout(Cmd::StartFx3, 0, 0, &[], START_TIMEOUT) {
            Ok(()) => {}
            Err(Error::Transfer { source: TransferError::Cancelled, .. }) => {
                tracing::debug!(
                    "RX-888: STARTFX3 returned no status stage (expected on firmware 2.4)"
                );
            }
            Err(e) => return Err(e),
        }
        self.streaming = true;
        let _ = self.set_led(true);
        Ok(())
    }

    /// Halt the GPIF engine.
    pub fn stop(&mut self) -> Result<()> {
        let _ = self.set_led(false);
        self.usb.vendor_out(Cmd::StopFx3, 0, 0, &[])?;
        self.streaming = false;
        Ok(())
    }

    /// Read the firmware's health counters.
    pub fn stats(&self) -> Result<Stats> {
        let b = self.usb.vendor_in(Cmd::GetStats, 0, 0, STATS_LEN)?;
        Stats::parse(&b).ok_or_else(|| {
            Error::Unsupported(format!("GETSTATS returned {} bytes, too short to decode", b.len()))
        })
    }

    /// Best-effort quiesce, for the drop path.
    pub fn shutdown(&mut self) {
        if self.streaming {
            let _ = self.stop();
        }
        let _ = self.set_led(false);
    }
}

/// Clamp the requested ADC rate to what the negotiated link can carry, and say
/// so if it had to.
///
/// The failure this prevents is specific and confusing: the receiver enumerates,
/// the firmware runs, the ADC clocks, and samples simply go missing, which looks
/// like a broken radio rather than a slow cable.
fn clamp_rate_to_link(requested: f64, usb: &UsbDev) -> (f64, Option<String>) {
    let budget = usb::usable_bytes_per_sec(usb.speed());
    let max_rate = budget / 2.0; // 16-bit real samples
    if requested <= max_rate {
        return (requested, None);
    }
    let clamped = ADC_RATES.iter().copied().filter(|r| *r <= max_rate).fold(MIN_ADC_HZ, f64::max);
    let msg = format!(
        "the RX-888 is on a {} link, which can only carry about {:.1} Msps — \
         the sample rate has been reduced to {:.1} Msps ({:.1} MHz of spectrum). \
         Use a USB 3 cable directly into a USB 3 port for the full rate.",
        usb::speed_name(usb.speed()),
        max_rate / 1e6,
        clamped / 1e6,
        clamped / 2e6,
    );
    tracing::warn!("{msg}");
    (clamped, Some(msg))
}

// ---- gain mappings -----------------------------------------------------

/// Attenuator code for a requested gain in dB (negative = attenuation).
fn att_code_for(db: f64) -> u8 {
    let atten = (-db).clamp(0.0, -ATT_MIN_DB);
    (atten / ATT_STEP_DB).round().clamp(0.0, ATT_MAX_CODE as f64) as u8
}

/// The gain a given attenuator code produces.
fn att_db_for(code: u8) -> f64 {
    -(code.min(ATT_MAX_CODE) as f64) * ATT_STEP_DB
}

/// The AD8370's linear voltage vernier, per code step, in the low range.
const AD8370_VERNIER: f64 = 0.055;
/// How much the high range multiplies it by.
const AD8370_PREGAIN: f64 = 7.079;
/// Bit 7 of the register selects the high range.
const AD8370_HIGH: u8 = 0x80;

/// The gain a given AD8370 register value produces.
///
/// `A_v = code · vernier · (high ? pregain : 1)`, straight from the datasheet's
/// transfer function. Code 0 mutes the part, which has no dB value; it is
/// reported as a large negative number so callers can still order gains.
pub fn ad8370_db_for(reg: u8) -> f64 {
    let code = (reg & !AD8370_HIGH) as f64;
    if code == 0.0 {
        return f64::NEG_INFINITY;
    }
    let av = code * AD8370_VERNIER * if reg & AD8370_HIGH != 0 { AD8370_PREGAIN } else { 1.0 };
    20.0 * av.log10()
}

/// The register value that comes closest to `db`.
///
/// Both ranges overlap across the middle of the span, so the choice is made by
/// which one lands nearer the request — preferring the low range on a tie, since
/// it reaches a given gain with less pre-gain ahead of the vernier.
pub fn ad8370_code_for(db: f64) -> u8 {
    let av = 10f64.powf(db / 20.0);
    let pick = |scale: f64| -> u8 { (av / scale).round().clamp(1.0, 127.0) as u8 };

    let low = pick(AD8370_VERNIER);
    let high = pick(AD8370_VERNIER * AD8370_PREGAIN) | AD8370_HIGH;

    let err = |reg: u8| (ad8370_db_for(reg) - db).abs();
    if err(low) <= err(high) { low } else { high }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attenuator_codes_span_the_documented_range() {
        assert_eq!(att_code_for(0.0), 0);
        assert_eq!(att_code_for(-31.5), 63);
        // Beyond the hardware's reach in both directions.
        assert_eq!(att_code_for(10.0), 0);
        assert_eq!(att_code_for(-99.0), 63);
        // The 0.5 dB grid.
        assert_eq!(att_code_for(-10.0), 20);
        assert_eq!(att_code_for(-10.2), 20);
        assert_eq!(att_db_for(20), -10.0);
    }

    #[test]
    fn attenuator_round_trips_on_its_own_grid() {
        for code in 0..=ATT_MAX_CODE {
            assert_eq!(att_code_for(att_db_for(code)), code, "code {code}");
        }
    }

    #[test]
    fn ad8370_reproduces_the_datasheet_range_endpoints() {
        // Low range tops out near +17 dB, high range near +34 dB — the two
        // figures the datasheet quotes.
        let low_max = ad8370_db_for(127);
        let high_max = ad8370_db_for(127 | AD8370_HIGH);
        assert!((low_max - 16.9).abs() < 0.3, "low range max was {low_max}");
        assert!((high_max - 33.9).abs() < 0.3, "high range max was {high_max}");
        // The high range sits 17 dB above the low one at the same code.
        let step = ad8370_db_for(64 | AD8370_HIGH) - ad8370_db_for(64);
        assert!((step - 17.0).abs() < 0.1, "range step was {step}");
    }

    #[test]
    fn ad8370_gain_is_monotonic_within_each_range() {
        for range in [0u8, AD8370_HIGH] {
            let mut prev = f64::NEG_INFINITY;
            for code in 1..=127u8 {
                let db = ad8370_db_for(code | range);
                assert!(db > prev, "code {code} range {range:#x} went backwards");
                prev = db;
            }
        }
    }

    #[test]
    fn ad8370_codes_land_close_to_what_was_asked_for() {
        // Across the usable span, the chosen code should be within half a step
        // of the request. The vernier is linear in voltage, so the dB step grows
        // as gain falls; 1 dB is a fair bound above -6 dB.
        for tenth in -60..=339 {
            let want = tenth as f64 / 10.0;
            let got = ad8370_db_for(ad8370_code_for(want));
            assert!((got - want).abs() <= 1.0, "asked {want:.1} dB, got {got:.1} dB");
        }
    }

    #[test]
    fn ad8370_uses_the_high_range_only_where_the_low_one_cannot_reach() {
        // Above the low range's ceiling there is no choice.
        assert_eq!(ad8370_code_for(30.0) & AD8370_HIGH, AD8370_HIGH);
        // Well inside the low range, it should not reach for the pre-gain.
        assert_eq!(ad8370_code_for(0.0) & AD8370_HIGH, 0);
        assert_eq!(ad8370_code_for(10.0) & AD8370_HIGH, 0);
    }

    #[test]
    fn a_muted_vga_has_no_finite_gain() {
        assert_eq!(ad8370_db_for(0), f64::NEG_INFINITY);
        // ...and is never chosen for a real request.
        assert!(ad8370_code_for(-40.0) >= 1);
    }

    #[test]
    fn the_default_rate_is_one_of_the_offered_rates() {
        assert!(ADC_RATES.contains(&DEFAULT_ADC_HZ));
        assert!(ADC_RATES.iter().all(|r| *r >= MIN_ADC_HZ && *r <= MAX_ADC_HZ));
    }
}
