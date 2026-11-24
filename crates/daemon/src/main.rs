mod engine;
mod service;
mod speaker;
mod voice;

use std::sync::{Arc, Mutex};

use anyhow::Context;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use vivus_core::Config;

use crate::engine::{Command, Engine, Shared};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = match Config::default_path() {
        Some(path) => {
            tracing::info!("config: {}", path.display());
            Config::load(&path)?
        }
        None => Config::default(),
    };

    let shared = Arc::new(Mutex::new(Shared::default()));
    let (tx, rx) = mpsc::unbounded_channel();

    let conn = zbus::connection::Builder::session()?
        .name(vivus_dbus::SERVICE)?
        .serve_at(vivus_dbus::PATH, service::Assistant::new(tx.clone(), shared.clone()))?
        .serve_at(vivus_dbus::PATH, service::Documents::new(shared.clone()))?
        .build()
        .await
        .context("registering org.vivus on the session bus (is another daemon running?)")?;
    tracing::info!("serving {} at {}", vivus_dbus::SERVICE, vivus_dbus::PATH);

    // Model loading takes a moment; D-Bus is already up meanwhile.
    let voice_config = config.clone();
    let voice_tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let voice = voice::Voice::new(&voice_config, voice_tx.clone());
        let _ = voice_tx.send(Command::VoiceReady(Box::new(voice)));
    });
    let speaker_config = config.clone();
    let speaker_tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let speaker = speaker::Speaker::new(&speaker_config, speaker_tx.clone());
        let _ = speaker_tx.send(Command::SpeakerReady(Box::new(speaker)));
    });

    let engine = tokio::spawn(Engine::new(config, conn, shared).run(rx));

    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
        res = engine => res?,
    }
    Ok(())
}
