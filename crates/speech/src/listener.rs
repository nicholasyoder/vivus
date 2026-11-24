//! Turns a continuous 16 kHz sample stream into utterances: VAD decides where
//! speech starts and ends, the recognizer produces partials and finals.

use std::collections::VecDeque;
use std::path::Path;

use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};

use crate::models::VAD_MODEL;
use crate::stt::SpeechRecognizer;
use crate::{Error, SAMPLE_RATE};

const VAD_WINDOW: usize = 512;

#[derive(Debug, Clone)]
pub struct VadOptions {
    pub threshold: f32,
    /// Silence that ends an utterance; the main end-of-turn latency knob.
    pub min_silence: f32,
    pub min_speech: f32,
    pub max_speech: f32,
}

impl Default for VadOptions {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_silence: 0.4,
            min_speech: 0.25,
            max_speech: 30.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    SpeechStarted,
    Partial(String),
    /// Empty when the utterance turned out to contain no words.
    Final(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// VAD splits speech into utterances (toggle-listen / conversation).
    Vad,
    /// Everything until `finish()` is one utterance (push-to-talk).
    Hold,
}

pub struct Listener {
    vad: VoiceActivityDetector,
    stt: Box<dyn SpeechRecognizer>,
    mode: Mode,
    /// Audio kept from before VAD fired, so the first syllable isn't lost.
    preroll: VecDeque<f32>,
    preroll_len: usize,
    window: Vec<f32>,
    in_utterance: bool,
}

impl Listener {
    pub fn new(
        models_dir: &Path,
        stt: Box<dyn SpeechRecognizer>,
        opts: &VadOptions,
    ) -> Result<Self, Error> {
        let model = models_dir.join(VAD_MODEL);
        if !model.exists() {
            return Err(Error::MissingFiles(model));
        }
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(model.to_string_lossy().into_owned()),
                threshold: opts.threshold,
                min_silence_duration: opts.min_silence,
                min_speech_duration: opts.min_speech,
                window_size: VAD_WINDOW as i32,
                max_speech_duration: opts.max_speech,
            },
            sample_rate: SAMPLE_RATE,
            num_threads: 1,
            ..Default::default()
        };
        let vad = VoiceActivityDetector::create(&config, opts.max_speech + 5.0)
            .ok_or_else(|| Error::LoadFailed(VAD_MODEL.into()))?;
        Ok(Self {
            vad,
            stt,
            mode: Mode::Vad,
            preroll: VecDeque::new(),
            preroll_len: ((opts.min_speech + 0.25) * SAMPLE_RATE as f32) as usize,
            window: Vec::with_capacity(VAD_WINDOW),
            in_utterance: false,
        })
    }

    /// Switching mode drops any utterance in progress.
    pub fn set_mode(&mut self, mode: Mode) {
        self.reset();
        self.mode = mode;
    }

    pub fn push(&mut self, mut samples: &[f32], out: &mut impl FnMut(Event)) {
        while !samples.is_empty() {
            let take = (VAD_WINDOW - self.window.len()).min(samples.len());
            self.window.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.window.len() == VAD_WINDOW {
                let window = std::mem::take(&mut self.window);
                self.process_window(&window, out);
                self.window = window;
                self.window.clear();
            }
        }
    }

    fn process_window(&mut self, window: &[f32], out: &mut impl FnMut(Event)) {
        if self.mode == Mode::Hold {
            if !self.in_utterance {
                self.in_utterance = true;
                out(Event::SpeechStarted);
            }
            self.feed(window, out);
            return;
        }

        self.vad.accept_waveform(window);
        let speech = self.vad.detected();
        // We track speech via detected(); the queued segments are unused.
        self.vad.clear();

        if self.in_utterance {
            self.feed(window, out);
            if !speech {
                self.finish(out);
            }
        } else if speech {
            self.in_utterance = true;
            out(Event::SpeechStarted);
            let preroll: Vec<f32> = self.preroll.drain(..).collect();
            self.feed(&preroll, out);
            self.feed(window, out);
        } else {
            self.preroll.extend(window);
            let excess = self.preroll.len().saturating_sub(self.preroll_len);
            self.preroll.drain(..excess);
        }
    }

    fn feed(&mut self, samples: &[f32], out: &mut impl FnMut(Event)) {
        if let Some(partial) = self.stt.accept(samples) {
            out(Event::Partial(partial));
        }
    }

    /// Ends the current utterance now (push-to-talk release).
    pub fn finish(&mut self, out: &mut impl FnMut(Event)) {
        if !self.window.is_empty() && self.in_utterance {
            let rest = std::mem::take(&mut self.window);
            self.feed(&rest, out);
        }
        if self.in_utterance {
            out(Event::Final(self.stt.finish()));
        }
        self.in_utterance = false;
        self.vad.reset();
    }

    /// Drops audio and any utterance in progress (cancel / stop listening).
    pub fn reset(&mut self) {
        self.stt.reset();
        self.vad.reset();
        self.preroll.clear();
        self.window.clear();
        self.in_utterance = false;
    }
}
