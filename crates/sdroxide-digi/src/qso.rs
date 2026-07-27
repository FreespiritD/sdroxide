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

    /// Start calling CQ.
    pub fn call_cq(&mut self) {
        self.dx = None;
        self.transcript.clear();
        self.final_msg = None;
        self.resend = false;
        self.step = QsoStep::CallingCq;
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
    }

    /// Hard reset.
    pub fn abort(&mut self) {
        self.step = QsoStep::Idle;
        self.dx = None;
    }

    /// True while we intend to transmit this cycle. `WaitCq` holds silently
    /// until the DX calls CQ; `Confirming` transmits only when a re-send of our
    /// final message is queued.
    pub fn wants_tx(&self) -> bool {
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
        let dx_call = self.dx.as_ref().map(|d| d.call.clone());
        let answered_us = dx_call.as_deref().is_some_and(|dx| {
            decodes
                .iter()
                .any(|d| d.from.as_deref() == Some(dx) && d.to.as_deref() == Some(my_call.as_str()))
        });
        let mut changed = false;
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

            // Calling CQ and someone answers → adopt them as the DX.
            if self.step == QsoStep::CallingCq && self.dx.is_none() {
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
            changed |= self.advance(&payload, now_utc);
        }
        changed
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
