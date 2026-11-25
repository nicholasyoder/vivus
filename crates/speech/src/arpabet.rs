//! ARPAbet (CMUdict phones, vowels carry a stress digit: `HH AY1 P ER0 L AE2 N D`)
//! converted to each model's phoneme set. Learned and user pronunciations are
//! stored this way so one entry serves every model. The tables and rules were
//! tuned against the words CMUdict shares with each lexicon (speech.md).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneSet {
    /// espeak-ng IPA (Kitten's shared lexicon).
    Espeak,
    /// misaki American (Kokoro `a*` voices): diphthongs are single letters (`A` = eɪ).
    MisakiUs,
    /// misaki British (Kokoro `b*` voices).
    MisakiGb,
}

impl PhoneSet {
    pub fn is_american(self) -> bool {
        self != Self::MisakiGb
    }

    /// Unstressed reduced vowel in suffixes ("boxes", "wanted").
    pub(crate) fn reduced_i(self) -> &'static str {
        if self == Self::MisakiGb { "ɪ" } else { "ᵻ" }
    }
}

/// Vowel: (stressed, unstressed) per [`PhoneSet`] in declaration order.
const VOWELS: &[(&str, [(&str, &str); 3])] = &[
    ("AA", [("ɑː", "ɑː"), ("ɑ", "ɑ"), ("ɑː", "ɑː")]),
    ("AE", [("æ", "æ"), ("æ", "æ"), ("a", "a")]),
    ("AH", [("ʌ", "ə"), ("ʌ", "ə"), ("ʌ", "ə")]),
    ("AO", [("ɔː", "ɔː"), ("ɔ", "ɔ"), ("ɔː", "ɔː")]),
    ("AW", [("aʊ", "aʊ"), ("W", "W"), ("W", "W")]),
    ("AY", [("aɪ", "aɪ"), ("I", "I"), ("I", "I")]),
    ("EH", [("ɛ", "ɛ"), ("ɛ", "ɛ"), ("ɛ", "ɛ")]),
    ("ER", [("ɜː", "ɚ"), ("ɜɹ", "əɹ"), ("ɜː", "ə")]),
    ("EY", [("eɪ", "eɪ"), ("A", "A"), ("A", "A")]),
    ("IH", [("ɪ", "ɪ"), ("ɪ", "ɪ"), ("ɪ", "ɪ")]),
    ("IY", [("iː", "i"), ("i", "i"), ("iː", "i")]),
    ("OW", [("oʊ", "oʊ"), ("O", "O"), ("Q", "Q")]),
    ("OY", [("ɔɪ", "ɔɪ"), ("Y", "Y"), ("Y", "Y")]),
    ("UH", [("ʊ", "ʊ"), ("ʊ", "ʊ"), ("ʊ", "ʊ")]),
    ("UW", [("uː", "uː"), ("u", "u"), ("uː", "u")]),
];

/// Consonant: (espeak, misaki). They differ only in the affricates' ligatures.
const CONSONANTS: &[(&str, &str, &str)] = &[
    ("B", "b", "b"),
    ("CH", "tʃ", "ʧ"),
    ("D", "d", "d"),
    ("DH", "ð", "ð"),
    ("F", "f", "f"),
    ("G", "ɡ", "ɡ"),
    ("HH", "h", "h"),
    ("JH", "dʒ", "ʤ"),
    ("K", "k", "k"),
    ("L", "l", "l"),
    ("M", "m", "m"),
    ("N", "n", "n"),
    ("NG", "ŋ", "ŋ"),
    ("P", "p", "p"),
    ("R", "ɹ", "ɹ"),
    ("S", "s", "s"),
    ("SH", "ʃ", "ʃ"),
    ("T", "t", "t"),
    ("TH", "θ", "θ"),
    ("V", "v", "v"),
    ("W", "w", "w"),
    ("Y", "j", "j"),
    ("Z", "z", "z"),
    ("ZH", "ʒ", "ʒ"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phone {
    pub symbol: &'static str,
    /// `Some(0..=2)` for vowels, `None` for consonants.
    pub stress: Option<u8>,
}

impl Phone {
    fn is_vowel(&self) -> bool {
        self.stress.is_some()
    }
}

/// Parses space-separated ARPAbet, case-insensitive. A vowel without a stress
/// digit counts as unstressed.
pub fn parse(phones: &str) -> Result<Vec<Phone>, String> {
    phones
        .split_whitespace()
        .map(|p| {
            let upper = p.to_ascii_uppercase();
            let (base, digit) = match upper.strip_suffix(['0', '1', '2']) {
                Some(b) => (b, Some(upper.as_bytes()[upper.len() - 1] - b'0')),
                None => (upper.as_str(), None),
            };
            if let Some((v, _)) = VOWELS.iter().find(|(v, _)| *v == base) {
                return Ok(Phone {
                    symbol: v,
                    stress: Some(digit.unwrap_or(0)),
                });
            }
            match CONSONANTS.iter().find(|(c, ..)| *c == base) {
                Some((c, ..)) if digit.is_none() => Ok(Phone {
                    symbol: c,
                    stress: None,
                }),
                _ => Err(format!("unknown phone `{p}`")),
            }
        })
        .collect()
}

pub fn to_phonemes(phones: &[Phone], set: PhoneSet) -> String {
    let mut out = String::new();
    let at = |i: usize| phones.get(i);
    for (i, p) in phones.iter().enumerate() {
        let next = at(i + 1);
        let next_is = |s: &str| next.is_some_and(|n| n.symbol == s);
        let next_vowel = next.is_some_and(Phone::is_vowel);
        let Some(stress) = p.stress else {
            let prev = i.checked_sub(1).and_then(at);
            // American flap: "water", "party".
            let flap = set.is_american()
                && p.symbol == "T"
                && prev.is_some_and(|p| p.is_vowel() || p.symbol == "R")
                && next.is_some_and(|n| n.stress == Some(0));
            // British is non-rhotic: no R unless a vowel follows.
            if set == PhoneSet::MisakiGb && p.symbol == "R" && !next_vowel {
                continue;
            }
            let (_, espeak, misaki) = CONSONANTS.iter().find(|(c, ..)| *c == p.symbol).unwrap();
            out.push_str(match (flap, set) {
                (true, _) => "ɾ",
                (_, PhoneSet::Espeak) => espeak,
                _ => misaki,
            });
            continue;
        };
        let (_, forms) = VOWELS.iter().find(|(v, _)| *v == p.symbol).unwrap();
        let (stressed, unstressed) = forms[set as usize];
        let mut v = if stress > 0 { stressed } else { unstressed }.to_owned();
        match (set, p.symbol) {
            (PhoneSet::Espeak, "AH") if stress == 0 && i == 0 => v = "ɐ".into(),
            (PhoneSet::Espeak, "AO") if next_is("R") => v = "oː".into(),
            (PhoneSet::Espeak, "ER") if next_vowel => v.push('ɹ'),
            (PhoneSet::MisakiGb, "AA") if !next_is("R") => v = "ɒ".into(),
            (PhoneSet::MisakiUs | PhoneSet::MisakiGb, "IH")
                if stress == 0
                    && i + 2 == phones.len()
                    && next_is("D")
                    && i > 0
                    && matches!(phones[i - 1].symbol, "T" | "D") =>
            {
                v = set.reduced_i().into()
            }
            _ => {}
        }
        // Only misaki US marks secondary stress consistently.
        match stress {
            1 => out.push('ˈ'),
            2 if set == PhoneSet::MisakiUs => out.push('ˌ'),
            _ => {}
        }
        out.push_str(&v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(arpa: &str, set: PhoneSet) -> String {
        to_phonemes(&parse(arpa).unwrap(), set)
    }

    #[test]
    fn converts_to_each_set() {
        // Each matches the model's own lexicon entry.
        assert_eq!(conv("HH AH0 L OW1", PhoneSet::MisakiUs), "həlˈO");
        assert_eq!(conv("W AO1 T ER0", PhoneSet::Espeak), "wˈɔːɾɚ");
        assert_eq!(conv("W AO1 T ER0", PhoneSet::MisakiUs), "wˈɔɾəɹ");
        assert_eq!(conv("W AO1 T ER0", PhoneSet::MisakiGb), "wˈɔːtə");
        assert_eq!(conv("P AA1 R T IY0", PhoneSet::MisakiUs), "pˈɑɹɾi");
        assert_eq!(conv("L AA1 T", PhoneSet::MisakiGb), "lˈɒt");
        assert_eq!(conv("AH0 B AW1 T", PhoneSet::Espeak), "ɐbˈaʊt");
        assert_eq!(conv("N IY1 D IH0 D", PhoneSet::MisakiUs), "nˈidᵻd");
    }

    #[test]
    fn rejects_unknown_phones() {
        assert!(parse("HH AY1 P ER0 L AE2 N D").is_ok());
        assert!(parse("hh ay1 p er l ae2 n d").is_ok());
        assert!(parse("HH XX1").is_err());
        assert!(parse("K1 AE1 T").is_err());
    }
}
