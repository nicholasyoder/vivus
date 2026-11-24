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
}

#[derive(Clone, clap::ValueEnum)]
enum PttAction {
    Start,
    Stop,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let conn = zbus::Connection::session().await?;
    let proxy = AssistantProxy::new(&conn).await?;

    let result = match cli.command {
        Cmd::ToggleListen => proxy.toggle_listen().await,
        Cmd::PushToTalk { action: PttAction::Start } => proxy.start_listening().await,
        Cmd::PushToTalk { action: PttAction::Stop } => proxy.stop_listening().await,
        Cmd::Ask { text } => proxy.ask(&text.join(" ")).await,
        Cmd::Cancel => proxy.cancel().await,
        Cmd::Quit => proxy.quit().await,
        Cmd::ToggleSidebar => proxy.toggle_sidebar().await,
        Cmd::Status => proxy.state().await.map(|s| println!("{s}")),
    };
    result.context("talking to vivus-daemon (is it running?)")
}
