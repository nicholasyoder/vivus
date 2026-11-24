//! Our own phonemizer: word → phonemes from the model's lexicon files plus
//! ARPAbet pronunciation files, with `-s`/`-ed`/`-ing`/`'s` suffix rules. The
//! result goes to sherpa as `phoneme_codepoints`, so we know which words are
//! missing before synthesis instead of sherpa dropping them silently.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};

use crate::arpabet::{self, PhoneSet};

/// Built-in ARPAbet entries for common words some lexicons lack ("or", "her").
const BUILTIN: &str = include_str!("../data/pronunciations.txt");

#[derive(Debug)]
pub struct Lexicon {
    /// Lowercase word, or words joined by one space, → phonemes (no separators
    /// except spaces between the words of a phrase).
    words: HashMap<String, Box<str>>,
    /// Most words in one key; the shared espeak lexicon has phrases for
    /// homographs ("the record").
    max_phrase: usize,
    set: PhoneSet,
}

/// Phonemes for a sentence, and the words that had none (left out).
#[derive(Debug, Default, PartialEq)]
pub struct Phonemized {
    pub phonemes: String,
    pub missing: Vec<String>,
}

/// Pronunciation files in priority order: the user's
/// (`$XDG_CONFIG_HOME/vivus/pronunciations.txt`), then the learned ones
/// (`$XDG_DATA_HOME/vivus/pronunciations.txt`). Either may not exist.
pub fn default_pronunciation_files() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = |var: &str, fallback: &str| {
        std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(fallback)))
            .map(|d| d.join("vivus/pronunciations.txt"))
    };
    [xdg("XDG_CONFIG_HOME", ".config"), xdg("XDG_DATA_HOME", ".local/share")]
        .into_iter()
        .flatten()
        .collect()
}

impl Lexicon {
    /// Sources are added highest priority first; a word already present keeps
    /// its entry. (sherpa's own loader lets the *last* file win.)
    pub fn new(set: PhoneSet) -> Self {
        Self {
            words: HashMap::new(),
            max_phrase: 1,
            set,
        }
    }

    /// Pronunciation files (highest priority first), the model's lexicon files
    /// (likewise), then the built-in entries. Missing pronunciation files are skipped.
    pub fn load(
        set: PhoneSet,
        pronunciations: &[impl AsRef<Path>],
        lexicons: &[impl AsRef<Path>],
    ) -> std::io::Result<Self> {
        let mut lex = Self::new(set);
        for path in pronunciations {
            let path = path.as_ref();
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    let n = lex.add_arpabet(&text, &path.display().to_string());
                    tracing::debug!("tts: {n} pronunciations from {}", path.display());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        for path in lexicons {
            lex.add_lexicon(&std::fs::read_to_string(path)?);
        }
        lex.add_arpabet(BUILTIN, "built-in pronunciations");
        Ok(lex)
    }

    /// Words with empty pronunciations, for tests of word-level logic.
    pub fn from_words<'a>(words: impl IntoIterator<Item = &'a str>) -> Self {
        let mut lex = Self::new(PhoneSet::MisakiUs);
        for w in words {
            lex.insert(w, String::new());
        }
        lex
    }

    pub fn phone_set(&self) -> PhoneSet {
        self.set
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    fn insert(&mut self, key: &str, phonemes: String) {
        let key = key.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        if key.is_empty() {
            return;
        }
        if let Entry::Vacant(e) = self.words.entry(key) {
            self.max_phrase = self.max_phrase.max(e.key().split(' ').count());
            e.insert(phonemes.into());
        }
    }

    /// sherpa lexicon text: Kokoro's `word p h o n e s` or the shared
    /// `word(s) || phones`, where a run of two or more spaces between phones
    /// marks a word break inside a phrase.
    pub fn add_lexicon(&mut self, text: &str) {
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let (key, phones) = match line.split_once("||") {
                Some((k, p)) => (k.trim(), p.trim()),
                None => match line.split_once(char::is_whitespace) {
                    Some((k, p)) => (k, p.trim()),
                    None => continue,
                },
            };
            let mut out = String::new();
            for p in phones.split(' ') {
                if p.is_empty() {
                    if !out.is_empty() && !out.ends_with(' ') {
                        out.push(' ');
                    }
                } else {
                    out.push_str(p);
                }
            }
            if !out.is_empty() {
                self.insert(key, out);
            }
        }
    }

    /// CMUdict-format text (`WORD  PH1 PH2 …`, `#` comments). Bad lines are
    /// logged and skipped. Returns how many entries were read.
    pub fn add_arpabet(&mut self, text: &str, source: &str) -> usize {
        let mut n = 0;
        for (i, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((word, phones)) = line.split_once(char::is_whitespace) else {
                if !line.is_empty() {
                    tracing::warn!("{source}:{}: no phones for `{line}`", i + 1);
                }
                continue;
            };
            match arpabet::parse(phones) {
                Ok(p) if !p.is_empty() => {
                    self.insert(word, arpabet::to_phonemes(&p, self.set));
                    n += 1;
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("{source}:{}: {e}", i + 1),
            }
        }
        n
    }

    /// Phonemes for one word: the lexicon's entry, else a suffix rule applied
    /// to a known base ("opens" → "open" + z).
    pub fn lookup(&self, word: &str) -> Option<Cow<'_, str>> {
        let word = word.to_lowercase();
        if let Some(p) = self.words.get(&word) {
            return Some(Cow::Borrowed(p));
        }
        self.suffixed(&word).map(Cow::Owned)
    }

    /// Case-insensitive. Hyphenated words count if every part is known.
    pub fn contains(&self, word: &str) -> bool {
        self.lookup(word).is_some()
            || (word.contains('-')
                && word.split('-').all(|p| p.is_empty() || self.lookup(p).is_some()))
    }

    /// Words of `text` (already normalized) that have no pronunciation, in order, deduplicated.
    pub fn missing(&self, text: &str) -> Vec<String> {
        self.phonemize(text).missing
    }

    fn exact(&self, word: &str) -> Option<&str> {
        self.words.get(word).map(|p| &**p)
    }

    fn suffixed(&self, word: &str) -> Option<String> {
        let set = self.set;
        let ri = set.reduced_i();
        let strip = |n: usize| word.get(..word.len().checked_sub(n)?).filter(|b| b.len() >= 2);
        // The first candidate base the lexicon knows.
        let base = |cands: &[Option<String>]| {
            cands.iter().flatten().find_map(|b| self.exact(b).map(str::to_owned))
        };

        if let Some(b) = word.strip_suffix("'s") {
            let p = self.exact(b).map(str::to_owned).or_else(|| self.suffixed(b))?;
            return Some(plural(&p, ri));
        }
        if word.ends_with('s') && !word.ends_with("ss") && !word.ends_with("us") {
            let cands = [
                word.strip_suffix("ies").map(|b| format!("{b}y")),
                strip(1).map(str::to_owned),
                word.strip_suffix("es").and_then(|_| strip(2)).map(str::to_owned),
            ];
            return base(&cands).map(|p| plural(&p, ri));
        }
        if word.ends_with("ed") {
            let cands = [
                word.strip_suffix("ied").map(|b| format!("{b}y")),
                strip(1).map(str::to_owned), // "used" → "use"
                strip(2).map(str::to_owned),
                undouble(word, 2),
            ];
            return base(&cands).map(|p| past(&p, ri, set));
        }
        if word.ends_with("ing") {
            let stem = strip(3)?;
            // "making" → "make", but "being" → "be", not "bee".
            let e_first = ends_vowel_consonant(stem);
            let with_e = Some(format!("{stem}e"));
            let cands = if e_first {
                [with_e, Some(stem.to_owned()), undouble(word, 3)]
            } else {
                [Some(stem.to_owned()), with_e, undouble(word, 3)]
            };
            return base(&cands).map(|p| join_vowel_suffix(&p, "ɪŋ", set));
        }
        None
    }

    /// Phonemes for normalized text: words separated by spaces, punctuation
    /// kept. Unknown words are left out and listed in `missing`.
    pub fn phonemize(&self, text: &str) -> Phonemized {
        let tokens = tokenize(text);
        let mut out = Phonemized::default();
        let space = |s: &mut String| {
            if !s.is_empty() && !s.ends_with(' ') {
                s.push(' ');
            }
        };
        let mut i = 0;
        while i < tokens.len() {
            match tokens[i] {
                Token::Space => space(&mut out.phonemes),
                Token::Punct(c) => {
                    if out.phonemes.ends_with(' ') && !matches!(c, '(' | '"' | '“') {
                        out.phonemes.pop();
                    }
                    out.phonemes.push(c);
                }
                Token::Word(w) => {
                    if let Some((p, used)) = self.phrase(&tokens[i..]) {
                        out.phonemes.push_str(p);
                        i += used;
                        continue;
                    }
                    if let Some(p) = self.article(w, &tokens[i + 1..]) {
                        out.phonemes.push_str(p);
                        i += 1;
                        continue;
                    }
                    match self.word_phonemes(w) {
                        Some(p) => out.phonemes.push_str(&p),
                        None => {
                            if !out.missing.iter().any(|m| m == w) {
                                out.missing.push(w.to_owned());
                            }
                            // Drop the space before it too.
                            if out.phonemes.ends_with(' ') {
                                out.phonemes.pop();
                            }
                        }
                    }
                }
            }
            i += 1;
        }
        out.phonemes = out.phonemes.trim().to_owned();
        out
    }

    /// Longest multi-word key starting at `tokens[0]`: its phonemes and how
    /// many tokens it covers.
    fn phrase<'a>(&'a self, tokens: &[Token]) -> Option<(&'a str, usize)> {
        if self.max_phrase < 2 {
            return None;
        }
        let mut words = Vec::new();
        let mut ends = Vec::new();
        for (j, t) in tokens.iter().enumerate() {
            match t {
                Token::Word(w) => {
                    words.push(w.to_lowercase());
                    ends.push(j + 1);
                    if words.len() == self.max_phrase {
                        break;
                    }
                }
                Token::Space if j > 0 && matches!(tokens[j - 1], Token::Word(_)) => {}
                _ => break,
            }
        }
        (2..=words.len())
            .rev()
            .find_map(|n| self.exact(&words[..n].join(" ")).map(|p| (p, ends[n - 1])))
    }

    /// "the" and "a" depend on context; the lexicons have only one form
    /// (Kokoro's US "the" is always "ði", "a" is the letter name everywhere).
    fn article(&self, word: &str, rest: &[Token]) -> Option<&'static str> {
        let next = match rest {
            [Token::Space, Token::Word(n), ..] => Some(*n),
            _ => None,
        };
        match word {
            "the" | "The" => {
                let p = next.and_then(|n| self.word_phonemes(n))?;
                let first = p.chars().find(|c| !matches!(c, 'ˈ' | 'ˌ'))?;
                Some(if is_vowel(first) { "ði" } else { "ðə" })
            }
            // A capital A before a single letter is spelled out ("A P I").
            "a" => Some("ɐ"),
            "A" if next.is_some_and(|n| n.chars().count() > 1) => Some("ɐ"),
            _ => None,
        }
    }

    fn word_phonemes(&self, word: &str) -> Option<Cow<'_, str>> {
        if let Some(p) = self.lookup(word) {
            return Some(p);
        }
        if !word.contains('-') {
            return None;
        }
        // "twenty-one", "built-in": each part, as separate words.
        let parts: Option<Vec<_>> = word
            .split('-')
            .filter(|p| !p.is_empty())
            .map(|p| self.lookup(p))
            .collect();
        Some(Cow::Owned(parts?.join(" ")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token<'a> {
    Word(&'a str),
    Space,
    Punct(char),
}

/// Punctuation both models have tokens for, or that they treat as a pause.
const PUNCT: &[char] = &[',', '.', ';', ':', '!', '?', '—', '…', '(', ')', '"', '“', '”'];

fn tokenize(text: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut start = None;
    let is_word = |c: char| c.is_alphanumeric() || c == '\'' || c == '’' || c == '-';
    for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
        if i < text.len() && is_word(c) {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take() {
            // Quotes and hyphens around a word aren't part of it.
            let w = text[s..i].trim_matches(|c| c == '\'' || c == '’' || c == '-');
            if !w.is_empty() {
                out.push(Token::Word(w));
            }
        }
        if i == text.len() {
            break;
        }
        if PUNCT.contains(&c) {
            out.push(Token::Punct(c));
        } else if out.last() != Some(&Token::Space) {
            // Whitespace; other symbols also separate words.
            out.push(Token::Space);
        }
    }
    out
}

/// The last phoneme, ignoring stress and length marks.
fn last_phoneme(p: &str) -> Option<char> {
    p.chars().rev().find(|c| !matches!(c, 'ˈ' | 'ˌ' | 'ː'))
}

fn plural(base: &str, reduced_i: &str) -> String {
    match last_phoneme(base) {
        Some('s' | 'z' | 'ʃ' | 'ʒ' | 'ʧ' | 'ʤ') => format!("{base}{reduced_i}z"),
        Some('p' | 't' | 'k' | 'f' | 'θ') => format!("{base}s"),
        _ => format!("{base}z"),
    }
}

fn past(base: &str, reduced_i: &str, set: PhoneSet) -> String {
    match last_phoneme(base) {
        Some('t' | 'd') => join_vowel_suffix(base, &format!("{reduced_i}d"), set),
        Some('p' | 'k' | 'f' | 'θ' | 's' | 'ʃ' | 'ʧ') => format!("{base}t"),
        _ => format!("{base}d"),
    }
}

/// American English flaps a final t before a vowel suffix ("getting").
fn join_vowel_suffix(base: &str, suffix: &str, set: PhoneSet) -> String {
    let mut chars: Vec<char> = base.chars().collect();
    let n = chars.len();
    if set.is_american() && n >= 2 && chars[n - 1] == 't' && (is_vowel(chars[n - 2]) || chars[n - 2] == 'ɹ') {
        chars[n - 1] = 'ɾ';
    }
    chars.into_iter().collect::<String>() + suffix
}

fn is_vowel(c: char) -> bool {
    "aeiouæɑɐɒɔəɚɛɜɪʊʌᵻᵊAIOQWYː".contains(c)
}

/// "running" → "run" (`suffix_len` 3), "stopped" → "stop" (2).
fn undouble(word: &str, suffix_len: usize) -> Option<String> {
    let stem = word.get(..word.len().checked_sub(suffix_len)?)?;
    let b = stem.as_bytes();
    (b.len() >= 3 && b[b.len() - 1] == b[b.len() - 2] && b[b.len() - 1].is_ascii_alphabetic())
        .then(|| stem[..stem.len() - 1].to_owned())
}

/// A single vowel then a single consonant ("mak", "us"), where a silent e was dropped.
fn ends_vowel_consonant(stem: &str) -> bool {
    let b: Vec<u8> = stem.bytes().rev().take(3).collect();
    let v = |c: u8| b"aeiou".contains(&c);
    match b.as_slice() {
        [c, x, rest @ ..] => {
            !v(*c) && *c != b'w' && *c != b'x' && *c != b'y' && v(*x) && rest.first().is_none_or(|r| !v(*r))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(entries: &str) -> Lexicon {
        let mut l = Lexicon::new(PhoneSet::MisakiUs);
        l.add_lexicon(entries);
        l
    }

    const WORDS: &str = "a A\nplan p l ˈæ n\np p ˈi\ni ˈI\nopen ˈO p ə n\ncat k ˈæ t\nbox b ˈɑ k s\nplay p l ˈA\nget ɡ ˈɛ t\n\
        need n ˈi d\nmake m ˈA k\nbe b i\nrun ɹ ˈʌ n\nstudy s t ˈʌ d i\nuse j ˈu z\n\
        the ð ə\nhello h ə l ˈO\nworld w ˈɜ ɹ l d\nvisit v ˈɪ z ɪ t\nstop s t ˈɑ p\nnvidia ɛ n v ˈɪ d i ə\n";

    #[test]
    fn suffix_rules() {
        let l = lex(WORDS);
        let p = |w: &str| l.lookup(w).map(|p| p.into_owned());
        assert_eq!(p("opens").as_deref(), Some("ˈOpənz"));
        assert_eq!(p("cats").as_deref(), Some("kˈæts"));
        assert_eq!(p("boxes").as_deref(), Some("bˈɑksᵻz"));
        assert_eq!(p("studies").as_deref(), Some("stˈʌdiz"));
        assert_eq!(p("played").as_deref(), Some("plˈAd"));
        assert_eq!(p("needed").as_deref(), Some("nˈidᵻd"));
        assert_eq!(p("visited").as_deref(), Some("vˈɪzɪɾᵻd"));
        assert_eq!(p("stopped").as_deref(), Some("stˈɑpt"));
        assert_eq!(p("used").as_deref(), Some("jˈuzd"));
        assert_eq!(p("making").as_deref(), Some("mˈAkɪŋ"));
        assert_eq!(p("being").as_deref(), Some("biɪŋ"));
        assert_eq!(p("running").as_deref(), Some("ɹˈʌnɪŋ"));
        assert_eq!(p("getting").as_deref(), Some("ɡˈɛɾɪŋ"));
        assert_eq!(p("NVIDIA's").as_deref(), Some("ɛnvˈɪdiəz"));
        assert_eq!(p("bus"), None);
        assert_eq!(p("zorbs"), None);
    }

    #[test]
    fn first_source_wins() {
        let l = lex("the ð i\nthe ð ə\n");
        assert_eq!(l.lookup("The").as_deref(), Some("ði"));
    }

    #[test]
    fn phonemizes_sentences() {
        let l = lex(WORDS);
        let r = l.phonemize("Hello, world. Open the zorbleflax box!");
        assert_eq!(r.phonemes, "həlˈO, wˈɜɹld. ˈOpən ðə bˈɑks!");
        assert_eq!(r.missing, ["zorbleflax"]);
        let r = l.phonemize("A cat, the open box, a hello. Plan A P I. The");
        assert_eq!(r.phonemes, "ɐ kˈæt, ði ˈOpən bˈɑks, ɐ həlˈO. plˈæn A pˈi ˈI. ðə");
        let r = l.phonemize("\"Hello\" (world) open-box 'cat'");
        assert_eq!(r.phonemes, "\"həlˈO\" (wˈɜɹld) ˈOpən bˈɑks kˈæt");
        assert!(r.missing.is_empty());
    }

    #[test]
    fn phrases() {
        let mut l = Lexicon::new(PhoneSet::Espeak);
        l.add_lexicon("the || ð ə\nrecord || ɹ ᵻ k ˈɔː ɹ d\nthe record || ð ə   ɹ ˈɛ k ɚ d\n");
        assert_eq!(l.phonemize("Play the record.").missing, ["Play"]);
        assert_eq!(l.phonemize("the record, record").phonemes, "ðə ɹˈɛkɚd, ɹᵻkˈɔːɹd");
    }

    #[test]
    fn arpabet_entries() {
        let mut l = Lexicon::new(PhoneSet::MisakiUs);
        assert_eq!(l.add_arpabet("# user\nHYPRLAND  HH AY1 P ER0 L AE2 N D\nBAD XX1\n", "test"), 1);
        l.add_lexicon("hyprland h ˈI\n");
        assert_eq!(l.lookup("Hyprland").as_deref(), Some("hˈIpəɹlˌænd"));
        assert_eq!(l.lookup("Hyprland's").as_deref(), Some("hˈIpəɹlˌændz"));
    }
}
