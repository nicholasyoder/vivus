//! Catalog of supported models. Directory names match the sherpa-onnx release
//! archives that `scripts/fetch-models.sh` downloads.

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
    pub dir: &'static str,
    pub kind: SttKind,
}

pub const STT_MODELS: &[SttModel] = &[
    SttModel {
        name: "parakeet-tdt-110m",
        dir: "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8",
        kind: SttKind::Redecode,
    },
    SttModel {
        name: "nemotron-streaming-0.6b",
        dir: "sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25",
        kind: SttKind::Streaming,
    },
];

pub const VAD_MODEL: &str = "silero_vad_v5.onnx";

impl SttModel {
    pub fn find(name: &str) -> Result<&'static SttModel, crate::Error> {
        STT_MODELS.iter().find(|m| m.name == name).ok_or_else(|| {
            let known: Vec<_> = STT_MODELS.iter().map(|m| m.name).collect();
            crate::Error::UnknownModel(name.into(), known.join(", "))
        })
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
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("vivus/models"))
}
