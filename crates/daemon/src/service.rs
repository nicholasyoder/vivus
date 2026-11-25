//! D-Bus server side of `dbus/org.vivus.Assistant.xml`. Methods only enqueue
//! commands; the engine owns all behaviour so it can cancel in-flight work.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use vivus_core::settings::Value as SettingValue;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
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

    async fn quit(&self) -> fdo::Result<()> {
        self.send(Command::Quit)
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

pub struct Settings {
    tx: UnboundedSender<Command>,
    shared: Arc<Mutex<Shared>>,
}

impl Settings {
    pub fn new(tx: UnboundedSender<Command>, shared: Arc<Mutex<Shared>>) -> Self {
        Self { tx, shared }
    }

    /// The engine owns the file; wait for it to accept or refuse the edit.
    async fn edit(&self, path: String, value: Option<SettingValue>) -> fdo::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::EditSetting { path, value, reply })
            .map_err(|_| fdo::Error::Failed("engine is not running".into()))?;
        rx.await
            .map_err(|_| fdo::Error::Failed("engine dropped the request".into()))?
            .map_err(fdo::Error::InvalidArgs)
    }
}

impl Settings {
    /// Sends a query to the engine and waits for its answer; its errors are `InvalidArgs`.
    async fn ask<T>(
        &self,
        cmd: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Command,
    ) -> fdo::Result<T> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(cmd(reply))
            .map_err(|_| fdo::Error::Failed("engine is not running".into()))?;
        rx.await
            .map_err(|_| fdo::Error::Failed("engine dropped the request".into()))?
            .map_err(fdo::Error::InvalidArgs)
    }
}

#[interface(name = "org.vivus.Settings")]
impl Settings {
    async fn get(&self) -> String {
        self.shared.lock().unwrap().config_json.clone()
    }

    async fn set(&self, path: String, value: OwnedValue) -> fdo::Result<()> {
        let value = crate::settings::to_toml(&value).map_err(fdo::Error::InvalidArgs)?;
        self.edit(path, Some(value)).await
    }

    async fn reset(&self, path: String) -> fdo::Result<()> {
        self.edit(path, None).await
    }

    async fn list_audio_devices(&self) -> Vec<vivus_dbus::AudioDevice> {
        self.shared.lock().unwrap().audio_devices.clone()
    }

    async fn list_providers(&self, role: String) -> fdo::Result<Vec<String>> {
        self.ask(|reply| Command::ListProviders { role, reply })
            .await
    }

    async fn list_models(
        &self,
        role: String,
        provider: String,
    ) -> fdo::Result<Vec<vivus_dbus::Choice>> {
        self.ask(|reply| Command::ListModels {
            role,
            provider,
            reply,
        })
        .await
    }

    async fn list_voices(
        &self,
        provider: String,
        model: String,
    ) -> fdo::Result<Vec<vivus_dbus::Choice>> {
        self.ask(|reply| Command::ListVoices {
            provider,
            model,
            reply,
        })
        .await
    }

    async fn set_api_key(&self, provider: String, key: String) -> fdo::Result<()> {
        self.ask(|reply| Command::SetApiKey {
            provider,
            key,
            reply,
        })
        .await
    }

    async fn api_key_status(&self, provider: String) -> fdo::Result<String> {
        self.ask(|reply| Command::ApiKeyStatus { provider, reply })
            .await
    }

    async fn meter_input(&self) -> fdo::Result<()> {
        self.tx
            .send(Command::MeterInput)
            .map_err(|_| fdo::Error::Failed("engine is not running".into()))
    }

    async fn get_input_gain(&self) -> f64 {
        self.shared.lock().unwrap().input_gain
    }

    async fn set_input_gain(&self, gain: f64) -> fdo::Result<()> {
        if !(0.0..=1.0).contains(&gain) {
            return Err(fdo::Error::InvalidArgs(format!(
                "gain {gain} is not between 0 and 1"
            )));
        }
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::SetInputGain {
                gain: gain as f32,
                reply,
            })
            .map_err(|_| fdo::Error::Failed("engine is not running".into()))?;
        rx.await
            .map_err(|_| fdo::Error::Failed("engine dropped the request".into()))?
            .map_err(fdo::Error::Failed)
    }

    #[zbus(signal)]
    pub async fn input_gain_changed(emitter: &SignalEmitter<'_>, gain: f64) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn input_level(
        emitter: &SignalEmitter<'_>,
        peak: f64,
        clipped: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn changed(emitter: &SignalEmitter<'_>, paths: &[String]) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn audio_devices_changed(
        emitter: &SignalEmitter<'_>,
        devices: &[vivus_dbus::AudioDevice],
    ) -> zbus::Result<()>;
}
