//! Kenwood CAT (ASCII, `;`-terminated). Frequency `FA<11 digits>;`, mode
//! `MD<x>;`, PTT `TX…;`/`RX;`, CW streamed to the rig's own keyer (`KY`).
//!
//! Close enough to Yaesu's "new CAT" to look like the same protocol and far
//! enough away that pointing the Yaesu family at a Kenwood fails in every
//! direction at once — which is how this file came to exist. The differences
//! that matter:
//!
//! * **Frequency is eleven digits**, not the family-dependent eight or nine.
//!   A Yaesu-width `FA` is a syntax error the rig answers with `?;`, so the
//!   dial never moves.
//! * **`MD` has no VFO parameter.** Yaesu's read (`MD0;`) is a Kenwood *set*
//!   to mode 0, which is the documented "setting failure" value.
//! * **`TX0;` transmits.** Yaesu unkeys with it. A Kenwood told `TX0;` keys up
//!   and stays there, because the command that unkeys it is `RX;`.
//! * **`FT2;`** — Yaesu's "split off" — selects the memory channel as transmit
//!   VFO on a Kenwood, which the reference guide explicitly forbids.
//!
//! DATA mode is a flag beside the mode (`DA`) rather than a mode of its own, so
//! USB and USB-DATA are both `MD2;` and only `DA;` tells them apart.
//!
//! Written from Kenwood's public *PC Control Command Reference Guide* for the
//! TS-590S/TS-590SG (B5A-0316-20/01) and the TS-2000 command tables. Not yet
//! verified against a rig.

use crate::{CatUpdate, Protocol};
use sdroxide_types::{KenwoodSend, Mode};
use tracing::debug;

/// Digits in the `FA`/`FB` frequency field. Fixed across the family, unlike
/// Yaesu — "enter 00014195000 for 14.195 MHz. Blank digits must be entered
/// as 0."
const FREQ_DIGITS: usize = 11;

/// Characters the `KY` buffer holds in one go.
const CW_MAX: usize = 24;

/// Non-alphanumeric characters the keyer accepts: the reference guide's own
/// table, then the six symbols that stand for prosigns. A semicolon is not
/// among them, which is what keeps operator text from ending the frame early.
const KEYER_PUNCTUATION: &str = " '\"()*+,-./:=?@[_<>]\\";

/// How many mode replies to wait through before concluding the rig has no `DA`
/// command. A TS-2000-generation rig has no DATA mode and answers `?;`, which
/// is indistinguishable from any other rejection — but a rig that *does* have
/// it answers the very first poll, so silence across a few rounds settles it.
const DATA_PROBE_POLLS: u32 = 3;

pub struct Kenwood {
    buf: String,
    /// Which `TX` form keys this rig — see [`KenwoodSend`].
    send: KenwoodSend,
    /// Mode digit from the rig's last `MD;` reply.
    mode_digit: Option<char>,
    /// Whether the rig last reported DATA mode on.
    data_on: bool,
    /// True once the rig has answered a `DA;`, i.e. it has a DATA mode at all.
    da_seen: bool,
    /// `MD;` replies seen, counted only until [`DATA_PROBE_POLLS`].
    md_replies: u32,
}

impl Kenwood {
    pub fn new(send: KenwoodSend) -> Self {
        Kenwood {
            buf: String::new(),
            send,
            mode_digit: None,
            data_on: false,
            da_seen: false,
            md_replies: 0,
        }
    }

    /// Whether to keep spending frames on `DA`. A rig that has answered one is
    /// asked forever; one that has stayed silent through [`DATA_PROBE_POLLS`]
    /// mode replies hasn't got the command, and asking again only earns another
    /// `?;` every poll for as long as the station is on.
    fn data_supported(&self) -> bool {
        self.da_seen || self.md_replies < DATA_PROBE_POLLS
    }

    /// The rig's mode and DATA flag combined into the app's mode.
    ///
    /// Deliberately not the inverse of [`Self::mode_frames`] over its whole
    /// range. A rig position that would be commanded back as something else
    /// yields `None` — the two would otherwise take turns correcting each
    /// other. That covers FSK/FSK-R (sdroxide's RTTY is its own modem in a data
    /// sideband, not the rig's FSK) and the DATA flavours of FM and AM, which
    /// have no round-trip-stable app equivalent.
    fn effective_mode(&self) -> Option<Mode> {
        Some(match (self.mode_digit?, self.data_on) {
            ('1', false) => Mode::Lsb,
            ('1', true) => Mode::Digl,
            ('2', false) => Mode::Usb,
            ('2', true) => Mode::Digu,
            // CW and CW-R are both CW to the app, and neither carries DATA.
            ('3' | '7', _) => Mode::Cw,
            ('4', false) => Mode::Nfm,
            ('5', false) => Mode::Am,
            _ => return None,
        })
    }
}

/// The rig's mode digit for an app mode, and whether DATA rides on top of it.
/// Kenwood `MD`: 1=LSB 2=USB 3=CW 4=FM 5=AM 6=FSK 7=CW-R 9=FSK-R (0 and 8 are
/// documented as setting failures).
fn mode_digit(m: Mode) -> (char, bool) {
    match m {
        Mode::Lsb => ('1', false),
        Mode::Cw => ('3', false),
        Mode::Nfm | Mode::Wfm => ('4', false),
        // RIFP keys the carrier itself: data over FM, not over a sideband.
        Mode::Rifp => ('4', true),
        Mode::Am | Mode::Sam | Mode::Dsb => ('5', false),
        Mode::Digl => ('1', true),
        Mode::Digu
        | Mode::Ft8
        | Mode::Js8
        | Mode::Wspr
        | Mode::Ft4
        | Mode::Ft2
        | Mode::Psk
        | Mode::Rtty
        | Mode::Olivia
        | Mode::Thor
        | Mode::Fsq
        | Mode::Hell
        | Mode::Rade => ('2', true),
        Mode::Usb | Mode::Spec | Mode::Sstv | Mode::Wefax | Mode::RfPaint => ('2', false),
    }
}

/// Reduce `text` to what the `KY` buffer will accept: the letters, digits and
/// punctuation listed in the reference guide, and nothing longer than the
/// buffer holds.
///
/// The bracketed prosigns the CW panel writes become the single symbols the rig
/// keys them as — `<SK>` is `>` to a Kenwood, not the two letters S and K.
fn keyer_text(text: &str) -> String {
    let mut s = text.trim().to_ascii_uppercase();
    // Documented abbreviation table: the symbol *is* the prosign.
    for (token, symbol) in
        [("<BT>", "["), ("<AR>", "_"), ("<AS>", "<"), ("<SK>", ">"), ("<KN>", "]"), ("<BK>", "\\")]
    {
        if s.contains(token) {
            s = s.replace(token, symbol);
        }
    }
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || KEYER_PUNCTUATION.contains(*c))
        // Collapse the runs of spaces a trimmed chunk boundary can leave behind.
        .scan(false, |prev_space, c| {
            let space = c == ' ';
            let keep = !(space && *prev_space);
            *prev_space = space;
            Some(keep.then_some(c))
        })
        .flatten()
        .take(CW_MAX)
        .collect()
}

impl Kenwood {
    /// `MD` for `m`, followed by the `DA` that selects or clears DATA mode.
    ///
    /// `MD` goes first: `DA` is an error in CW and FSK, so the rig has to be in
    /// a mode that accepts it before it arrives. Modes that carry no DATA flag
    /// send no `DA` at all rather than an explicit `DA0;`, which in CW would be
    /// exactly that error.
    fn mode_frames(&self, m: Mode) -> Vec<u8> {
        let (digit, data) = mode_digit(m);
        let mut out = format!("MD{digit};").into_bytes();
        // CW (3) and CW-R (7) reject `DA` outright; FSK is never commanded here.
        if self.data_supported() && !matches!(digit, '3' | '7') {
            out.extend_from_slice(if data { b"DA1;" } else { b"DA0;" });
        }
        out
    }
}

impl Protocol for Kenwood {
    fn set_freq(&mut self, hz: f64) -> Vec<u8> {
        let hz = hz.round().clamp(0.0, 99_999_999_999.0) as u64;
        format!("FA{hz:0FREQ_DIGITS$};").into_bytes()
    }

    fn set_mode(&mut self, m: Mode) -> Vec<u8> {
        self.mode_frames(m)
    }

    fn ptt(&self, on: bool) -> Vec<u8> {
        if on {
            match self.send {
                KenwoodSend::Standard => b"TX;".to_vec(),
                KenwoodSend::Data => b"TX1;".to_vec(),
            }
        } else {
            // Never `TX0;` — that is a *transmit* command on this family.
            b"RX;".to_vec()
        }
    }

    fn poll_requests(&self) -> Vec<Vec<u8>> {
        let mut reqs = vec![b"FA;".to_vec(), b"MD;".to_vec()];
        if self.data_supported() {
            reqs.push(b"DA;".to_vec());
        }
        reqs
    }

    fn clear_offsets(&self) -> Vec<Vec<u8>> {
        vec![
            // Stop unsolicited status the rig would otherwise push at us — we
            // poll, and a previous program may have left auto-information on.
            b"AI0;".to_vec(),
            // Clear the clarifier while it is still on: `RC` is an error once
            // RIT and XIT are both off, so it cannot come after them.
            b"RC;".to_vec(),
            b"RT0;".to_vec(),
            b"XT0;".to_vec(),
            // Receive on VFO A, which also returns the rig to simplex — the
            // documented side effect of `FR`, and the only split-off this
            // family has. It doubles as the fix for a rig left on VFO B or in
            // memory mode, where every `FA` we send would land on a VFO that
            // isn't the one being listened to.
            b"FR0;".to_vec(),
        ]
    }

    fn cw_chunk_len(&self) -> usize {
        CW_MAX
    }

    fn send_cw(&mut self, text: &str) -> Vec<Vec<u8>> {
        let msg = keyer_text(text);
        if msg.is_empty() {
            return Vec::new();
        }
        let mut frames = Vec::new();
        // Break-in has to be on or the keyer runs into the sidetone and never
        // keys the transmitter. `VX` is that switch only while the rig is in CW
        // — in any other mode it is the VOX switch, and turning VOX on under a
        // station whose sound card is live would key the rig on its own audio.
        // So it goes out only once the rig has told us it is in CW.
        if matches!(self.mode_digit, Some('3') | Some('7')) {
            frames.push(b"VX1;".to_vec());
        }
        // One space between `KY` and the text — the documented P1, and the
        // difference between keying and a syntax error.
        frames.push(format!("KY {msg};").into_bytes());
        frames
    }

    fn abort_cw(&mut self) -> Vec<Vec<u8>> {
        // `KY0;` — the documented stop for a message in progress.
        vec![b"KY0;".to_vec()]
    }

    fn set_cw_wpm(&mut self, wpm: f32) -> Vec<Vec<u8>> {
        let wpm = wpm.round().clamp(4.0, 60.0) as u32;
        vec![format!("KS{wpm:03};").into_bytes()]
    }

    fn parse(&mut self, buf: &mut Vec<u8>) -> Vec<CatUpdate> {
        self.buf.push_str(&String::from_utf8_lossy(buf));
        buf.clear();
        let mut out = Vec::new();
        // USB and USB-DATA are the same `MD`, so a mode is only whole once both
        // replies are in. Emitting per-reply would report plain USB for the
        // moment between them every poll.
        let mut mode_touched = false;
        while let Some(idx) = self.buf.find(';') {
            let msg: String = self.buf.drain(..=idx).collect();
            let msg = msg.trim_end_matches(';').trim();
            if let Some(rest) = msg.strip_prefix("FA") {
                if rest.len() == FREQ_DIGITS
                    && let Ok(hz) = rest.parse::<u64>()
                {
                    out.push(CatUpdate::Freq(hz as f64));
                }
            } else if let Some(rest) = msg.strip_prefix("MD") {
                if let Some(d) = rest.chars().next() {
                    self.mode_digit = Some(d);
                    self.md_replies = self.md_replies.saturating_add(1).min(DATA_PROBE_POLLS);
                    mode_touched = true;
                }
            } else if let Some(rest) = msg.strip_prefix("DA") {
                if let Some(d) = rest.chars().next() {
                    self.data_on = d == '1';
                    self.da_seen = true;
                    mode_touched = true;
                }
            } else if msg == "?" {
                // The rig refused the last command. Nothing identifies which,
                // so this can only be a breadcrumb — but it is the difference
                // between "the radio is ignoring me" and silence.
                debug!("Kenwood CAT: rig rejected a command (?)");
            } else if msg == "E" || msg == "O" {
                // Framing/overrun (`E`) or a receive-buffer overrun (`O`) — the
                // serial line itself, not the command.
                debug!("Kenwood CAT: serial error from rig ({msg})");
            }
        }
        if mode_touched && let Some(m) = self.effective_mode() {
            out.push(CatUpdate::Mode(m));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kenwood() -> Kenwood {
        Kenwood::new(KenwoodSend::Standard)
    }

    fn parse_str(k: &mut Kenwood, s: &str) -> Vec<CatUpdate> {
        let mut buf = s.as_bytes().to_vec();
        k.parse(&mut buf)
    }

    fn frames(v: Vec<Vec<u8>>) -> Vec<String> {
        v.iter().map(|f| String::from_utf8_lossy(f).into_owned()).collect()
    }

    #[test]
    fn frequency_is_eleven_digits() {
        let mut k = kenwood();
        assert_eq!(k.set_freq(14_074_000.0), b"FA00014074000;".to_vec());
        assert_eq!(k.set_freq(7_055_000.0), b"FA00007055000;".to_vec());
        // A reply at that width is a frequency; anything else is not, and must
        // not be read as one at some other scale.
        assert_eq!(parse_str(&mut k, "FA00014195000;"), vec![CatUpdate::Freq(14_195_000.0)]);
        assert!(parse_str(&mut k, "FA014074000;").is_empty()); // Yaesu's nine
        assert!(parse_str(&mut k, "FAxxxxxxxxxxx;").is_empty());
    }

    #[test]
    fn unkeying_uses_rx_because_tx0_would_transmit() {
        let k = kenwood();
        assert_eq!(k.ptt(true), b"TX;".to_vec());
        assert_eq!(k.ptt(false), b"RX;".to_vec());
        // The data-input SEND, for rigs that route transmit audio by which TX
        // command keyed them.
        assert_eq!(Kenwood::new(KenwoodSend::Data).ptt(true), b"TX1;".to_vec());
        assert_eq!(Kenwood::new(KenwoodSend::Data).ptt(false), b"RX;".to_vec());
    }

    #[test]
    fn data_mode_rides_beside_the_mode_not_inside_it() {
        let mut k = kenwood();
        // `MD` first: `DA` is an error in a mode that has no DATA flavour.
        assert_eq!(k.set_mode(Mode::Digu), b"MD2;DA1;".to_vec());
        assert_eq!(k.set_mode(Mode::Usb), b"MD2;DA0;".to_vec());
        assert_eq!(k.set_mode(Mode::Digl), b"MD1;DA1;".to_vec());
        // CW rejects `DA` outright, so none is sent.
        assert_eq!(k.set_mode(Mode::Cw), b"MD3;".to_vec());
    }

    #[test]
    fn usb_and_usb_data_are_told_apart_by_the_data_flag() {
        let mut k = kenwood();
        // Both replies arrive in one poll; the mode is reported once, whole.
        assert_eq!(parse_str(&mut k, "MD2;DA1;"), vec![CatUpdate::Mode(Mode::Digu)]);
        assert_eq!(parse_str(&mut k, "MD2;DA0;"), vec![CatUpdate::Mode(Mode::Usb)]);
        assert_eq!(parse_str(&mut k, "MD1;DA1;"), vec![CatUpdate::Mode(Mode::Digl)]);
        assert_eq!(parse_str(&mut k, "MD3;DA0;"), vec![CatUpdate::Mode(Mode::Cw)]);
        // CW-R is CW to the app.
        assert_eq!(parse_str(&mut k, "MD7;"), vec![CatUpdate::Mode(Mode::Cw)]);
    }

    #[test]
    fn rig_positions_without_a_stable_round_trip_are_left_alone() {
        let mut k = kenwood();
        // FSK / FSK-R: sdroxide's RTTY is its own modem in a data sideband and
        // would be commanded back as `MD2;DA1;`.
        assert!(parse_str(&mut k, "MD6;DA0;").is_empty());
        assert!(parse_str(&mut k, "MD9;DA0;").is_empty());
        // Documented setting-failure values.
        assert!(parse_str(&mut k, "MD0;").is_empty());
        assert!(parse_str(&mut k, "MD8;").is_empty());
        // FM-DATA and AM-DATA have no app equivalent, but plain FM and AM do.
        assert!(parse_str(&mut k, "MD4;DA1;").is_empty());
        assert_eq!(parse_str(&mut k, "MD4;DA0;"), vec![CatUpdate::Mode(Mode::Nfm)]);
        assert!(parse_str(&mut k, "MD5;DA1;").is_empty());
        assert_eq!(parse_str(&mut k, "MD5;DA0;"), vec![CatUpdate::Mode(Mode::Am)]);
    }

    #[test]
    fn every_app_mode_the_rig_can_hold_survives_a_round_trip() {
        // What we command has to be what we read back, or the app and the rig
        // spend the session correcting each other.
        for m in [Mode::Lsb, Mode::Usb, Mode::Cw, Mode::Nfm, Mode::Am, Mode::Digl, Mode::Digu] {
            let mut k = kenwood();
            let sent = String::from_utf8(k.set_mode(m)).unwrap();
            // Echo the set back as the rig's replies would read.
            let (digit, data) = mode_digit(m);
            let reply = format!("MD{digit};DA{};", u8::from(data));
            assert_eq!(
                parse_str(&mut k, &reply),
                vec![CatUpdate::Mode(m)],
                "{m:?} was set with {sent} and read back as something else"
            );
        }
    }

    #[test]
    fn a_rig_without_a_data_command_stops_being_asked() {
        let mut k = kenwood();
        assert!(frames(k.poll_requests()).contains(&"DA;".to_string()));
        // A TS-2000-generation rig answers the mode and rejects `DA`.
        for _ in 0..DATA_PROBE_POLLS {
            assert_eq!(parse_str(&mut k, "MD2;?;"), vec![CatUpdate::Mode(Mode::Usb)]);
        }
        assert_eq!(frames(k.poll_requests()), vec!["FA;", "MD;"]);
        // And no longer carries a `DA` it would only reject again.
        assert_eq!(k.set_mode(Mode::Digu), b"MD2;".to_vec());
    }

    #[test]
    fn a_rig_that_answers_da_keeps_being_asked() {
        let mut k = kenwood();
        for _ in 0..DATA_PROBE_POLLS + 2 {
            parse_str(&mut k, "MD2;DA0;");
        }
        assert!(frames(k.poll_requests()).contains(&"DA;".to_string()));
    }

    #[test]
    fn replies_split_across_reads_are_reassembled() {
        let mut k = kenwood();
        assert!(parse_str(&mut k, "FA000140").is_empty());
        assert_eq!(parse_str(&mut k, "74000;"), vec![CatUpdate::Freq(14_074_000.0)]);
    }

    #[test]
    fn cw_is_streamed_to_the_keyer_with_break_in_only_when_in_cw() {
        let mut k = kenwood();
        // Mode unknown: `VX` would be the VOX switch, so it is not sent.
        assert_eq!(frames(k.send_cw("cq de w1aw")), vec!["KY CQ DE W1AW;"]);
        // In USB it is still the VOX switch.
        parse_str(&mut k, "MD2;DA0;");
        assert_eq!(frames(k.send_cw("test")), vec!["KY TEST;"]);
        // In CW it is break-in, which the keyer needs.
        parse_str(&mut k, "MD3;");
        assert_eq!(frames(k.send_cw("test")), vec!["VX1;", "KY TEST;"]);
    }

    #[test]
    fn keyer_text_keeps_only_what_the_rig_can_key() {
        // Upper-cased, runs of spaces collapsed, edges trimmed.
        assert_eq!(keyer_text("  r r  tu  "), "R R TU");
        // `=` is BT and is in the rig's own character table, so it goes as is.
        assert_eq!(keyer_text("r r = tu"), "R R = TU");
        // Bracketed prosigns become the single symbols the rig keys them as.
        assert_eq!(keyer_text("tu <sk>"), "TU >");
        assert_eq!(keyer_text("<bt> <ar> <as> <kn> <bk>"), "[ _ < ] \\");
        // A semicolon would end the frame early; it never reaches the wire.
        assert_eq!(keyer_text("de;w1aw"), "DEW1AW");
        assert_eq!(keyer_text("w1aw/p ur 599 ok?"), "W1AW/P UR 599 OK?");
        // Nothing sendable produces no frames at all rather than an empty `KY`.
        assert!(keyer_text("   ").is_empty());
        assert!(kenwood().send_cw("~~~").is_empty());
        // Longer than the buffer holds is truncated, not refused.
        assert_eq!(keyer_text(&"a".repeat(80)).len(), CW_MAX);
    }

    #[test]
    fn keyer_speed_is_three_digits_within_the_rigs_range() {
        let mut k = kenwood();
        let frame = |k: &mut Kenwood, wpm: f32| String::from_utf8(k.set_cw_wpm(wpm)[0].clone());
        assert_eq!(frame(&mut k, 20.0).unwrap(), "KS020;");
        // The panel offers speeds the rig's keyer does not go to.
        assert_eq!(frame(&mut k, 80.0).unwrap(), "KS060;");
        assert_eq!(frame(&mut k, 1.0).unwrap(), "KS004;");
    }

    #[test]
    fn the_rigs_own_offsets_are_cleared_in_an_order_it_accepts() {
        // `RC` before `RT0`/`XT0`: it is an error once both are already off.
        // `FR0` last, and never `FT2` — that is Yaesu's split-off and a
        // forbidden memory-channel select here.
        assert_eq!(frames(kenwood().clear_offsets()), vec!["AI0;", "RC;", "RT0;", "XT0;", "FR0;"]);
    }
}
