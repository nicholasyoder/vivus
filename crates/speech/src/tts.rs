//! Text to speech: one sentence in, its samples out. Callers split text into
//! sentences ([`crate::SentenceSplitter`]) so playback starts after the first.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig};

use crate::Error;
use crate::arpabet::PhoneSet;
use crate::lexicon::{Lexicon, Phonemized};
use crate::models::{KITTEN_LEXICON, TtsKind, TtsModel};
use crate::normalize::normalize;

/// Single-codepoint tokens in a model's `tokens.txt` (`sym id`; a line with
/// only an id is the space).
fn read_tokens(path: &str) -> std::io::Result<HashSet<char>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter_map(|line| match line.rsplit_once(' ') {
            Some((sym, _)) if sym.is_empty() => Some(' '),
            Some((sym, _)) => {
                let mut c = sym.chars();
                c.next().filter(|_| c.next().is_none())
            }
            None => None,
        })
        .collect())
}

pub struct Options {
    pub num_threads: i32,
    /// 1.0 is the model's natural pace.
    pub speed: f32,
    /// ARPAbet pronunciation files, highest priority first; they win over the
    /// model's lexicon. See [`crate::lexicon::default_pronunciation_files`].
    pub pronunciations: Vec<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            num_threads: 2,
            speed: 1.0,
            pronunciations: Vec::new(),
        }
    }
}

pub struct Synthesizer {
    tts: OfflineTts,
    generation: GenerationConfig,
    lexicon: Lexicon,
    tokens: HashSet<char>,
}

impl Synthesizer {
    /// `voice` defaults to the model's default voice.
    pub fn create(
        models_dir: &Path,
        model: &str,
        voice: Option<&str>,
        opts: &Options,
    ) -> Result<Self, Error> {
        let model = TtsModel::find(model)?;
        let voice = voice.unwrap_or(model.default_voice);
        let sid = model.speaker_id(voice)?;

        let dir = models_dir.join(model.dir);
        let file = |name: &str| {
            let p = dir.join(name);
            p.exists()
                .then(|| p.to_string_lossy().into_owned())
                .ok_or_else(|| Error::MissingFiles(dir.clone()))
        };

        let shared = |name: &str| {
            let p = models_dir.join(name);
            p.exists()
                .then(|| p.to_string_lossy().into_owned())
                .ok_or_else(|| Error::MissingFiles(p.clone()))
        };

        // We phonemize ourselves and pass codepoints, so sherpa gets no lexicon.
        let mut config = OfflineTtsConfig::default();
        config.model.num_threads = opts.num_threads;
        config.max_num_sentences = 1;
        let tokens = file("tokens.txt")?;
        let (set, lexicons) = match model.kind {
            TtsKind::Kokoro => {
                let k = &mut config.model.kokoro;
                k.model = Some(file(model.model_file)?);
                k.voices = Some(file("voices.bin")?);
                k.tokens = Some(tokens.clone());
                // British voices (bf_/bm_) get the British lexicon first. The
                // other fills gaps (the US one lacks many plurals and -ed forms).
                let (us, gb) = (file("lexicon-us-en.txt")?, file("lexicon-gb-en.txt")?);
                if voice.starts_with('b') {
                    (PhoneSet::MisakiGb, vec![gb, us])
                } else {
                    (PhoneSet::MisakiUs, vec![us, gb])
                }
            }
            TtsKind::Kitten => {
                let k = &mut config.model.kitten;
                k.model = Some(file(model.model_file)?);
                k.voices = Some(file("voices.bin")?);
                k.tokens = Some(tokens.clone());
                // The archive has no lexicon; this shared espeak-IPA one (also
                // used by Piper voices) sits beside the models.
                (PhoneSet::Espeak, vec![shared(KITTEN_LEXICON)?])
            }
        };

        let failed = |_| Error::LoadFailed(model.name.into());
        let lexicon = Lexicon::load(set, &opts.pronunciations, &lexicons).map_err(failed)?;
        let tokens = read_tokens(&tokens).map_err(failed)?;
        let tts =
            OfflineTts::create(&config).ok_or_else(|| Error::LoadFailed(model.name.into()))?;
        Ok(Self {
            lexicon,
            tokens,
            tts,
            generation: GenerationConfig {
                sid,
                speed: opts.speed,
                ..Default::default()
            },
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.tts.sample_rate() as u32
    }

    /// Written text as this model will say it (numbers, units, … as words).
    pub fn normalize(&self, text: &str) -> String {
        normalize(text, &self.lexicon)
    }

    /// Phonemes for normalized `text`, and the words that have none.
    pub fn phonemize(&self, text: &str) -> Phonemized {
        self.lexicon.phonemize(text)
    }

    /// Mono f32 samples at [`Self::sample_rate`]. Empty if nothing was speakable.
    /// Normalizes and phonemizes first; logs words with no pronunciation.
    pub fn synthesize(&self, text: &str) -> Vec<f32> {
        let Phonemized { phonemes, missing } = self.phonemize(&self.normalize(text));
        if !missing.is_empty() {
            tracing::info!("tts: no pronunciation, skipped: {}", missing.join(", "));
        }
        let phonemes: Vec<char> = phonemes.chars().filter(|c| self.tokens.contains(c)).collect();
        // Punctuation alone would come out as a short noise.
        if phonemes.iter().all(|c| c.is_whitespace() || c.is_ascii_punctuation() || "—…“”".contains(*c)) {
            return Vec::new();
        }
        let codepoints = phonemes.into_iter().map(|c| c as i32).collect();
        let generation = GenerationConfig {
            phoneme_codepoints: Some(vec![codepoints]),
            ..self.generation.clone()
        };
        self.tts
            .generate_with_config("", &generation, None::<fn(&[f32], f32) -> bool>)
            .map(|audio| audio.samples().to_vec())
            .unwrap_or_default()
    }
}
