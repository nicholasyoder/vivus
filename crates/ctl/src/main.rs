mod models;

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use vivus_dbus::{AssistantProxy, SettingsProxy};
use zbus::zvariant;

/// Control vivus-daemon. Bind these to keys on compositors without a
/// GlobalShortcuts portal.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start listening, or stop if already listening.
    ToggleListen,
    /// Push-to-talk: bind `start` to key press and `stop` to release.
    PushToTalk {
        #[arg(value_enum)]
        action: PttAction,
    },
    /// Send a text request, bypassing speech-to-text.
    Ask { text: Vec<String> },
    /// Interrupt whatever the assistant is doing.
    Cancel,
    /// Shut the daemon down.
    Quit,
    /// Show or hide the sidebar.
    ToggleSidebar,
    /// Print the assistant state.
    Status,
    /// Read or change the config through the daemon, which validates and applies it.
    Settings {
        #[command(subcommand)]
        action: SettingsCmd,
    },
    /// List or download local speech models (doesn't need the daemon).
    Models {
        #[command(subcommand)]
        action: ModelsCmd,
        /// Models directory; set it if the config's `models_dir` isn't the default.
        #[arg(long, global = true)]
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ModelsCmd {
    /// Show known models and which are installed.
    List,
    /// Download models: tiers (light, standard, all) or model names. Default: light.
    Fetch { which: Vec<String> },
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// Print the effective config as JSON, or one value by dotted path.
    Get { path: Option<String> },
    /// Set a dotted path (`audio.input`) to a TOML value; bare words are strings.
    /// An inline table writes a whole table: `providers.mine '{kind =
    /// "openai-compatible", base_url = "https://…/v1"}'`.
    Set { path: String, value: String },
    /// Remove a key so its default applies.
    Reset { path: String },
    /// List audio inputs and outputs: the names `audio.input` / `audio.output` take.
    Devices,
    /// Show the mic level (what speech recognition hears) until Ctrl+C.
    Meter,
    /// Print the mic's gain (its PipeWire volume, 0 to 1), or set it.
    Gain { value: Option<f64> },
    /// Configured providers that can serve a role (stt, tts, router, answer, agent, vision).
    Providers { role: String },
    /// Models for `roles.<role>.model`: the role's provider unless one is given.
    Models {
        role: String,
        provider: Option<String>,
    },
    /// TTS voices for `roles.tts.voice`: the configured model unless one is given.
    Voices { model: Option<String> },
    /// Where a provider's API key comes from; `--set` reads a new key from
    /// stdin (kept out of shell history), `--remove` deletes the stored one.
    Key {
        provider: String,
        #[arg(long, conflicts_with = "remove")]
        set: bool,
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum PttAction {
    Start,
    Stop,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Cmd::Models { action, dir } = &cli.command {
        let dir = match dir {
            Some(d) => d.clone(),
            None => models::default_dir()?,
        };
        return match action {
            ModelsCmd::List => {
                models::list(&dir);
                Ok(())
            }
            ModelsCmd::Fetch { which } if which.is_empty() => {
                models::fetch(&dir, &["light".into()]).await
            }
            ModelsCmd::Fetch { which } => models::fetch(&dir, which).await,
        };
    }
    let conn = zbus::Connection::session().await?;
    if let Cmd::Settings { action } = cli.command {
        return settings(&conn, action).await;
    }
    let proxy = AssistantProxy::new(&conn).await?;

    let result = match cli.command {
        Cmd::ToggleListen => proxy.toggle_listen().await,
        Cmd::PushToTalk {
            action: PttAction::Start,
        } => proxy.start_listening().await,
        Cmd::PushToTalk {
            action: PttAction::Stop,
        } => proxy.stop_listening().await,
        Cmd::Ask { text } => proxy.ask(&text.join(" ")).await,
        Cmd::Cancel => proxy.cancel().await,
        Cmd::Quit => proxy.quit().await,
        Cmd::ToggleSidebar => proxy.toggle_sidebar().await,
        Cmd::Status => proxy.state().await.map(|s| println!("{s}")),
        Cmd::Models { .. } | Cmd::Settings { .. } => unreachable!(),
    };
    result.context("talking to vivus-daemon (is it running?)")
}

async fn settings(conn: &zbus::Connection, action: SettingsCmd) -> anyhow::Result<()> {
    let proxy = SettingsProxy::new(conn).await?;
    match action {
        SettingsCmd::Get { path } => {
            let json = proxy
                .get()
                .await
                .context("talking to vivus-daemon (is it running?)")?;
            let mut value: serde_json::Value = serde_json::from_str(&json)?;
            for key in path.iter().flat_map(|p| p.split('.')) {
                value = value
                    .get_mut(key)
                    .map(serde_json::Value::take)
                    .with_context(|| format!("no setting `{}`", path.as_deref().unwrap_or("")))?;
            }
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        SettingsCmd::Set { path, value } => {
            let value = value
                .parse::<toml_edit::Value>()
                .unwrap_or_else(|_| value.as_str().into());
            proxy.set(&path, &to_variant(&value)?).await?;
        }
        SettingsCmd::Reset { path } => proxy.reset(&path).await?,
        SettingsCmd::Devices => {
            let devices = proxy
                .list_audio_devices()
                .await
                .context("talking to vivus-daemon (is it running?)")?;
            for (name, description, is_default, direction) in devices {
                let mark = if is_default { "*" } else { " " };
                println!("{direction:<6} {mark} {name}\n           {description}");
            }
        }
        SettingsCmd::Gain { value: Some(v) } => proxy
            .set_input_gain(v)
            .await
            .context("talking to vivus-daemon (is it running?)")?,
        SettingsCmd::Gain { value: None } => {
            let gain = proxy
                .get_input_gain()
                .await
                .context("talking to vivus-daemon (is it running?)")?;
            if gain < 0.0 {
                println!("unknown");
            } else {
                println!("{gain:.2}");
            }
        }
        SettingsCmd::Providers { role } => {
            for p in proxy.list_providers(&role).await.context(DAEMON)? {
                println!("{p}");
            }
        }
        SettingsCmd::Models { role, provider } => {
            let provider = match provider {
                Some(p) => p,
                None => role_value(&proxy, &role, "provider").await?,
            };
            print_choices(proxy.list_models(&role, &provider).await.context(DAEMON)?);
        }
        SettingsCmd::Voices { model } => {
            let provider = role_value(&proxy, "tts", "provider").await?;
            let model = match model {
                Some(m) => m,
                None => role_value(&proxy, "tts", "model").await?,
            };
            print_choices(proxy.list_voices(&provider, &model).await.context(DAEMON)?);
        }
        SettingsCmd::Key {
            provider,
            set,
            remove,
        } => {
            if set || remove {
                let mut key = String::new();
                if set {
                    let tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
                    let stty = |arg| std::process::Command::new("stty").arg(arg).status();
                    if tty {
                        eprint!("API key for {provider}: ");
                        let _ = stty("-echo");
                    }
                    let read = std::io::stdin().read_line(&mut key);
                    if tty {
                        let _ = stty("echo");
                        eprintln!();
                    }
                    read?;
                    anyhow::ensure!(!key.trim().is_empty(), "no key given");
                }
                proxy
                    .set_api_key(&provider, key.trim())
                    .await
                    .context(DAEMON)?;
            }
            println!("{}", proxy.api_key_status(&provider).await.context(DAEMON)?);
        }
        SettingsCmd::Meter => {
            use futures_util::StreamExt;
            let mut levels = proxy.receive_input_level().await?;
            let mut lease = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tokio::select! {
                    _ = lease.tick() => proxy.meter_input().await
                        .context("talking to vivus-daemon (is it running?)")?,
                    Some(sig) = levels.next() => {
                        let args = sig.args()?;
                        let db = 20.0 * args.peak.max(1e-6).log10();
                        let bar = "#".repeat(((db + 60.0).max(0.0) / 60.0 * 50.0) as usize);
                        let clip = if args.clipped { " CLIP" } else { "" };
                        println!("{db:6.1} dBFS {bar:<50}{clip}");
                    }
                }
            }
        }
    }
    Ok(())
}

const DAEMON: &str = "talking to vivus-daemon (is it running?)";

/// `roles.<role>.<key>` from the effective config; empty if unset.
async fn role_value(proxy: &SettingsProxy<'_>, role: &str, key: &str) -> anyhow::Result<String> {
    let json: serde_json::Value = serde_json::from_str(&proxy.get().await.context(DAEMON)?)?;
    Ok(json["roles"][role][key]
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

/// An empty id is the default (unset).
fn print_choices(list: Vec<(String, String)>) {
    for (id, label) in list {
        let id = if id.is_empty() { "(unset)" } else { &id };
        if *id == label {
            println!("{id}");
        } else {
            println!("{id:<28} {label}");
        }
    }
}

fn to_variant(v: &toml_edit::Value) -> anyhow::Result<zvariant::Value<'static>> {
    use toml_edit::Value as T;
    Ok(match v {
        T::String(s) => s.value().clone().into(),
        T::Integer(n) => (*n.value()).into(),
        T::Float(n) => (*n.value()).into(),
        T::Boolean(b) => (*b.value()).into(),
        T::Array(a) => a
            .iter()
            .map(|v| to_variant(v).map(zvariant::Value::new))
            .collect::<anyhow::Result<Vec<_>>>()?
            .into(),
        T::InlineTable(t) => {
            let mut dict = std::collections::HashMap::new();
            for (k, v) in t.iter() {
                dict.insert(k.to_owned(), zvariant::Value::new(to_variant(v)?));
            }
            zvariant::Value::from(dict)
        }
        _ => anyhow::bail!("only strings, numbers, booleans, arrays and tables can be set"),
    })
}
