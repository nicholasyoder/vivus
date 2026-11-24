//! Text to speech: one sentence in, its samples out. Callers split text into
//! sentences ([`crate::SentenceSplitter`]) so playback starts after the first.

use std::path::Path;

use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig};

use crate::Error;
use crate::lexicon::Lexicon;
use crate::normalize::normalize;
use crate::models::{KITTEN_LEXICON, TtsKind, TtsModel};

/// Common words the shared Kitten lexicon lacks ("or", "her"). Listed first, so
/// these win. A stopgap until we phonemize ourselves.
const KITTEN_EXTRA: &str = include_str!("../data/extra-kitten.txt");

/// sherpa only reads lexicons from files, so copy ours beside the models.
fn write_extra(models_dir: &Path, name: &str, contents: &str) -> Result<String, Error> {
    let path = models_dir.join(name);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(contents) {
        std::fs::write(&path, contents).map_err(|_| Error::LoadFailed(name.into()))?;
    }
    Ok(path.to_string_lossy().into_owned())
}

pub struct Options {
    pub num_threads: i32,
    /// 1.0 is the model's natural pace.
    pub speed: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            num_threads: 2,
            speed: 1.0,
        }
    }
}

pub struct Synthesizer {
    tts: OfflineTts,
    generation: GenerationConfig,
    lexicon: Lexicon,
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

        let mut config = OfflineTtsConfig::default();
        let lexicons: Vec<String>;
        config.model.num_threads = opts.num_threads;
        // We hand it one sentence at a time.
        config.max_num_sentences = 1;
        match model.kind {
            TtsKind::Kokoro => {
                let k = &mut config.model.kokoro;
                k.model = Some(file(model.model_file)?);
                k.voices = Some(file("voices.bin")?);
                k.tokens = Some(file("tokens.txt")?);
                // British voices (bf_/bm_) get the British lexicon first. The
                // other one fills gaps (the US one lacks many plurals and -ed
                // forms); sherpa takes the first file's entry for a word.
                let (us, gb) = (file("lexicon-us-en.txt")?, file("lexicon-gb-en.txt")?);
                lexicons = if voice.starts_with('b') { vec![gb, us] } else { vec![us, gb] };
                k.lexicon = Some(lexicons.join(","));
            }
            TtsKind::Kitten => {
                let k = &mut config.model.kitten;
                k.model = Some(file(model.model_file)?);
                k.voices = Some(file("voices.bin")?);
                k.tokens = Some(file("tokens.txt")?);
                // sherpa-onnx 2.x has no espeak-ng; this shared espeak-IPA
                // lexicon (also used by Piper voices) sits beside the models.
                let extra = write_extra(models_dir, "vivus-extra-kitten.txt", KITTEN_EXTRA)?;
                lexicons = vec![extra, shared(KITTEN_LEXICON)?];
                k.lexicon = Some(lexicons.join(","));
            }
        }

        let tts =
            OfflineTts::create(&config).ok_or_else(|| Error::LoadFailed(model.name.into()))?;
        let lexicon = Lexicon::load(&lexicons).map_err(|_| Error::LoadFailed(model.name.into()))?;
        Ok(Self {
            lexicon,
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

    /// Words of normalized `text` the model would drop.
    pub fn missing_words(&self, text: &str) -> Vec<String> {
        self.lexicon.missing(text)
    }

    /// Mono f32 samples at [`Self::sample_rate`]. Empty if nothing was speakable.
    /// Normalizes first and logs the words still missing from the lexicon.
    pub fn synthesize(&self, text: &str) -> Vec<f32> {
        let text = self.normalize(text);
        if text.is_empty() {
            return Vec::new();
        }
        let missing = self.missing_words(&text);
        if !missing.is_empty() {
            tracing::info!("tts: not in lexicon, skipped: {}", missing.join(", "));
        }
        self.tts
            .generate_with_config(&text, &self.generation, None::<fn(&[f32], f32) -> bool>)
            .map(|audio| audio.samples().to_vec())
            .unwrap_or_default()
    }
}
