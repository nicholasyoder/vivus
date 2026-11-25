//! Owns assistant behaviour. A final transcript (or `Ask`) starts a reply: the
//! router runs as a task whose events come back as `Command::Reply`, tagged
//! with a generation so events from a cancelled reply are ignored.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use vivus_core::settings::Value as SettingValue;
use vivus_core::{AssistantState, Config, DocumentStore};
use vivus_speech::{Event, Mode};
use zbus::Connection;

use crate::clipboard::SharedClipboard;
use crate::router::{ReplyEvent, Router};
use crate::service;
use crate::settings;
use crate::speaker::Speaker;
use crate::voice::Voice;
use crate::windows::SharedWindows;

/// State read synchronously by D-Bus property/method handlers. Never hold the
/// lock across an await.
#[derive(Default)]
pub struct Shared {
    pub state: AssistantState,
    pub documents: DocumentStore,
    /// The effective config, for `org.vivus.Settings.Get`.
    pub config_json: String,
    pub audio_devices: Vec<vivus_dbus::AudioDevice>,
}

/// Where the config lives and what it last contained, to tell our own writes
/// from hand edits.
pub struct ConfigFile {
    pub path: Option<PathBuf>,
    pub text: String,
}

#[derive(Debug)]
pub enum Command {
    ToggleListen,
    StartListening,
    StopListening,
    Ask(String),
    Cancel,
    Quit,
    VoiceReady(Box<anyhow::Result<Voice>>),
    SpeakerReady(Box<anyhow::Result<Speaker>>),
    Speech(Event),
    /// The spoken reply has finished playing.
    SpeakingFinished,
    /// A window gained focus; its title may be spoken soon.
    WindowFocused(String),
    Reply {
        generation: u64,
        event: ReplyEvent,
    },
    /// `org.vivus.Settings.Set` (`Some`) or `Reset` (`None`).
    EditSetting {
        path: String,
        value: Option<SettingValue>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// The config file may have been edited by hand.
    ConfigFileChanged,
    /// Audio sources and sinks, after hotplug or a default change.
    AudioDevices(Vec<vivus_audio::Device>),
}

/// The reply in progress.
struct Reply {
    generation: u64,
    /// `None` for a fixed line spoken without the router.
    task: Option<AbortHandle>,
    text: String,
    /// TTS has been given text, so `SpeakingFinished` will follow `end()`.
    speaking: bool,
}

pub struct Engine {
    config: Config,
    config_file: ConfigFile,
    conn: Connection,
    tx: UnboundedSender<Command>,
    shared: Arc<Mutex<Shared>>,
    voice: Option<Voice>,
    speaker: Option<Speaker>,
    /// Set while in a listening mode, including while paused during a reply.
    listen_mode: Option<Mode>,
    /// The mic was stopped for a reply because there's no echo cancellation.
    mic_paused: bool,
    /// Built on first use and kept, so its HTTP connection is reused.
    router: Option<Arc<Router>>,
    reply: Option<Reply>,
    generation: u64,
    windows: Option<SharedWindows>,
    clipboard: Option<SharedClipboard>,
    audio_devices: Vec<vivus_audio::Device>,
}

impl Engine {
    pub fn new(
        config: Config,
        config_file: ConfigFile,
        conn: Connection,
        tx: UnboundedSender<Command>,
        shared: Arc<Mutex<Shared>>,
        windows: Option<SharedWindows>,
        clipboard: Option<SharedClipboard>,
    ) -> Self {
        Self {
            config,
            config_file,
            conn,
            tx,
            shared,
            voice: None,
            speaker: None,
            listen_mode: None,
            mic_paused: false,
            router: None,
            reply: None,
            generation: 0,
            windows,
            clipboard,
            audio_devices: Vec::new(),
        }
    }

    pub async fn run(mut self, mut rx: UnboundedReceiver<Command>) {
        while let Some(cmd) = rx.recv().await {
            let chatty = matches!(
                cmd,
                Command::Speech(Event::Partial(_))
                    | Command::Reply {
                        event: ReplyEvent::Text(_),
                        ..
                    }
            );
            if !chatty {
                tracing::debug!(?cmd, "command");
            }
            if matches!(cmd, Command::Quit) {
                tracing::info!("quit requested");
                // While the audio threads are all up, so ducked apps get restored.
                self.cancel_reply();
                break;
            }
            if let Err(e) = self.handle(cmd).await {
                tracing::error!("handling command: {e:#}");
            }
        }
    }

    async fn handle(&mut self, cmd: Command) -> zbus::Result<()> {
        match cmd {
            Command::VoiceReady(voice) => match *voice {
                Ok(v) => {
                    tracing::info!("voice ready");
                    self.voice = Some(v);
                }
                Err(e) => tracing::error!("voice unavailable: {e:#}"),
            },
            Command::SpeakerReady(speaker) => match *speaker {
                Ok(s) => {
                    tracing::info!("speech output ready");
                    self.speaker = Some(s);
                }
                Err(e) => tracing::error!("speech output unavailable: {e:#}"),
            },
            Command::SpeakingFinished => {
                // A reply still in progress means this is from an older, cancelled one.
                if self.state() == AssistantState::Speaking && self.reply.is_none() {
                    self.after_reply().await?;
                }
            }
            Command::ToggleListen => {
                if self.listen_mode.is_some() {
                    self.stop_listening().await?;
                } else {
                    self.start_listening(Mode::Vad).await?;
                }
            }
            Command::StartListening => self.start_listening(Mode::Hold).await?,
            Command::StopListening => {
                if self.listen_mode == Some(Mode::Hold) {
                    if let Some(v) = &mut self.voice {
                        v.finish();
                    }
                    self.listen_mode = None;
                    // The final transcript arrives as a Speech event.
                    self.set_state(AssistantState::Thinking).await?;
                } else {
                    self.stop_listening().await?;
                }
            }
            Command::Cancel => self.stop_listening().await?,
            Command::WindowFocused(title) => {
                if let Some(s) = &self.speaker {
                    s.prefetch(&title);
                }
            }
            Command::Quit => {} // handled in run()
            Command::Ask(text) => {
                self.transcript("user", &text, true).await?;
                self.respond(&text).await?;
            }
            Command::Speech(event) => self.on_speech(event).await?,
            Command::EditSetting { path, value, reply } => {
                let result = self.edit_setting(&path, value).await;
                if let Err(e) = &result {
                    tracing::info!("setting {path} refused: {e:#}");
                }
                let _ = reply.send(result.map_err(|e| format!("{e:#}")));
            }
            Command::ConfigFileChanged => self.reload_config().await?,
            Command::AudioDevices(list) => self.on_audio_devices(list).await?,
            Command::Reply { generation, event } => {
                if self
                    .reply
                    .as_ref()
                    .is_some_and(|r| r.generation == generation)
                {
                    self.on_reply(event).await?;
                }
            }
        }
        Ok(())
    }

    async fn start_listening(&mut self, mode: Mode) -> zbus::Result<()> {
        // Talking over a reply interrupts it.
        self.cancel_reply();
        let Some(voice) = &mut self.voice else {
            tracing::warn!("listen requested but voice isn't available (see earlier errors)");
            return Ok(());
        };
        voice.start(mode);
        // Push-to-talk: talking starts with the key, not when the VAD notices.
        self.user_talking(mode == Mode::Hold);
        self.listen_mode = Some(mode);
        self.mic_paused = false;
        self.set_state(AssistantState::Listening).await
    }

    async fn stop_listening(&mut self) -> zbus::Result<()> {
        self.cancel_reply();
        self.user_talking(false);
        if let Some(v) = &mut self.voice {
            v.stop();
        }
        self.listen_mode = None;
        self.mic_paused = false;
        self.set_state(AssistantState::Idle).await
    }

    async fn on_speech(&mut self, event: Event) -> zbus::Result<()> {
        match &event {
            Event::SpeechPossible => self.user_talking(true),
            Event::NotSpeech | Event::Final(_) => self.user_talking(false),
            _ => {}
        }
        match event {
            // Pause first, so a cough costs a hiccup rather than the reply.
            Event::SpeechPossible | Event::NotSpeech
                if self.state() == AssistantState::Speaking
                    && self.listen_mode == Some(Mode::Vad) =>
            {
                if let Some(s) = &self.speaker {
                    if event == Event::SpeechPossible {
                        tracing::debug!("possible speech: pausing");
                        s.pause();
                    } else {
                        tracing::debug!("not speech: resuming");
                        s.resume();
                    }
                }
            }
            Event::SpeechPossible | Event::NotSpeech => {}
            Event::SpeechStarted => {
                // Barge-in: talking over a reply (or its thinking) drops it.
                let busy = self.reply.is_some() || self.state() == AssistantState::Speaking;
                if busy && self.listen_mode == Some(Mode::Vad) {
                    tracing::info!("barge-in: speech during {}", self.state());
                    self.cancel_reply();
                    self.set_state(AssistantState::Listening).await?;
                }
            }
            Event::Partial(text) => self.transcript("user", &text, false).await?,
            Event::Final(text) if text.is_empty() || self.config.debug.transcribe_only => {
                if !text.is_empty() {
                    tracing::info!("heard (transcribe only): {text}");
                }
                self.transcript("user", &text, true).await?;
                if self.listen_mode.is_none() {
                    self.set_state(AssistantState::Idle).await?;
                }
            }
            Event::Final(text) => {
                tracing::info!("heard: {text}");
                if let Some(s) = &self.speaker {
                    // Names the user says tend to come back in the reply.
                    s.prefetch(&text);
                }
                self.transcript("user", &text, true).await?;
                self.respond(&text).await?;
            }
        }
        Ok(())
    }

    async fn respond(&mut self, text: &str) -> zbus::Result<()> {
        self.cancel_reply();
        self.set_state(AssistantState::Thinking).await?;

        let router = match self.router() {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("router unavailable: {e:#}");
                return self
                    .say_only("I can't answer yet: no language model is set up.")
                    .await;
            }
        };
        self.generation += 1;
        let generation = self.generation;
        let (tx, shared, text) = (self.tx.clone(), self.shared.clone(), text.to_owned());
        let task = tokio::spawn(async move {
            router
                .respond(&text, &shared, |event| {
                    let _ = tx.send(Command::Reply { generation, event });
                })
                .await;
        });
        self.reply = Some(Reply {
            generation,
            task: Some(task.abort_handle()),
            text: String::new(),
            speaking: false,
        });
        Ok(())
    }

    fn router(&mut self) -> anyhow::Result<Arc<Router>> {
        if self.router.is_none() {
            let mut router = Router::from_config(&self.config)?;
            router.windows = self.windows.clone();
            router.clipboard = self.clipboard.clone();
            self.router = Some(Arc::new(router));
        }
        Ok(self.router.clone().unwrap())
    }

    async fn edit_setting(
        &mut self,
        path: &str,
        value: Option<SettingValue>,
    ) -> anyhow::Result<()> {
        let file = self
            .config_file
            .path
            .clone()
            .context("no config path (HOME is unset)")?;
        // Edit what's on disk now, in case a hand edit hasn't been reloaded yet.
        let current = Config::read(&file)?;
        let (text, config) = vivus_core::settings::edit(&current, &file, path, value)?;
        settings::write(&file, &text)?;
        self.config_file.text = text;
        Ok(self.apply_config(config).await?)
    }

    async fn reload_config(&mut self) -> zbus::Result<()> {
        let Some(file) = &self.config_file.path else {
            return Ok(());
        };
        let text = match Config::read(file) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("{e}");
                return Ok(());
            }
        };
        if text == self.config_file.text {
            return Ok(());
        }
        self.config_file.text = text;
        match Config::parse(&self.config_file.text, file) {
            Ok(config) => self.apply_config(config).await,
            Err(e) => {
                tracing::warn!("config has errors, keeping the last good one: {e}");
                Ok(())
            }
        }
    }

    async fn apply_config(&mut self, config: Config) -> zbus::Result<()> {
        let paths = vivus_core::settings::changed_paths(&self.config, &config);
        if paths.is_empty() {
            return Ok(());
        }
        tracing::info!("config changed: {}", paths.join(", "));
        let later: Vec<_> = paths
            .iter()
            .filter(|p| settings::needs_restart(p))
            .cloned()
            .collect();
        if !later.is_empty() {
            tracing::info!("applies after restart: {}", later.join(", "));
        }
        // Rebuilt from the new config on the next turn.
        if paths
            .iter()
            .any(|p| p.starts_with("roles.") || p.starts_with("providers."))
        {
            self.router = None;
        }
        self.shared.lock().unwrap().config_json = config.to_json();
        self.config = config;
        if paths.iter().any(|p| settings::reopens_mic(p))
            && let Some(v) = &mut self.voice
        {
            v.reconnect(&self.config.audio);
        }
        let iface = self
            .conn
            .object_server()
            .interface::<_, service::Settings>(vivus_dbus::PATH)
            .await?;
        service::Settings::changed(iface.signal_emitter(), &paths).await
    }

    async fn on_audio_devices(&mut self, list: Vec<vivus_audio::Device>) -> zbus::Result<()> {
        let present = |devs: &[vivus_audio::Device], name: &str| {
            devs.iter()
                .any(|d| d.name == name && d.direction == vivus_audio::Direction::Source)
        };
        // A stream whose target vanished stays on the fallback device; move it back.
        if let Some(input) = &self.config.audio.input
            && present(&list, input)
            && !present(&self.audio_devices, input)
            && !self.audio_devices.is_empty()
            && let Some(v) = &mut self.voice
        {
            tracing::info!("input {input} is back");
            v.reconnect(&self.config.audio);
        }
        let wire: Vec<vivus_dbus::AudioDevice> = list
            .iter()
            .map(|d| {
                (
                    d.name.clone(),
                    d.description.clone(),
                    d.is_default,
                    d.direction.as_str().into(),
                )
            })
            .collect();
        tracing::debug!("audio devices: {wire:?}");
        self.shared.lock().unwrap().audio_devices = wire.clone();
        self.audio_devices = list;
        let iface = self
            .conn
            .object_server()
            .interface::<_, service::Settings>(vivus_dbus::PATH)
            .await?;
        service::Settings::audio_devices_changed(iface.signal_emitter(), &wire).await
    }

    async fn on_reply(&mut self, event: ReplyEvent) -> zbus::Result<()> {
        match event {
            ReplyEvent::Text(t) => {
                let reply = self.reply.as_mut().unwrap();
                reply.text.push_str(&t);
                let start = !reply.speaking && !reply.text.trim().is_empty();
                let text = reply.text.trim().to_owned();
                if let Some(speaker) = &self.speaker {
                    if start {
                        // Without echo cancellation the mic would hear the reply.
                        if let (Some(_), Some(voice)) = (self.listen_mode, &mut self.voice)
                            && !voice.echo_cancelled()
                        {
                            voice.stop();
                            self.mic_paused = true;
                        }
                        self.reply.as_mut().unwrap().speaking = true;
                        self.set_state(AssistantState::Speaking).await?;
                    }
                    speaker.say(&t);
                }
                if !text.is_empty() {
                    self.transcript("assistant", &text, false).await?;
                }
            }
            ReplyEvent::Flush => {
                if let Some(speaker) = &self.speaker {
                    speaker.flush();
                }
            }
            ReplyEvent::Document(doc) => {
                self.document_changed(&doc).await?;
                self.request_sidebar("show").await?;
            }
            ReplyEvent::Done => {
                let reply = self.reply.take().unwrap();
                self.transcript("assistant", reply.text.trim(), true)
                    .await?;
                match &self.speaker {
                    Some(speaker) if reply.speaking => speaker.end(),
                    _ => self.after_reply().await?,
                }
            }
        }
        Ok(())
    }

    /// A fixed spoken line outside the router (e.g. when it isn't configured).
    async fn say_only(&mut self, line: &str) -> zbus::Result<()> {
        self.generation += 1;
        let generation = self.generation;
        self.reply = Some(Reply {
            generation,
            task: None,
            text: String::new(),
            speaking: false,
        });
        self.on_reply(ReplyEvent::Text(line.into())).await?;
        self.on_reply(ReplyEvent::Done).await
    }

    /// Back to listening in conversation mode, else idle.
    async fn after_reply(&mut self) -> zbus::Result<()> {
        match (self.listen_mode, &mut self.voice) {
            (Some(mode), Some(voice)) => {
                // Restarting would drop speech already in progress.
                if std::mem::take(&mut self.mic_paused) {
                    voice.start(mode);
                }
                self.set_state(AssistantState::Listening).await
            }
            _ => self.set_state(AssistantState::Idle).await,
        }
    }

    /// Drops the reply in progress: the LLM request closes and TTS goes quiet.
    fn user_talking(&self, talking: bool) {
        if let Some(s) = &self.speaker {
            s.user_talking(talking);
        }
    }

    fn cancel_reply(&mut self) {
        if let Some(task) = self.reply.take().and_then(|r| r.task) {
            task.abort();
        }
        if let Some(s) = &self.speaker {
            s.stop();
        }
    }

    fn state(&self) -> AssistantState {
        self.shared.lock().unwrap().state
    }

    async fn set_state(&self, state: AssistantState) -> zbus::Result<()> {
        {
            let mut shared = self.shared.lock().unwrap();
            if shared.state == state {
                return Ok(());
            }
            shared.state = state;
        }
        tracing::info!("state: {state}");
        let iface = self.assistant().await?;
        iface
            .get()
            .await
            .state_changed(iface.signal_emitter())
            .await
    }

    async fn transcript(&self, role: &str, text: &str, is_final: bool) -> zbus::Result<()> {
        let iface = self.assistant().await?;
        service::Assistant::transcript(iface.signal_emitter(), role, text, is_final).await
    }

    async fn request_sidebar(&self, action: &str) -> zbus::Result<()> {
        let iface = self.assistant().await?;
        service::Assistant::sidebar_visibility_requested(iface.signal_emitter(), action).await
    }

    async fn document_changed(&self, doc: &vivus_core::Document) -> zbus::Result<()> {
        let iface = self
            .conn
            .object_server()
            .interface::<_, service::Documents>(vivus_dbus::PATH)
            .await?;
        service::Documents::document_changed(
            iface.signal_emitter(),
            &doc.id,
            &doc.title,
            &doc.markdown,
        )
        .await
    }

    async fn assistant(
        &self,
    ) -> zbus::Result<zbus::object_server::InterfaceRef<service::Assistant>> {
        self.conn
            .object_server()
            .interface::<_, service::Assistant>(vivus_dbus::PATH)
            .await
    }
}
