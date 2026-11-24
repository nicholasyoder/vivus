//! Local speech models via sherpa-onnx: VAD, STT (and later TTS).
//! Everything here is blocking CPU work; run it on a dedicated thread.

pub mod listener;
pub mod models;
pub mod stt;

pub use listener::{Event, Listener, Mode, VadOptions};
pub use models::{SttKind, SttModel, default_models_dir};
pub use stt::SpeechRecognizer;

/// All models here take 16 kHz mono f32 samples.
pub const SAMPLE_RATE: i32 = 16_000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown STT model `{0}` (known: {1})")]
    UnknownModel(String, String),
    #[error("model files missing in {0} (run scripts/fetch-models.sh)")]
    MissingFiles(std::path::PathBuf),
    #[error("sherpa-onnx failed to load {0}")]
    LoadFailed(String),
}
