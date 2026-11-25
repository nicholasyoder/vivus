//! Mic capture plus the speech worker thread. Audio and control messages share
//! one channel so a Finish is processed after the audio captured before it.

use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{Context, bail};
use tokio::sync::mpsc::UnboundedSender;
use vivus_audio::{Capture, CaptureOptions, EchoCancelOptions};
use vivus_core::config::{Audio, Config, Role, Speech};
use vivus_speech::{Listener, Mode, Recorder, VadOptions, stt};

use crate::engine::Command;

pub(crate) const DEFAULT_STT_MODEL: &str = "parakeet-tdt-110m";

enum Msg {
    Audio(Vec<f32>),
    Start(Mode),
    Finish,
    Stop,
    EndOfTurnSilence(f32),
}

pub struct Voice {
    tx: mpsc::Sender<Msg>,
    /// `None` if reopening the mic after a device change failed.
    capture: Option<Capture>,
    /// Whether the mic should be delivering audio, to restore after a reconnect.
    active: bool,
    metering: bool,
    events: UnboundedSender<Command>,
}

impl std::fmt::Debug for Voice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Voice")
    }
}

impl Voice {
    /// Loads models (slow: run off the async runtime).
    pub fn new(config: &Config, events: UnboundedSender<Command>) -> anyhow::Result<Self> {
        let LocalSpeech {
            models_dir,
            threads,
            model,
            ..
        } = resolve_local(config, "stt", config.roles.stt.as_ref(), DEFAULT_STT_MODEL)?;
        tracing::info!(
            "stt: {model} ({threads} threads) from {}",
            models_dir.display()
        );

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
        if config.audio.record_utterances {
            let dir = vivus_speech::default_recordings_dir().context("no home directory")?;
            let recorder = Recorder::new(&dir, &model)
                .with_context(|| format!("creating {}", dir.display()))?;
            tracing::info!("recording utterances to {}", dir.display());
            listener.set_recorder(Some(recorder));
        }

        let events_for_levels = events.clone();
        let (tx, rx) = mpsc::channel::<Msg>();
        std::thread::Builder::new()
            .name("vivus-speech".into())
            .spawn(move || {
                let speech_events = events.clone();
                let mut emit = |e| {
                    let _ = speech_events.send(Command::Speech(e));
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
                        Msg::EndOfTurnSilence(s) => {
                            if let Err(e) = listener.set_min_silence(s) {
                                tracing::error!("setting end-of-turn silence: {e}");
                            }
                        }
                    }
                }
            })
            .context("spawning speech thread")?;

        let capture = spawn_capture(&config.audio, tx.clone(), events_for_levels.clone())?;
        Ok(Self {
            tx,
            capture: Some(capture),
            active: false,
            metering: false,
            events: events_for_levels,
        })
    }

    /// Reopens the mic with new audio settings or after its device came back.
    /// The old stream (and its echo canceller) goes first, since node names clash.
    pub fn reconnect(&mut self, audio: &Audio) {
        self.capture = None;
        match spawn_capture(audio, self.tx.clone(), self.events.clone()) {
            Ok(c) => {
                c.set_active(self.active);
                c.set_metering(self.metering);
                self.capture = Some(c);
                tracing::info!("mic reopened");
            }
            Err(e) => tracing::error!("reopening the mic: {e:#}"),
        }
    }

    fn set_active(&mut self, active: bool) {
        self.active = active;
        if let Some(c) = &self.capture {
            c.set_active(active);
        }
    }

    /// `Command::InputLevel` reports while on; the mic stays open.
    pub fn set_metering(&mut self, on: bool) {
        self.metering = on;
        if let Some(c) = &self.capture {
            c.set_metering(on);
        }
    }

    /// Drops an utterance in progress.
    pub fn set_end_of_turn_silence(&self, seconds: f32) {
        let _ = self.tx.send(Msg::EndOfTurnSilence(seconds));
    }

    pub fn start(&mut self, mode: Mode) {
        let _ = self.tx.send(Msg::Start(mode));
        self.set_active(true);
    }

    /// End the utterance in progress and transcribe it (push-to-talk release).
    pub fn finish(&mut self) {
        self.set_active(false);
        let _ = self.tx.send(Msg::Finish);
    }

    /// The mic doesn't hear our own speech, so it can stay open while speaking.
    pub fn echo_cancelled(&self) -> bool {
        self.capture.as_ref().is_some_and(Capture::echo_cancelled)
    }

    /// Stop listening and discard any utterance in progress.
    pub fn stop(&mut self) {
        self.set_active(false);
        let _ = self.tx.send(Msg::Stop);
    }
}

fn spawn_capture(
    audio: &Audio,
    tx: mpsc::Sender<Msg>,
    events: UnboundedSender<Command>,
) -> Result<Capture, vivus_audio::Error> {
    Capture::spawn(
        CaptureOptions {
            target: audio.input.clone(),
            echo_cancel: audio.echo_cancel.then(|| EchoCancelOptions {
                reference: audio.output.clone(),
            }),
            keep_open: audio.keep_mic_open,
        },
        move |samples| {
            let _ = tx.send(Msg::Audio(samples));
        },
        move |peak, clipped| {
            let _ = events.send(Command::InputLevel { peak, clipped });
        },
    )
}

/// A speech role resolved to the local-speech provider.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocalSpeech {
    pub models_dir: PathBuf,
    pub threads: i32,
    pub model: String,
    pub voice: Option<String>,
}

/// Speech roles run on local speech, with `default_model` when unset.
pub(crate) fn resolve_local(
    config: &Config,
    name: &str,
    role: Option<&Role>,
    default_model: &str,
) -> anyhow::Result<LocalSpeech> {
    if let Some(p) = role.and_then(|r| r.provider.as_deref()) {
        bail!("{name} provider `{p}`: only local speech is implemented so far")
    }
    Ok(LocalSpeech {
        models_dir: models_dir(&config.speech)?,
        threads: config.speech.threads,
        model: role
            .and_then(|r| r.model.clone())
            .unwrap_or_else(|| default_model.into()),
        voice: role.and_then(|r| r.voice.clone()),
    })
}

/// `[speech] models_dir`, or the default under the data dir.
pub(crate) fn models_dir(speech: &Speech) -> anyhow::Result<PathBuf> {
    match &speech.models_dir {
        Some(d) => Ok(expand_tilde(d)),
        None => vivus_speech::default_models_dir().context("no home directory"),
    }
}

fn expand_tilde(path: &std::path::Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.into(),
    }
}
