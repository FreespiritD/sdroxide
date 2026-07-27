//! The mfsk-core adapter: FT8/FT4 decode and encode wrapped behind stable
//! sdroxide types. **This is the only file that touches raw mfsk-core
//! decode-result fields** — if the crate's field names change, only
//! `decode_slot` needs updating.

use mfsk_core::msg::hash_table::CallsignHashTable;
use mfsk_core::msg::wsjt77;
use sdroxide_types::{Decode, Mode};

use crate::params::{AUDIO_MAX_HZ, AUDIO_MIN_HZ};

const SYNC_MIN: f32 = 1.5;
const MAX_CAND: usize = 120;

/// Which of the 77-bit message layouts a decode came from, read straight from
/// the `i3`/`n3` type bits. Guessing this from the text can't work — free text
/// is allowed to look exactly like a standard message ("W1AW RR73" is both a
/// valid free-text string and a valid exchange), so the bits decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MsgKind {
    /// 13 characters of arbitrary text — no addressee, no sender.
    FreeText,
    /// DXpedition (Fox): `CALL1 RR73; CALL2 <fox> REPORT`.
    Fox,
    /// ARRL Field Day: `CALL1 CALL2 [FD]`.
    FieldDay,
    /// The everyday exchange: `<to> <from> <grid|report|RRR|RR73|73>`.
    Standard,
    /// ARRL RTTY Roundup: `[TU; ]<to> <from> [R] RST <state|serial>`.
    RttyRu,
    /// One non-standard (compound / long) callsign plus one hashed one.
    NonStandard,
    /// A layout this build doesn't model.
    Other,
}

/// Read the `i3` (bits 74–76) and `n3` (bits 71–73) message-type fields.
fn msg_kind(bits77: &[u8; 77]) -> MsgKind {
    let field = |start: usize| (bits77[start] & 1) << 2 | (bits77[start + 1] & 1) << 1 | (bits77[start + 2] & 1);
    match (field(74), field(71)) {
        (0, 0) => MsgKind::FreeText,
        (0, 1) => MsgKind::Fox,
        (0, 3 | 4) => MsgKind::FieldDay,
        (1 | 2, _) => MsgKind::Standard,
        (3, _) => MsgKind::RttyRu,
        (4, _) => MsgKind::NonStandard,
        _ => MsgKind::Other,
    }
}

/// Encode/decode engine for one digital mode.
pub struct Ft8Modem {
    mode: Mode,
    /// Callsigns heard this session, so the `<...>` placeholders in
    /// non-standard-callsign and DXpedition messages resolve to real calls.
    hashes: CallsignHashTable,
}

impl Ft8Modem {
    pub fn new(mode: Mode) -> Self {
        Ft8Modem { mode, hashes: CallsignHashTable::new() }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Register callsigns we already know (ours, and the station we're
    /// working), so the first hashed message naming them resolves instead of
    /// showing `<...>`.
    pub fn seed_hashes(&mut self, calls: &[String]) {
        for c in calls.iter().filter(|c| !c.is_empty()) {
            self.hashes.insert(c);
        }
    }

    /// Decode one full receive slot of 12 kHz mono i16 audio.
    ///
    /// FT8 and FT4 return different mfsk-core result types, so each is mapped
    /// to our stable [`Decode`] inside its own arm — the only place that
    /// reads raw mfsk-core fields.
    ///
    /// Takes `&mut self` because every callsign heard is folded into the hash
    /// table, which is what lets a later `<...>` message name a real station.
    pub fn decode_slot(&mut self, audio_12k: &[i16], slot_utc: i64) -> Vec<Decode> {
        let mode = self.mode;
        let ht = &self.hashes;
        let decodes: Vec<Decode> = match mode {
            Mode::Ft4 => {
                let depth = mfsk_core::core::pipeline::DecodeDepth::BpAllOsd;
                mfsk_core::ft4::decode::decode_frame_with_options(
                    audio_12k, AUDIO_MIN_HZ, AUDIO_MAX_HZ, SYNC_MIN, None, depth, MAX_CAND,
                )
                .into_iter()
                .filter_map(|r| {
                    let bits: [u8; 77] = r.message77().try_into().ok()?;
                    build_decode(&bits, r.snr_db, r.dt_sec, r.freq_hz, slot_utc, ht)
                })
                .collect()
            }
            _ => {
                let depth = mfsk_core::ft8::decode::DecodeDepth::BpAllOsd;
                mfsk_core::ft8::decode::decode_frame(
                    audio_12k, AUDIO_MIN_HZ, AUDIO_MAX_HZ, SYNC_MIN, None, depth, MAX_CAND,
                )
                .into_iter()
                .filter_map(|r| {
                    build_decode(&r.message77, r.snr_db, r.dt_sec, r.freq_hz, slot_utc, ht)
                })
                .collect()
            }
        };
        // Remember who we heard, for the next slot's hashed messages.
        for d in &decodes {
            for call in [d.to.as_deref(), d.from.as_deref()].into_iter().flatten() {
                self.hashes.insert(call);
            }
        }
        decodes
    }

    /// Synthesize a message into 12 kHz mono f32 burst audio at tone offset
    /// `audio_hz`. Returns the audio and the message *as it will be received* —
    /// a form FT8 can't carry in full (a report to a compound call, an
    /// over-long free-text line) is degraded rather than dropped, and the
    /// caller logs what actually went out. `None` if nothing can be packed.
    pub fn encode_burst_12k(
        &self,
        text: &str,
        audio_hz: f32,
        amplitude: f32,
    ) -> Option<(Vec<f32>, String)> {
        let (msg77, sent) = pack_message(text)?;
        let audio = match self.mode {
            Mode::Ft4 => {
                let tones = mfsk_core::ft4::encode::message_to_tones(&msg77);
                mfsk_core::ft4::encode::tones_to_f32(&tones, audio_hz, amplitude)
            }
            _ => {
                let tones = mfsk_core::ft8::wave_gen::message_to_tones(&msg77);
                mfsk_core::ft8::wave_gen::tones_to_f32(&tones, audio_hz, amplitude)
            }
        };
        Some((audio, sent))
    }
}

/// Pack `text` into a 77-bit message, degrading to the nearest layout FT8 can
/// carry, and report the message as the far end will read it.
///
/// The ladder is: the standard exchange, then the non-standard-callsign layout
/// (which can only carry `RRR` / `RR73` / `73`, so a grid or report is
/// dropped), then 13 characters of free text.
fn pack_message(text: &str) -> Option<([u8; 77], String)> {
    let text = text.trim().to_ascii_uppercase();
    let toks: Vec<&str> = text.split_whitespace().collect();
    let (c1, c2, payload) = (
        toks.first().copied().unwrap_or(""),
        toks.get(1).copied().unwrap_or(""),
        toks.get(2).copied().unwrap_or(""),
    );

    // 1. The everyday message, when both calls are standard.
    if toks.len() <= 3 {
        if let Some(m) = wsjt77::pack77(c1, c2, payload) {
            return Some((m, join3(c1, c2, payload)));
        }
    }

    // 2. One compound / non-standard callsign, the other one hashed. Only a
    //    bare RRR / RR73 / 73 fits alongside it — a grid or report is lost, so
    //    the returned text says so.
    let rpt = match payload {
        "RRR" | "RR73" | "73" => payload,
        _ => "",
    };
    if c1 == "CQ" && !c2.is_empty() && !wsjt77::is_standard_callsign(c2) {
        let m = wsjt77::pack77_type4(c2, "", "", true)?;
        return Some((m, format!("CQ {c2}")));
    }
    let nonstd_first = !c1.is_empty() && !wsjt77::is_standard_callsign(c1) && c1 != "CQ";
    let nonstd_second = !c2.is_empty() && !wsjt77::is_standard_callsign(c2);
    if nonstd_first != nonstd_second {
        let (nonstd, std_call) = if nonstd_first { (c1, c2) } else { (c2, c1) };
        if wsjt77::is_valid_callsign(nonstd) && wsjt77::is_standard_callsign(std_call) {
            if let Some(mut m) = wsjt77::pack77_type4(nonstd, std_call, rpt, false) {
                // Bit 70 (`iflip`) decides which call is read first. mfsk-core
                // always hashes-first, which is the layout for *being* the
                // non-standard station; addressing one needs the other order.
                m[70] = u8::from(nonstd_first);
                let sent = if nonstd_first {
                    join3(nonstd, &format!("<{std_call}>"), rpt)
                } else {
                    join3(&format!("<{std_call}>"), nonstd, rpt)
                };
                return Some((m, sent));
            }
        }
    }

    // 3. Free text: 13 characters, from a restricted alphabet.
    let free: String = text.chars().take(13).collect();
    let m = wsjt77::pack77_free_text(free.trim_end())?;
    Some((m, free.trim_end().to_string()))
}

/// Join up to three message tokens, dropping the empty ones.
fn join3(a: &str, b: &str, c: &str) -> String {
    [a, b, c].iter().filter(|t| !t.is_empty()).copied().collect::<Vec<_>>().join(" ")
}

/// Unpack 77 message bits and build a [`Decode`], or `None` if unpacking fails.
/// `hashes` resolves the `<...>` placeholders of hashed callsigns.
fn build_decode(
    bits77: &[u8; 77],
    snr_db: f32,
    dt_sec: f32,
    freq_hz: f32,
    slot_utc: i64,
    hashes: &CallsignHashTable,
) -> Option<Decode> {
    let text = wsjt77::unpack77_with_hash(bits77, hashes)?;
    let p = parse_message(&text, msg_kind(bits77));
    Some(Decode {
        slot_utc,
        snr_db: snr_db.round() as i16,
        dt: dt_sec,
        audio_hz: freq_hz,
        message: text,
        to: p.to,
        from: p.from,
        grid: p.grid,
        is_cq: p.is_cq,
        cq_dx: p.cq_dx,
        free_text: p.free_text,
    })
}

/// The addressing a decoded message carries.
#[derive(Debug, Default, PartialEq)]
struct Parsed {
    to: Option<String>,
    from: Option<String>,
    grid: Option<String>,
    is_cq: bool,
    cq_dx: bool,
    free_text: bool,
}

/// Pull the addressee, sender and grid out of a decoded message.
///
/// Every layout except free text is `<to> <from> [payload…]` once its
/// decorations are stripped, so the shared tail handles them all; the kind
/// (from the type bits, never guessed from the text) decides which decorations
/// to strip and whether there is any addressing at all.
fn parse_message(text: &str, kind: MsgKind) -> Parsed {
    // Free text carries no addressing, however much it may look like it does.
    if kind == MsgKind::FreeText {
        return Parsed { free_text: true, ..Default::default() };
    }
    let mut toks: Vec<&str> = text.split_whitespace().collect();
    // "TU; " opens an ARRL RTTY Roundup exchange; the rest is ordinary.
    if toks.first() == Some(&"TU;") {
        toks.remove(0);
    }
    if toks.is_empty() {
        return Parsed::default();
    }

    // A CQ names only its sender: "CQ [DX|POTA|EU…] <from> [grid]".
    if toks[0] == "CQ" {
        return Parsed {
            from: toks.iter().skip(1).find(|t| is_callish(t)).and_then(|t| call_of(t)),
            grid: toks.last().filter(|t| is_grid(t)).map(|s| s.to_string()),
            is_cq: true,
            cq_dx: toks.get(1) == Some(&"DX"),
            ..Default::default()
        };
    }

    // DXpedition (Fox): "CALL1 RR73; CALL2 <fox> REPORT" — CALL1's contact is
    // finished and CALL2 is being worked, both by the (hashed) fox. Read it as
    // the fox addressing CALL2; CALL1's RR73 is not ours to act on.
    if toks.get(1) == Some(&"RR73;") {
        return Parsed {
            to: toks.get(2).and_then(|t| call_of(t)),
            from: toks.get(3).and_then(|t| call_of(t)),
            ..Default::default()
        };
    }

    // Standard, RTTY Roundup, Field Day and non-standard-callsign layouts all
    // put the addressee first and the sender second.
    Parsed {
        to: toks.first().and_then(|t| call_of(t)),
        from: toks.get(1).and_then(|t| call_of(t)),
        // A grid can arrive bare ("FN42") or rogered ("R FN42").
        grid: match toks.get(2) {
            Some(&"R") => toks.get(3),
            other => other,
        }
        .filter(|t| is_grid(t))
        .map(|s| s.to_string()),
        ..Default::default()
    }
}

/// The callsign a message token names: `<...>` is a hash nobody has resolved
/// (no callsign at all), `<W1AW>` is a resolved one, anything else is itself.
fn call_of(tok: &str) -> Option<String> {
    let inner = tok.strip_prefix('<').and_then(|t| t.strip_suffix('>')).unwrap_or(tok);
    (!inner.is_empty() && inner != "...").then(|| inner.to_string())
}

fn is_grid(t: &str) -> bool {
    // "RR73" is a sign-off, not a locator, though it is syntactically a valid
    // grid (and would plot at ~83°N/175°E). Never treat it as a position.
    if t == "RR73" {
        return false;
    }
    let b = t.as_bytes();
    b.len() == 4
        && (b'A'..=b'R').contains(&b[0].to_ascii_uppercase()) // Maidenhead fields A..R
        && (b'A'..=b'R').contains(&b[1].to_ascii_uppercase())
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
}

fn is_callish(t: &str) -> bool {
    let t = t.strip_prefix('<').and_then(|x| x.strip_suffix('>')).unwrap_or(t);
    t.len() >= 3 && t.chars().any(|c| c.is_ascii_digit()) && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '/')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a message, pad it into a full slot, and decode it back — the
    /// round trip every on-air message actually makes.
    fn round_trip(mode: Mode, text: &str) -> (String, Vec<Decode>) {
        let mut modem = Ft8Modem::new(mode);
        let (burst, sent) = modem.encode_burst_12k(text, 1500.0, 0.5).expect("encode");
        let slot_s = if mode == Mode::Ft4 { 7.5 } else { 15.0 };
        let mut slot = vec![0.0f32; (0.5 * 12_000.0) as usize];
        slot.extend_from_slice(&burst);
        slot.resize((slot_s * 12_000.0) as usize, 0.0);
        let i16buf: Vec<i16> = slot.iter().map(|&s| (s * 20_000.0) as i16).collect();
        (sent, modem.decode_slot(&i16buf, 0))
    }

    #[test]
    fn parse_cq_and_qso_messages() {
        let p = parse_message("CQ AB1CD FN42", MsgKind::Standard);
        assert_eq!(p.to, None);
        assert_eq!(p.from.as_deref(), Some("AB1CD"));
        assert_eq!(p.grid.as_deref(), Some("FN42"));
        assert!(p.is_cq && !p.cq_dx);

        let p = parse_message("W9XYZ AB1CD -13", MsgKind::Standard);
        assert_eq!(p.to.as_deref(), Some("W9XYZ"));
        assert_eq!(p.from.as_deref(), Some("AB1CD"));
        assert_eq!(p.grid, None);
        assert!(!p.is_cq);

        let p = parse_message("AB1CD W9XYZ EM48", MsgKind::Standard);
        assert_eq!(p.to.as_deref(), Some("AB1CD"));
        assert_eq!(p.from.as_deref(), Some("W9XYZ"));
        assert_eq!(p.grid.as_deref(), Some("EM48"));

        // A rogered grid ("R FN42") still places the station.
        let p = parse_message("AB1CD W9XYZ R EM48", MsgKind::Standard);
        assert_eq!(p.grid.as_deref(), Some("EM48"));

        // "CQ DX": the caller only wants stations outside their own entity.
        let p = parse_message("CQ DX AB1CD FN42", MsgKind::Standard);
        assert_eq!(p.to, None);
        assert_eq!(p.from.as_deref(), Some("AB1CD"), "the DX modifier is not the sender");
        assert_eq!(p.grid.as_deref(), Some("FN42"));
        assert!(p.is_cq && p.cq_dx);

        // "RR73" is a sign-off, not a locator — it must not be read as a grid.
        let p = parse_message("AB1CD W9XYZ RR73", MsgKind::Standard);
        assert_eq!(p.from.as_deref(), Some("W9XYZ"));
        assert_eq!(p.grid, None, "RR73 must not parse as a grid position");
        // Invalid Maidenhead fields (S..Z) aren't grids either.
        assert!(!is_grid("ZZ99"));
        assert!(is_grid("FN42"));
    }

    #[test]
    fn free_text_carries_no_addressing() {
        // Free text may look exactly like an exchange — only the type bits know
        // the difference, and reading a sender out of it would aim the QSO
        // machine at a station that never transmitted.
        let p = parse_message("W9XYZ RR73", MsgKind::FreeText);
        assert!(p.free_text);
        assert_eq!((p.to, p.from, p.grid), (None, None, None));
        assert!(!p.is_cq);
        // The same text as a real message does address someone.
        let p = parse_message("W9XYZ RR73", MsgKind::Standard);
        assert_eq!(p.to.as_deref(), Some("W9XYZ"));
    }

    #[test]
    fn hashed_and_non_standard_callsigns_parse() {
        // An unresolved hash is nobody: better no callsign than "<...>".
        let p = parse_message("<...> DL/W1AW RR73", MsgKind::NonStandard);
        assert_eq!(p.to, None);
        assert_eq!(p.from.as_deref(), Some("DL/W1AW"));

        // A resolved one names the station it stands for.
        let p = parse_message("DL/W1AW <AB1CD> 73", MsgKind::NonStandard);
        assert_eq!(p.to.as_deref(), Some("DL/W1AW"));
        assert_eq!(p.from.as_deref(), Some("AB1CD"));

        // A non-standard call can call CQ too.
        let p = parse_message("CQ DL/W1AW", MsgKind::NonStandard);
        assert!(p.is_cq);
        assert_eq!(p.from.as_deref(), Some("DL/W1AW"));
    }

    #[test]
    fn contest_and_dxpedition_layouts_parse() {
        // ARRL RTTY Roundup: the "TU;" opener is not the addressee.
        let p = parse_message("TU; K1ABC W9XYZ R 589 MA", MsgKind::RttyRu);
        assert_eq!(p.to.as_deref(), Some("K1ABC"));
        assert_eq!(p.from.as_deref(), Some("W9XYZ"));
        assert_eq!(p.grid, None);

        // Field Day.
        let p = parse_message("K1ABC W9XYZ [FD]", MsgKind::FieldDay);
        assert_eq!(p.to.as_deref(), Some("K1ABC"));
        assert_eq!(p.from.as_deref(), Some("W9XYZ"));

        // DXpedition (Fox): the fox is working W9XYZ; K1ABC's contact is done.
        let p = parse_message("K1ABC RR73; W9XYZ <DX1FOX> +03", MsgKind::Fox);
        assert_eq!(p.to.as_deref(), Some("W9XYZ"));
        assert_eq!(p.from.as_deref(), Some("DX1FOX"), "the fox sent it");
    }

    #[test]
    fn ft8_encode_decode_round_trip() {
        let (sent, decodes) = round_trip(Mode::Ft8, "CQ AB1CD FN42");
        assert_eq!(sent, "CQ AB1CD FN42");
        assert!(
            decodes.iter().any(|d| d.message == "CQ AB1CD FN42" && !d.free_text),
            "got {:?}",
            decodes.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ft4_encode_decode_round_trip() {
        let (_, decodes) = round_trip(Mode::Ft4, "CQ AB1CD FN42");
        assert!(
            decodes.iter().any(|d| d.message == "CQ AB1CD FN42"),
            "got {:?}",
            decodes.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn free_text_goes_out_as_free_text() {
        // Not an exchange, so it can only travel as 13 characters of text.
        // Checked at the packer rather than through a decode: mfsk-core's
        // decoder drops any message whose words aren't callsign-shaped (its
        // guard against CRC-14 false decodes), which takes real free text with
        // it — so what we can verify here is that we transmit the right thing.
        let (bits, sent) = pack_message("TNX QSO 73 GL").expect("packs");
        assert_eq!(sent, "TNX QSO 73 GL");
        assert_eq!(msg_kind(&bits), MsgKind::FreeText);
        assert_eq!(wsjt77::unpack77(&bits).as_deref(), Some("TNX QSO 73 GL"));

        // Over-long text is cut to the 13 characters FT8 carries, and the
        // caller is told what actually went out.
        let (_, sent) = pack_message("THANKS FOR THE CONTACT").expect("packs");
        assert_eq!(sent, "THANKS FOR TH");
    }

    #[test]
    fn a_compound_call_is_addressable() {
        // The everyday layout can't hold "DL/W1AW", so the message degrades to
        // the non-standard form: our call travels as a hash, and the report is
        // dropped — the returned text says exactly what went out.
        let (sent, decodes) = round_trip(Mode::Ft8, "DL/W1AW AB1CD RR73");
        assert_eq!(sent, "DL/W1AW <AB1CD> RR73");
        let d = decodes.iter().find(|d| d.message.contains("DL/W1AW")).expect("decoded");
        assert_eq!(d.to.as_deref(), Some("DL/W1AW"), "the compound call is the addressee");
        assert_eq!(d.from, None, "our call is hashed, and this receiver has never heard it");

        // A receiver that has heard us resolves the hash to our callsign.
        let mut modem = Ft8Modem::new(Mode::Ft8);
        modem.seed_hashes(&["AB1CD".to_string()]);
        let (burst, _) = modem.encode_burst_12k("DL/W1AW AB1CD RR73", 1500.0, 0.5).unwrap();
        let mut slot = vec![0.0f32; 6_000];
        slot.extend_from_slice(&burst);
        slot.resize(15 * 12_000, 0.0);
        let buf: Vec<i16> = slot.iter().map(|&s| (s * 20_000.0) as i16).collect();
        let d = modem
            .decode_slot(&buf, 0)
            .into_iter()
            .find(|d| d.message.contains("DL/W1AW"))
            .expect("decoded");
        assert_eq!(d.from.as_deref(), Some("AB1CD"));
        assert_eq!(d.message, "DL/W1AW <AB1CD> RR73");
    }

    #[test]
    fn a_compound_call_can_call_cq() {
        let (sent, decodes) = round_trip(Mode::Ft8, "CQ DL/W1AW");
        assert_eq!(sent, "CQ DL/W1AW");
        let d = decodes.iter().find(|d| d.message.contains("DL/W1AW")).expect("decoded");
        assert!(d.is_cq);
        assert_eq!(d.from.as_deref(), Some("DL/W1AW"));
    }
}
