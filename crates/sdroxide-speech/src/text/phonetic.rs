//! Callsigns, grid squares, and the letters they are made of.
//!
//! A callsign is the one token in an announcement that must not be misheard.
//! Everything else the operator can ask for again; a wrong callsign is a wrong
//! log entry. So the default is the ITU phonetic alphabet, at about two seconds
//! for a six-character call, which is the right trade for a message addressed
//! to you.

use sdroxide_types::CallsignStyle;

use super::numbers::digit;

/// ITU/NATO phonetics. `juliett` keeps the ITU spelling with two Ts, which is
/// also how [`crate::lexicon`] holds its pronunciation.
const NATO: [&str; 26] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliett",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
];

/// Letter names, for [`CallsignStyle::Letters`]. Spelled the way they are said
/// rather than the way they are written, so the dictionary can pronounce them —
/// `aitch`, `doubleyou` and `eff` are in [`crate::lexicon`], the rest are
/// ordinary words.
const LETTERS: [&str; 26] = [
    "ay",
    "bee",
    "see",
    "dee",
    "ee",
    "eff",
    "gee",
    "aitch",
    "eye",
    "jay",
    "kay",
    "el",
    "em",
    "en",
    "oh",
    "pee",
    "cue",
    "are",
    "ess",
    "tee",
    "you",
    "vee",
    "doubleyou",
    "ex",
    "why",
    "zee",
];

/// One letter, in the requested style. Empty for anything not A–Z.
pub fn letter(ch: char, style: CallsignStyle) -> &'static str {
    let Some(i) = (ch.to_ascii_uppercase() as u8).checked_sub(b'A') else {
        return "";
    };
    let i = i as usize;
    if i >= 26 {
        return "";
    }
    match style {
        CallsignStyle::Phonetic => NATO[i],
        // `AsIs` never reaches here — the caller hands the raw text over
        // instead — but a letter name is a better fallback than nothing.
        CallsignStyle::Letters | CallsignStyle::AsIs => LETTERS[i],
    }
}

/// Spell a callsign out.
///
/// `/` becomes "stroke", the Region 1 reading, which is what the rest of this
/// program assumes throughout its band plan. Digits are ordinary digit words,
/// so `9A1AAA` opens "niner"-free with plain "nine" — the aviation forms buy
/// nothing here and cost familiarity.
pub fn callsign(call: &str, style: CallsignStyle) -> String {
    // `AsIs` hands the letters over as written and lets the voice do what it
    // will with them — but the digits still become words. Leaving them as
    // digits would not read them faster, it would drop them: the phonemizer
    // has no entry for `1`, so "K1ABC" would be spoken as "kabc".
    if style == CallsignStyle::AsIs {
        let mut out: Vec<String> = Vec::new();
        let mut run = String::new();
        for ch in call.chars() {
            match ch {
                'a'..='z' | 'A'..='Z' => run.push(ch.to_ascii_lowercase()),
                _ => {
                    if !run.is_empty() {
                        out.push(std::mem::take(&mut run));
                    }
                    match ch {
                        '0'..='9' => out.push(digit(ch as u8 - b'0').to_string()),
                        '/' => out.push("stroke".into()),
                        '-' => out.push("dash".into()),
                        _ => {}
                    }
                }
            }
        }
        if !run.is_empty() {
            out.push(run);
        }
        return out.join(" ");
    }

    let mut out: Vec<&str> = Vec::new();
    for ch in call.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' => out.push(letter(ch, style)),
            '0'..='9' => out.push(digit(ch as u8 - b'0')),
            '/' => out.push("stroke"),
            '-' => out.push("dash"),
            _ => {}
        }
    }
    out.join(" ")
}

/// Read a Maidenhead locator: `JN47ab` becomes
/// "juliett november four seven alpha bravo".
///
/// The field and square letters go through the same phonetics as a callsign —
/// a grid is logged and a wrong one is a wrong distance.
pub fn grid(grid: &str, style: CallsignStyle) -> String {
    callsign(grid, style)
}

/// Whether a token looks like an amateur callsign.
///
/// Deliberately loose. Getting this wrong in one direction spells a word out
/// letter by letter, which is merely slow; getting it wrong in the other reads
/// a callsign as a word, which is useless. Requiring both a letter and a digit
/// already excludes almost all English.
pub fn looks_like_callsign(token: &str) -> bool {
    let core = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/');
    if core.len() < 3 || core.len() > 12 {
        return false;
    }
    let mut has_alpha = false;
    let mut has_digit = false;
    for ch in core.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' => has_alpha = true,
            '0'..='9' => has_digit = true,
            '/' => {}
            _ => return false,
        }
    }
    has_alpha && has_digit
}

/// Whether a token looks like a 4- or 6-character Maidenhead locator.
pub fn looks_like_grid(token: &str) -> bool {
    let b = token.as_bytes();
    if b.len() != 4 && b.len() != 6 {
        return false;
    }
    let field = |c: u8| c.is_ascii_alphabetic() && c.to_ascii_uppercase() <= b'R';
    let sub = |c: u8| c.is_ascii_alphabetic() && c.to_ascii_lowercase() <= b'x';
    field(b[0])
        && field(b[1])
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && (b.len() == 4 || (sub(b[4]) && sub(b[5])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callsigns_in_every_style() {
        assert_eq!(callsign("K1ABC", CallsignStyle::Phonetic), "kilo one alpha bravo charlie");
        assert_eq!(callsign("K1ABC", CallsignStyle::Letters), "kay one ay bee see");
        // Even "as written" spells the digit: a bare `1` reaches a
        // dictionary that has no entry for it and simply vanishes.
        assert_eq!(callsign("K1ABC", CallsignStyle::AsIs), "k one abc");
    }

    #[test]
    fn portable_and_prefixed_calls() {
        assert_eq!(
            callsign("DL/K1ABC", CallsignStyle::Phonetic),
            "delta lima stroke kilo one alpha bravo charlie"
        );
        assert_eq!(callsign("9A1AAA", CallsignStyle::Phonetic), "nine alpha one alpha alpha alpha");
    }

    #[test]
    fn grids() {
        assert_eq!(grid("JN47", CallsignStyle::Phonetic), "juliett november four seven");
        assert_eq!(
            grid("JN47ab", CallsignStyle::Phonetic),
            "juliett november four seven alpha bravo"
        );
    }

    #[test]
    fn callsign_detection() {
        for yes in ["K1ABC", "9A1AAA", "DL/K1ABC", "VP2E", "W1AW", "OE3ABC"] {
            assert!(looks_like_callsign(yes), "{yes} should look like a callsign");
        }
        for no in ["CQ", "the", "hello", "73", "ab"] {
            assert!(!looks_like_callsign(no), "{no} should not look like a callsign");
        }
        // Shape alone cannot separate `RR73` from a callsign — it is letters
        // and a digit in a plausible length. That is why `Speaker::token`
        // consults the abbreviation table *before* asking this question.
        assert!(looks_like_callsign("RR73"));
    }

    #[test]
    fn grid_detection() {
        for yes in ["JN47", "JN47ab", "FN31pr", "AA00"] {
            assert!(looks_like_grid(yes), "{yes} should look like a grid");
        }
        for no in ["K1ABC", "ZZ99", "JN4", "JN47abc", "1234"] {
            assert!(!looks_like_grid(no), "{no} should not look like a grid");
        }
    }
}
