//! Local speech models via sherpa-onnx: VAD, STT and TTS.
//! Everything here is blocking CPU work; run it on a dedicated thread.

pub mod arpabet;
pub mod lexicon;
pub mod listener;
pub mod models;
pub mod normalize;
pub mod recorder;
pub mod sentences;
pub mod stt;
pub mod tts;

pub use arpabet::PhoneSet;
pub use lexicon::{Lexicon, Phonemized, default_pronunciation_files, learned_pronunciation_file};
pub use listener::{Event, Listener, Mode, VadOptions};
pub use models::{
    SttKind, SttModel, TtsKind, TtsModel, default_models_dir, default_recordings_dir,
};
pub use recorder::Recorder;
pub use sentences::SentenceSplitter;
pub use stt::SpeechRecognizer;
pub use tts::{SharedLexicon, Synthesizer, missing_words};

/// STT and VAD take 16 kHz mono f32 samples. TTS output rate is per model.
pub const SAMPLE_RATE: i32 = 16_000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown model `{0}` (known: {1})")]
    UnknownModel(String, String),
    #[error("unknown voice `{0}` (known: {1})")]
    UnknownVoice(String, String),
    #[error("model files missing in {0} (run scripts/fetch-models.sh)")]
    MissingFiles(std::path::PathBuf),
    #[error("sherpa-onnx failed to load {0}")]
    LoadFailed(String),
}
