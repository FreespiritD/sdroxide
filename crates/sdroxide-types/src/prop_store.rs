//! Every source's view of what is getting through, folded into one field per
//! band.
//!
//! Three things draw this: the flat map in the operating panel, the 3D globe,
//! and — for a browser watching a remote station — the server, which folds the
//! same observations and relays the result. A cell hot on one and cold on
//! another reads as a bug, so the rule lives here once rather than in each of
//! them.
//!
//! Here rather than beside the views because the *server* needs it too: the
//! solar tab has its own socket and no logbook of its own, so the field it
//! draws has to be built where the decodes arrive.
//!
//! What arrives here is whatever list the caller happens to be holding: the
//! rolling decode window, the WSPR receptions, the whole logbook. Every one of
//! those is re-presented on most frames, so the store remembers what it has
//! already counted and folds each observation exactly once. Without that a
//! station sitting in the decode list for two hundred frames would heat its
//! path two hundred times.

use std::collections::HashSet;
use std::sync::Arc;

use crate::propagation::{DEFAULT_HM_KM, PropField, PropObservation, PropSource, margin_db};
use crate::{Band, Decode, QsoRecord, WsprSpot, grid_to_latlon};

/// Which sources feed the field.
///
/// A bitmask rather than a set of booleans because it is persisted and passed
/// to the globe, and because the chips that toggle it want an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropSources(pub u8);

impl Default for PropSources {
    /// Everything. Each source is a real observation of a real path, and a map
    /// that quietly ignored the logbook would be a map of the last hour rather
    /// than of the station.
    fn default() -> Self {
        PropSources((1 << PropSource::ALL.len()) - 1)
    }
}

impl PropSources {
    pub fn has(self, s: PropSource) -> bool {
        match PropSource::ALL.iter().position(|x| *x == s) {
            Some(i) => self.0 & (1 << i) != 0,
            None => false,
        }
    }

    pub fn toggle(&mut self, s: PropSource) {
        if let Some(i) = PropSource::ALL.iter().position(|x| *x == s) {
            self.0 ^= 1 << i;
        }
    }
}

/// How many observation keys to remember before forgetting the oldest.
///
/// Bounds the de-duplication set. Ten thousand is several hours of a busy band;
/// past that the earliest are long outside the decay window anyway, so
/// re-counting one would change nothing visible.
const MAX_RECORDED: usize = 10_000;

/// The propagation field, and what has already been folded into it.
pub struct PropStore {
    field: Arc<PropField>,
    /// Observations already counted, so a rolling list re-presented every frame
    /// is folded once. Keyed by (source, slot, far-end grid, band index).
    recorded: HashSet<(u8, i64, String, u8)>,
    /// Insertion order, so `recorded` can be pruned without a timestamp.
    order: std::collections::VecDeque<(u8, i64, String, u8)>,
    /// Logbook length last folded in, so the whole book is not re-walked every
    /// frame — the staleness trick the awards heat map already uses.
    log_len: usize,
    sources: PropSources,
    /// The locator every path in the field was measured from.
    home: String,
    /// Assumed F2 reflection height.
    ///
    /// A constant, and left one deliberately. It could be backed out of a
    /// nearby ionosonde's own M(3000) — but the height enters the inference at
    /// deposit time, so using it would mean threading the sounder network into
    /// every panel that folds a decode, and 300 km against 280 moves the floor
    /// by a few per cent. That is well inside the uncertainty the confidence
    /// line already declares out loud.
    hm_km: f64,
}

impl Default for PropStore {
    fn default() -> Self {
        PropStore {
            field: Arc::new(PropField::default()),
            recorded: HashSet::new(),
            order: Default::default(),
            log_len: 0,
            sources: PropSources::default(),
            home: String::new(),
            hm_km: DEFAULT_HM_KM,
        }
    }
}

impl PropStore {
    /// How fast an observation is forgotten.
    pub fn set_halflife_min(&mut self, minutes: f32) {
        let s = (minutes.max(1.0) as f64) * 60.0;
        if (self.field.halflife_s - s).abs() > 0.5 {
            Arc::make_mut(&mut self.field).halflife_s = s;
        }
    }

    pub fn set_sources(&mut self, s: PropSources) {
        self.sources = s;
    }

    /// Note where this station is standing, forgetting the field if it moved.
    ///
    /// Every control point in the field is halfway along a path from *here*.
    /// After a move they are all somewhere else, and a map of paths from an
    /// address the operator has left is not a stale map — it is a wrong one.
    pub fn set_home(&mut self, grid: &str) {
        let g = grid.trim().to_ascii_uppercase();
        if g == self.home {
            return;
        }
        if !self.home.is_empty() {
            self.clear();
        }
        self.home = g;
    }

    /// The snapshot both views read, aged to `now_utc`.
    ///
    /// Shared rather than copied: the globe republishes this every frame, and a
    /// couple of hundred kilobytes at sixty frames a second is the one part of
    /// this that would cost anything.
    pub fn field(&mut self, now_utc: i64) -> Arc<PropField> {
        Arc::make_mut(&mut self.field).decay(now_utc);
        Arc::clone(&self.field)
    }

    /// The field without ageing it — for a caller that has already asked this
    /// frame.
    pub fn peek(&self) -> Arc<PropField> {
        Arc::clone(&self.field)
    }

    /// Fold in slotted-mode decodes.
    ///
    /// `dial_hz` is attached here because a [`Decode`] carries only its audio
    /// offset: by the time it reaches any other caller the dial may have moved,
    /// and a decode filed under the wrong band is worse on this map than a
    /// decode dropped.
    pub fn observe_decodes(
        &mut self,
        decodes: &[Decode],
        source: PropSource,
        dial_hz: f64,
        my_grid: &str,
        now_utc: i64,
    ) {
        if !self.sources.has(source) {
            return;
        }
        let Some(home) = grid_to_latlon(my_grid) else { return };
        for d in decodes {
            let Some(grid) = d.grid.as_deref().filter(|g| !g.is_empty()) else { continue };
            let Some(far) = grid_to_latlon(grid) else { continue };
            let freq = dial_hz + d.audio_hz as f64;
            let margin = margin_db(source, d.snr_db as f32, None);
            self.fold(far, home, freq, d.slot_utc, source, Some(margin), grid, now_utc);
        }
    }

    /// Fold in WSPR receptions.
    ///
    /// Both directions become the same kind of statement. What differs is which
    /// end is which — a beacon we decoded transmitted from its own grid to
    /// ours; a report of us went the other way — and the path is the same
    /// either way, which is exactly why they belong in one field.
    pub fn observe_wspr(&mut self, spots: &[WsprSpot], my_grid: &str, now_utc: i64) {
        let Some(home) = grid_to_latlon(my_grid) else { return };
        for s in spots {
            let source =
                if s.is_heard_by_other() { PropSource::WsprHeardUs } else { PropSource::Wspr };
            if !self.sources.has(source) {
                continue;
            }
            let (far_grid, tx, rx) = if s.is_heard_by_other() {
                // We transmitted; they received.
                let Some(g) = s.reporter_grid.as_deref().filter(|g| !g.is_empty()) else {
                    continue;
                };
                let Some(far) = grid_to_latlon(g) else { continue };
                (g, home, far)
            } else {
                let Some(g) = s.grid.as_deref().filter(|g| !g.is_empty()) else { continue };
                let Some(far) = grid_to_latlon(g) else { continue };
                (g, far, home)
            };
            // WSPR is the one source that says what power it was sent with, and
            // that is what makes a 200 mW report mean what it means.
            let margin = margin_db(source, s.snr_db as f32, Some(s.power_dbm as f32));
            self.fold_pair(tx, rx, s.freq_hz, s.slot_utc, source, Some(margin), far_grid, now_utc);
        }
    }

    /// Fold in the logbook — every contact is a path that was open once.
    ///
    /// Keyed on the book's length, so a book of ten thousand contacts is walked
    /// when it grows and not on every frame.
    pub fn observe_log(&mut self, log: &[QsoRecord], my_grid: &str, now_utc: i64) {
        if !self.sources.has(PropSource::Logged) || log.len() == self.log_len {
            return;
        }
        let from = self.log_len.min(log.len());
        self.log_len = log.len();
        let Some(home) = grid_to_latlon(my_grid) else { return };
        for q in &log[from..] {
            let Some(grid) = q.grid.as_deref().filter(|g| !g.is_empty()) else { continue };
            let Some(far) = grid_to_latlon(grid) else { continue };
            // Contacts older than the decay window would be deposited and
            // immediately decayed away, so the walk skips them rather than
            // doing the work twice.
            if now_utc - q.start_utc > (self.field.halflife_s * 8.0) as i64 {
                continue;
            }
            // No margin: an RST is not an SNR. See `propagation`'s module doc.
            self.fold(far, home, q.freq_hz, q.start_utc, PropSource::Logged, None, grid, now_utc);
        }
    }

    /// Forget everything.
    ///
    /// Called when the operator's own locator changes: every path in the field
    /// was computed from where this station was standing, so after a move the
    /// control points are all in the wrong place. Keeping them would be worse
    /// than an empty map — they would look like evidence.
    pub fn clear(&mut self) {
        let hl = self.field.halflife_s;
        self.field = Arc::new(PropField { halflife_s: hl, ..PropField::default() });
        self.recorded.clear();
        self.order.clear();
        self.log_len = 0;
    }

    /// The locator the field is measured from.
    #[cfg(test)]
    pub fn home(&self) -> &str {
        &self.home
    }

    #[allow(clippy::too_many_arguments)]
    fn fold(
        &mut self,
        far: (f64, f64),
        home: (f64, f64),
        freq_hz: f64,
        at_utc: i64,
        source: PropSource,
        margin: Option<f32>,
        key_grid: &str,
        now_utc: i64,
    ) {
        self.fold_pair(far, home, freq_hz, at_utc, source, margin, key_grid, now_utc);
    }

    #[allow(clippy::too_many_arguments)]
    fn fold_pair(
        &mut self,
        tx: (f64, f64),
        rx: (f64, f64),
        freq_hz: f64,
        at_utc: i64,
        source: PropSource,
        margin: Option<f32>,
        key_grid: &str,
        now_utc: i64,
    ) {
        let band = Band::containing(freq_hz);
        if band == Band::Gen {
            return;
        }
        let band_ix = Band::ALL.iter().position(|b| *b == band).unwrap_or(0) as u8;
        let src_ix = PropSource::ALL.iter().position(|s| *s == source).unwrap_or(0) as u8;
        let key = (src_ix, at_utc, key_grid.to_ascii_uppercase(), band_ix);
        if !self.recorded.insert(key.clone()) {
            return;
        }
        self.order.push_back(key);
        while self.order.len() > MAX_RECORDED {
            if let Some(old) = self.order.pop_front() {
                self.recorded.remove(&old);
            }
        }
        let Some(obs) = PropObservation::new(tx, rx, freq_hz, at_utc, source, margin) else {
            return;
        };
        let hm = self.hm_km;
        Arc::make_mut(&mut self.field).deposit(&obs, now_utc, hm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    /// London, and a station in Moscow — one hop apart.
    const HOME: &str = "IO91";
    const FAR: &str = "KO85";

    fn decode(grid: &str, slot: i64, snr: i16) -> Decode {
        Decode {
            slot_utc: slot,
            snr_db: snr,
            dt: 0.2,
            audio_hz: 1500.0,
            message: format!("CQ AB1CD {grid}"),
            to: None,
            from: Some("AB1CD".into()),
            grid: Some(grid.into()),
            is_cq: true,
            cq_to: None,
            free_text: false,
            rr73_to: None,
        }
    }

    fn wspr(grid: &str, slot: i64) -> WsprSpot {
        WsprSpot {
            slot_utc: slot,
            call: "AB1CD".into(),
            grid: Some(grid.into()),
            power_dbm: 37,
            freq_hz: 14_097_100.0,
            snr_db: -24,
            dt: 0.3,
            drift_hz: 0.0,
            reporter: None,
            reporter_grid: None,
        }
    }

    fn heat_at(s: &PropStore, band: Band, grid: &str) -> f32 {
        let (lat, lon) = grid_to_latlon(grid).expect("a grid");
        let (r, c) = crate::cell_of(lat, lon);
        s.field.plane(band).map(|p| p.weight[r * crate::PROP_GRID_W + c]).unwrap_or(0.0)
    }

    /// The rolling decode list is re-presented on most frames. Counting it each
    /// time would heat one path two hundred times a second.
    #[test]
    fn a_decode_re_observed_for_a_hundred_frames_is_counted_once() {
        let mut s = PropStore::default();
        let d = [decode(FAR, NOW, -12)];
        s.observe_decodes(&d, PropSource::Ft8, 14_074_000.0, HOME, NOW);
        let once = s.field.plane(Band::M20).unwrap().peak();
        for _ in 0..100 {
            s.observe_decodes(&d, PropSource::Ft8, 14_074_000.0, HOME, NOW);
        }
        assert_eq!(s.field.plane(Band::M20).unwrap().peak(), once);
    }

    /// A later slot from the same station is a new observation, not a repeat.
    #[test]
    fn the_same_station_in_a_new_slot_counts_again() {
        let mut s = PropStore::default();
        s.observe_decodes(&[decode(FAR, NOW, -12)], PropSource::Ft8, 14_074_000.0, HOME, NOW);
        let once = s.field.plane(Band::M20).unwrap().peak();
        s.observe_decodes(&[decode(FAR, NOW + 15, -12)], PropSource::Ft8, 14_074_000.0, HOME, NOW);
        assert!(s.field.plane(Band::M20).unwrap().peak() > once);
    }

    /// The band comes from the dial the decode was heard on. A decode carries
    /// only its audio offset, so getting this wrong files a whole evening of
    /// traffic under the wrong band.
    #[test]
    fn a_decode_is_filed_under_the_band_its_dial_was_on() {
        let mut s = PropStore::default();
        s.observe_decodes(&[decode(FAR, NOW, -12)], PropSource::Ft8, 7_074_000.0, HOME, NOW);
        assert!(s.field.plane(Band::M40).is_some(), "not on 40 m");
        assert!(s.field.plane(Band::M20).is_none(), "leaked onto 20 m");
    }

    /// Both directions of a WSPR path describe the same stretch of ionosphere,
    /// so they must land in the same place.
    #[test]
    fn a_report_of_us_heats_the_same_path_as_a_beacon_we_decoded() {
        let mut heard = PropStore::default();
        heard.observe_wspr(&[wspr(FAR, NOW)], HOME, NOW);

        let mut reported = PropStore::default();
        let us = WsprSpot {
            call: "K1ABC".into(),
            grid: Some(HOME.into()),
            reporter: Some("AB1CD".into()),
            reporter_grid: Some(FAR.into()),
            ..wspr(FAR, NOW)
        };
        reported.observe_wspr(&[us], HOME, NOW);

        let a = heard.field.plane(Band::M20).expect("a plane");
        let b = reported.field.plane(Band::M20).expect("a plane");
        // Same cells, same weights — the path does not care which end sent.
        assert_eq!(a.weight, b.weight);
    }

    /// Turning a source off has to actually stop it contributing.
    #[test]
    fn a_source_that_is_switched_off_contributes_nothing() {
        let mut s = PropStore::default();
        let mut src = PropSources::default();
        src.toggle(PropSource::Ft8);
        s.set_sources(src);
        s.observe_decodes(&[decode(FAR, NOW, -12)], PropSource::Ft8, 14_074_000.0, HOME, NOW);
        assert!(s.field.plane(Band::M20).is_none());
        // And the others still do.
        s.observe_wspr(&[wspr(FAR, NOW)], HOME, NOW);
        assert!(s.field.plane(Band::M20).is_some());
    }

    /// The logbook is walked when it grows, not on every frame.
    #[test]
    fn the_logbook_is_only_walked_for_what_is_new() {
        let mut s = PropStore::default();
        let mut log = vec![QsoRecord {
            grid: Some(FAR.into()),
            freq_hz: 14_074_000.0,
            start_utc: NOW,
            ..Default::default()
        }];
        s.observe_log(&log, HOME, NOW);
        let once = s.field.plane(Band::M20).unwrap().peak();
        for _ in 0..50 {
            s.observe_log(&log, HOME, NOW);
        }
        assert_eq!(s.field.plane(Band::M20).unwrap().peak(), once);

        log.push(QsoRecord {
            grid: Some(FAR.into()),
            freq_hz: 14_074_000.0,
            start_utc: NOW + 600,
            ..Default::default()
        });
        s.observe_log(&log, HOME, NOW);
        assert!(s.field.plane(Band::M20).unwrap().peak() > once);
    }

    /// A contact from last year is not evidence about tonight's ionosphere.
    #[test]
    fn an_ancient_contact_is_skipped_rather_than_deposited_and_decayed() {
        let mut s = PropStore::default();
        let log = vec![QsoRecord {
            grid: Some(FAR.into()),
            freq_hz: 14_074_000.0,
            start_utc: NOW - 400 * 86_400,
            ..Default::default()
        }];
        s.observe_log(&log, HOME, NOW);
        assert!(s.field.plane(Band::M20).is_none(), "a year-old QSO heated the map");
    }

    /// Everything on this map is relative to where the operator is. Without a
    /// grid there is no path to speak of, and inventing one would be worse than
    /// an empty map.
    #[test]
    fn nothing_is_placed_until_the_operator_says_where_they_are() {
        let mut s = PropStore::default();
        s.observe_decodes(&[decode(FAR, NOW, -12)], PropSource::Ft8, 14_074_000.0, "", NOW);
        s.observe_wspr(&[wspr(FAR, NOW)], "", NOW);
        assert!(s.field.live_bands().is_empty());
    }

    #[test]
    fn the_field_ages_when_it_is_read() {
        let mut s = PropStore::default();
        s.observe_decodes(&[decode(FAR, NOW, -12)], PropSource::Ft8, 14_074_000.0, HOME, NOW);
        let before = s.peek().plane(Band::M20).unwrap().peak();
        let f = s.field(NOW + crate::PROP_DEFAULT_HALFLIFE_S as i64);
        let after = f.plane(Band::M20).unwrap().peak();
        assert!((after / before - 0.5).abs() < 0.01, "{after} is not half of {before}");
    }

    /// Every path in the field starts at the operator's own locator. After a
    /// move they are all somewhere else, and stale evidence that *looks* like
    /// evidence is worse than none.
    #[test]
    fn moving_the_station_forgets_the_field() {
        let mut s = PropStore::default();
        s.set_home(HOME);
        s.observe_decodes(&[decode(FAR, NOW, -12)], PropSource::Ft8, 14_074_000.0, HOME, NOW);
        assert!(!s.peek().live_bands().is_empty());
        // Same locator, differently typed: not a move.
        s.set_home("io91");
        assert!(!s.peek().live_bands().is_empty(), "re-typing the locator wiped the field");
        s.set_home("JN88");
        assert!(s.peek().live_bands().is_empty(), "a move left the old paths behind");
        assert_eq!(s.home(), "JN88");
    }

    /// The first locator is not a move — the field is empty anyway, and
    /// clearing on the first sight of it would throw away a session's worth of
    /// evidence every time the config arrived a frame late.
    #[test]
    fn learning_the_locator_for_the_first_time_is_not_a_move() {
        let mut s = PropStore::default();
        s.observe_decodes(&[decode(FAR, NOW, -12)], PropSource::Ft8, 14_074_000.0, HOME, NOW);
        s.set_home(HOME);
        assert!(!s.peek().live_bands().is_empty());
    }

    /// The de-duplication set is bounded: a station left running for days must
    /// not grow it without limit.
    #[test]
    fn the_seen_set_stays_bounded_over_a_long_session() {
        let mut s = PropStore::default();
        for i in 0..(MAX_RECORDED as i64 + 500) {
            s.observe_decodes(
                &[decode(FAR, NOW + i * 15, -12)],
                PropSource::Ft8,
                14_074_000.0,
                HOME,
                NOW,
            );
        }
        assert!(s.recorded.len() <= MAX_RECORDED, "{} keys retained", s.recorded.len());
        assert_eq!(s.recorded.len(), s.order.len());
    }

    #[test]
    fn heat_lands_at_the_control_point_rather_than_on_either_station() {
        let mut s = PropStore::default();
        s.observe_decodes(&[decode(FAR, NOW, -12)], PropSource::Ft8, 14_074_000.0, HOME, NOW);
        assert_eq!(heat_at(&s, Band::M20, HOME), 0.0, "the operator's own cell was heated");
        assert_eq!(heat_at(&s, Band::M20, FAR), 0.0, "the far station's cell was heated");
        assert!(s.field.plane(Band::M20).unwrap().peak() > 0.0, "nothing was heated at all");
    }
}
