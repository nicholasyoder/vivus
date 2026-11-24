//! Speech recognizers with one streaming interface: feed samples, get partials,
//! then `finish()` for the final text.

use std::path::Path;

use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OnlineRecognizer, OnlineRecognizerConfig,
    OnlineStream,
};

use crate::models::{SttKind, SttModel, TransducerFiles};
use crate::{Error, SAMPLE_RATE};

pub trait SpeechRecognizer: Send {
    /// Feed 16 kHz mono samples of the current utterance. Returns the partial
    /// transcript when it changed.
    fn accept(&mut self, samples: &[f32]) -> Option<String>;

    /// End of utterance: returns the final transcript and resets for the next one.
    fn finish(&mut self) -> String;

    /// Drop the current utterance without decoding it.
    fn reset(&mut self);
}

pub struct Options {
    pub num_threads: i32,
    /// Redecode models only: seconds of new audio between partial decodes.
    pub redecode_interval: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            num_threads: 2,
            redecode_interval: 0.3,
        }
    }
}

pub fn create(
    models_dir: &Path,
    model: &str,
    opts: &Options,
) -> Result<Box<dyn SpeechRecognizer>, Error> {
    let model = SttModel::find(model)?;
    let files = model.files(models_dir)?;
    Ok(match model.kind {
        SttKind::Streaming => Box::new(Streaming::new(model.name, &files, opts)?),
        SttKind::Redecode => Box::new(Redecode::new(model.name, &files, opts)?),
    })
}

fn path(p: &Path) -> Option<String> {
    Some(p.to_string_lossy().into_owned())
}

struct Streaming {
    recognizer: OnlineRecognizer,
    stream: OnlineStream,
    text: String,
}

impl Streaming {
    fn new(name: &str, files: &TransducerFiles, opts: &Options) -> Result<Self, Error> {
        let mut config = OnlineRecognizerConfig::default();
        config.model_config.transducer.encoder = path(&files.encoder);
        config.model_config.transducer.decoder = path(&files.decoder);
        config.model_config.transducer.joiner = path(&files.joiner);
        config.model_config.tokens = path(&files.tokens);
        config.model_config.num_threads = opts.num_threads;
        config.decoding_method = Some("greedy_search".into());
        // Endpointing is the VAD's job.
        config.enable_endpoint = false;

        let recognizer =
            OnlineRecognizer::create(&config).ok_or_else(|| Error::LoadFailed(name.into()))?;
        let stream = recognizer.create_stream();
        Ok(Self {
            recognizer,
            stream,
            text: String::new(),
        })
    }

    fn decode(&mut self) -> bool {
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
        let text = self
            .recognizer
            .get_result(&self.stream)
            .map(|r| r.text.trim().to_owned())
            .unwrap_or_default();
        let changed = text != self.text;
        self.text = text;
        changed
    }
}

impl SpeechRecognizer for Streaming {
    fn accept(&mut self, samples: &[f32]) -> Option<String> {
        self.stream.accept_waveform(SAMPLE_RATE, samples);
        self.decode().then(|| self.text.clone())
    }

    fn finish(&mut self) -> String {
        // No silence padding needed: the VAD only ends an utterance after its
        // min-silence window, which has already been fed in.
        self.stream.input_finished();
        self.decode();
        let text = std::mem::take(&mut self.text);
        self.reset();
        text
    }

    fn reset(&mut self) {
        self.stream = self.recognizer.create_stream();
        self.text.clear();
    }
}

struct Redecode {
    recognizer: OfflineRecognizer,
    samples: Vec<f32>,
    decoded_len: usize,
    interval: usize,
    text: String,
}

impl Redecode {
    fn new(name: &str, files: &TransducerFiles, opts: &Options) -> Result<Self, Error> {
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.transducer.encoder = path(&files.encoder);
        config.model_config.transducer.decoder = path(&files.decoder);
        config.model_config.transducer.joiner = path(&files.joiner);
        config.model_config.tokens = path(&files.tokens);
        config.model_config.model_type = Some("nemo_transducer".into());
        config.model_config.num_threads = opts.num_threads;
        config.decoding_method = Some("greedy_search".into());

        let recognizer =
            OfflineRecognizer::create(&config).ok_or_else(|| Error::LoadFailed(name.into()))?;
        Ok(Self {
            recognizer,
            samples: Vec::new(),
            decoded_len: 0,
            interval: (opts.redecode_interval * SAMPLE_RATE as f32) as usize,
            text: String::new(),
        })
    }

    fn decode_all(&mut self) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE, &self.samples);
        self.recognizer.decode(&stream);
        self.decoded_len = self.samples.len();
        stream
            .get_result()
            .map(|r| r.text.trim().to_owned())
            .unwrap_or_default()
    }
}

impl SpeechRecognizer for Redecode {
    fn accept(&mut self, samples: &[f32]) -> Option<String> {
        self.samples.extend_from_slice(samples);
        if self.samples.len() - self.decoded_len < self.interval {
            return None;
        }
        let text = self.decode_all();
        if text == self.text {
            return None;
        }
        self.text = text;
        Some(self.text.clone())
    }

    fn finish(&mut self) -> String {
        let text = if self.samples.is_empty() {
            String::new()
        } else {
            self.decode_all()
        };
        self.reset();
        text
    }

    fn reset(&mut self) {
        self.samples.clear();
        self.decoded_len = 0;
        self.text.clear();
    }
}
