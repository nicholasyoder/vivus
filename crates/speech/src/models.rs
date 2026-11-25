//! Catalog of supported models. Directory names match the sherpa-onnx release
//! archives that `vivusctl models fetch` downloads (`crates/ctl/src/models.rs`).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttKind {
    /// Native streaming transducer (sherpa `OnlineRecognizer`).
    Streaming,
    /// Offline NeMo transducer, re-decoded as the utterance grows.
    Redecode,
}

#[derive(Debug, Clone, Copy)]
pub struct SttModel {
    pub name: &'static str,
    /// For pickers.
    pub label: &'static str,
    pub dir: &'static str,
    pub kind: SttKind,
}

pub const STT_MODELS: &[SttModel] = &[
    SttModel {
        name: "parakeet-tdt-110m",
        label: "Parakeet TDT 110M (light)",
        dir: "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8",
        kind: SttKind::Redecode,
    },
    SttModel {
        name: "parakeet-tdt-0.6b",
        label: "Parakeet TDT 0.6B (standard)",
        dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
        kind: SttKind::Redecode,
    },
    SttModel {
        name: "nemotron-streaming-0.6b",
        label: "Nemotron streaming 0.6B (no punctuation)",
        dir: "sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25",
        kind: SttKind::Streaming,
    },
];

pub const VAD_MODEL: &str = "silero_vad_v5.onnx";
/// espeak-IPA lexicon from the sherpa-onnx `tts-models` release, for Kitten.
pub const KITTEN_LEXICON: &str = "lexicon-en-us.txt";

impl SttModel {
    pub fn find(name: &str) -> Result<&'static SttModel, crate::Error> {
        STT_MODELS.iter().find(|m| m.name == name).ok_or_else(|| {
            let known: Vec<_> = STT_MODELS.iter().map(|m| m.name).collect();
            crate::Error::UnknownModel(name.into(), known.join(", "))
        })
    }

    /// Its files are in `models_dir`.
    pub fn installed(&self, models_dir: &Path) -> bool {
        self.files(models_dir).is_ok()
    }

    pub(crate) fn files(&self, models_dir: &Path) -> Result<TransducerFiles, crate::Error> {
        let dir = models_dir.join(self.dir);
        let files = TransducerFiles {
            encoder: dir.join("encoder.int8.onnx"),
            decoder: dir.join("decoder.int8.onnx"),
            joiner: dir.join("joiner.int8.onnx"),
            tokens: dir.join("tokens.txt"),
        };
        let all = [&files.encoder, &files.decoder, &files.joiner, &files.tokens];
        if all.iter().all(|p| p.exists()) {
            Ok(files)
        } else {
            Err(crate::Error::MissingFiles(dir))
        }
    }
}

pub(crate) struct TransducerFiles {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
}

/// `$VIVUS_MODELS`, else `$XDG_DATA_HOME/vivus/models`, else `~/.local/share/vivus/models`.
pub fn default_models_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("VIVUS_MODELS").filter(|v| !v.is_empty()) {
        return Some(dir.into());
    }
    Some(data_dir()?.join("models"))
}

/// `$XDG_DATA_HOME/vivus/recordings`, else `~/.local/share/vivus/recordings`.
pub fn default_recordings_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("recordings"))
}

fn data_dir() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("vivus"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsKind {
    Kokoro,
    Kitten,
}

#[derive(Debug, Clone, Copy)]
pub struct TtsModel {
    pub name: &'static str,
    pub label: &'static str,
    pub dir: &'static str,
    pub kind: TtsKind,
    pub model_file: &'static str,
    /// Voice name to speaker id, as in the model's `speaker2id` metadata.
    pub voices: &'static [(&'static str, i32)],
    pub default_voice: &'static str,
}

/// English voices only: other languages need lexicons we don't load.
const KOKORO_VOICES: &[(&str, i32)] = &[
    ("af_alloy", 0),
    ("af_aoede", 1),
    ("af_bella", 2),
    ("af_heart", 3),
    ("af_jessica", 4),
    ("af_kore", 5),
    ("af_nicole", 6),
    ("af_nova", 7),
    ("af_river", 8),
    ("af_sarah", 9),
    ("af_sky", 10),
    ("am_adam", 11),
    ("am_echo", 12),
    ("am_eric", 13),
    ("am_fenrir", 14),
    ("am_liam", 15),
    ("am_michael", 16),
    ("am_onyx", 17),
    ("am_puck", 18),
    ("am_santa", 19),
    ("bf_alice", 20),
    ("bf_emma", 21),
    ("bf_isabella", 22),
    ("bf_lily", 23),
    ("bm_daniel", 24),
    ("bm_fable", 25),
    ("bm_george", 26),
    ("bm_lewis", 27),
];

/// Upstream KittenTTS names, then the `expr-voice-*` ids they alias.
const KITTEN_VOICES: &[(&str, i32)] = &[
    ("jasper", 0),
    ("bella", 1),
    ("bruno", 2),
    ("luna", 3),
    ("hugo", 4),
    ("rosie", 5),
    ("leo", 6),
    ("kiki", 7),
    ("expr-voice-2-m", 0),
    ("expr-voice-2-f", 1),
    ("expr-voice-3-m", 2),
    ("expr-voice-3-f", 3),
    ("expr-voice-4-m", 4),
    ("expr-voice-4-f", 5),
    ("expr-voice-5-m", 6),
    ("expr-voice-5-f", 7),
];

/// fp32, not int8: int8 is about twice as slow on x86 CPUs (see speech.md).
pub const TTS_MODELS: &[TtsModel] = &[
    TtsModel {
        name: "kitten-nano",
        label: "Kitten nano (light)",
        dir: "kitten-nano-en-v0_8-fp32",
        kind: TtsKind::Kitten,
        model_file: "model.fp32.onnx",
        voices: KITTEN_VOICES,
        default_voice: "bella",
    },
    TtsModel {
        name: "kokoro",
        label: "Kokoro v1.0 (standard)",
        dir: "kokoro-multi-lang-v1_0",
        kind: TtsKind::Kokoro,
        model_file: "model.onnx",
        voices: KOKORO_VOICES,
        default_voice: "af_heart",
    },
];

impl TtsModel {
    pub fn find(name: &str) -> Result<&'static TtsModel, crate::Error> {
        TTS_MODELS.iter().find(|m| m.name == name).ok_or_else(|| {
            let known: Vec<_> = TTS_MODELS.iter().map(|m| m.name).collect();
            crate::Error::UnknownModel(name.into(), known.join(", "))
        })
    }

    /// The files `Synthesizer::create` needs are in `models_dir`.
    pub fn installed(&self, models_dir: &Path) -> bool {
        let dir = models_dir.join(self.dir);
        let shared = match self.kind {
            TtsKind::Kitten => vec![models_dir.join(KITTEN_LEXICON)],
            TtsKind::Kokoro => vec![dir.join("lexicon-us-en.txt"), dir.join("lexicon-gb-en.txt")],
        };
        [self.model_file, "voices.bin", "tokens.txt"]
            .iter()
            .map(|f| dir.join(f))
            .chain(shared)
            .all(|p| p.exists())
    }

    /// Voice names with a readable label, skipping aliases.
    pub fn voice_labels(&self) -> Vec<(&'static str, String)> {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for &(name, id) in self.voices {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            out.push((name, self.voice_label(name, id)));
        }
        out
    }

    /// Kokoro: `af_heart` → "Heart (American, female)". Kitten: the name, with
    /// the gender from its `expr-voice-N-m/f` alias.
    fn voice_label(&self, name: &str, id: i32) -> String {
        let title = |s: &str| {
            let mut c = s.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        };
        let gender = |c| match c {
            'f' => "female",
            'm' => "male",
            _ => "",
        };
        match self.kind {
            TtsKind::Kokoro => {
                let mut c = name.chars();
                let (accent, sex) = (c.next(), c.next());
                let accent = match accent {
                    Some('a') => "American",
                    Some('b') => "British",
                    _ => "",
                };
                let rest = name.get(3..).unwrap_or(name);
                format!("{} ({accent}, {})", title(rest), gender(sex.unwrap_or(' ')))
            }
            TtsKind::Kitten => {
                let sex = self
                    .voices
                    .iter()
                    .find(|(n, i)| *i == id && n.starts_with("expr-voice-"))
                    .and_then(|(n, _)| n.chars().last());
                match sex.map(gender) {
                    Some(g) if !g.is_empty() => format!("{} ({g})", title(name)),
                    _ => title(name),
                }
            }
        }
    }

    pub fn speaker_id(&self, voice: &str) -> Result<i32, crate::Error> {
        let voice = voice.to_ascii_lowercase();
        self.voices
            .iter()
            .find(|(name, _)| *name == voice)
            .map(|(_, id)| *id)
            .ok_or_else(|| {
                let known: Vec<_> = self.voices.iter().map(|(n, _)| *n).collect();
                crate::Error::UnknownVoice(voice, known.join(", "))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_labels() {
        let kokoro = TtsModel::find("kokoro").unwrap().voice_labels();
        assert_eq!(kokoro.len(), KOKORO_VOICES.len());
        assert!(kokoro.contains(&("af_heart", "Heart (American, female)".into())));
        assert!(kokoro.contains(&("bm_george", "George (British, male)".into())));
        let kitten = TtsModel::find("kitten-nano").unwrap().voice_labels();
        assert_eq!(kitten.len(), 8);
        assert_eq!(kitten[0], ("jasper", "Jasper (male)".into()));
        assert_eq!(kitten[1], ("bella", "Bella (female)".into()));
    }
}
