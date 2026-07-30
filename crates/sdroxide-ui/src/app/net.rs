//! The network cockpit's outbound half: callsign lookup and QSO upload.
//!
//! Lookups and uploads are queued here and drained into [`Command`]s by the
//! frame loop, so nothing in the UI ever blocks on the network. What comes
//! back is applied to the in-progress QSO and appended to the rolling log the
//! SPOTS window shows.

use sdroxide_types::{
    CallsignInfo, LookupProvider, NetworkConfig, QsoRecord, UploadResult, UploadTarget,
};

use crate::app::SdroxideApp;
use crate::app::persist::persist_qso_log;

/// Fill `dst` from `src` only when `dst` is blank and `src` is non-empty;
/// returns whether it changed anything.
fn fill_blank(dst: &mut String, src: Option<&str>) -> bool {
    if dst.trim().is_empty() {
        if let Some(v) = src.map(str::trim) {
            if !v.is_empty() {
                *dst = v.to_string();
                return true;
            }
        }
    }
    false
}

/// Which upload targets are enabled for auto-upload in `cfg`.
fn auto_upload_targets(cfg: &NetworkConfig) -> Vec<UploadTarget> {
    let mut t = Vec::new();
    if cfg.auto_upload_eqsl {
        t.push(UploadTarget::Eqsl);
    }
    if cfg.auto_upload_qrz {
        t.push(UploadTarget::QrzLogbook);
    }
    if cfg.auto_upload_clublog {
        t.push(UploadTarget::ClubLog);
    }
    t
}

/// Upload targets that have credentials configured (for the manual per-QSO
/// upload button).
pub(in crate::app) fn configured_upload_targets(cfg: &NetworkConfig) -> Vec<UploadTarget> {
    let mut t = Vec::new();
    if !cfg.eqsl.user.trim().is_empty() {
        t.push(UploadTarget::Eqsl);
    }
    if !cfg.qrz_logbook_key.trim().is_empty() {
        t.push(UploadTarget::QrzLogbook);
    }
    if !cfg.clublog.user.trim().is_empty() && !cfg.clublog_api_key.trim().is_empty() {
        t.push(UploadTarget::ClubLog);
    }
    t
}

/// Single-record ADIF + targets for auto-upload of a freshly logged QSO, or
/// `None` when auto-upload is off or no target is enabled.
pub(in crate::app) fn auto_upload_adif(
    cfg: &NetworkConfig,
    rec: &QsoRecord,
) -> Option<(u64, String, Vec<UploadTarget>)> {
    if !cfg.auto_upload {
        return None;
    }
    let targets = auto_upload_targets(cfg);
    if targets.is_empty() {
        return None;
    }
    Some((rec.id, sdroxide_types::qso_log_to_adif(std::slice::from_ref(rec)), targets))
}

impl SdroxideApp {
    /// Queue an auto-lookup if a provider + auto-lookup are configured.
    ///
    /// Returns whether one was actually queued, so a caller that rations
    /// lookups can tell "asked" from "lookup is switched off" and not burn its
    /// one-per-interval budget on a request that never left.
    pub(in crate::app) fn queue_lookup(&mut self, call: String) -> bool {
        let call = call.trim().to_string();
        if call.is_empty()
            || !self.net_cfg_edit.auto_lookup
            || self.net_cfg_edit.lookup_provider == LookupProvider::None
        {
            return false;
        }
        self.pending_lookups.push(call);
        true
    }

    /// Merge a callsign-lookup result into the open log entry and, if none
    /// matches, into the most recent logged QSO for that call — filling only
    /// blank fields.
    pub(in crate::app) fn apply_callsign(&mut self, info: CallsignInfo) {
        let mut summary = info.call.clone();
        if let Some(n) = &info.name {
            summary.push_str(&format!(" — {n}"));
        }
        if let Some(q) = &info.qth {
            summary.push_str(&format!(", {q}"));
        }
        if let Some(c) = &info.country {
            summary.push_str(&format!(" ({c})"));
        }
        self.push_net_log(summary);
        // Keep the whole record: the JS8 map places stations by the grid in it,
        // and that station may never have a log entry to merge into.
        self.callsign_cache.insert(info.call.to_ascii_uppercase(), info.clone());

        // 1) The open entry form, if it's for this call.
        if let Some(f) = self.log_edit.as_mut() {
            if f.call.trim().eq_ignore_ascii_case(&info.call) {
                fill_blank(&mut f.name, info.name.as_deref());
                fill_blank(&mut f.qth, info.qth.as_deref());
                fill_blank(&mut f.grid, info.grid.as_deref());
                fill_blank(&mut f.state, info.state.as_deref());
                fill_blank(&mut f.country, info.country.as_deref());
                return;
            }
        }
        // 2) Otherwise, enrich the most recent logged QSO with that call.
        if let Some(rec) = self
            .qso_log
            .iter_mut()
            .filter(|q| q.call.eq_ignore_ascii_case(&info.call))
            .max_by_key(|q| q.start_utc)
        {
            let mut changed = false;
            changed |= fill_blank(&mut rec.name, info.name.as_deref());
            changed |= fill_blank(&mut rec.qth, info.qth.as_deref());
            changed |= fill_blank(&mut rec.state, info.state.as_deref());
            changed |= fill_blank(&mut rec.country, info.country.as_deref());
            if rec.grid.as_deref().unwrap_or("").is_empty() {
                if let Some(g) = &info.grid {
                    rec.grid = Some(g.clone());
                    changed = true;
                }
            }
            if rec.dxcc.is_none() && info.dxcc.is_some() {
                rec.dxcc = info.dxcc;
                changed = true;
            }
            if rec.cq_zone.is_none() && info.cq_zone.is_some() {
                rec.cq_zone = info.cq_zone;
                changed = true;
            }
            if rec.itu_zone.is_none() && info.itu_zone.is_some() {
                rec.itu_zone = info.itu_zone;
                changed = true;
            }
            if changed {
                persist_qso_log(&self.qso_log);
                self.log_content_changed();
            }
        }
    }

    /// Record an upload result and mark the QSO's sent flag on success.
    pub(in crate::app) fn on_upload_result(&mut self, r: UploadResult) {
        let status = if r.ok { "OK" } else { "FAIL" };
        self.push_net_log(format!("{} → {}: {}", r.target.label(), status, r.message));
        if r.ok {
            if let Some(rec) = self.qso_log.iter_mut().find(|q| q.id == r.qso_id) {
                match r.target {
                    UploadTarget::Eqsl => rec.eqsl_sent = true,
                    UploadTarget::QrzLogbook => rec.qrz_sent = true,
                    UploadTarget::ClubLog => rec.clublog_sent = true,
                }
                persist_qso_log(&self.qso_log);
            }
        }
    }

    pub(in crate::app) fn push_net_log(&mut self, line: String) {
        self.net_log.insert(0, line);
        self.net_log.truncate(50);
    }

    /// Drain a pending ADIF import: parse, lightly de-dup against the current
    /// log (same call+band within 2 minutes), append with fresh ids, persist.
    pub(in crate::app) fn poll_adif_import(&mut self) {
        let text = self.adif_import_inbox.lock().ok().and_then(|mut g| g.take());
        let Some(text) = text else { return };
        let records = sdroxide_types::adif_to_qso_log(&text);
        let mut added = 0usize;
        let mut skipped = 0usize;
        for mut r in records {
            if r.call.trim().is_empty() {
                continue;
            }
            let dup = self.qso_log.iter().any(|q| {
                q.call.eq_ignore_ascii_case(&r.call)
                    && q.band.eq_ignore_ascii_case(&r.band)
                    && (q.start_utc - r.start_utc).abs() < 120
            });
            if dup {
                skipped += 1;
                continue;
            }
            r.id = self.next_log_id();
            self.qso_log.push(r);
            added += 1;
        }
        if added > 0 {
            persist_qso_log(&self.qso_log);
        }
        self.push_net_log(format!("ADIF import: {added} added, {skipped} duplicates skipped"));
    }
}
