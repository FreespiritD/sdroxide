//! What to answer, and — mostly — what not to.
//!
//! This is the only place in the JS8 implementation that decides to transmit
//! without an operator asking, so it is deliberately conservative. Every rule
//! here exists to stop the station doing something on the air that its operator
//! would not have chosen:
//!
//! * Only queries **addressed to us** are answered. A query to `@ALLCALL` is
//!   addressed to us; one to another callsign is not, however clearly we heard
//!   it.
//! * Only the commands upstream marks auto-repliable (`autoreply_cmds`) are
//!   answered at all. Everything else is displayed and left to the operator.
//! * We never answer ourselves. One misconfiguration — an operator's own
//!   callsign arriving via a relay, or a loopback — would otherwise become a
//!   two-station beacon with no off switch.
//! * Answering is off unless the operator turned it on.
//!
//! Relay (`>`), the message store (` MSG TO:`, ` QUERY MSGS`) and the APRS
//! gateway are decoded and displayed but never acted on. They are out of scope
//! by choice, not oversight: each one makes this station responsible for
//! traffic it did not originate.

use sdroxide_types::Js8Msg;

use super::frame::{AUTOREPLY_CMDS, Directed, cmd_code};

/// What a received message is asking of us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Js8Intent {
    /// "What is my signal report?"
    SnrQuery,
    /// "What is your grid?"
    GridQuery,
    /// "Who are you hearing?"
    HearingQuery,
    /// "What is your status message?"
    StatusQuery,
    /// "Say again."
    RepeatQuery,
    /// A station announcing itself.
    Heartbeat,
    /// A call for contacts.
    Cq,
    /// Something we understand but do not answer.
    Other,
}

/// Station facts an auto-reply may quote.
#[derive(Debug, Clone, Default)]
pub struct StationInfo {
    pub call: String,
    pub grid: String,
    /// Free-text status the operator set, sent in reply to ` STATUS?`.
    pub status: String,
    /// Callsigns heard recently, most recent first, for ` HEARING?`.
    pub hearing: Vec<String>,
}

/// Policy switches, straight from the operator's settings.
#[derive(Debug, Clone, Copy)]
pub struct ReplyPolicy {
    /// Master switch. Off means this module never produces a transmission.
    pub auto_reply: bool,
}

impl Default for ReplyPolicy {
    fn default() -> Self {
        // Answering queries is the useful default and is what makes a JS8
        // station worth leaving on; beaconing is not, and lives elsewhere.
        ReplyPolicy { auto_reply: true }
    }
}

/// Classify a received message.
pub fn intent(msg: &Js8Msg) -> Js8Intent {
    match msg.cmd.as_deref() {
        Some("SNR?") => Js8Intent::SnrQuery,
        Some("GRID?") => Js8Intent::GridQuery,
        Some("HEARING?") => Js8Intent::HearingQuery,
        Some("STATUS?") => Js8Intent::StatusQuery,
        Some("AGN?") => Js8Intent::RepeatQuery,
        Some("HB") => Js8Intent::Heartbeat,
        Some("CQ") => Js8Intent::Cq,
        _ => Js8Intent::Other,
    }
}

/// The reply this message deserves, if any.
///
/// Returns the directed frame to transmit. `None` means "say nothing", which is
/// the answer for the large majority of received traffic.
pub fn auto_reply(msg: &Js8Msg, me: &StationInfo, policy: ReplyPolicy) -> Option<Directed> {
    if !policy.auto_reply || me.call.is_empty() {
        return None;
    }
    // Not for us, not our problem. `to_me` already covers @ALLCALL and groups.
    if !msg.to_me || msg.from.is_empty() {
        return None;
    }
    // The loop guard. Worth keeping even though the sender should never be us:
    // relays and loopbacks make it possible, and the failure mode is a station
    // that transmits forever.
    if msg.from.eq_ignore_ascii_case(&me.call) {
        return None;
    }
    // Only complete messages — replying to half a query invites replying twice.
    if !msg.complete {
        return None;
    }

    let (cmd_text, body) = match intent(msg) {
        Js8Intent::SnrQuery => (" SNR", Some(msg.snr_db.to_string())),
        Js8Intent::GridQuery => (" GRID", Some(me.grid.clone())),
        Js8Intent::StatusQuery => (" STATUS", Some(me.status.clone())),
        Js8Intent::HearingQuery => (" HEARING", Some(me.hearing.join(" "))),
        // A heartbeat or CQ is an announcement, not a question. Answering every
        // one would flood the band, which is precisely what heartbeats exist to
        // avoid.
        _ => return None,
    };

    let cmd = cmd_code(cmd_text)?;
    // Belt and braces: refuse anything upstream does not mark auto-repliable,
    // so widening the match above cannot quietly widen what we transmit.
    let query_cmd = msg.cmd.as_deref().and_then(|c| cmd_code(&format!(" {c}")))?;
    if !AUTOREPLY_CMDS.contains(&query_cmd) {
        return None;
    }

    // An empty answer is worse than none — it says "I am here" while answering
    // nothing, and costs a full transmission to do it.
    if body.as_deref().is_some_and(str::is_empty) {
        return None;
    }

    Some(Directed {
        from: me.call.clone(),
        to: msg.from.clone(),
        cmd,
        num: if matches!(intent(msg), Js8Intent::SnrQuery) {
            Some(i32::from(msg.snr_db))
        } else {
            None
        },
        portable_from: false,
        portable_to: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> StationInfo {
        StationInfo {
            call: "N0JDS".into(),
            grid: "FN42".into(),
            status: "IDLE".into(),
            hearing: vec!["KN4CRD".into(), "VK3ABC".into()],
        }
    }

    fn query(from: &str, to: &str, cmd: &str, to_me: bool) -> Js8Msg {
        Js8Msg {
            from: from.into(),
            to: to.into(),
            text: String::new(),
            cmd: Some(cmd.into()),
            snr_db: -7,
            audio_hz: 1500.0,
            first_slot_utc: 1000,
            last_slot_utc: 1000,
            frames: 1,
            complete: true,
            to_me,
        }
    }

    #[test]
    fn a_query_addressed_to_us_is_answered() {
        for (cmd, expect) in
            [("SNR?", " SNR"), ("GRID?", " GRID"), ("STATUS?", " STATUS"), ("HEARING?", " HEARING")]
        {
            let m = query("KN4CRD", "N0JDS", cmd, true);
            let r = auto_reply(&m, &me(), ReplyPolicy::default())
                .unwrap_or_else(|| panic!("{cmd} went unanswered"));
            assert_eq!(r.to, "KN4CRD");
            assert_eq!(r.from, "N0JDS");
            assert_eq!(super::super::frame::cmd_text(r.cmd), Some(expect));
        }
    }

    #[test]
    fn a_query_for_someone_else_is_ignored() {
        let m = query("KN4CRD", "VK3ABC", "SNR?", false);
        assert!(auto_reply(&m, &me(), ReplyPolicy::default()).is_none());
    }

    #[test]
    fn an_allcall_query_is_answered() {
        // @ALLCALL reaches us, and the assembler sets `to_me` accordingly.
        let m = query("KN4CRD", "@ALLCALL", "SNR?", true);
        assert!(auto_reply(&m, &me(), ReplyPolicy::default()).is_some());
    }

    #[test]
    fn we_never_answer_ourselves() {
        // The loop that turns one misconfiguration into an unattended beacon.
        let m = query("N0JDS", "N0JDS", "SNR?", true);
        assert!(auto_reply(&m, &me(), ReplyPolicy::default()).is_none());
        let m = query("n0jds", "N0JDS", "GRID?", true);
        assert!(auto_reply(&m, &me(), ReplyPolicy::default()).is_none(), "case must not matter");
    }

    #[test]
    fn auto_reply_is_off_when_the_operator_turned_it_off() {
        let m = query("KN4CRD", "N0JDS", "SNR?", true);
        assert!(auto_reply(&m, &me(), ReplyPolicy { auto_reply: false }).is_none());
    }

    #[test]
    fn a_station_with_no_callsign_stays_silent() {
        // Transmitting without a callsign is not legal anywhere.
        let m = query("KN4CRD", "N0JDS", "SNR?", true);
        let anon = StationInfo { call: String::new(), ..me() };
        assert!(auto_reply(&m, &anon, ReplyPolicy::default()).is_none());
    }

    #[test]
    fn announcements_are_not_answered() {
        // Heartbeats and CQs are announcements. Replying to every one would
        // flood exactly the band heartbeats exist to keep quiet.
        for cmd in ["HB", "CQ"] {
            let m = query("KN4CRD", "@ALLCALL", cmd, true);
            assert!(auto_reply(&m, &me(), ReplyPolicy::default()).is_none(), "{cmd}");
        }
    }

    #[test]
    fn commands_upstream_does_not_auto_reply_to_are_left_alone() {
        for cmd in ["QSL", "73", "SK", "RR", "FB"] {
            let m = query("KN4CRD", "N0JDS", cmd, true);
            assert!(auto_reply(&m, &me(), ReplyPolicy::default()).is_none(), "{cmd}");
        }
    }

    #[test]
    fn relay_and_message_store_requests_are_shown_but_not_acted_on() {
        // Explicitly out of scope: each makes this station responsible for
        // traffic it did not originate.
        for cmd in [">", "MSG TO:", "QUERY MSGS"] {
            let m = query("KN4CRD", "N0JDS", cmd, true);
            assert!(auto_reply(&m, &me(), ReplyPolicy::default()).is_none(), "{cmd}");
        }
    }

    #[test]
    fn a_half_received_query_is_not_answered_yet() {
        let mut m = query("KN4CRD", "N0JDS", "SNR?", true);
        m.complete = false;
        assert!(auto_reply(&m, &me(), ReplyPolicy::default()).is_none());
    }

    #[test]
    fn an_snr_reply_carries_the_report_we_measured() {
        let m = query("KN4CRD", "N0JDS", "SNR?", true);
        let r = auto_reply(&m, &me(), ReplyPolicy::default()).expect("answered");
        assert_eq!(r.num, Some(-7));
    }

    #[test]
    fn we_say_nothing_rather_than_answer_emptily() {
        // A station with no grid set should not transmit " GRID" and nothing.
        let m = query("KN4CRD", "N0JDS", "GRID?", true);
        let blank = StationInfo { grid: String::new(), ..me() };
        assert!(auto_reply(&m, &blank, ReplyPolicy::default()).is_none());

        let m = query("KN4CRD", "N0JDS", "HEARING?", true);
        let deaf = StationInfo { hearing: Vec::new(), ..me() };
        assert!(auto_reply(&m, &deaf, ReplyPolicy::default()).is_none());
    }

    #[test]
    fn intent_classifies_the_commands_we_act_on() {
        assert_eq!(intent(&query("A", "B", "SNR?", true)), Js8Intent::SnrQuery);
        assert_eq!(intent(&query("A", "B", "GRID?", true)), Js8Intent::GridQuery);
        assert_eq!(intent(&query("A", "B", "QSL", true)), Js8Intent::Other);
    }
}
