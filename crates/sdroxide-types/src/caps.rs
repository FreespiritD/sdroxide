use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    Rx,
    Tx,
}

/// One adjustable gain stage exposed by the device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GainElement {
    pub name: String,
    pub direction: Direction,
    pub min_db: f64,
    pub max_db: f64,
    pub step_db: f64,
}

/// Device capabilities probed once at open time. Drives all UI adaptation
/// (e.g. `tx_channels == 0` hides every TX control).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeviceCaps {
    pub driver: String,
    pub label: String,

    pub rx_channels: usize,
    pub tx_channels: usize,
    /// Whether RX keeps running during TX. Conservative default: false.
    pub full_duplex: bool,
    /// The source delivers already-demodulated real audio (a CAT rig on a
    /// sound card), so the engine bypasses the DDC/demod chain and shows a
    /// narrow audio-band panadapter. Sound-card *IQ* leaves this `false` and
    /// runs the normal wideband path.
    pub audio_mode: bool,
    /// Transmit by sending raw 48 kHz audio (`IqSource::tx_write_audio`) — the
    /// device modulates it — instead of modulated IQ, even when RX is wideband
    /// IQ (`audio_mode = false`). Used by the TCI backend (IQ RX, audio TX).
    pub tx_audio: bool,

    /// Tunable ranges in Hz: (min, max).
    pub freq_ranges_rx: Vec<(f64, f64)>,
    pub freq_ranges_tx: Vec<(f64, f64)>,

    /// Discrete supported rates, if the device reports any.
    pub sample_rates: Vec<f64>,
    /// Continuous rate ranges: (min, max).
    pub rate_ranges: Vec<(f64, f64)>,

    pub gains: Vec<GainElement>,
    pub antennas_rx: Vec<String>,
    pub antennas_tx: Vec<String>,

    /// Sensor names from the SoapySDR sensor API (device- and channel-level).
    pub sensors: Vec<String>,
    pub has_swr_sensor: bool,
    pub has_fwd_power_sensor: bool,
    /// This front end's receive LO is shared with sibling streams on the same
    /// physical device (the AD9361's two receive chains have one
    /// synthesiser): retuning this radio moves the others, and theirs moves
    /// this one — the engine adopts such moves as centre changes. Appended
    /// last (postcard layout; `PROTO_VERSION` bumped with it).
    #[serde(default)]
    pub shared_lo_rx: bool,
}

impl DeviceCaps {
    pub fn is_transmit_capable(&self) -> bool {
        self.tx_channels > 0
    }

    /// Whether a *published* receive range covers `hz`. A device that publishes
    /// none can reach nothing by this answer, so anything deciding whether to
    /// allow or offer a frequency wants [`Self::may_rx_hz`] instead.
    pub fn can_rx_hz(&self, hz: f64) -> bool {
        self.freq_ranges_rx.iter().any(|&(lo, hi)| hz >= lo && hz <= hi)
    }

    /// Whether a *published* transmit range covers `hz`. As with
    /// [`Self::can_rx_hz`], "no ranges" reads as "nothing" here; the gate that
    /// decides whether a key-down is allowed is [`Self::may_tx_hz`].
    pub fn can_tx_hz(&self, hz: f64) -> bool {
        self.freq_ranges_tx.iter().any(|&(lo, hi)| hz >= lo && hz <= hi)
    }

    /// Whether receiving here is permitted: inside a published range, or
    /// anywhere at all on a device that publishes no ranges.
    ///
    /// Publishing a tuning range is optional in SoapySDR — `getFrequencyRange`
    /// has no meaningful default and plenty of drivers never implement it — so
    /// an empty list means "this driver didn't say", not "this radio tunes
    /// nowhere". Taking silence as a prohibition would leave such a device
    /// unable to do the thing it is demonstrably doing.
    pub fn may_rx_hz(&self, hz: f64) -> bool {
        self.freq_ranges_rx.is_empty() || self.can_rx_hz(hz)
    }

    /// Whether transmitting here is permitted, by the same rule as
    /// [`Self::may_rx_hz`]: a driver that publishes no transmit range is taken
    /// at its word rather than silenced.
    ///
    /// This is not the only thing between an operator and the antenna —
    /// [`Self::is_transmit_capable`] has already established there is a
    /// transmitter, the amateur-band gate still applies, and the driver may
    /// still refuse the tune. An operator who wants a firmer limit than the
    /// driver gives can state one: see `RadioConfig::freq_ranges_tx`.
    pub fn may_tx_hz(&self, hz: f64) -> bool {
        self.freq_ranges_tx.is_empty() || self.can_tx_hz(hz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case this rule exists for: a driver (SoapySX among others) that
    /// implements neither frequency-range call. Its ranges arrive empty, and
    /// every empty list has to read as "unknown", never as "nothing" — the
    /// device receives and transmits perfectly well.
    #[test]
    fn a_device_that_publishes_no_ranges_may_still_receive_and_transmit() {
        let silent = DeviceCaps { rx_channels: 1, tx_channels: 1, ..Default::default() };
        for hz in [1_800_000.0, 145_500_000.0, 435_000_000.0] {
            assert!(silent.may_rx_hz(hz), "receive at {hz} Hz");
            assert!(silent.may_tx_hz(hz), "transmit at {hz} Hz");
            // The strict form still answers "not in any published range", which
            // is what it is for.
            assert!(!silent.can_rx_hz(hz));
            assert!(!silent.can_tx_hz(hz));
        }
    }

    /// A device that does publish its ranges is held to them.
    #[test]
    fn published_ranges_are_still_enforced() {
        let caps = DeviceCaps {
            rx_channels: 1,
            tx_channels: 1,
            freq_ranges_rx: vec![(100_000.0, 148_000_000.0)],
            freq_ranges_tx: vec![(1_800_000.0, 54_000_000.0)],
            ..Default::default()
        };
        assert!(caps.may_rx_hz(145_000_000.0));
        assert!(!caps.may_rx_hz(435_000_000.0));
        assert!(caps.may_tx_hz(14_200_000.0));
        assert!(!caps.may_tx_hz(145_000_000.0), "outside the transmit range and inside the RX one");
        // Edges are inclusive, both ends.
        assert!(caps.may_tx_hz(1_800_000.0) && caps.may_tx_hz(54_000_000.0));
    }
}
