//! Mic capture plus the speech worker thread. Audio and control messages share
//! one channel so a Finish is processed after the audio captured before it.

use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{Context, bail};
use tokio::sync::mpsc::UnboundedSender;
use vivus_audio::{Capture, CaptureOptions};
use vivus_core::config::{Config, Provider};
use vivus_speech::{Listener, Mode, VadOptions, stt};

use crate::engine::Command;

const DEFAULT_STT_MODEL: &str = "parakeet-tdt-110m";

enum Msg {
    Audio(Vec<f32>),
    Start(Mode),
    Finish,
    Stop,
}

pub struct Voice {
    tx: mpsc::Sender<Msg>,
    capture: Capture,
}

impl std::fmt::Debug for Voice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Voice")
    }
}

impl Voice {
    /// Loads models (slow: run off the async runtime).
    pub fn new(config: &Config, events: UnboundedSender<Command>) -> anyhow::Result<Self> {
        let (models_dir, threads, model) = resolve_stt(config)?;
        tracing::info!("stt: {model} ({threads} threads) from {}", models_dir.display());

        let rec = stt::create(
            &models_dir,
            &model,
            &stt::Options {
                num_threads: threads,
                ..Default::default()
            },
        )?;
        let vad = VadOptions {
            min_silence: config.audio.end_of_turn_silence,
            ..Default::default()
        };
        let mut listener = Listener::new(&models_dir, rec, &vad)?;

        let (tx, rx) = mpsc::channel::<Msg>();
        std::thread::Builder::new()
            .name("vivus-speech".into())
            .spawn(move || {
                let mut emit = |e| {
                    let _ = events.send(Command::Speech(e));
                };
                let mut active = false;
                for msg in rx {
                    match msg {
                        Msg::Audio(samples) if active => listener.push(&samples, &mut emit),
                        Msg::Audio(_) => {}
                        Msg::Start(mode) => {
                            listener.set_mode(mode);
                            active = true;
                        }
                        Msg::Finish => {
                            listener.finish(&mut emit);
                            active = false;
                        }
                        Msg::Stop => {
                            listener.reset();
                            active = false;
                        }
                    }
                }
            })
            .context("spawning speech thread")?;

        let audio_tx = tx.clone();
        let capture = Capture::spawn(
            CaptureOptions {
                target: config.audio.input.clone(),
            },
            move |samples| {
                let _ = audio_tx.send(Msg::Audio(samples));
            },
        )?;

        Ok(Self { tx, capture })
    }

    pub fn start(&self, mode: Mode) {
        let _ = self.tx.send(Msg::Start(mode));
        self.capture.set_active(true);
    }

    /// End the utterance in progress and transcribe it (push-to-talk release).
    pub fn finish(&self) {
        self.capture.set_active(false);
        let _ = self.tx.send(Msg::Finish);
    }

    /// Stop listening and discard any utterance in progress.
    pub fn stop(&self) {
        self.capture.set_active(false);
        let _ = self.tx.send(Msg::Stop);
    }
}

fn resolve_stt(config: &Config) -> anyhow::Result<(PathBuf, i32, String)> {
    let default_dir = || vivus_speech::default_models_dir().context("no home directory");
    let Some(role) = &config.roles.stt else {
        return Ok((default_dir()?, 2, DEFAULT_STT_MODEL.into()));
    };
    match &config.providers[&role.provider] {
        Provider::LocalSpeech {
            models_dir,
            threads,
        } => {
            let dir = match models_dir {
                Some(d) => expand_tilde(d),
                None => default_dir()?,
            };
            let model = role.model.clone().unwrap_or_else(|| DEFAULT_STT_MODEL.into());
            Ok((dir, *threads, model))
        }
        _ => bail!("stt provider `{}`: only local-speech is implemented so far", role.provider),
    }
}

fn expand_tilde(path: &std::path::Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.into(),
    }
}
