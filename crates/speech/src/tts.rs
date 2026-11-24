//! Text to speech: one sentence in, its samples out. Callers split text into
//! sentences ([`crate::SentenceSplitter`]) so playback starts after the first.

use std::path::Path;

use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig};

use crate::Error;
use crate::models::{KITTEN_LEXICON, TtsKind, TtsModel};

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
        config.model.num_threads = opts.num_threads;
        // We hand it one sentence at a time.
        config.max_num_sentences = 1;
        match model.kind {
            TtsKind::Kokoro => {
                let k = &mut config.model.kokoro;
                k.model = Some(file(model.model_file)?);
                k.voices = Some(file("voices.bin")?);
                k.tokens = Some(file("tokens.txt")?);
                // British voices (bf_/bm_) get the British lexicon.
                let gb = voice.starts_with('b');
                let lexicon = if gb { "lexicon-gb-en.txt" } else { "lexicon-us-en.txt" };
                k.lexicon = Some(file(lexicon)?);
            }
            TtsKind::Kitten => {
                let k = &mut config.model.kitten;
                k.model = Some(file(model.model_file)?);
                k.voices = Some(file("voices.bin")?);
                k.tokens = Some(file("tokens.txt")?);
                // sherpa-onnx 2.x has no espeak-ng; this shared espeak-IPA
                // lexicon (also used by Piper voices) sits beside the models.
                k.lexicon = Some(shared(KITTEN_LEXICON)?);
            }
        }

        let tts =
            OfflineTts::create(&config).ok_or_else(|| Error::LoadFailed(model.name.into()))?;
        Ok(Self {
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

    /// Mono f32 samples at [`Self::sample_rate`]. Empty if nothing was speakable.
    pub fn synthesize(&self, text: &str) -> Vec<f32> {
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        self.tts
            .generate_with_config(text, &self.generation, None::<fn(&[f32], f32) -> bool>)
            .map(|audio| audio.samples().to_vec())
            .unwrap_or_default()
    }
}
