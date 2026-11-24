//! Try a provider from the config the way the router uses it: content is
//! speech, longer material goes through a `show` tool.
//!
//!     cargo run -p vivus-inference --example chat -- [--role answer] What time zone is Tokyo in?

use std::io::Write;

use anyhow::{Context, bail};
use serde_json::json;
use vivus_core::Config;
use vivus_inference::{ChatRequest, Client, Event, Message, Tool};

const SYSTEM: &str = "You are a voice assistant. Your text reply is spoken aloud: \
one or two short sentences, no lists, URLs or code. Put anything longer in the \
sidebar with the `show` tool and say a one-sentence summary.";

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut role_name = "router".to_string();
    if args.first().is_some_and(|a| a == "--role") && args.len() > 1 {
        role_name = args.remove(1);
        args.remove(0);
    }
    if args.is_empty() {
        bail!("usage: chat [--role NAME] PROMPT...");
    }

    let config = Config::load(&Config::default_path().context("no config path")?)?;
    let role = config
        .roles
        .iter()
        .find(|(n, _)| *n == role_name)
        .map(|(_, r)| r.clone())
        .with_context(|| format!("no [roles.{role_name}] in the config"))?;
    let client = Client::from_provider(&config.providers[&role.provider])?;
    let model = role.model.context("role has no model")?;

    let mut req = ChatRequest::new(
        model,
        vec![Message::System(SYSTEM.into()), Message::User(args.join(" "))],
    );
    req.tools.push(Tool {
        name: "show".into(),
        description: "Show a markdown document in the sidebar.".into(),
        parameters: json!({"type": "object", "properties": {"markdown": {"type": "string"}}, "required": ["markdown"]}),
    });

    let start = std::time::Instant::now();
    let mut first = None;
    let mut first_spoken = None;
    let mut arg_chunks = 0;
    let mut stream = client.chat(&req).await?;
    let mut out = std::io::stdout();
    print!("[spoken] ");
    while let Some(ev) = stream.next().await {
        first.get_or_insert_with(|| start.elapsed());
        match ev? {
            Event::Text(t) => {
                first_spoken.get_or_insert_with(|| start.elapsed());
                print!("{t}");
            }
            Event::Reasoning(_) => {}
            Event::ToolCallStart { name, .. } => print!("\n[{name}] "),
            Event::ToolCallArgs { delta, .. } => {
                arg_chunks += 1;
                print!("{delta}");
            }
            Event::Done(reply) => {
                println!(
                    "\n\n-- first event {:?}, first spoken text {:?}, total {:?}, {arg_chunks} tool-argument chunks, finish {:?}, usage {:?}",
                    first.unwrap_or_default(),
                    first_spoken,
                    start.elapsed(),
                    reply.finish_reason,
                    reply.usage
                );
            }
        }
        out.flush()?;
    }
    Ok(())
}
