//! The station's transmit interlock: at most one radio keyed at any moment.
//!
//! One shared [`TxGate`] is handed to every engine in the process. It is not a
//! hardware arbiter — each engine still talks to its own front end — it is the
//! operator-level rule that keying radio B while radio A is on the air is a
//! mistake, refused with a notice rather than acted on.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Change signal for the station-shared stores (memories, folders, band
/// stacks, digi operator config), shared by every engine in the process the
/// way [`TxGate`] is.
///
/// Each engine holds those stores as whole in-memory copies and writes them
/// back whole, so an engine that saved must tell the others to re-read or the
/// next engine to save would clobber the change. The writer bumps the
/// generation; every engine compares it against the last value it saw once per
/// loop tick — an atomic load — and reloads from disk when it moved. Purely
/// engine-side, so it also covers saves no UI drove, like a band change from a
/// CAT dial spun on a radio whose tab is not focused.
#[derive(Debug, Default)]
pub struct StoreSync {
    generation: AtomicU64,
}

impl StoreSync {
    pub fn new() -> Self {
        StoreSync::default()
    }

    /// Announce a completed write. Returns the new generation, which the
    /// writer records as already-seen so it does not reload its own save.
    pub fn bump(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// See the module doc. `0` = free; otherwise the holder's radio id + 1, so
/// radio id 0 is representable.
#[derive(Debug, Default)]
pub struct TxGate {
    owner: AtomicU32,
}

impl TxGate {
    pub fn new() -> Self {
        TxGate::default()
    }

    /// Claim the gate for `radio`. True when it was free or already ours —
    /// re-acquiring is normal (PTT while tuning, the digi sequencer keying
    /// again mid-over), so it must not deadlock the holder out.
    pub fn try_acquire(&self, radio: u32) -> bool {
        let tag = radio + 1;
        self.owner
            .compare_exchange(0, tag, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| true)
            .unwrap_or_else(|held| held == tag)
    }

    /// Release the gate if `radio` holds it. Releasing someone else's claim is
    /// refused silently: an unkey on a radio that never keyed must not free the
    /// gate out from under the radio that did.
    pub fn release(&self, radio: u32) {
        let tag = radio + 1;
        let _ = self.owner.compare_exchange(tag, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    /// The radio currently holding the gate, if any.
    pub fn holder(&self) -> Option<u32> {
        match self.owner.load(Ordering::Acquire) {
            0 => None,
            tag => Some(tag - 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_radio_at_a_time_and_reacquiring_is_not_a_deadlock() {
        let gate = TxGate::new();
        assert_eq!(gate.holder(), None);
        assert!(gate.try_acquire(0), "a free gate must key");
        assert!(gate.try_acquire(0), "the holder keying again must not be refused");
        assert!(!gate.try_acquire(1), "a second radio must be refused while the first is keyed");
        assert_eq!(gate.holder(), Some(0));

        gate.release(1);
        assert_eq!(gate.holder(), Some(0), "an unkey on a radio that never keyed frees nothing");
        gate.release(0);
        assert_eq!(gate.holder(), None);
        assert!(gate.try_acquire(1), "released, the gate serves the next radio");
        assert_eq!(gate.holder(), Some(1));
    }

    #[test]
    fn radio_id_zero_is_distinguishable_from_free() {
        let gate = TxGate::new();
        assert!(gate.try_acquire(0));
        assert_eq!(gate.holder(), Some(0), "id 0 must not read back as \"nobody\"");
    }
}
