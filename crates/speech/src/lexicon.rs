//! The words a TTS model's lexicon knows. sherpa-onnx silently drops any other
//! word, so the normalizer checks against this and we log what's still missing.

use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Lexicon {
    words: HashSet<String>,
}

impl Lexicon {
    /// Reads sherpa lexicon files: Kokoro's `word phones…` and the shared
    /// `word || phones…` format (whose multi-word entries are skipped).
    pub fn load(paths: &[impl AsRef<Path>]) -> std::io::Result<Self> {
        let mut words = HashSet::new();
        for path in paths {
            let text = std::fs::read_to_string(path)?;
            for line in text.lines() {
                if line.starts_with('#') {
                    continue;
                }
                let word = match line.split_once("||") {
                    Some((w, _)) => w.trim(),
                    None => line.split_whitespace().next().unwrap_or(""),
                };
                if !word.is_empty() && !word.contains(' ') {
                    words.insert(word.to_lowercase());
                }
            }
        }
        Ok(Self { words })
    }

    pub fn from_words<'a>(words: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            words: words.into_iter().map(str::to_lowercase).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Case-insensitive. Hyphenated words count if every part is known,
    /// because sherpa splits them.
    pub fn contains(&self, word: &str) -> bool {
        let word = word.to_lowercase();
        self.words.contains(&word)
            || (word.contains('-')
                && word.split('-').all(|p| p.is_empty() || self.words.contains(p)))
    }

    /// Words of `text` (already normalized) that the model would drop, in order, deduplicated.
    pub fn missing(&self, text: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for token in text.split_whitespace() {
            let word = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
            let word = word.trim_matches('\'');
            if !word.is_empty() && !self.contains(word) && !out.iter().any(|w| w == word) {
                out.push(word.to_owned());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_missing_words() {
        let lex = Lexicon::from_words(["open", "the", "pods", "twenty", "one", "it's"]);
        assert_eq!(
            lex.missing("Open Hyprland, the pods. It's twenty-one, Hyprland!"),
            ["Hyprland"]
        );
    }
}
