//! Owns assistant behaviour. Inference isn't wired up yet: a final transcript
//! (or `Ask`) produces a placeholder sidebar document.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedReceiver;
use vivus_core::{AssistantState, Config, DocumentStore};
use vivus_speech::{Event, Mode};
use zbus::Connection;

use crate::service;
use crate::voice::Voice;

/// State read synchronously by D-Bus property/method handlers. Never hold the
/// lock across an await.
#[derive(Default)]
pub struct Shared {
    pub state: AssistantState,
    pub documents: DocumentStore,
}

#[derive(Debug)]
pub enum Command {
    ToggleListen,
    StartListening,
    StopListening,
    Ask(String),
    Cancel,
    VoiceReady(Box<anyhow::Result<Voice>>),
    Speech(Event),
}

pub struct Engine {
    #[allow(dead_code)] // used once inference lands
    config: Config,
    conn: Connection,
    shared: Arc<Mutex<Shared>>,
    voice: Option<Voice>,
    /// Set while the mic is open.
    listen_mode: Option<Mode>,
}

impl Engine {
    pub fn new(config: Config, conn: Connection, shared: Arc<Mutex<Shared>>) -> Self {
        Self {
            config,
            conn,
            shared,
            voice: None,
            listen_mode: None,
        }
    }

    pub async fn run(mut self, mut rx: UnboundedReceiver<Command>) {
        while let Some(cmd) = rx.recv().await {
            if !matches!(cmd, Command::Speech(Event::Partial(_))) {
                tracing::debug!(?cmd, "command");
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
                    if let Some(v) = &self.voice {
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
            Command::Ask(text) => {
                self.transcript("user", &text, true).await?;
                self.respond(&text).await?;
            }
            Command::Speech(event) => self.on_speech(event).await?,
        }
        Ok(())
    }

    async fn start_listening(&mut self, mode: Mode) -> zbus::Result<()> {
        let Some(voice) = &self.voice else {
            tracing::warn!("listen requested but voice isn't available (see earlier errors)");
            return Ok(());
        };
        voice.start(mode);
        self.listen_mode = Some(mode);
        self.set_state(AssistantState::Listening).await
    }

    async fn stop_listening(&mut self) -> zbus::Result<()> {
        if let Some(v) = &self.voice {
            v.stop();
        }
        self.listen_mode = None;
        self.set_state(AssistantState::Idle).await
    }

    async fn on_speech(&mut self, event: Event) -> zbus::Result<()> {
        match event {
            Event::SpeechStarted => {}
            Event::Partial(text) => self.transcript("user", &text, false).await?,
            Event::Final(text) if text.is_empty() => {
                self.transcript("user", "", true).await?;
                if self.listen_mode.is_none() {
                    self.set_state(AssistantState::Idle).await?;
                }
            }
            Event::Final(text) => {
                tracing::info!("heard: {text}");
                self.transcript("user", &text, true).await?;
                self.respond(&text).await?;
            }
        }
        Ok(())
    }

    /// Placeholder until the router lands. Returns to listening in conversation mode.
    async fn respond(&self, text: &str) -> zbus::Result<()> {
        self.set_state(AssistantState::Thinking).await?;

        let markdown =
            format!("> {text}\n\n*No inference backend yet; this document is a placeholder.*\n");
        let doc = self.shared.lock().unwrap().documents.create(text, markdown).clone();
        self.document_changed(&doc).await?;
        self.request_sidebar("show").await?;
        self.transcript("assistant", "Inference isn't wired up yet.", true).await?;

        let next = match self.listen_mode {
            Some(_) => AssistantState::Listening,
            None => AssistantState::Idle,
        };
        self.set_state(next).await
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
        iface.get().await.state_changed(iface.signal_emitter()).await
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
