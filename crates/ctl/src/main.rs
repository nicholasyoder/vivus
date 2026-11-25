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
    Set { path: String, value: String },
    /// Remove a key so its default applies.
    Reset { path: String },
    /// List audio inputs and outputs: the names `audio.input` / `audio.output` take.
    Devices,
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
    }
    Ok(())
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
        _ => anyhow::bail!("only strings, numbers, booleans and arrays can be set"),
    })
}
