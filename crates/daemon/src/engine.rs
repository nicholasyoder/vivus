//! Owns assistant behaviour. Currently a stub: `Ask` echoes into a sidebar
//! document so the D-Bus/sidebar plumbing can be exercised end to end.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedReceiver;
use vivus_core::{AssistantState, Config, DocumentStore};
use zbus::Connection;

use crate::service;

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
}

pub struct Engine {
    #[allow(dead_code)] // used once inference lands
    config: Config,
    conn: Connection,
    shared: Arc<Mutex<Shared>>,
}

impl Engine {
    pub fn new(config: Config, conn: Connection, shared: Arc<Mutex<Shared>>) -> Self {
        Self {
            config,
            conn,
            shared,
        }
    }

    pub async fn run(self, mut rx: UnboundedReceiver<Command>) {
        while let Some(cmd) = rx.recv().await {
            tracing::debug!(?cmd, "command");
            if let Err(e) = self.handle(cmd).await {
                tracing::error!("handling command: {e:#}");
            }
        }
    }

    async fn handle(&self, cmd: Command) -> zbus::Result<()> {
        match cmd {
            Command::ToggleListen => {
                let next = match self.state() {
                    AssistantState::Listening => AssistantState::Idle,
                    _ => AssistantState::Listening,
                };
                self.set_state(next).await?;
            }
            Command::StartListening => self.set_state(AssistantState::Listening).await?,
            Command::StopListening | Command::Cancel => self.set_state(AssistantState::Idle).await?,
            Command::Ask(text) => self.ask(&text).await?,
        }
        Ok(())
    }

    async fn ask(&self, text: &str) -> zbus::Result<()> {
        self.transcript("user", text).await?;
        self.set_state(AssistantState::Thinking).await?;

        let markdown = format!(
            "> {text}\n\n*No inference backend yet; this document is a placeholder.*\n"
        );
        let doc = self.shared.lock().unwrap().documents.create(text, markdown).clone();
        self.document_changed(&doc).await?;
        self.request_sidebar("show").await?;

        self.transcript("assistant", "Inference isn't wired up yet.").await?;
        self.set_state(AssistantState::Idle).await
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
        iface.get().await.state_changed(iface.signal_emitter()).await
    }

    async fn transcript(&self, role: &str, text: &str) -> zbus::Result<()> {
        let iface = self.assistant().await?;
        service::Assistant::transcript(iface.signal_emitter(), role, text).await
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
        service::Documents::document_changed(iface.signal_emitter(), &doc.id, &doc.title, &doc.markdown)
            .await
    }

    async fn assistant(&self) -> zbus::Result<zbus::object_server::InterfaceRef<service::Assistant>> {
        self.conn
            .object_server()
            .interface::<_, service::Assistant>(vivus_dbus::PATH)
            .await
    }
}
