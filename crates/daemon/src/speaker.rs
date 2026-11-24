//! TTS worker thread plus playback. Text arrives as streamed deltas and is
//! split into sentences here, so words the lexicon lacks can be looked up
//! before the `vivus-tts` thread synthesizes their sentence.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedSender;
use vivus_audio::{Playback, PlaybackOptions, PlaybackSender};
use vivus_core::Config;
use vivus_speech::{SentenceSplitter, SharedLexicon, Synthesizer, missing_words, tts};

use crate::engine::Command;
use crate::pronounce::{self, Pronouncer};
use crate::voice::{LocalSpeech, resolve_local};

const DEFAULT_TTS_MODEL: &str = "kitten-nano";
/// How long a sentence may wait for pronunciation lookups. Later sentences
/// usually wait during the previous one's playback, so only the first is felt.
const LOOKUP_WAIT: Duration = Duration::from_secs(3);

/// Every message carries the reply generation it belongs to; `stop()` bumps
/// the generation so queued and in-flight work for the old reply is dropped.
enum Msg {
    Sentence(u64, String),
    End(u64),
}

pub struct Speaker {
    tx: mpsc::Sender<Msg>,
    playback: Playback,
    /// Held while checking the generation and queueing audio, so a `stop()`
    /// can't slip between the check and the send.
    generation: Arc<Mutex<u64>>,
    splitter: Mutex<SentenceSplitter>,
    lexicon: SharedLexicon,
    pronouncer: Option<Pronouncer>,
}

impl std::fmt::Debug for Speaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Speaker")
    }
}

impl Speaker {
    /// Loads the model (slow: run off the async runtime).
    pub fn new(
        config: &Config,
        events: UnboundedSender<Command>,
        rt: &Handle,
    ) -> anyhow::Result<Self> {
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
        let lexicon = synth.lexicon();
        let pronouncer = Pronouncer::from_config(config, lexicon.clone(), rt);
        if let Some(p) = &pronouncer {
            let names = pronounce::app_names().join(". ");
            p.request(&missing_words(&lexicon.read().unwrap(), &names), false);
        }

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
            pronouncer: pronouncer.clone(),
        };
        std::thread::Builder::new()
            .name("vivus-tts".into())
            .spawn(move || worker.run(rx))
            .context("spawning tts thread")?;

        Ok(Self {
            tx,
            playback,
            generation,
            splitter: Mutex::new(SentenceSplitter::new()),
            lexicon,
            pronouncer,
        })
    }

    /// Append text to the current reply.
    pub fn say(&self, text: &str) {
        let sentences = self.splitter.lock().unwrap().push(text);
        for sentence in sentences {
            self.queue(sentence);
        }
    }

    /// Speak what's buffered now; more text may follow later.
    pub fn flush(&self) {
        let sentence = self.splitter.lock().unwrap().finish();
        if let Some(sentence) = sentence {
            self.queue(sentence);
        }
    }

    /// The reply is complete; `SpeakingFinished` follows once it has played.
    pub fn end(&self) {
        self.flush();
        let _ = self.tx.send(Msg::End(*self.generation.lock().unwrap()));
    }

    /// Silence the current reply now and drop the rest of it.
    pub fn stop(&self) {
        let mut generation = self.generation.lock().unwrap();
        *generation += 1;
        self.splitter.lock().unwrap().clear();
        self.playback.stop();
    }

    /// Hold playback (a possible interruption); `resume` or `stop` follows.
    pub fn pause(&self) {
        self.playback.pause();
    }

    pub fn resume(&self) {
        self.playback.resume();
    }

    /// Looks up unknown words in `text` in the background (e.g. what the user said).
    pub fn prefetch(&self, text: &str) {
        self.lookup(text, false);
    }

    fn queue(&self, sentence: String) {
        self.lookup(&sentence, true);
        let _ = self.tx.send(Msg::Sentence(*self.generation.lock().unwrap(), sentence));
    }

    fn lookup(&self, text: &str, urgent: bool) {
        if let Some(p) = &self.pronouncer {
            let missing = missing_words(&self.lexicon.read().unwrap(), text);
            if !missing.is_empty() {
                p.request(&missing, urgent);
            }
        }
    }
}

struct Worker {
    synth: Synthesizer,
    playback: PlaybackSender,
    generation: Arc<Mutex<u64>>,
    pronouncer: Option<Pronouncer>,
}

impl Worker {
    fn run(self, rx: mpsc::Receiver<Msg>) {
        for msg in rx {
            match msg {
                Msg::Sentence(g, sentence) => {
                    if g == *self.generation.lock().unwrap() {
                        self.speak(g, &sentence);
                    }
                }
                Msg::End(g) => {
                    if g == *self.generation.lock().unwrap() {
                        self.playback.end();
                    }
                }
            }
        }
    }

    fn speak(&self, generation: u64, sentence: &str) {
        if let Some(p) = &self.pronouncer {
            let missing = missing_words(&self.synth.lexicon().read().unwrap(), sentence);
            let t = Instant::now();
            let cancelled = || *self.generation.lock().unwrap() != generation;
            if !missing.is_empty() && p.wait(&missing, t + LOOKUP_WAIT, cancelled) {
                let ms = t.elapsed().as_secs_f64() * 1e3;
                tracing::debug!("tts: waited {ms:.0} ms for pronunciations");
            }
        }
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
