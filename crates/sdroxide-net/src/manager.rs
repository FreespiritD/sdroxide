//! The spot manager: owns the feed threads (DX cluster, POTA, SOTA, PSK
//! Reporter) and the on-demand worker threads (lookup, upload, confirmations),
//! merges spots across feeds, prunes by age, and hands the engine a stream of
//! [`NetEvent`]s to forward as `RadioEvent`s.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::Receiver;
use sdroxide_types::{NetworkConfig, Spot, SpotKind, UploadResult, UploadTarget, grid_to_latlon};

use crate::cluster::ClusterHandle;
use crate::event::{EventTx, FeedBatch, FeedTx, NetEvent};
use crate::poll::{self, PollHandle};
use crate::{pota, pskreporter, sota};

/// UTC seconds now (native-only wall clock).
fn now_utc() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub struct SpotManager {
    cfg: NetworkConfig,
    feed_tx: FeedTx,
    feed_rx: Receiver<FeedBatch>,
    event_tx: EventTx,
    event_rx: Receiver<NetEvent>,
    /// Latest spots per feed kind (each feed replaces its own set).
    by_kind: HashMap<SpotKind, Vec<Spot>>,
    last_snapshot: Vec<Spot>,
    /// Current dial frequency (Hz) as bits, shared with the PSK feed.
    dial_bits: Arc<AtomicU64>,

    cluster: Option<ClusterHandle>,
    pota: Option<PollHandle>,
    sota: Option<PollHandle>,
    psk: Option<PollHandle>,
}

impl SpotManager {
    /// Create an idle manager (no feeds until [`SpotManager::set_config`]).
    pub fn new() -> Self {
        let (feed_tx, feed_rx) = crossbeam_channel::unbounded();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        SpotManager {
            cfg: NetworkConfig::default(),
            feed_tx,
            feed_rx,
            event_tx,
            event_rx,
            by_kind: HashMap::new(),
            last_snapshot: Vec::new(),
            dial_bits: Arc::new(AtomicU64::new(14_074_000f64.to_bits())),
            cluster: None,
            pota: None,
            sota: None,
            psk: None,
        }
    }

    /// Apply a new configuration, (re)starting only the feeds whose settings
    /// changed. Disabled feeds have their threads dropped and spots cleared.
    pub fn set_config(&mut self, cfg: NetworkConfig) {
        let old = std::mem::replace(&mut self.cfg, cfg);
        if old.cluster != self.cfg.cluster || old.my_call != self.cfg.my_call {
            self.rebuild_cluster();
        }
        if old.pota != self.cfg.pota {
            self.rebuild_pota();
        }
        if old.sota != self.cfg.sota {
            self.rebuild_sota();
        }
        if old.psk != self.cfg.psk {
            self.rebuild_psk();
        }
    }

    /// Update the operator's dial frequency, so band-scoped feeds query the
    /// right slice.
    pub fn set_dial(&self, hz: f64) {
        self.dial_bits.store(hz.to_bits(), Ordering::Relaxed);
    }

    /// Kick off a callsign lookup; the result arrives via [`SpotManager::poll`].
    pub fn lookup(&self, call: String) {
        let provider = self.cfg.lookup_provider;
        let qrz = self.cfg.qrz.clone();
        let hamqth = self.cfg.hamqth.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            match crate::lookup::lookup(provider, &qrz, &hamqth, &call) {
                Ok(info) => {
                    let _ = tx.send(NetEvent::Callsign(info));
                }
                Err(e) => {
                    let _ = tx.send(NetEvent::Status(Some(format!("Lookup {call}: {e}"))));
                }
            }
        });
    }

    /// Upload one QSO's ADIF to the given targets; results arrive via `poll`.
    pub fn upload(&self, qso_id: u64, adif: String, targets: Vec<UploadTarget>) {
        let cfg = self.cfg.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            for target in targets {
                let (ok, message) = match crate::upload::upload(&cfg, target, &adif) {
                    Ok(m) => (true, m),
                    Err(e) => (false, e),
                };
                let _ = tx.send(NetEvent::Upload(UploadResult { qso_id, target, ok, message }));
            }
        });
    }

    /// Download QSL confirmations; results arrive via `poll`.
    pub fn sync_confirmations(&self) {
        let cfg = self.cfg.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(NetEvent::Status(Some("Syncing confirmations…".into())));
            let (recs, errs) = crate::upload::sync_confirmations(&cfg);
            for e in errs {
                let _ = tx.send(NetEvent::Status(Some(e)));
            }
            let n = recs.len();
            let _ = tx.send(NetEvent::Confirmations(recs));
            let _ = tx.send(NetEvent::Status(Some(format!("Confirmation sync: {n} records"))));
        });
    }

    /// Drain everything pending: feed updates (merged into a fresh spot
    /// snapshot when the set changed) plus worker results.
    pub fn poll(&mut self) -> Vec<NetEvent> {
        let mut got_feed = false;
        while let Ok((kind, spots)) = self.feed_rx.try_recv() {
            self.by_kind.insert(kind, spots);
            got_feed = true;
        }
        let mut out: Vec<NetEvent> = self.event_rx.try_iter().collect();
        // Recompute the snapshot when feeds changed (also catches age-outs on
        // the periodic polls, since feeds re-send their full set on each cycle).
        if got_feed || out.iter().any(|e| matches!(e, NetEvent::Status(_))) {
            let snap = self.snapshot();
            if snap != self.last_snapshot {
                self.last_snapshot = snap.clone();
                out.push(NetEvent::Spots(snap));
            }
        }
        out
    }

    /// Force a fresh snapshot emit on the next poll (e.g. after age-out).
    fn snapshot(&self) -> Vec<Spot> {
        let now = now_utc();
        let max_age = self.cfg.spot_max_age_secs.max(60) as i64;
        let mut v: Vec<Spot> = Vec::new();
        for spots in self.by_kind.values() {
            for s in spots {
                if now - s.when_utc > max_age {
                    continue;
                }
                let mut s = s.clone();
                if s.loc.is_none() {
                    if let Some(g) = &s.grid {
                        s.loc = grid_to_latlon(g);
                    }
                }
                v.push(s);
            }
        }
        v.sort_by(|a, b| a.freq_hz.total_cmp(&b.freq_hz));
        v
    }

    fn rebuild_cluster(&mut self) {
        self.cluster = None; // drop stops the thread
        self.by_kind.remove(&SpotKind::DxCluster);
        if self.cfg.cluster.enabled && !self.cfg.cluster.host.trim().is_empty() {
            let login = self.cfg.cluster_login().to_string();
            self.cluster = Some(ClusterHandle::connect(
                self.cfg.cluster.clone(),
                login,
                now_utc,
                self.feed_tx.clone(),
                self.event_tx.clone(),
            ));
        }
    }

    fn rebuild_pota(&mut self) {
        self.pota = None;
        self.by_kind.remove(&SpotKind::Pota);
        if self.cfg.pota.enabled {
            let interval = Duration::from_secs(self.cfg.pota.interval_secs.max(15) as u64);
            self.pota = Some(poll::spawn(
                "sdroxide-pota",
                SpotKind::Pota,
                interval,
                self.feed_tx.clone(),
                self.event_tx.clone(),
                move || pota::fetch(now_utc()),
            ));
        }
    }

    fn rebuild_sota(&mut self) {
        self.sota = None;
        self.by_kind.remove(&SpotKind::Sota);
        if self.cfg.sota.enabled {
            let interval = Duration::from_secs(self.cfg.sota.interval_secs.max(15) as u64);
            self.sota = Some(poll::spawn(
                "sdroxide-sota",
                SpotKind::Sota,
                interval,
                self.feed_tx.clone(),
                self.event_tx.clone(),
                move || sota::fetch(now_utc()),
            ));
        }
    }

    fn rebuild_psk(&mut self) {
        self.psk = None;
        self.by_kind.remove(&SpotKind::PskReporter);
        if self.cfg.psk.enabled {
            let interval = Duration::from_secs(self.cfg.psk.interval_secs.max(60) as u64);
            let dial = Arc::clone(&self.dial_bits);
            self.psk = Some(poll::spawn(
                "sdroxide-pskreporter",
                SpotKind::PskReporter,
                interval,
                self.feed_tx.clone(),
                self.event_tx.clone(),
                move || pskreporter::fetch(f64::from_bits(dial.load(Ordering::Relaxed)), now_utc()),
            ));
        }
    }
}

impl Default for SpotManager {
    fn default() -> Self {
        Self::new()
    }
}
