mod apps;
mod choices;
mod clipboard;
mod conversation;
mod engine;
mod keyring;
mod permission;
mod pronounce;
mod router;
mod service;
mod settings;
mod shortcuts;
mod speaker;
mod voice;
mod windows;

use std::sync::{Arc, Mutex};

use anyhow::Context;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use vivus_core::Config;

use crate::engine::{Command, ConfigFile, Engine, Services, Shared};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let path = Config::default_path();
    let text = match &path {
        Some(p) => {
            tracing::info!("config: {}", p.display());
            Config::read(p)?
        }
        None => String::new(),
    };
    let config = match &path {
        Some(p) => Config::parse(&text, p)?,
        None => Config::default(),
    };

    // Before anything builds an inference client.
    keyring::load().await;

    let shared = Arc::new(Mutex::new(Shared {
        config_json: config.to_json(),
        input_gain: -1.0,
        ..Default::default()
    }));
    let (tx, rx) = mpsc::unbounded_channel();

    // Dropping the watcher stops it, so it lives as long as main.
    let _watcher = path.as_deref().and_then(|p| {
        settings::watch(p, tx.clone())
            .inspect_err(|e| tracing::warn!("not watching the config file: {e:#}"))
            .ok()
    });

    let conn = zbus::connection::Builder::session()?
        .name(vivus_dbus::SERVICE)?
        .serve_at(
            vivus_dbus::PATH,
            service::Assistant::new(tx.clone(), shared.clone()),
        )?
        .serve_at(vivus_dbus::PATH, service::Documents::new(shared.clone()))?
        .serve_at(
            vivus_dbus::PATH,
            service::Settings::new(tx.clone(), shared.clone()),
        )?
        .build()
        .await
        .context("registering org.vivus on the session bus (is another daemon running?)")?;
    tracing::info!("serving {} at {}", vivus_dbus::SERVICE, vivus_dbus::PATH);

    // Model loading takes a moment; D-Bus is already up meanwhile.
    let voice_config = config.clone();
    let voice_tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let voice = voice::Voice::new(&voice_config, voice_tx.clone());
        let _ = voice_tx.send(Command::VoiceReady(0, Box::new(voice)));
    });
    let speaker_config = config.clone();
    let speaker_tx = tx.clone();
    let rt = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let speaker = speaker::Speaker::new(&speaker_config, speaker_tx.clone(), &rt);
        let _ = speaker_tx.send(Command::SpeakerReady(0, Box::new(speaker)));
    });

    let focus_tx = tx.clone();
    let windows = vivus_wayland::Windows::connect(Box::new(move |w| {
        let _ = focus_tx.send(Command::WindowFocused(w.title.clone()));
    }))
    .inspect_err(|e| tracing::warn!("window control unavailable: {e:#}"))
    .ok()
    .map(|w| Arc::new(w) as windows::SharedWindows);

    let clipboard = vivus_wayland::Clipboard::connect()
        .inspect_err(|e| tracing::warn!("clipboard unavailable: {e:#}"))
        .ok()
        .map(|c| Arc::new(c) as clipboard::SharedClipboard);

    let (devices_tx, volume_tx) = (tx.clone(), tx.clone());
    let devices = vivus_audio::Devices::spawn(
        move |list| {
            let _ = devices_tx.send(Command::AudioDevices(list));
        },
        move |name, volume| {
            let _ = volume_tx.send(Command::SourceVolume {
                name: name.into(),
                volume,
            });
        },
    )
    .inspect_err(|e| tracing::warn!("audio device list unavailable: {e:#}"))
    .ok();

    shortcuts::spawn(&config.shortcuts, tx.clone(), conn.clone());

    let engine = tokio::spawn(
        Engine::new(
            config,
            ConfigFile { path, text },
            conn,
            tx,
            shared,
            Services {
                windows,
                clipboard,
                devices,
            },
        )
        .run(rx),
    );

    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
        res = engine => res?,
    }
    Ok(())
}
