//! Splits streamed text (LLM deltas) into sentences for TTS, so synthesis can
//! start as soon as the first sentence is complete.

/// Past this many bytes without a sentence end, split at a clause boundary
/// (`,` `;` `:`) instead, so a long first sentence doesn't delay audio.
const SOFT_LIMIT: usize = 120;

const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "e.g", "i.e", "approx", "no", "fig",
    "mt", "jan", "feb", "mar", "apr", "jun", "jul", "aug", "sep", "sept", "oct", "nov", "dec",
];

#[derive(Debug, Default)]
pub struct SentenceSplitter {
    buf: String,
}

impl SentenceSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append text; returns the sentences it completed.
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.buf.push_str(text);
        let mut out = Vec::new();
        while let Some(end) = self.boundary() {
            let rest = self.buf.split_off(end);
            let sentence = std::mem::replace(&mut self.buf, rest);
            push_trimmed(&mut out, &sentence);
        }
        out
    }

    /// End of text: whatever is left is the last sentence.
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        let rest = rest.trim();
        (!rest.is_empty()).then(|| rest.to_owned())
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Byte offset just past the first complete sentence. A terminator only
    /// counts once the following whitespace has arrived.
    fn boundary(&self) -> Option<usize> {
        let b = self.buf.as_bytes();
        let mut soft = None;
        for (i, c) in self.buf.char_indices() {
            match c {
                '\n' => return Some(i + 1),
                '.' | '!' | '?' | '…' => {
                    // Closing quotes/brackets belong to the sentence.
                    let mut j = i + c.len_utf8();
                    while j < b.len() && matches!(b[j], b'"' | b'\'' | b')' | b']') {
                        j += 1;
                    }
                    match b.get(j) {
                        Some(next) if next.is_ascii_whitespace() => {
                            if c != '.' || !self.is_abbreviation(i) {
                                return Some(j);
                            }
                        }
                        _ => {}
                    }
                }
                ',' | ';' | ':' if i >= SOFT_LIMIT && soft.is_none() => {
                    if b.get(i + 1).is_some_and(|n| n.is_ascii_whitespace()) {
                        soft = Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        soft
    }

    /// The word before the `.` at `dot` is an abbreviation or an initial.
    fn is_abbreviation(&self, dot: usize) -> bool {
        let word = self.buf[..dot]
            .rsplit(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("");
        let word = word.to_ascii_lowercase();
        (word.len() == 1 && word.chars().all(|c| c.is_alphabetic()))
            || ABBREVIATIONS.contains(&word.as_str())
            // "U.S." style: letters separated by dots.
            || (word.contains('.') && word.split('.').all(|p| p.len() == 1))
    }
}

fn push_trimmed(out: &mut Vec<String>, s: &str) {
    let s = s.trim();
    if !s.is_empty() {
        out.push(s.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(chunks: &[&str]) -> Vec<String> {
        let mut s = SentenceSplitter::new();
        let mut out: Vec<String> = chunks.iter().flat_map(|c| s.push(c)).collect();
        out.extend(s.finish());
        out
    }

    #[test]
    fn splits_streamed_deltas() {
        assert_eq!(
            split(&["It's 3", ".5 degr", "ees. Want", " more? Sure!", " Bye"]),
            ["It's 3.5 degrees.", "Want more?", "Sure!", "Bye"]
        );
    }

    #[test]
    fn waits_for_whitespace_after_terminator() {
        let mut s = SentenceSplitter::new();
        assert!(s.push("Done.").is_empty());
        assert_eq!(s.push(" Next"), ["Done."]);
    }

    #[test]
    fn keeps_abbreviations_and_quotes() {
        assert_eq!(
            split(&["Dr. Smith met e.g. the U.S. team on Nov. 3. He said \"hi.\" Then left."]),
            ["Dr. Smith met e.g. the U.S. team on Nov. 3.", "He said \"hi.\"", "Then left."]
        );
    }

    #[test]
    fn newlines_end_sentences() {
        assert_eq!(split(&["- one\n- two\n"]), ["- one", "- two"]);
    }

    #[test]
    fn long_clause_splits_at_comma() {
        let long = format!("{}, and then the rest", "word ".repeat(30));
        let out = split(&[&long]);
        assert_eq!(out.len(), 2);
        assert!(out[1].starts_with("and then"));
    }
}
