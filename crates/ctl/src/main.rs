mod models;

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use vivus_dbus::AssistantProxy;

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
        Cmd::Models { .. } => unreachable!(),
    };
    result.context("talking to vivus-daemon (is it running?)")
}
