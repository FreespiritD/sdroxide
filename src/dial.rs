//! The one frequency control a rig with no DDC has, and the four things that
//! have to take turns on it.
//!
//! A CAT-controlled transceiver and a networked Icom are the same shape here:
//! the radio tunes, sdroxide does not, so VFO, RIT, XIT and split cannot be
//! software offsets the way they are on an SDR. They are all the same knob, and
//! the rules for sharing it are an interlock rather than arithmetic — which is
//! why this lives on its own, where it can be tested without a serial port or a
//! socket.

/// Where the rig's dial has to be, and who currently owns it.
///
/// A CAT rig has exactly one frequency control and no DDC behind it, so the VFO,
/// RIT, XIT and split all have to take turns on the dial: it sits on the receive
/// frequency (VFO + RIT) while receiving, and an over that transmits somewhere
/// else — XIT, or split onto the other VFO — borrows it until unkey. That makes
/// the dial an interlock rather than a number, so it lives here where it can be
/// tested without a serial port. Each method returns the frequency to command,
/// or `None` when the dial should stay where it is.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Dial {
    /// The operator's VFO — the dial with RIT taken back out.
    pub(crate) vfo: f64,
    /// How much RIT the dial carries while receiving (0 when RIT is off).
    rit: f64,
    /// Where an over has parked the dial, or `None` while receiving.
    tx: Option<f64>,
}

impl Dial {
    /// A dial parked on `vfo`, with no RIT and no over in progress.
    pub(crate) fn at(vfo: f64) -> Dial {
        Dial { vfo, ..Dial::default() }
    }

    /// Where the dial belongs while receiving.
    pub(crate) fn rx_hz(&self) -> f64 {
        self.vfo + self.rit
    }

    /// Move the VFO. Nothing is commanded while an over owns the dial —
    /// retuning then would drag the transmitter off the frequency it was
    /// cleared to use; [`Self::end_tx`] picks the new VFO up on unkey.
    pub(crate) fn set_vfo(&mut self, hz: f64) -> Option<f64> {
        self.vfo = hz;
        self.tx.is_none().then(|| self.rx_hz())
    }

    /// Change the RIT offset (0 = RIT off). Same deferral as [`Self::set_vfo`].
    pub(crate) fn set_rit(&mut self, hz: f64) -> Option<f64> {
        if self.rit == hz {
            return None;
        }
        self.rit = hz;
        self.tx.is_none().then(|| self.rx_hz())
    }

    /// Take the dial for an over on `tx_hz`. `None` when transmit already lands
    /// where we listen (no split, no XIT), so the common case costs no retune.
    pub(crate) fn begin_tx(&mut self, tx_hz: f64) -> Option<f64> {
        if (tx_hz - self.rx_hz()).abs() < 1.0 {
            return None;
        }
        self.tx = Some(tx_hz);
        Some(tx_hz)
    }

    /// Give the dial back, including any retune deferred during the over.
    /// `None` when the over never moved it.
    pub(crate) fn end_tx(&mut self) -> Option<f64> {
        self.tx.take().map(|_| self.rx_hz())
    }

    /// Fold a dial frequency the *rig* reported back into the VFO, returning the
    /// operator's new VFO. `None` while an over owns the dial: what the rig
    /// reports then is our own transmit frequency, which says nothing about
    /// where the operator wants to listen.
    pub(crate) fn report(&mut self, dial_hz: f64) -> Option<f64> {
        if self.tx.is_some() {
            return None;
        }
        self.vfo = dial_hz - self.rit;
        Some(self.vfo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(vfo: f64) -> Dial {
        Dial { vfo, ..Dial::default() }
    }

    #[test]
    fn rit_rides_on_the_dial_and_comes_back_out_of_what_the_rig_reports() {
        let mut d = at(14_074_000.0);
        // RIT on: the dial moves, the VFO does not.
        assert_eq!(d.set_rit(700.0), Some(14_074_700.0));
        assert_eq!(d.vfo, 14_074_000.0);
        // The rig echoes that same dial back on its next poll. Folding the
        // offset out has to land on the VFO we started from — otherwise every
        // poll would walk the VFO up by the RIT offset.
        assert_eq!(d.report(14_074_700.0), Some(14_074_000.0));
        // The operator turns the rig's own dial 1 kHz up: their VFO moved with
        // it, and RIT still sits on top.
        assert_eq!(d.report(14_075_700.0), Some(14_075_000.0));
        assert_eq!(d.rx_hz(), 14_075_700.0);
        // Clearing RIT puts the dial back on the VFO.
        assert_eq!(d.set_rit(0.0), Some(14_075_000.0));
        // Re-asserting the same offset is not a retune.
        assert_eq!(d.set_rit(0.0), None);
    }

    #[test]
    fn transmitting_where_we_listen_never_touches_the_dial() {
        let mut d = at(14_074_000.0);
        assert_eq!(d.begin_tx(14_074_000.0), None, "no split, no XIT: nothing to do");
        assert_eq!(d.end_tx(), None);
        // Sub-hertz differences are rounding, not an offset worth a CAT write.
        assert_eq!(d.begin_tx(14_074_000.4), None);
    }

    #[test]
    fn split_borrows_the_dial_for_the_over_and_gives_it_back() {
        let mut d = at(14_074_000.0);
        d.set_rit(700.0);
        // Split onto the other VFO: transmit takes the dial…
        assert_eq!(d.begin_tx(14_200_000.0), Some(14_200_000.0));
        // …and holds it. A report while it does is our own transmit frequency,
        // so it must not be mistaken for the operator moving the dial.
        assert_eq!(d.report(14_200_000.0), None);
        assert_eq!(d.vfo, 14_074_000.0, "the VFO is untouched by the over");
        // Unkey: back to the receive frequency, RIT included.
        assert_eq!(d.end_tx(), Some(14_074_700.0));
        assert_eq!(d.end_tx(), None, "the dial is only given back once");
    }

    #[test]
    fn a_retune_during_an_over_waits_for_unkey() {
        let mut d = at(14_074_000.0);
        assert_eq!(d.begin_tx(14_200_000.0), Some(14_200_000.0));
        // Retuning mid-over would drag the transmitter off the frequency it was
        // cleared to use, so nothing is commanded…
        assert_eq!(d.set_vfo(14_080_000.0), None);
        assert_eq!(d.set_rit(-500.0), None);
        // …but both are remembered, and unkey lands on the new receive frequency.
        assert_eq!(d.end_tx(), Some(14_079_500.0));
    }
}
