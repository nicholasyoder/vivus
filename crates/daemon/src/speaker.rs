//! TTS worker thread plus playback. Text arrives as streamed deltas, is split
//! into sentences and synthesized one at a time, so audio starts after the first.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use anyhow::Context;
use tokio::sync::mpsc::UnboundedSender;
use vivus_audio::{Playback, PlaybackOptions, PlaybackSender};
use vivus_core::Config;
use vivus_speech::{SentenceSplitter, Synthesizer, tts};

use crate::engine::Command;
use crate::voice::{LocalSpeech, resolve_local};

const DEFAULT_TTS_MODEL: &str = "kitten-nano";

/// Every message carries the reply generation it belongs to; `stop()` bumps
/// the generation so queued and in-flight work for the old reply is dropped.
enum Msg {
    Text(u64, String),
    /// Speak the buffered partial sentence without ending the reply.
    Flush(u64),
    End(u64),
}

pub struct Speaker {
    tx: mpsc::Sender<Msg>,
    playback: Playback,
    /// Held while checking the generation and queueing audio, so a `stop()`
    /// can't slip between the check and the send.
    generation: Arc<Mutex<u64>>,
}

impl std::fmt::Debug for Speaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Speaker")
    }
}

impl Speaker {
    /// Loads the model (slow: run off the async runtime).
    pub fn new(config: &Config, events: UnboundedSender<Command>) -> anyhow::Result<Self> {
        let LocalSpeech {
            models_dir,
            threads,
            model,
            voice,
        } = resolve_local(config, "tts", config.roles.tts.as_ref(), DEFAULT_TTS_MODEL)?;
        let voice_name = voice.as_deref().unwrap_or("default");
        tracing::info!(
            "tts: {model}/{voice_name} ({threads} threads) from {}",
            models_dir.display()
        );

        let synth = Synthesizer::create(
            &models_dir,
            &model,
            voice.as_deref(),
            &tts::Options {
                num_threads: threads,
                pronunciations: vivus_speech::default_pronunciation_files(),
                ..Default::default()
            },
        )?;
        // The first inference is several times slower; pay for it now.
        synth.synthesize("Ready.");

        let playback = Playback::spawn(
            PlaybackOptions {
                target: config.audio.output.clone(),
                sample_rate: synth.sample_rate(),
            },
            move || {
                let _ = events.send(Command::SpeakingFinished);
            },
        )?;

        let generation = Arc::new(Mutex::new(0));
        let (tx, rx) = mpsc::channel::<Msg>();
        let worker = Worker {
            synth,
            playback: playback.sender(),
            generation: generation.clone(),
            splitter: SentenceSplitter::new(),
            current: 0,
        };
        std::thread::Builder::new()
            .name("vivus-tts".into())
            .spawn(move || worker.run(rx))
            .context("spawning tts thread")?;

        Ok(Self {
            tx,
            playback,
            generation,
        })
    }

    /// Append text to the current reply.
    pub fn say(&self, text: &str) {
        let _ = self.tx.send(Msg::Text(*self.generation.lock().unwrap(), text.into()));
    }

    /// Speak what's buffered now; more text may follow later.
    pub fn flush(&self) {
        let _ = self.tx.send(Msg::Flush(*self.generation.lock().unwrap()));
    }

    /// The reply is complete; `SpeakingFinished` follows once it has played.
    pub fn end(&self) {
        let _ = self.tx.send(Msg::End(*self.generation.lock().unwrap()));
    }

    /// Silence the current reply now and drop the rest of it.
    pub fn stop(&self) {
        let mut generation = self.generation.lock().unwrap();
        *generation += 1;
        self.playback.stop();
    }
}

struct Worker {
    synth: Synthesizer,
    playback: PlaybackSender,
    generation: Arc<Mutex<u64>>,
    splitter: SentenceSplitter,
    /// Generation the splitter's buffered text belongs to.
    current: u64,
}

impl Worker {
    fn run(mut self, rx: mpsc::Receiver<Msg>) {
        for msg in rx {
            let (generation, text, end) = match msg {
                Msg::Text(g, t) => (g, Some(t), false),
                Msg::Flush(g) => (g, None, false),
                Msg::End(g) => (g, None, true),
            };
            if generation != *self.generation.lock().unwrap() {
                continue;
            }
            if generation != self.current {
                self.splitter.clear();
                self.current = generation;
            }
            match text {
                Some(text) => {
                    for sentence in self.splitter.push(&text) {
                        self.speak(generation, &sentence);
                    }
                }
                None => {
                    if let Some(sentence) = self.splitter.finish() {
                        self.speak(generation, &sentence);
                    }
                    if !end {
                        continue;
                    }
                    let current = self.generation.lock().unwrap();
                    if *current == generation {
                        self.playback.end();
                    }
                }
            }
        }
    }

    fn speak(&self, generation: u64, sentence: &str) {
        let t = Instant::now();
        let samples = self.synth.synthesize(sentence);
        tracing::debug!(
            "tts: {:.0} ms for {:.1} s: {sentence}",
            t.elapsed().as_secs_f64() * 1e3,
            samples.len() as f32 / self.synth.sample_rate() as f32
        );
        let current = self.generation.lock().unwrap();
        if *current == generation && !samples.is_empty() {
            self.playback.play(samples);
        }
    }
}
