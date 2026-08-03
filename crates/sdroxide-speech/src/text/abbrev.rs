//! On-air shorthand, read the way an operator would say it.
//!
//! Applied to **free text only** — the body of an FT8 message, a JS8 or FSQ
//! message, a CW or RTTY tail. Never to phrases this crate generates itself,
//! which are already words.
//!
//! Without this pass, `73` reaches the phonemizer as a token no dictionary has
//! and disappears; `CQ` is read as a word; `RR73` becomes nothing at all. The
//! table is what turns a decoded line into something an operator recognises
//! hearing.

/// Multi-token entries, matched first and longest-first.
///
/// Only where the pair means something the words separately do not.
const PHRASES: &[(&str, &str)] = &[("hi hi", "hi hi"), ("cq dx", "C Q D X"), ("de", "from")];

/// Single tokens. Uppercase keys; lookup is case-insensitive.
///
/// Spaced capitals ("Q S L") are the way to make the phonemizer spell
/// something out: each letter goes through the dictionary as its own word.
const WORDS: &[(&str, &str)] = &[
    // Sign-off numbers. These are the ones that vanish without the table.
    ("73", "seventy three"),
    ("72", "seventy two"),
    ("88", "eighty eight"),
    ("RR73", "roger roger seventy three"),
    ("RRR", "roger roger roger"),
    ("RR", "roger roger"),
    // Q-codes and procedural signals: spelled, because that is how they are said.
    ("CQ", "C Q"),
    ("QSL", "Q S L"),
    ("QRZ", "Q R Z"),
    ("QSO", "Q S O"),
    ("QRM", "Q R M"),
    ("QRN", "Q R N"),
    ("QRT", "Q R T"),
    ("QRV", "Q R V"),
    ("QRX", "Q R X"),
    ("QSY", "Q S Y"),
    ("QTH", "Q T H"),
    ("QRP", "Q R P"),
    ("QRO", "Q R O"),
    ("QSB", "Q S B"),
    ("QRL", "Q R L"),
    ("RST", "R S T"),
    ("SNR", "S N R"),
    ("DX", "D X"),
    ("SK", "S K"),
    ("AR", "A R"),
    ("KN", "K N"),
    ("BK", "break"),
    // Ordinary shorthand.
    ("TU", "thank you"),
    ("TNX", "thanks"),
    ("TKS", "thanks"),
    ("PSE", "please"),
    ("UR", "your"),
    ("ES", "and"),
    ("AGN", "again"),
    ("ABT", "about"),
    ("HW", "how copy"),
    ("GM", "good morning"),
    ("GA", "good afternoon"),
    ("GE", "good evening"),
    ("GN", "good night"),
    ("GL", "good luck"),
    ("GUD", "good"),
    ("FB", "fine business"),
    ("SRI", "sorry"),
    ("CFM", "confirm"),
    ("RPT", "repeat"),
    ("WX", "weather"),
    ("PWR", "power"),
    ("ANT", "antenna"),
    ("RIG", "rig"),
    ("HR", "here"),
    ("NW", "now"),
    ("VY", "very"),
    ("WID", "with"),
    ("OM", "old man"),
    ("YL", "young lady"),
    ("XYL", "X Y L"),
    ("OP", "operator"),
    ("NAME", "name"),
    // Award and activity programmes.
    ("POTA", "pota"),
    ("SOTA", "sota"),
    ("IOTA", "iota"),
    ("WWFF", "W W F F"),
    ("SES", "special event station"),
];

/// The replacement for one token, if the table has one.
///
/// `K` is deliberately absent: on its own it means "over", but it is also a
/// callsign letter and a mode name, and the caller decides from position.
pub fn lookup(token: &str) -> Option<&'static str> {
    WORDS.iter().find(|(k, _)| k.eq_ignore_ascii_case(token)).map(|(_, v)| *v)
}

/// A multi-token replacement starting at `words[i]`, and how many it consumed.
pub fn lookup_phrase(words: &[&str], i: usize) -> Option<(&'static str, usize)> {
    for (key, val) in PHRASES {
        let n = key.split(' ').count();
        if i + n <= words.len() {
            let matches =
                key.split(' ').zip(&words[i..i + n]).all(|(k, w)| k.eq_ignore_ascii_case(w));
            if matches {
                return Some((val, n));
            }
        }
    }
    None
}

/// Whether a token is an all-capitals initialism worth spelling out.
///
/// One to five letters, no digits, and originally capitalised. Longer than that
/// and it is more likely a shouted word than an initialism. A single capital
/// counts: a lone `R` or `N` in a decoded line is a letter, not a word.
pub fn is_initialism(token: &str) -> bool {
    let n = token.chars().count();
    (1..=5).contains(&n) && token.chars().all(|c| c.is_ascii_uppercase())
}

/// Whether a token reads as an RST report — `599`, `5NN`, `579`.
///
/// Cut numbers are expanded before the shape is checked, so `5NN` and `599`
/// take the same path. The digit ranges are what makes this safe to run ahead
/// of the plain-integer rule: an ordinary `250` or `100` fails, because no RST
/// has a zero in it.
pub fn looks_like_rst(token: &str) -> bool {
    let b: Vec<char> = token.chars().map(expand_cut).collect();
    b.len() == 3
        && ('1'..='5').contains(&b[0])
        && ('1'..='9').contains(&b[1])
        && ('1'..='9').contains(&b[2])
}

/// A cut number as the digit it stands for. Anything else passes through.
pub fn expand_cut(c: char) -> char {
    match c.to_ascii_uppercase() {
        'N' => '9',
        'T' => '0',
        'A' => '1',
        'E' => '5',
        other => other,
    }
}

/// Space out a token's letters so each is spoken as its own word.
pub fn spaced(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_off_numbers_are_covered() {
        // These are the tokens that disappear entirely without the table:
        // no dictionary has an entry for "73".
        assert_eq!(lookup("73"), Some("seventy three"));
        assert_eq!(lookup("RR73"), Some("roger roger seventy three"));
        assert_eq!(lookup("rr73"), Some("roger roger seventy three"));
    }

    #[test]
    fn q_codes_are_spelled() {
        assert_eq!(lookup("QSL"), Some("Q S L"));
        assert_eq!(lookup("qrz"), Some("Q R Z"));
        assert_eq!(lookup("CQ"), Some("C Q"));
    }

    #[test]
    fn phrases_win_over_words() {
        let words = ["CQ", "DX", "K1ABC"];
        assert_eq!(lookup_phrase(&words, 0), Some(("C Q D X", 2)));
        assert_eq!(lookup_phrase(&words, 2), None);
    }

    #[test]
    fn initialisms() {
        assert!(is_initialism("RST"));
        assert!(is_initialism("SSB"));
        // A lone capital is a letter to spell, not a word.
        assert!(is_initialism("K"));
        assert!(!is_initialism("hello"));
        assert!(!is_initialism("K1ABC"));
        assert_eq!(spaced("RST"), "R S T");
    }

    #[test]
    fn no_duplicate_keys() {
        for (k, _) in WORDS {
            assert_eq!(
                WORDS.iter().filter(|(o, _)| o.eq_ignore_ascii_case(k)).count(),
                1,
                "duplicate abbreviation: {k}"
            );
        }
    }
}
