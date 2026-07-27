//! The FT8/FT4 QSO state machine — pure, deterministic, unit-testable.
//! Given our identity, the operator's message templates, and incoming
//! decodes, it decides the next message to transmit and tracks progress
//! through the standard exchange.

use sdroxide_types::{Decode, DigiConfig, DigiStatus, Mode, QsoRecord, QsoStep, TranscriptLine};

/// Give up waiting for a picked non-CQ station to call CQ after this long.
const WAIT_CQ_S: i64 = 300;
/// Keep a finished contact live this long to re-send the final message if the
/// DX repeats theirs (they didn't hear our 73 / RR73).
const CONFIRM_S: i64 = 300;

/// The payload half of a message (`<to> <from> PAYLOAD`).
#[derive(Debug, Clone, PartialEq)]
enum Payload {
    Grid(String),
    Report(i16),
    RReport(i16),
    Rrr,
    Rr73,
    B73,
    Other,
}

fn classify_payload(text: &str) -> Payload {
    let toks: Vec<&str> = text.split_whitespace().collect();
    let Some(p) = toks.get(2) else { return Payload::Other };
    match *p {
        "RR73" => Payload::Rr73,
        "RRR" => Payload::Rrr,
        "73" => Payload::B73,
        s if s.starts_with("R-") || s.starts_with("R+") => {
            s[1..].parse().map(Payload::RReport).unwrap_or(Payload::Other)
        }
        s if (s.starts_with('-') || s.starts_with('+')) && s[1..].parse::<i16>().is_ok() => {
            Payload::Report(s[1..].parse::<i16>().map(|v| if s.starts_with('-') { -v } else { v }).unwrap())
        }
        s if is_grid(s) => Payload::Grid(s.to_string()),
        _ => Payload::Other,
    }
}

fn is_grid(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 4 && b[0].is_ascii_uppercase() && b[1].is_ascii_uppercase() && b[2].is_ascii_digit() && b[3].is_ascii_digit()
}

#[derive(Debug, Clone)]
struct Dx {
    call: String,
    grid: Option<String>,
    rpt_sent: Option<i16>, // report we sent them (their SNR at us)
    rpt_rcvd: Option<i16>, // report they sent us
    started_utc: i64,
    last_utc: i64,
}

pub struct QsoMachine {
    cfg: DigiConfig,
    mode: Mode,
    step: QsoStep,
    dx: Option<Dx>,
    audio_hz: f32,
    tx_even: bool,
    /// The current QSO's message exchange (TX and RX lines).
    transcript: Vec<TranscriptLine>,
    /// A QSO that just completed and should be logged.
    completed: Option<QsoRecord>,
    /// Deadline while in [`QsoStep::WaitCq`] / [`QsoStep::Confirming`] (Unix s).
    deadline_utc: i64,
    /// The final message (73 / RR73) we sent, re-sent while `Confirming` if the
    /// DX repeats theirs.
    final_msg: Option<String>,
    /// A re-send of `final_msg` is queued for the next transmit slot.
    resend: bool,
    /// A message the operator queued by hand. It takes the next transmit slot
    /// whatever the sequencer had planned, then the exchange carries on from
    /// where it was.
    manual: Option<String>,
    /// Callsigns, and the DXCC entities they were in, worked this session —
    /// what makes one answer to our CQ worth more than another.
    worked_calls: std::collections::HashSet<String>,
    worked_entities: std::collections::HashSet<String>,
    /// When something last counted as progress: a reply, or the operator doing
    /// anything at all. 0 means "not stamped yet" — operator actions have no
    /// clock of their own, so the next tick stamps them.
    progress_utc: i64,
    /// Transmissions since that moment.
    tx_since_progress: u32,
    /// The watchdog stopped the sequencer; cleared by the next operator action.
    watchdog: bool,
}

impl QsoMachine {
    pub fn new(mode: Mode, cfg: DigiConfig) -> Self {
        let tx_even = cfg.tx_even;
        QsoMachine {
            cfg,
            mode,
            step: QsoStep::Idle,
            dx: None,
            audio_hz: 1500.0,
            tx_even,
            transcript: Vec::new(),
            completed: None,
            deadline_utc: 0,
            final_msg: None,
            resend: false,
            manual: None,
            worked_calls: std::collections::HashSet::new(),
            worked_entities: std::collections::HashSet::new(),
            progress_utc: 0,
            tx_since_progress: 0,
            watchdog: false,
        }
    }

    pub fn set_config(&mut self, cfg: DigiConfig) {
        self.tx_even = cfg.tx_even;
        self.cfg = cfg;
    }

    pub fn set_audio_hz(&mut self, hz: f32) {
        self.audio_hz = hz;
    }

    pub fn step(&self) -> QsoStep {
        self.step
    }

    /// The callsign of the station we're currently working, if any.
    pub fn dx_call(&self) -> Option<&str> {
        self.dx.as_ref().map(|d| d.call.as_str())
    }

    /// Our own station callsign.
    pub fn my_call(&self) -> &str {
        &self.cfg.my_call
    }

    /// Note that the sequencer got somewhere: a reply arrived, or the operator
    /// acted. Restarts both the watchdog and the unanswered-call count.
    ///
    /// The timestamp is left for the next [`tick`](Self::tick) to fill in,
    /// because operator actions arrive without a clock.
    fn progress(&mut self) {
        self.progress_utc = 0;
        self.tx_since_progress = 0;
        self.watchdog = false;
    }

    /// True when the transmit watchdog has stopped the sequencer.
    pub fn tx_watchdog(&self) -> bool {
        self.watchdog
    }

    /// Start calling CQ.
    pub fn call_cq(&mut self) {
        self.dx = None;
        self.transcript.clear();
        self.final_msg = None;
        self.resend = false;
        self.manual = None;
        self.progress();
        self.step = QsoStep::CallingCq;
    }

    /// Jump the exchange to `step`, the way WSJT-X's Tx1–Tx6 buttons do: the
    /// operator picks which message goes next and the sequencer carries on from
    /// there. Steps that address a station need one; `Idle` stands down.
    pub fn set_step(&mut self, step: QsoStep) -> bool {
        let needs_dx = !matches!(step, QsoStep::Idle | QsoStep::CallingCq);
        if needs_dx && self.dx.is_none() {
            return false;
        }
        if step == QsoStep::CallingCq {
            self.call_cq();
            return true;
        }
        self.manual = None;
        self.progress();
        self.step = step;
        true
    }

    /// Send `text` verbatim in the next transmit slot, then carry on with the
    /// exchange. Empty text cancels a message queued but not yet sent.
    pub fn queue_text(&mut self, text: String) {
        let text = text.trim().to_ascii_uppercase();
        self.manual = (!text.is_empty()).then_some(text);
        self.progress();
    }

    /// Record a message we transmitted (called by the controller when it
    /// actually keys the burst).
    pub fn record_tx(&mut self, text: &str) {
        self.transcript.push(TranscriptLine::sent(text));
    }

    /// Engage a decoded station. `snr` is their signal at us — the report we'll
    /// send. When `wait_for_cq` we hold in [`QsoStep::WaitCq`] (the operator
    /// picked a station that isn't calling CQ and isn't calling us) and only
    /// start transmitting once they call CQ or address us; otherwise we reply
    /// with our grid right away.
    pub fn start_qso(
        &mut self,
        from: String,
        grid: Option<String>,
        snr: i16,
        wait_for_cq: bool,
        now_utc: i64,
    ) {
        self.transcript.clear();
        self.final_msg = None;
        self.resend = false;
        self.manual = None;
        self.progress();
        self.dx = Some(Dx {
            call: from,
            grid,
            rpt_sent: Some(snr),
            rpt_rcvd: None,
            started_utc: now_utc,
            last_utc: now_utc,
        });
        if wait_for_cq {
            self.step = QsoStep::WaitCq;
            self.deadline_utc = now_utc + WAIT_CQ_S;
        } else {
            self.step = QsoStep::TxGrid;
        }
    }

    /// Graceful stop: no new bursts planned, revert to idle.
    pub fn stop(&mut self) {
        self.step = QsoStep::Idle;
        self.manual = None;
    }

    /// Hard reset.
    pub fn abort(&mut self) {
        self.step = QsoStep::Idle;
        self.dx = None;
        self.manual = None;
    }

    /// True while we intend to transmit this cycle. `WaitCq` holds silently
    /// until the DX calls CQ; `Confirming` transmits only when a re-send of our
    /// final message is queued.
    pub fn wants_tx(&self) -> bool {
        if self.manual.is_some() && !self.cfg.my_call.trim().is_empty() {
            return true; // the operator asked for this one explicitly
        }
        match self.step {
            QsoStep::Idle | QsoStep::Done | QsoStep::WaitCq => false,
            QsoStep::Confirming => self.resend,
            _ => true,
        }
    }

    /// Advance timeouts (called each engine tick). Returns true if the state
    /// changed: gives up a stale `WaitCq`, and retires a `Confirming` contact
    /// once its re-send window elapses.
    pub fn tick(&mut self, now_utc: i64) -> bool {
        if self.progress_utc == 0 {
            self.progress_utc = now_utc;
        }
        // Transmit watchdog: nothing has come back and nobody has touched the
        // controls for the configured span, so stop calling. The contact stays
        // on screen — picking a message or calling CQ resumes.
        let limit = self.cfg.tx_watchdog_min as i64 * 60;
        if limit > 0 && !self.watchdog && self.wants_tx() && now_utc - self.progress_utc >= limit {
            self.watchdog = true;
            self.manual = None;
            self.step = QsoStep::Idle;
            self.transcript.push(TranscriptLine::note(format!(
                "transmit watchdog: {} minutes with no progress",
                self.cfg.tx_watchdog_min
            )));
            return true;
        }
        match self.step {
            QsoStep::WaitCq | QsoStep::Confirming if now_utc >= self.deadline_utc => {
                self.step = QsoStep::Idle;
                self.dx = None;
                self.resend = false;
                self.final_msg = None;
                true
            }
            _ => false,
        }
    }

    /// Fold in decodes from a slot; advance the exchange when the DX replied
    /// to us. While calling CQ, the first station to answer us is adopted as
    /// the DX. Returns true if the state changed.
    pub fn on_rx(&mut self, decodes: &[Decode], now_utc: i64) -> bool {
        let my_call = self.cfg.my_call.clone();
        if my_call.is_empty() {
            return false;
        }
        // A pileup can decode both their answer to us *and* other traffic from
        // them in the same slot; an answer to us always wins over "they're
        // working someone else".
        let mut changed = false;
        let dx_call = self.dx.as_ref().map(|d| d.call.clone());
        let answered_us = dx_call.as_deref().is_some_and(|dx| {
            decodes
                .iter()
                .any(|d| d.from.as_deref() == Some(dx) && d.to.as_deref() == Some(my_call.as_str()))
        });
        // A CQ can be answered by several stations in one slot. Choose between
        // them once, up front, rather than taking whichever the decoder
        // happened to report first.
        let answerer = (self.step == QsoStep::CallingCq && self.dx.is_none())
            .then(|| self.pick_answerer(decodes, &my_call))
            .flatten();
        if let Some(pick) = &answerer {
            let others: Vec<&str> = decodes
                .iter()
                .filter(|d| d.to.as_deref() == Some(my_call.as_str()))
                .filter_map(|d| d.from.as_deref())
                .filter(|c| !c.is_empty() && c != pick)
                .collect();
            if !others.is_empty() {
                // The ones we're not taking are worth knowing about: they are
                // still calling, and the decode list will have scrolled on.
                self.transcript.push(TranscriptLine::note(format!(
                    "also calling: {}",
                    others.join(", ")
                )));
                changed = true;
            }
        }
        for d in decodes {
            let Some(from) = d.from.as_deref().filter(|f| !f.is_empty()) else { continue };
            let to_me = d.to.as_deref() == Some(my_call.as_str());
            // Our station answering someone else: they're in another exchange.
            let other = (!to_me && !answered_us && !d.is_cq && dx_call.as_deref() == Some(from))
                .then(|| d.to.as_deref().filter(|t| !t.is_empty() && *t != my_call))
                .flatten();

            // Waiting for our picked station: start replying once they call CQ
            // (they're free) or address us directly (no need to keep waiting).
            if self.step == QsoStep::WaitCq {
                if self.dx.as_ref().map(|x| x.call.as_str()) == Some(from) && (d.is_cq || to_me) {
                    if let Some(dx) = self.dx.as_mut() {
                        dx.last_utc = now_utc;
                        if dx.grid.is_none() {
                            if let Payload::Grid(g) = classify_payload(&d.message) {
                                dx.grid = Some(g);
                            }
                        }
                    }
                    self.step = QsoStep::TxGrid;
                    changed = true;
                } else if let Some(other) = other {
                    // Still busy — keep the log showing who with.
                    changed |= self.note_working(from, other);
                }
                continue;
            }

            // We called them, but they took someone else's call: stop calling
            // and hold until they're free again rather than doubling into their
            // exchange. Only while our own exchange is still unfinished — once
            // we owe them a 73 / RR73 that message goes out regardless, so the
            // contact gets completed and logged.
            if let Some(other) = other {
                if matches!(self.step, QsoStep::TxGrid | QsoStep::TxRReport) {
                    self.note_working(from, other);
                    self.step = QsoStep::WaitCq;
                    self.deadline_utc = now_utc + WAIT_CQ_S;
                    changed = true;
                    continue;
                }
            }

            if !to_me {
                continue; // everything else must be addressed to us
            }
            let payload = classify_payload(&d.message);

            // Contact just logged: if the DX repeats their message they didn't
            // hear our final one — queue a single re-send. A bare 73 means they
            // got it and are closing, so nothing to do.
            if self.step == QsoStep::Confirming {
                if self.dx.as_ref().map(|x| x.call.as_str()) == Some(from) {
                    self.transcript.push(TranscriptLine::rcvd(d.message.clone()));
                    if !matches!(payload, Payload::B73) {
                        self.resend = true;
                    }
                    changed = true;
                }
                continue;
            }

            // Calling CQ and someone answers → adopt the best of them.
            if self.step == QsoStep::CallingCq && self.dx.is_none() && answerer.as_deref() == Some(from) {
                let grid = match &payload {
                    Payload::Grid(g) => Some(g.clone()),
                    _ => d.grid.clone(),
                };
                self.dx = Some(Dx {
                    call: from.to_string(),
                    grid,
                    rpt_sent: Some(d.snr_db),
                    rpt_rcvd: None,
                    started_utc: now_utc,
                    last_utc: now_utc,
                });
                self.transcript.push(TranscriptLine::rcvd(d.message.clone()));
                changed |= self.advance(&payload, now_utc);
                continue;
            }

            // Otherwise only the station we're working advances us.
            if self.dx.as_ref().map(|d| d.call.as_str()) != Some(from) {
                continue;
            }
            if let Some(dx) = self.dx.as_mut() {
                dx.last_utc = now_utc;
                if dx.grid.is_none() {
                    if let Payload::Grid(g) = &payload {
                        dx.grid = Some(g.clone());
                    }
                }
                if dx.rpt_sent.is_none() {
                    dx.rpt_sent = Some(d.snr_db);
                }
            }
            self.transcript.push(TranscriptLine::rcvd(d.message.clone()));
            self.progress();
            self.progress_utc = now_utc;
            changed |= self.advance(&payload, now_utc);
        }
        changed
    }

    /// Which of the stations answering our CQ to work first.
    ///
    /// Signal strength decides between comparable stations, but not across the
    /// board: a station we have already worked this session goes last whatever
    /// its signal, and among signals of similar strength (6 dB bands — the
    /// difference between "solid" and "marginal" matters, a couple of dB does
    /// not) a new DXCC entity is worth more than a repeat one. Beyond that the
    /// strongest wins, because it is the one most likely to complete.
    fn pick_answerer(&self, decodes: &[Decode], my_call: &str) -> Option<String> {
        decodes
            .iter()
            .filter(|d| d.to.as_deref() == Some(my_call))
            .filter_map(|d| {
                let call = d.from.as_deref().filter(|f| !f.is_empty())?;
                Some((call, d.snr_db))
            })
            .max_by_key(|(call, snr)| {
                let fresh = !self.worked_calls.contains(&call.to_ascii_uppercase());
                let new_entity = sdroxide_types::entity_name(call)
                    .is_some_and(|e| !self.worked_entities.contains(e));
                (fresh, snr.div_euclid(6), new_entity, *snr)
            })
            .map(|(call, _)| call.to_string())
    }

    /// Note in the transcript that the station we called is working someone
    /// else. Deduplicated against the line before it, so holding through a whole
    /// exchange adds one line per partner rather than one per slot. Returns true
    /// if a line was added.
    fn note_working(&mut self, dx: &str, other: &str) -> bool {
        let text = format!("{dx} is working {other}");
        if self.transcript.last().is_some_and(|l| l.overheard && l.text == text) {
            return false;
        }
        self.transcript.push(TranscriptLine::note(text));
        true
    }

    fn advance(&mut self, payload: &Payload, _now_utc: i64) -> bool {
        let prev = self.step;
        match (self.step, payload) {
            // They answered our CQ with their grid → send them a report.
            (QsoStep::CallingCq, Payload::Grid(_)) => self.step = QsoStep::TxReport,
            // (Answerer) they sent us a report → send R+report.
            (QsoStep::TxGrid, Payload::Report(r)) => {
                self.set_rcvd(*r);
                self.step = QsoStep::TxRReport;
            }
            // They sent R+report back → send RR73.
            (QsoStep::TxReport, Payload::RReport(r)) => {
                self.set_rcvd(*r);
                self.step = QsoStep::TxRr73;
            }
            // (Answerer) they rogered → send 73.
            (QsoStep::TxRReport, Payload::Rrr | Payload::Rr73) => {
                self.step = QsoStep::Tx73;
            }
            // (CQ caller) at TxRr73 we log + enter Confirming once our RR73 goes
            // out (see `note_tx_sent`); a 73 arriving first just confirms it.
            _ => {}
        }
        self.step != prev
    }

    fn set_rcvd(&mut self, r: i16) {
        if let Some(dx) = self.dx.as_mut() {
            dx.rpt_rcvd = Some(r);
        }
    }

    /// Log the QSO and enter [`QsoStep::Confirming`], keeping the DX so we can
    /// re-send our final message for a few minutes if they didn't hear it.
    fn log_qso(&mut self, now_utc: i64) {
        if let Some(dx) = self.dx.as_ref() {
            self.worked_calls.insert(dx.call.to_ascii_uppercase());
            if let Some(e) = sdroxide_types::entity_name(&dx.call) {
                self.worked_entities.insert(e.to_string());
            }
            self.completed = Some(QsoRecord {
                call: dx.call.clone(),
                grid: dx.grid.clone(),
                rst_sent: dx.rpt_sent,
                rst_rcvd: dx.rpt_rcvd,
                freq_hz: 0.0, // filled by the controller (needs dial freq)
                mode: self.mode.label().to_string(),
                band: String::new(), // filled by the controller
                start_utc: dx.started_utc,
                end_utc: now_utc,
                my_call: self.cfg.my_call.clone(),
                my_grid: self.cfg.my_grid.clone(),
                ..Default::default() // id assigned by the logbook, no comment
            });
        }
        self.step = QsoStep::Confirming;
        self.deadline_utc = now_utc + CONFIRM_S;
        self.resend = false;
    }

    /// The message to transmit this slot, or None if we shouldn't key.
    pub fn plan_tx(&self) -> Option<String> {
        // Nothing goes out without a station callsign: every message is built
        // around ours, and an unconfigured station must never key. (The message
        // packer degrades unpackable text to free text, so this is the guard
        // that keeps a bare "CQ" off the air.)
        if self.cfg.my_call.trim().is_empty() {
            return None;
        }
        if let Some(m) = &self.manual {
            return Some(m.clone());
        }
        let dx = self.dx.as_ref();
        let dx_call = dx.map(|d| d.call.as_str()).unwrap_or("");
        let mc = &self.cfg.my_call;
        // FT8/FT4 use the 4-character Maidenhead locator; a 6-char grid like
        // "JN78ve" is truncated to "JN78" for the transmitted message.
        let mg: String = self.cfg.my_grid.chars().take(4).collect();
        let rpt_sent = dx.and_then(|d| d.rpt_sent);
        let fill = |tmpl: &str, rpt: Option<i16>| DigiConfig::fill(tmpl, mc, &mg, dx_call, rpt);
        match self.step {
            QsoStep::CallingCq => Some(fill(&self.cfg.msg_cq, None)),
            QsoStep::TxGrid => Some(fill(&self.cfg.msg_grid, None)),
            QsoStep::TxReport => Some(fill(&self.cfg.msg_report, rpt_sent)),
            QsoStep::TxRReport => Some(fill(&self.cfg.msg_rreport, rpt_sent)),
            QsoStep::TxRr73 => Some(fill(&self.cfg.msg_rr73, None)),
            QsoStep::Tx73 => Some(fill(&self.cfg.msg_73, None)),
            // Re-send our final message only when the DX prompted it.
            QsoStep::Confirming => self.resend.then(|| self.final_msg.clone()).flatten(),
            QsoStep::Idle | QsoStep::WaitCq | QsoStep::Done => None,
        }
    }

    /// The controller calls this after each burst finishes. When the final
    /// message (73 as answerer, RR73 as CQ caller) has gone out, log the QSO and
    /// move to `Confirming`; while confirming, a queued re-send has now left.
    pub fn note_tx_sent(&mut self, now_utc: i64) {
        // A hand-queued message took this slot; the exchange is where it was.
        if self.manual.take().is_some() {
            return;
        }
        self.tx_since_progress += 1;
        // Calling one station that never comes back: give up rather than call
        // into the void all afternoon. (Repeating a CQ is exempt — that *is*
        // the operation; the watchdog above bounds it instead.)
        let repeats = self.cfg.max_tx_repeats;
        if repeats > 0
            && self.tx_since_progress >= repeats
            && matches!(self.step, QsoStep::TxGrid | QsoStep::TxReport | QsoStep::TxRReport)
        {
            let call = self.dx.as_ref().map(|d| d.call.clone()).unwrap_or_default();
            self.transcript
                .push(TranscriptLine::note(format!("no reply from {call} after {repeats} calls")));
            self.step = QsoStep::Idle;
            return;
        }
        match self.step {
            QsoStep::Tx73 | QsoStep::TxRr73 => {
                self.final_msg = self.plan_tx();
                self.log_qso(now_utc);
            }
            QsoStep::Confirming => self.resend = false,
            _ => {}
        }
    }

    /// Take a completed QSO record for logging (fields freq_hz/band still 0).
    pub fn take_completed(&mut self) -> Option<QsoRecord> {
        self.completed.take()
    }

    pub fn status(&self, transmitting: bool) -> DigiStatus {
        DigiStatus {
            mode: self.mode,
            step: self.step,
            dx_call: self.dx.as_ref().map(|d| d.call.clone()),
            dx_grid: self.dx.as_ref().and_then(|d| d.grid.clone()),
            tx_next: self.wants_tx(),
            tx_pending_msg: self.plan_tx(),
            audio_hz: self.audio_hz,
            tx_even: self.tx_even,
            transmitting,
            tx_watchdog: self.watchdog,
            transcript: self.transcript.clone(),
            config: self.cfg.clone(),
            // FT8/FT4 don't use the continuous keyboard-text fields.
            text_rx: String::new(),
            tx_sent: 0,
            fsq_heard: Vec::new(),
            fsq_messages: Vec::new(),
            rade: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DigiConfig {
        DigiConfig { my_call: "AB1CD".into(), my_grid: "FN42".into(), ..Default::default() }
    }

    fn decode(msg: &str) -> Decode {
        Decode {
            slot_utc: 0,
            snr_db: -10,
            dt: 0.1,
            audio_hz: 1500.0,
            message: msg.to_string(),
            to: msg.split_whitespace().next().filter(|t| *t != "CQ").map(|s| s.to_string()),
            from: {
                let t: Vec<&str> = msg.split_whitespace().collect();
                if t.first() == Some(&"CQ") { t.get(1).map(|s| s.to_string()) } else { t.get(1).map(|s| s.to_string()) }
            },
            grid: None,
            is_cq: msg.starts_with("CQ"),
            cq_dx: msg.starts_with("CQ DX"),
            free_text: false,
        }
    }

    #[test]
    fn grid_truncated_to_four_for_ft8() {
        // A 6-character locator is cut to the 4-char Maidenhead grid in messages.
        let cfg = DigiConfig { my_call: "AB1CD".into(), my_grid: "JN78ve".into(), ..cfg() };
        let mut q = QsoMachine::new(Mode::Ft8, cfg);
        q.start_qso("W9XYZ".into(), Some("EM48".into()), -10, false, 100);
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD JN78"));
    }

    #[test]
    fn answerer_full_sequence() {
        // We (AB1CD) answer W9XYZ's CQ and run the QSO to completion.
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), Some("EM48".into()), -10, false, 100);
        assert_eq!(q.step(), QsoStep::TxGrid);
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD FN42"));

        // They send us a report → we send R+report.
        assert!(q.on_rx(&[decode("AB1CD W9XYZ -13")], 115));
        assert_eq!(q.step(), QsoStep::TxRReport);
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD R-10"));

        // They roger → we send 73.
        assert!(q.on_rx(&[decode("AB1CD W9XYZ RR73")], 130));
        assert_eq!(q.step(), QsoStep::Tx73);
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD 73"));

        // Our 73 goes out → logged, and we hold in Confirming (ready to re-send).
        q.note_tx_sent(145);
        assert_eq!(q.step(), QsoStep::Confirming);
        assert!(!q.wants_tx());
        let rec = q.take_completed().expect("logged");
        assert_eq!(rec.call, "W9XYZ");
        assert_eq!(rec.rst_sent, Some(-10));
        assert_eq!(rec.rst_rcvd, Some(-13));
        assert_eq!(rec.my_call, "AB1CD");
    }

    #[test]
    fn cq_caller_sequence() {
        // We call CQ; W9XYZ answers with a grid; we run the exchange.
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.call_cq();
        assert_eq!(q.plan_tx().as_deref(), Some("CQ AB1CD FN42"));

        // W9XYZ answers our CQ → we adopt them and send a report (their SNR).
        assert!(q.on_rx(&[decode("AB1CD W9XYZ EM48")], 100));
        assert_eq!(q.step(), QsoStep::TxReport);
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD -10"));

        // They send R+report → we send RR73.
        assert!(q.on_rx(&[decode("AB1CD W9XYZ R-12")], 115));
        assert_eq!(q.step(), QsoStep::TxRr73);
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD RR73"));

        // Our RR73 goes out → logged + Confirming; their 73 just confirms it.
        q.note_tx_sent(130);
        assert_eq!(q.step(), QsoStep::Confirming);
        let rec = q.take_completed().expect("logged");
        assert_eq!(rec.rst_sent, Some(-10));
        assert_eq!(rec.rst_rcvd, Some(-12));
        assert!(q.on_rx(&[decode("AB1CD W9XYZ 73")], 145));
        assert!(!q.wants_tx(), "a bare 73 needs no re-send");
    }

    /// A decode addressed to us from `call` at `snr`.
    fn answer(call: &str, snr: i16) -> Decode {
        let mut d = decode(&format!("AB1CD {call} EM48"));
        d.snr_db = snr;
        d
    }

    #[test]
    fn the_strongest_of_several_answers_is_worked_first() {
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.call_cq();
        q.on_rx(&[answer("W9XYZ", -18), answer("K1ABC", -4), answer("G4XYZ", -21)], 100);
        assert_eq!(q.dx_call(), Some("K1ABC"));
        // The ones we passed over are recorded, since they're still calling.
        let note = q.status(false).transcript.iter().find(|l| l.overheard).cloned();
        let note = note.expect("a note listing the other callers");
        assert!(note.text.contains("W9XYZ") && note.text.contains("G4XYZ"), "{}", note.text);
        assert!(!note.text.contains("K1ABC"), "the station we took is not an 'also'");
    }

    #[test]
    fn a_station_worked_this_session_waits_its_turn() {
        // Run a full QSO with W9XYZ, then have them answer the next CQ
        // alongside a weaker station we haven't worked.
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.call_cq();
        q.on_rx(&[answer("W9XYZ", -2)], 100);
        q.on_rx(&[decode("AB1CD W9XYZ R-12")], 115);
        q.note_tx_sent(130); // RR73 out → logged
        assert!(q.take_completed().is_some());

        q.call_cq();
        q.on_rx(&[answer("W9XYZ", -2), answer("K1ABC", -20)], 145);
        assert_eq!(q.dx_call(), Some("K1ABC"), "a dupe loses to a station not yet worked");
    }

    #[test]
    fn a_new_entity_wins_between_comparable_signals() {
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        // Work a US station first, so "United States" is no longer new.
        q.call_cq();
        q.on_rx(&[answer("K1ABC", -2)], 100);
        q.on_rx(&[decode("AB1CD K1ABC R-12")], 115);
        q.note_tx_sent(130);
        assert!(q.take_completed().is_some());

        // Two answers within the same signal band: the new entity is worth more.
        q.call_cq();
        q.on_rx(&[answer("W9XYZ", -3), answer("DL1ABC", -6)], 145);
        assert_eq!(q.dx_call(), Some("DL1ABC"));

        // A much stronger signal still wins — a marginal new one that can't
        // complete is worth less than a contact that will.
        q.abort();
        q.call_cq();
        q.on_rx(&[answer("W9XYZ", 3), answer("DL1ABC", -22)], 160);
        assert_eq!(q.dx_call(), Some("W9XYZ"));
    }

    #[test]
    fn ignores_other_stations() {
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), None, -10, false, 100);
        // Traffic between two other stations must not touch our exchange.
        assert!(!q.on_rx(&[decode("K1ABC G4XYZ -05")], 115));
        assert_eq!(q.step(), QsoStep::TxGrid);
        // Nor must a report someone else sent *our* DX (that's their SNR, not
        // ours) — only what the DX sends us advances the QSO.
        assert!(!q.on_rx(&[decode("W9XYZ K1ABC -05")], 130));
        assert_eq!(q.step(), QsoStep::TxGrid);
    }

    #[test]
    fn reply_to_non_cq_waits_for_cq() {
        // Picking a station that isn't calling CQ holds silently until they do.
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), None, -10, true, 100);
        assert_eq!(q.step(), QsoStep::WaitCq);
        assert!(!q.wants_tx());
        assert_eq!(q.plan_tx(), None);

        // Them working someone else keeps us waiting (and says who with).
        q.on_rx(&[decode("K1ABC W9XYZ RR73")], 115);
        assert_eq!(q.step(), QsoStep::WaitCq);
        assert!(!q.wants_tx());

        // They call CQ → we start replying.
        assert!(q.on_rx(&[decode("CQ W9XYZ EM48")], 130));
        assert_eq!(q.step(), QsoStep::TxGrid);
        assert!(q.wants_tx());
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD FN42"));
    }

    #[test]
    fn dx_taking_another_caller_holds_us_until_their_next_cq() {
        // We answered W9XYZ's CQ, but they came back to K1ABC instead.
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), Some("EM48".into()), -10, false, 100);
        assert_eq!(q.step(), QsoStep::TxGrid);

        assert!(q.on_rx(&[decode("K1ABC W9XYZ -13")], 115));
        assert_eq!(q.step(), QsoStep::WaitCq, "we must stop calling into their QSO");
        assert!(!q.wants_tx());
        assert_eq!(q.plan_tx(), None);
        let note = q.status(false).transcript.pop().expect("a note about who they're working");
        assert!(note.overheard);
        assert_eq!(note.text, "W9XYZ is working K1ABC");

        // Their exchange continues: one note per partner, not one per slot.
        assert!(!q.on_rx(&[decode("K1ABC W9XYZ RR73")], 130));
        assert_eq!(q.status(false).transcript.iter().filter(|l| l.overheard).count(), 1);
        // A new partner is worth a new line.
        assert!(q.on_rx(&[decode("G4XYZ W9XYZ -07")], 145));
        assert_eq!(
            q.status(false).transcript.last().map(|l| l.text.clone()),
            Some("W9XYZ is working G4XYZ".into())
        );

        // They're free again → we resume calling them.
        assert!(q.on_rx(&[decode("CQ W9XYZ EM48")], 160));
        assert_eq!(q.step(), QsoStep::TxGrid);
        assert!(q.wants_tx());
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD FN42"));
    }

    #[test]
    fn an_answer_to_us_beats_other_traffic_in_the_same_slot() {
        // Both their reply to us and a stray decode to someone else land in one
        // slot: the reply wins, so we keep the QSO instead of standing down.
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), Some("EM48".into()), -10, false, 100);
        assert!(q.on_rx(&[decode("K1ABC W9XYZ -13"), decode("AB1CD W9XYZ -09")], 115));
        assert_eq!(q.step(), QsoStep::TxRReport);
        assert!(q.status(false).transcript.iter().all(|l| !l.overheard));
    }

    #[test]
    fn a_final_message_still_goes_out_when_they_move_on() {
        // They rogered us and moved to K1ABC before our 73 left: the contact is
        // complete, so we send it and log rather than standing down.
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), Some("EM48".into()), -10, false, 100);
        q.on_rx(&[decode("AB1CD W9XYZ -13")], 115);
        q.on_rx(&[decode("AB1CD W9XYZ RR73")], 130);
        assert_eq!(q.step(), QsoStep::Tx73);
        assert!(!q.on_rx(&[decode("K1ABC W9XYZ -05")], 145));
        assert_eq!(q.step(), QsoStep::Tx73);
        assert!(q.wants_tx());
    }

    #[test]
    fn the_operator_can_pick_which_message_goes_next() {
        // Mid-exchange, jumping to RR73 skips the rest of the sequence and the
        // machine carries on from there — WSJT-X's Tx4 button.
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), Some("EM48".into()), -10, false, 100);
        assert!(q.set_step(QsoStep::TxRr73));
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD RR73"));
        // Sending it still completes and logs the contact.
        q.note_tx_sent(115);
        assert_eq!(q.step(), QsoStep::Confirming);
        assert!(q.take_completed().is_some());

        // A step that addresses a station is refused when there is none.
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        assert!(!q.set_step(QsoStep::TxReport));
        assert_eq!(q.step(), QsoStep::Idle);
        // Calling CQ needs no DX.
        assert!(q.set_step(QsoStep::CallingCq));
        assert_eq!(q.plan_tx().as_deref(), Some("CQ AB1CD FN42"));
    }

    #[test]
    fn a_queued_message_takes_one_slot_and_leaves_the_exchange_alone() {
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), Some("EM48".into()), -10, false, 100);
        q.queue_text("w9xyz ab1cd tnx".into());
        assert!(q.wants_tx());
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD TNX"), "sent verbatim, uppercased");

        // After it goes out the sequencer picks up exactly where it was.
        q.note_tx_sent(115);
        assert_eq!(q.step(), QsoStep::TxGrid);
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD FN42"));

        // Queued at the last step, it must not log the QSO in place of the 73.
        q.on_rx(&[decode("AB1CD W9XYZ -13")], 130);
        q.on_rx(&[decode("AB1CD W9XYZ RR73")], 145);
        assert_eq!(q.step(), QsoStep::Tx73);
        q.queue_text("TNX 73 GL".into());
        q.note_tx_sent(160);
        assert_eq!(q.step(), QsoStep::Tx73, "the 73 still owes us a slot");
        assert!(q.take_completed().is_none(), "nothing to log until the 73 is sent");
        q.note_tx_sent(175);
        assert_eq!(q.step(), QsoStep::Confirming);
        assert!(q.take_completed().is_some());
    }

    #[test]
    fn a_queued_message_still_needs_a_callsign() {
        let mut q = QsoMachine::new(Mode::Ft8, DigiConfig::default());
        q.queue_text("CQ TEST".into());
        assert!(!q.wants_tx());
        assert_eq!(q.plan_tx(), None);
    }

    #[test]
    fn an_unconfigured_station_never_transmits() {
        let mut q = QsoMachine::new(Mode::Ft8, DigiConfig::default());
        q.call_cq();
        assert_eq!(q.plan_tx(), None, "no callsign, no transmission");
    }

    #[test]
    fn the_watchdog_stops_a_station_calling_into_an_empty_band() {
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.call_cq();
        assert!(!q.tick(100), "the clock starts at the first tick");
        assert!(q.wants_tx());
        // Well inside the window nothing happens.
        assert!(!q.tick(100 + 5 * 60));
        // Past it the sequencer stands down and says why.
        assert!(q.tick(100 + 6 * 60));
        assert!(!q.wants_tx());
        assert!(q.tx_watchdog());
        assert_eq!(q.step(), QsoStep::Idle);
        let note = q.status(false).transcript.pop().expect("a note");
        assert!(note.overheard && note.text.contains("watchdog"), "{}", note.text);

        // Calling CQ again clears it and restarts the clock.
        q.call_cq();
        assert!(!q.tx_watchdog());
        assert!(!q.tick(100 + 7 * 60), "the window restarts from the operator's action");
        assert!(q.wants_tx());
    }

    #[test]
    fn a_reply_keeps_the_watchdog_at_bay() {
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), Some("EM48".into()), -10, false, 100);
        q.tick(100);
        // They answer five minutes in: the window starts again from there.
        q.on_rx(&[decode("AB1CD W9XYZ -13")], 100 + 5 * 60);
        assert!(!q.tick(100 + 10 * 60), "progress reset the watchdog");
        assert!(q.wants_tx());
        assert!(q.tick(100 + 11 * 60 + 1), "…but it still fires when they stop replying");
        assert!(q.tx_watchdog());
    }

    #[test]
    fn calling_a_station_that_never_answers_gives_up() {
        let cfg = DigiConfig { max_tx_repeats: 3, ..cfg() };
        let mut q = QsoMachine::new(Mode::Ft8, cfg);
        q.start_qso("W9XYZ".into(), Some("EM48".into()), -10, false, 100);
        for i in 0..2 {
            q.note_tx_sent(100 + i * 15);
            assert_eq!(q.step(), QsoStep::TxGrid, "still calling after {} sends", i + 1);
        }
        q.note_tx_sent(130);
        assert_eq!(q.step(), QsoStep::Idle, "gave up after the third unanswered call");
        assert!(!q.wants_tx());
        let note = q.status(false).transcript.pop().expect("a note");
        assert!(note.text.contains("W9XYZ") && note.text.contains("3 calls"), "{}", note.text);
    }

    #[test]
    fn repeating_a_cq_is_not_an_unanswered_call() {
        // A CQ run repeats by design; only the watchdog bounds it.
        let cfg = DigiConfig { max_tx_repeats: 3, ..cfg() };
        let mut q = QsoMachine::new(Mode::Ft8, cfg);
        q.call_cq();
        for _ in 0..10 {
            q.note_tx_sent(100);
        }
        assert_eq!(q.step(), QsoStep::CallingCq);
        assert!(q.wants_tx());
    }

    #[test]
    fn waitcq_times_out() {
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), None, -10, true, 100);
        assert!(!q.tick(200)); // within the window
        assert!(q.tick(100 + WAIT_CQ_S)); // deadline reached → give up
        assert_eq!(q.step(), QsoStep::Idle);
    }

    #[test]
    fn confirming_resends_final_when_dx_repeats() {
        // Answerer: after our 73 we re-send it if the DX repeats their RR73.
        let mut q = QsoMachine::new(Mode::Ft8, cfg());
        q.start_qso("W9XYZ".into(), Some("EM48".into()), -10, false, 100);
        q.on_rx(&[decode("AB1CD W9XYZ -13")], 115);
        q.on_rx(&[decode("AB1CD W9XYZ RR73")], 130);
        q.note_tx_sent(145); // 73 sent → Confirming
        assert_eq!(q.step(), QsoStep::Confirming);
        assert!(!q.wants_tx());

        // They repeat RR73 (didn't hear our 73) → one re-send is queued.
        assert!(q.on_rx(&[decode("AB1CD W9XYZ RR73")], 160));
        assert!(q.wants_tx());
        assert_eq!(q.plan_tx().as_deref(), Some("W9XYZ AB1CD 73"));

        // The re-send goes out → back to standby.
        q.note_tx_sent(175);
        assert!(!q.wants_tx());
        assert_eq!(q.step(), QsoStep::Confirming);

        // After the confirm window the contact is retired.
        assert!(q.tick(145 + CONFIRM_S));
        assert_eq!(q.step(), QsoStep::Idle);
    }
}
