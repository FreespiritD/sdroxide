//! Award tallies (DXCC / WAS / WAZ / grid squares) computed purely over the
//! logbook, using [`crate::entity`] to resolve a callsign's DXCC entity and CQ
//! zone when the record doesn't carry them explicitly. Worked-vs-confirmed is
//! driven by [`QsoRecord::is_confirmed`].

use std::collections::BTreeMap;

use crate::{QsoRecord, entity};

/// Worked / confirmed status for one award slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    pub worked: bool,
    pub confirmed: bool,
}

/// The full set of award tallies for a (filtered) log.
#[derive(Debug, Clone, Default)]
pub struct Awards {
    /// DXCC entities by name.
    pub dxcc: BTreeMap<String, Status>,
    /// Worked All States by 2-letter state.
    pub was: BTreeMap<String, Status>,
    /// Worked All Zones by CQ zone (1..40).
    pub waz: BTreeMap<u8, Status>,
    /// Grid squares (4-char).
    pub grids: BTreeMap<String, Status>,
}

/// (worked, confirmed) counts of a status map.
pub fn counts<K>(map: &BTreeMap<K, Status>) -> (usize, usize) {
    let worked = map.values().filter(|s| s.worked).count();
    let confirmed = map.values().filter(|s| s.confirmed).count();
    (worked, confirmed)
}

/// The 50 US states plus DC — the WAS target set.
pub const US_STATES: [&str; 51] = [
    "AL", "AK", "AZ", "AR", "CA", "CO", "CT", "DE", "FL", "GA", "HI", "ID", "IL", "IN", "IA", "KS",
    "KY", "LA", "ME", "MD", "MA", "MI", "MN", "MS", "MO", "MT", "NE", "NV", "NH", "NJ", "NM", "NY",
    "NC", "ND", "OH", "OK", "OR", "PA", "RI", "SC", "SD", "TN", "TX", "UT", "VT", "VA", "WA", "WV",
    "WI", "WY", "DC",
];

fn is_us_state(s: &str) -> bool {
    US_STATES.contains(&s)
}

/// Compute award tallies over `log`, optionally filtered to one `band` and/or
/// `mode` (case-insensitive; `None` = all).
pub fn compute_awards(log: &[QsoRecord], band: Option<&str>, mode: Option<&str>) -> Awards {
    let mut a = Awards::default();
    for q in log {
        if q.call.trim().is_empty() {
            continue;
        }
        if let Some(b) = band {
            if !q.band.eq_ignore_ascii_case(b) {
                continue;
            }
        }
        if let Some(m) = mode {
            if !q.mode.eq_ignore_ascii_case(m) {
                continue;
            }
        }
        let conf = q.is_confirmed();
        let ent = entity::resolve_callsign(&q.call);

        // DXCC entity (prefer the resolver; fall back to the record's country).
        let ent_name = ent.map(|e| e.name.to_string()).or_else(|| {
            let c = q.country.trim();
            (!c.is_empty()).then(|| c.to_string())
        });
        if let Some(name) = ent_name {
            let s = a.dxcc.entry(name).or_default();
            s.worked = true;
            s.confirmed |= conf;
        }

        // WAZ: record's CQ zone, else the resolver's.
        if let Some(z) = q.cq_zone.or_else(|| ent.map(|e| e.cq_zone)).filter(|&z| (1..=40).contains(&z)) {
            let s = a.waz.entry(z).or_default();
            s.worked = true;
            s.confirmed |= conf;
        }

        // WAS: a US state on the record.
        let st = q.state.trim().to_ascii_uppercase();
        if is_us_state(&st) {
            let s = a.was.entry(st).or_default();
            s.worked = true;
            s.confirmed |= conf;
        }

        // Grid squares (4-char).
        if let Some(g) = &q.grid {
            if g.len() >= 4 {
                let g4 = g[..4].to_ascii_uppercase();
                if g4.as_bytes()[0].is_ascii_alphabetic() {
                    let s = a.grids.entry(g4).or_default();
                    s.worked = true;
                    s.confirmed |= conf;
                }
            }
        }
    }
    a
}

/// The DXCC entity name for a callsign, if resolvable.
pub fn entity_name(call: &str) -> Option<&'static str> {
    entity::resolve_callsign(call).map(|e| e.name)
}
