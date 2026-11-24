//! The router: one streaming call per request. Its text content is the spoken
//! reply; `show` puts material in the sidebar, `escalate` hands off (stubbed
//! until the answer/agent roles land). See docs/inference.md#routing.

use std::sync::Mutex;
use std::time::Instant;

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;
use vivus_core::{Config, Document};
use vivus_inference::{ChatRequest, Client, Event, Message, Tool, ToolCall, ToolChoice};

use crate::engine::Shared;

/// Groq's free tier rejects requests asking for more than 1000 output tokens per minute.
const MAX_TOKENS: u32 = 1000;
/// The first call plus follow-ups after tool results.
const MAX_ROUNDS: usize = 3;

const SYSTEM: &str = "\
You are Vivus, a voice assistant on the user's Linux desktop. Your text reply is \
spoken aloud by a speech synthesizer, and a sidebar next to it can show documents.

Rules for the spoken reply:
- One or two short, natural sentences. No markdown, lists, emoji, URLs or code.
- Write numbers, times, dates, units, currency and symbols as words \
(\"twenty-five degrees\", \"half past three\", \"forty percent\"), and spell out \
abbreviations you'd say as words.
- Short factual answers are spoken only; don't call a tool for them.
- Anything with lists, steps, code, links, tables, or more than about two sentences: \
call `show` with the full detail as markdown, AND say a one-sentence spoken summary \
before calling it. Never call `show` without saying something first.
- Never read code, URLs or long numbers aloud; put them in the sidebar.
- If the request needs current information, research, more care than a quick \
answer, or actions on the computer you have no tool for, first say a short filler \
line (\"Let me look into that.\"), then call `escalate`.

Examples:
User: what's the capital of Australia
Assistant: Canberra.
User: give me a packing list for a weekend hike
Assistant: I've put a weekend hiking packing list in the sidebar. \
[calls show with title \"Weekend hike packing list\" and the list as markdown]
User: what's the latest news about AI
Assistant: Let me look that up. [calls escalate with role \"answer\"]";

pub struct Router {
    client: Client,
    model: String,
}

/// What a reply produces, in order; always ends with `Done`.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyEvent {
    /// Spoken text, as it streams.
    Text(String),
    /// A sidebar document was created or changed.
    Document(Document),
    Done,
}

impl Router {
    pub fn new(client: Client, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    pub fn from_config(config: &Config) -> anyhow::Result<Self> {
        let role = config
            .roles
            .router
            .as_ref()
            .context("no [roles.router] in the config")?;
        let model = role.model.clone().context("[roles.router] has no model")?;
        let provider = &config.providers[&role.provider];
        let client = Client::from_provider(provider)
            .with_context(|| format!("router provider `{}`", role.provider))?;
        Ok(Self::new(client, model))
    }

    /// Answers one request. Dropping the future cancels the call in flight.
    pub async fn respond(
        &self,
        text: &str,
        shared: &Mutex<Shared>,
        emit: impl Fn(ReplyEvent) + Sync,
    ) {
        let start = Instant::now();
        let mut turn = Turn {
            shared,
            emit: &emit,
            spoken: false,
            shown: false,
        };
        if let Err(e) = self.run(text, &mut turn, start).await {
            tracing::error!("router: {e:#}");
            turn.say(" Sorry, I couldn't reach the language model.");
        }
        if !turn.spoken {
            // Models sometimes call `show` and say nothing despite the prompt.
            turn.say(if turn.shown {
                "I've put that in the sidebar."
            } else {
                "Sorry, I don't have an answer for that."
            });
        }
        tracing::info!("router: reply done in {:.2} s", start.elapsed().as_secs_f64());
        emit(ReplyEvent::Done);
    }

    async fn run(&self, text: &str, turn: &mut Turn<'_>, start: Instant) -> anyhow::Result<()> {
        let mut messages = vec![Message::System(SYSTEM.into()), Message::User(text.into())];
        for round in 0..MAX_ROUNDS {
            let mut req = ChatRequest::new(&self.model, messages.clone());
            req.tools = tools();
            req.max_tokens = Some(MAX_TOKENS);
            if round + 1 == MAX_ROUNDS {
                req.tool_choice = Some(ToolChoice::None);
            }
            let mut stream = self.client.chat(&req).await?;
            let mut reply = None;
            let mut separated = !turn.spoken;
            while let Some(ev) = stream.next().await {
                match ev? {
                    Event::Text(t) => {
                        if !turn.spoken && !t.trim().is_empty() {
                            tracing::info!(
                                "router: first text after {:.2} s",
                                start.elapsed().as_secs_f64()
                            );
                        }
                        // Keep a follow-up's first sentence from running into the last one.
                        if !separated {
                            turn.say(" ");
                            separated = true;
                        }
                        turn.say(&t);
                    }
                    Event::Done(r) => reply = Some(r),
                    _ => {}
                }
            }
            let reply = reply.context("stream ended without a reply")?;
            if reply.tool_calls.is_empty() {
                return Ok(());
            }
            messages.push(Message::from(&reply));
            let mut follow_up = !turn.spoken;
            for call in &reply.tool_calls {
                let (result, needs_answer) = turn.tool(call);
                follow_up |= needs_answer;
                messages.push(Message::Tool {
                    tool_call_id: call.id.clone(),
                    content: result,
                });
            }
            if !follow_up {
                return Ok(());
            }
        }
        Ok(())
    }
}

struct Turn<'a> {
    shared: &'a Mutex<Shared>,
    emit: &'a (dyn Fn(ReplyEvent) + Sync),
    /// Some non-blank text has been emitted.
    spoken: bool,
    shown: bool,
}

#[derive(Deserialize)]
struct ShowArgs {
    #[serde(default)]
    title: Option<String>,
    markdown: String,
    #[serde(default)]
    document_id: Option<String>,
    #[serde(default)]
    append: bool,
}

#[derive(Deserialize)]
struct EscalateArgs {
    role: String,
    task: String,
}

impl Turn<'_> {
    fn say(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !text.trim().is_empty() {
            self.spoken = true;
        }
        (self.emit)(ReplyEvent::Text(text.into()));
    }

    /// Runs a tool call; returns its result and whether the model must answer it.
    fn tool(&mut self, call: &ToolCall) -> (String, bool) {
        tracing::info!("router: tool {} {}", call.name, call.arguments);
        match call.name.as_str() {
            "show" => match call.parse_args::<ShowArgs>() {
                Ok(args) => {
                    let doc = self.show(args);
                    let result = format!("Shown in the sidebar as document {}.", doc.id);
                    (self.emit)(ReplyEvent::Document(doc));
                    self.shown = true;
                    (result, false)
                }
                Err(e) => (format!("Invalid arguments: {e}"), true),
            },
            "escalate" => match call.parse_args::<EscalateArgs>() {
                Ok(args) => {
                    tracing::warn!("router: escalation to {} not implemented: {}", args.role, args.task);
                    (
                        "Escalation isn't available yet. In one short sentence, tell the user \
                         you can't do that yet."
                            .into(),
                        true,
                    )
                }
                Err(e) => (format!("Invalid arguments: {e}"), true),
            },
            other => (format!("Unknown tool `{other}`."), true),
        }
    }

    fn show(&self, args: ShowArgs) -> Document {
        let mut shared = self.shared.lock().unwrap();
        let docs = &mut shared.documents;
        let existing = args.document_id.as_deref().filter(|id| docs.get(id).is_some());
        let doc = match existing {
            Some(id) if args.append => docs.append(id, &format!("\n\n{}", args.markdown)),
            Some(id) => docs.replace(id, args.markdown),
            None => {
                let title = args
                    .title
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| title_from(&args.markdown));
                Some(docs.create(title, args.markdown))
            }
        };
        doc.expect("document exists").clone()
    }
}

/// The first heading or line, for documents the model didn't title.
fn title_from(markdown: &str) -> String {
    let line = markdown.lines().find(|l| !l.trim().is_empty()).unwrap_or("Document");
    line.trim_start_matches('#').trim().chars().take(80).collect()
}

fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "show".into(),
            description: "Show a markdown document in the sidebar: lists, steps, code, \
                          links, tables or anything longer than two sentences. Say a \
                          one-sentence spoken summary as well."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "Short document title."},
                    "markdown": {"type": "string"},
                    "document_id": {"type": "string", "description": "Existing document to change; omit for a new one."},
                    "append": {"type": "boolean", "description": "With document_id: append instead of replacing."}
                },
                "required": ["markdown"]
            }),
        },
        Tool {
            name: "escalate".into(),
            description: "Hand the request to a stronger model. `answer` for knowledge, \
                          current events and web questions; `agent` for multi-step tasks \
                          and computer control. Say a short filler line first."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "role": {"type": "string", "enum": ["answer", "agent"]},
                    "task": {"type": "string", "description": "The request, restated with any needed context."}
                },
                "required": ["role", "task"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// Serves one canned SSE response per connection, in order; returns the request bodies.
    async fn serve(responses: Vec<Vec<String>>) -> (Router, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        tokio::spawn(async move {
            for chunks in responses {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    let n = sock.read(&mut tmp).await.unwrap();
                    buf.extend_from_slice(&tmp[..n]);
                    let text = String::from_utf8_lossy(&buf);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len: usize = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            log.lock().unwrap().push(text[end + 4..].to_string());
                            break;
                        }
                    }
                }
                let mut out = String::from(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                );
                for c in chunks {
                    out.push_str(&format!("data: {c}\n\n"));
                }
                out.push_str("data: [DONE]\n\n");
                sock.write_all(out.as_bytes()).await.unwrap();
                let _ = sock.shutdown().await;
            }
        });
        (Router::new(Client::new(url, None), "m"), requests)
    }

    fn text(t: &str) -> String {
        json!({"choices":[{"delta":{"content":t}}]}).to_string()
    }

    fn call(name: &str, args: serde_json::Value) -> String {
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1",
            "function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
        .to_string()
    }

    async fn respond(router: &Router, shared: &Mutex<Shared>) -> Vec<ReplyEvent> {
        let events = Mutex::new(Vec::new());
        router.respond("hi", shared, |e| events.lock().unwrap().push(e)).await;
        events.into_inner().unwrap()
    }

    fn spoken(events: &[ReplyEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                ReplyEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn text_and_show_in_one_call() {
        let (router, requests) = serve(vec![vec![
            text("Here's the "),
            text("list."),
            call("show", json!({"title":"List","markdown":"- a\n- b"})),
        ]])
        .await;
        let shared = Mutex::new(Shared::default());
        let events = respond(&router, &shared).await;

        assert_eq!(spoken(&events), "Here's the list.");
        let doc = events
            .iter()
            .find_map(|e| match e {
                ReplyEvent::Document(d) => Some(d.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!((doc.title.as_str(), doc.markdown.as_str()), ("List", "- a\n- b"));
        assert_eq!(events.last(), Some(&ReplyEvent::Done));
        // Spoken and shown: no follow-up call.
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert!(requests.lock().unwrap()[0].contains("\"max_tokens\":1000"));
    }

    #[tokio::test]
    async fn silent_show_gets_a_follow_up() {
        let (router, requests) = serve(vec![
            vec![call("show", json!({"markdown":"# Packing\n- tent"}))],
            vec![text("It's in the sidebar.")],
        ])
        .await;
        let shared = Mutex::new(Shared::default());
        let events = respond(&router, &shared).await;

        assert_eq!(spoken(&events), "It's in the sidebar.");
        assert_eq!(shared.lock().unwrap().documents.get("doc-1").unwrap().title, "Packing");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].contains("\"role\":\"tool\""));
    }

    #[tokio::test]
    async fn escalation_stub_answers_after_the_filler() {
        let (router, _) = serve(vec![
            vec![text("Let me check."), call("escalate", json!({"role":"answer","task":"news"}))],
            vec![text("I can't do that yet.")],
        ])
        .await;
        let shared = Mutex::new(Shared::default());
        let events = respond(&router, &shared).await;
        assert_eq!(spoken(&events), "Let me check. I can't do that yet.");
    }

    #[tokio::test]
    async fn unreachable_provider_apologises() {
        // Nothing listens here.
        let router = Router::new(Client::new("http://127.0.0.1:9/v1", None), "m");
        let shared = Mutex::new(Shared::default());
        let events = respond(&router, &shared).await;
        assert!(spoken(&events).contains("couldn't reach"));
        assert_eq!(events.last(), Some(&ReplyEvent::Done));
    }
}
