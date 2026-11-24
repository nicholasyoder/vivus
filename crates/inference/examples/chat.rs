//! Try a provider from the config: streams a reply, offering a `speak` tool and
//! printing its `text` as it arrives.
//!
//!     cargo run -p vivus-inference --example chat -- [--role answer] What time zone is Tokyo in?

use std::io::Write;

use anyhow::{Context, bail};
use serde_json::json;
use vivus_core::Config;
use vivus_inference::partial::StringField;
use vivus_inference::{ChatRequest, Client, Event, Message, Tool};

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
        vec![
            Message::System("You are a voice assistant. Answer by calling `speak` with one or two short sentences.".into()),
            Message::User(args.join(" ")),
        ],
    );
    req.tools.push(Tool {
        name: "speak".into(),
        description: "Say something aloud to the user.".into(),
        parameters: json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
    });

    let start = std::time::Instant::now();
    let mut first = None;
    let mut first_spoken = None;
    let mut arg_chunks = 0;
    let mut speak: Option<(usize, StringField)> = None;
    let mut stream = client.chat(&req).await?;
    let mut out = std::io::stdout();
    while let Some(ev) = stream.next().await {
        first.get_or_insert_with(|| start.elapsed());
        match ev? {
            Event::Text(t) => print!("{t}"),
            Event::Reasoning(_) => {}
            Event::ToolCallStart { index, name, .. } => {
                print!("\n[{name}] ");
                if name == "speak" {
                    speak = Some((index, StringField::new("text")));
                }
            }
            Event::ToolCallArgs { index, delta } => {
                arg_chunks += 1;
                match &mut speak {
                    Some((i, f)) if *i == index => {
                        if let Some(t) = f.push(&delta) {
                            first_spoken.get_or_insert_with(|| start.elapsed());
                            print!("{t}");
                        }
                    }
                    _ => print!("{delta}"),
                }
            }
            Event::Done(reply) => {
                println!(
                    "\n\n-- first event {:?}, first spoken text {:?}, total {:?}, {arg_chunks} argument chunks, finish {:?}, usage {:?}",
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
