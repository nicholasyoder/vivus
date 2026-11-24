//! D-Bus server side of `dbus/org.vivus.Assistant.xml`. Methods only enqueue
//! commands; the engine owns all behaviour so it can cancel in-flight work.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface};

use crate::engine::{Command, Shared};

pub struct Assistant {
    tx: UnboundedSender<Command>,
    shared: Arc<Mutex<Shared>>,
}

impl Assistant {
    pub fn new(tx: UnboundedSender<Command>, shared: Arc<Mutex<Shared>>) -> Self {
        Self { tx, shared }
    }

    fn send(&self, cmd: Command) -> fdo::Result<()> {
        self.tx
            .send(cmd)
            .map_err(|_| fdo::Error::Failed("engine is not running".into()))
    }
}

#[interface(name = "org.vivus.Assistant")]
impl Assistant {
    #[zbus(property)]
    async fn state(&self) -> String {
        self.shared.lock().unwrap().state.as_str().into()
    }

    async fn toggle_listen(&self) -> fdo::Result<()> {
        self.send(Command::ToggleListen)
    }

    async fn start_listening(&self) -> fdo::Result<()> {
        self.send(Command::StartListening)
    }

    async fn stop_listening(&self) -> fdo::Result<()> {
        self.send(Command::StopListening)
    }

    async fn ask(&self, text: String) -> fdo::Result<()> {
        self.send(Command::Ask(text))
    }

    async fn cancel(&self) -> fdo::Result<()> {
        self.send(Command::Cancel)
    }

    async fn toggle_sidebar(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        Ok(Self::sidebar_visibility_requested(&emitter, "toggle").await?)
    }

    #[zbus(signal)]
    pub async fn transcript(
        emitter: &SignalEmitter<'_>,
        role: &str,
        text: &str,
        is_final: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn sidebar_visibility_requested(
        emitter: &SignalEmitter<'_>,
        action: &str,
    ) -> zbus::Result<()>;
}

pub struct Documents {
    shared: Arc<Mutex<Shared>>,
}

impl Documents {
    pub fn new(shared: Arc<Mutex<Shared>>) -> Self {
        Self { shared }
    }
}

#[interface(name = "org.vivus.Documents")]
impl Documents {
    async fn list(&self) -> Vec<String> {
        self.shared.lock().unwrap().documents.ids()
    }

    async fn get(&self, id: &str) -> fdo::Result<(String, String)> {
        let shared = self.shared.lock().unwrap();
        let doc = shared
            .documents
            .get(id)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("no document `{id}`")))?;
        Ok((doc.title.clone(), doc.markdown.clone()))
    }

    #[zbus(signal)]
    pub async fn document_changed(
        emitter: &SignalEmitter<'_>,
        id: &str,
        title: &str,
        markdown: &str,
    ) -> zbus::Result<()>;
}
