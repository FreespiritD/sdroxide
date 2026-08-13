//! Yaesu "new CAT" (ASCII, `;`-terminated). Frequency `FA<n digits>;`, mode
//! `MD0<x>;`, PTT `TX1;`/`TX0;`, CW from the rig's own keyer (`KM`/`KY`).
//!
//! The one thing that is not the same across the family is how wide the
//! frequency field is: eight digits on the FT-450/950/2000 and FTDX1200/3000/
//! 5000 generation, nine on the FT-891/991/991A, FTDX10/101 and FT-710. A set
//! written at the wrong width is not rounded or clamped — the rig rejects the
//! whole command with `?;` and stays where it was, so a mismatch presents as a
//! radio that answers every poll and ignores every retune. Rather than ask the
//! operator which generation they own, [`Yaesu`] reads the width off the rig:
//! the answer to `FA;` carries the model's native digit count, so the first
//! poll reply settles it (this is what Hamlib derives from the `IF;` reply
//! length). Until one arrives the newer nine is assumed, and a set written
//! before the rig corrected us is re-issued — see [`Protocol::reframed`].

use crate::{CatUpdate, Protocol};
use sdroxide_types::Mode;
use tracing::{debug, info};

/// Frequency-field width assumed before the rig has answered a poll. The newer
/// generation, which is also the one still being sold.
const DEFAULT_WIDTH: usize = 9;

/// Keyer memory the CW text is written to before it is played. Yaesu has no
/// streaming keying command — the only way to key text is to store it and
/// trigger playback — so sending CW *overwrites whatever the operator had in
/// this memory*. Channel 1 is Hamlib's choice too, so the two agree about which
/// memory is the scratch one.
const CW_MEM: u8 = 1;

/// Longest message the rig's keyer memory holds.
const CW_MAX: usize = 50;

pub struct Yaesu {
    buf: String,
    /// Digits in the `FA`/`FB` frequency field on this particular rig.
    width: usize,
    /// True once the rig has told us its width, so a later reply that disagrees
    /// (there should be none) doesn't keep re-triggering a retune.
    width_known: bool,
    /// Set when [`Self::width`] changed under a frame we had already written.
    reframed: bool,
}

impl Yaesu {
    pub fn new() -> Self {
        Yaesu { buf: String::new(), width: DEFAULT_WIDTH, width_known: false, reframed: false }
    }

    /// Adopt the frequency-field width the rig just demonstrated.
    fn learn_width(&mut self, digits: usize) {
        // Only the two widths the family actually uses; anything else is a
        // malformed reply and must not redefine how we address the rig.
        if !(8..=9).contains(&digits) || (self.width_known && digits == self.width) {
            return;
        }
        if digits != self.width {
            info!(
                from = self.width,
                to = digits,
                "Yaesu CAT: rig uses a {digits}-digit frequency field"
            );
            self.width = digits;
            // Whatever we set before this was written at the wrong width, so the
            // rig rejected it and is still on its old frequency.
            self.reframed = true;
        }
        self.width_known = true;
    }
}

impl Default for Yaesu {
    fn default() -> Self {
        Yaesu::new()
    }
}

fn mode_digit(m: Mode) -> char {
    // Yaesu MD map (FT-891/991 family): 1=LSB 2=USB 3=CW 4=FM 5=AM
    // 6=RTTY-L 7=CW-R 8=DATA-L 9=RTTY-U A=DATA-FM B=FM-N C=DATA-U
    match m {
        Mode::Lsb => '1',
        Mode::Cw => '3',
        Mode::Nfm | Mode::Wfm => '4',
        // RIFP keys the carrier itself: data over FM, not over a sideband.
        Mode::Rifp => 'A',
        Mode::Am | Mode::Sam | Mode::Dsb => '5',
        Mode::Digl => '8',
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
        | Mode::Rade => 'C',
        Mode::Usb | Mode::Spec | Mode::Sstv | Mode::Wefax | Mode::RfPaint => '2',
    }
}

/// A mode digit the rig reported → the app's mode.
///
/// Deliberately not the inverse of [`mode_digit`] over its whole range. Only
/// the digits that mean one thing on both sides are followed; the rig's
/// RTTY-L/RTTY-U/DATA-FM positions map onto app modes that would be commanded
/// back as a *different* digit, and the two would then take turns correcting
/// each other. `None` leaves the app's mode alone, which is the right answer
/// for a rig position sdroxide has no equivalent of.
fn mode_from_digit(d: char) -> Option<Mode> {
    Some(match d.to_ascii_uppercase() {
        '1' => Mode::Lsb,
        '2' => Mode::Usb,
        '3' | '7' => Mode::Cw, // CW and CW-R are both CW to the app
        '4' | 'B' => Mode::Nfm,
        '5' => Mode::Am,
        '8' => Mode::Digl,
        'C' => Mode::Digu,
        _ => return None,
    })
}

/// Reduce `text` to what a Yaesu keyer memory will accept: upper case, the
/// letters, digits and punctuation the rig can key, and nothing longer than the
/// memory holds. A character the rig would refuse is dropped rather than sent,
/// because `KM` is all-or-nothing — one bad byte and the rig rejects the whole
/// message and sends none of it.
fn keyer_text(text: &str) -> String {
    text.trim()
        .chars()
        // A line break is a word break, not nothing: dropping it would run the
        // end of one line into the start of the next and send them as one word.
        .map(|c| if c.is_ascii_whitespace() { ' ' } else { c.to_ascii_uppercase() })
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '/' | '?' | '.' | ','))
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

impl Protocol for Yaesu {
    fn set_freq(&mut self, hz: f64) -> Vec<u8> {
        let hz = hz.round().max(0.0) as u64;
        format!("FA{hz:0width$};", width = self.width).into_bytes()
    }
    fn set_mode(&mut self, m: Mode) -> Vec<u8> {
        format!("MD0{};", mode_digit(m)).into_bytes()
    }
    fn ptt(&self, on: bool) -> Vec<u8> {
        if on { b"TX1;".to_vec() } else { b"TX0;".to_vec() }
    }
    fn poll_requests(&self) -> Vec<Vec<u8>> {
        vec![b"FA;".to_vec(), b"MD0;".to_vec()]
    }
    fn clear_offsets(&self) -> Vec<Vec<u8>> {
        // Clarifier to zero, RIT/XIT off, transmit back on VFO-A (split off) —
        // sdroxide puts all three on the dial itself. `FT2` rather than the
        // `ST0` other families use: this generation has no `ST` command, and a
        // split the rig was still holding would transmit us a VFO away.
        vec![b"RC;".to_vec(), b"RT0;".to_vec(), b"XT0;".to_vec(), b"FT2;".to_vec()]
    }

    fn cw_chunk_len(&self) -> usize {
        CW_MAX
    }

    fn send_cw(&mut self, text: &str) -> Vec<Vec<u8>> {
        let msg = keyer_text(text);
        if msg.is_empty() {
            return Vec::new();
        }
        vec![
            // Break-in has to be on or the keyer runs into the sidetone and
            // never keys the transmitter — the exact "CW does nothing" this
            // path exists to fix. Idempotent, so it rides with every chunk.
            b"BI1;".to_vec(),
            format!("KM{CW_MEM}{msg};").into_bytes(),
            // Playback of a *stored text* memory is the 6..A range; 1..5 plays
            // the paddle-recorded memories, which is not what `KM` wrote.
            // (The FT-710 is the exception and uses 1..5 — it is also the one
            // model of the family sdroxide has no other reason to special-case,
            // so it is left for a report from someone holding one.)
            format!("KY{};", CW_MEM + 5).into_bytes(),
        ]
    }

    fn set_cw_wpm(&mut self, wpm: f32) -> Vec<Vec<u8>> {
        // The rig keys at its own speed, so the panel's WPM has to be its WPM.
        let wpm = wpm.round().clamp(4.0, 60.0) as u32;
        vec![format!("KS{wpm:03};").into_bytes()]
    }

    fn reframed(&mut self) -> bool {
        std::mem::take(&mut self.reframed)
    }

    fn parse(&mut self, buf: &mut Vec<u8>) -> Vec<CatUpdate> {
        // Accumulate ASCII, split on ';'.
        self.buf.push_str(&String::from_utf8_lossy(buf));
        buf.clear();
        let mut out = Vec::new();
        while let Some(idx) = self.buf.find(';') {
            let msg: String = self.buf.drain(..=idx).collect();
            let msg = msg.trim_end_matches(';').trim();
            if let Some(rest) = msg.strip_prefix("FA") {
                if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                    // The reply's own width is the rig telling us how wide its
                    // frequency field is; learn it before anything else so the
                    // next set is addressed the way this rig expects.
                    self.learn_width(rest.len());
                    if let Ok(hz) = rest.parse::<u64>() {
                        out.push(CatUpdate::Freq(hz as f64));
                    }
                }
            } else if let Some(rest) = msg.strip_prefix("MD") {
                // `MD<P1><P2>` — P1 selects the VFO (always 0 here), P2 is the
                // mode itself.
                if let Some(m) = rest.chars().nth(1).and_then(mode_from_digit) {
                    out.push(CatUpdate::Mode(m));
                }
            } else if msg == "?" {
                // The rig refused the last command. Nothing identifies which,
                // so this can only be a breadcrumb — but it is the difference
                // between "the radio is ignoring me" and silence.
                debug!("Yaesu CAT: rig rejected a command (?)");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(y: &mut Yaesu, s: &str) -> Vec<CatUpdate> {
        let mut buf = s.as_bytes().to_vec();
        y.parse(&mut buf)
    }

    #[test]
    fn frequency_width_is_learned_from_the_rigs_own_reply() {
        let mut y = Yaesu::new();
        // Nothing heard yet: the newer nine-digit form.
        assert_eq!(y.set_freq(14_074_000.0), b"FA014074000;".to_vec());
        assert!(!y.reframed());

        // An FTDX1200-generation rig answers `FA;` with eight digits. Every set
        // written so far was rejected, so the caller is told to re-issue.
        let updates = parse_str(&mut y, "FA07055000;");
        assert_eq!(updates, vec![CatUpdate::Freq(7_055_000.0)]);
        assert!(y.reframed(), "a width change invalidates the frames already written");
        assert!(!y.reframed(), "and is reported exactly once");
        assert_eq!(y.set_freq(14_074_000.0), b"FA14074000;".to_vec());

        // Later replies at the width we already learned are not a change.
        parse_str(&mut y, "FA14074000;");
        assert!(!y.reframed());
    }

    #[test]
    fn a_nine_digit_rig_confirms_the_assumed_width_without_a_retune() {
        let mut y = Yaesu::new();
        assert_eq!(parse_str(&mut y, "FA014074000;"), vec![CatUpdate::Freq(14_074_000.0)]);
        assert!(!y.reframed(), "the assumption was right; nothing to re-issue");
        assert_eq!(y.set_freq(7_055_000.0), b"FA007055000;".to_vec());
    }

    #[test]
    fn a_malformed_reply_does_not_redefine_the_frequency_field() {
        let mut y = Yaesu::new();
        // Neither of these is a width the family uses.
        parse_str(&mut y, "FA1407400;"); // 7 digits
        parse_str(&mut y, "FA0140740000;"); // 10
        assert!(!y.reframed());
        assert_eq!(y.set_freq(14_074_000.0), b"FA014074000;".to_vec());
        // Nor is a non-numeric payload a frequency at all.
        assert!(parse_str(&mut y, "FAxxxxxxxxx;").is_empty());
    }

    #[test]
    fn mode_replies_are_followed_only_where_both_sides_agree() {
        let mut y = Yaesu::new();
        assert_eq!(parse_str(&mut y, "MD03;"), vec![CatUpdate::Mode(Mode::Cw)]);
        assert_eq!(parse_str(&mut y, "MD0C;"), vec![CatUpdate::Mode(Mode::Digu)]);
        assert_eq!(parse_str(&mut y, "MD01;"), vec![CatUpdate::Mode(Mode::Lsb)]);
        // RTTY-L/-U and DATA-FM have no round-trip-stable app equivalent.
        assert!(parse_str(&mut y, "MD06;").is_empty());
        assert!(parse_str(&mut y, "MD09;").is_empty());
        // A rejection is not a mode.
        assert!(parse_str(&mut y, "?;").is_empty());
    }

    #[test]
    fn replies_split_across_reads_are_reassembled() {
        let mut y = Yaesu::new();
        assert!(parse_str(&mut y, "FA0140").is_empty());
        assert_eq!(parse_str(&mut y, "74000;"), vec![CatUpdate::Freq(14_074_000.0)]);
    }

    #[test]
    fn cw_text_is_stored_then_played() {
        let mut y = Yaesu::new();
        let frames = y.send_cw("cq de w1aw");
        let sent: Vec<String> =
            frames.iter().map(|f| String::from_utf8_lossy(f).into_owned()).collect();
        assert_eq!(sent, vec!["BI1;", "KM1CQ DE W1AW;", "KY6;"]);
    }

    #[test]
    fn keyer_text_keeps_only_what_the_rig_can_key() {
        // Upper-cased, prosign punctuation the memory has no character for
        // dropped, runs of spaces collapsed, edges trimmed.
        assert_eq!(keyer_text("  r r = tu <sk>  "), "R R TU SK");
        assert_eq!(keyer_text("w1aw/p ur 599 ok?"), "W1AW/P UR 599 OK?");
        // A semicolon would end the frame early; it never reaches the wire.
        assert_eq!(keyer_text("de;w1aw"), "DEW1AW");
        // A line break is a word break — dropping it would send one word.
        assert_eq!(keyer_text("tnx fer call\nur 599"), "TNX FER CALL UR 599");
        // Nothing sendable produces no frames at all rather than an empty
        // memory write.
        assert!(keyer_text("   ").is_empty());
        assert!(Yaesu::new().send_cw("<>").is_empty());
        // Longer than the memory holds is truncated, not refused.
        assert_eq!(keyer_text(&"a".repeat(80)).len(), CW_MAX);
    }

    #[test]
    fn keyer_speed_is_three_digits_within_the_rigs_range() {
        let mut y = Yaesu::new();
        let frame = |y: &mut Yaesu, wpm: f32| String::from_utf8(y.set_cw_wpm(wpm)[0].clone());
        assert_eq!(frame(&mut y, 20.0).unwrap(), "KS020;");
        assert_eq!(frame(&mut y, 4.0).unwrap(), "KS004;");
        // The panel offers speeds the rig's keyer does not go to.
        assert_eq!(frame(&mut y, 80.0).unwrap(), "KS060;");
        assert_eq!(frame(&mut y, 1.0).unwrap(), "KS004;");
    }

    #[test]
    fn split_is_cleared_with_the_command_this_generation_has() {
        let frames: Vec<String> = Yaesu::new()
            .clear_offsets()
            .iter()
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        assert_eq!(frames, vec!["RC;", "RT0;", "XT0;", "FT2;"]);
    }
}
