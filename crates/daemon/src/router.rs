//! The router: one streaming call per request. Its text content is the spoken
//! reply; `show` puts material in the sidebar, `escalate` hands the request to
//! the `answer` or `agent` role. See docs/inference.md#routing.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::json;
use vivus_core::config::Provider;
use vivus_core::{Config, Document};
use vivus_inference::{
    ChatRequest, ClaudeCode, ClaudeEvent, ClaudeRequest, Client, Event, Message, Tool, ToolCall,
    ToolChoice,
};

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
- Short factual answers are spoken only; don't call a tool for them.
- Anything with lists, steps, code, links, tables, or more than about two sentences: \
call `show` with the full detail as markdown, AND say a one-sentence spoken summary \
before calling it. Never call `show` without saying something first.
- Never read code, URLs or long numbers aloud; put them in the sidebar.
- If the request needs current information, research, more care than a quick \
answer, or actions on the computer you have no tool for, first say a short filler \
line (\"Let me look into that.\"), then call `escalate`. Role `answer` is for \
questions: explanations, knowledge, the news. Role `agent` is for tasks with several \
steps: research that compares sources, anything on this computer (files, programs, \
settings), or doing something rather than saying something.

Examples:
User: what's the capital of Australia
Assistant: Canberra.
User: give me a packing list for a weekend hike
Assistant: I've put a weekend hiking packing list in the sidebar. \
[calls show with title \"Weekend hike packing list\" and the list as markdown]
User: what's the latest news about AI
Assistant: Let me look that up. [calls escalate with role \"answer\"]
User: compare the three best-reviewed budget mechanical keyboards
Assistant: On it. [calls escalate with role \"agent\"]";

/// For an OpenAI-compatible `answer` or `agent` role.
const ESCALATED_SYSTEM: &str = "\
You are Vivus, a voice assistant on the user's Linux desktop. A quicker model \
handed this request to you and has already told the user it's working on it, so \
don't open with a filler line. Your text reply is spoken aloud by a speech \
synthesizer, and a sidebar next to it can show documents.

- Speak one or two short, natural sentences: no markdown, lists, emoji, URLs or code.
- Put detail (lists, steps, code, links, tables, anything longer) in the sidebar \
with `show`, and say a one-sentence spoken summary before calling it.
- You have no web access or tools on the computer yet. If the request needs \
current information you don't have, say so briefly rather than guessing.";

/// Appended to Claude Code's system prompt.
const CLAUDE_SYSTEM: &str = "\
You are the agent behind Vivus, a voice assistant on the user's Linux desktop. \
The user spoke this request and has already been told you're on it. Your final \
message is split: its first paragraph is read aloud by a speech synthesizer, and \
everything after the first blank line goes to a sidebar as markdown. So start the \
final message with one or two plain spoken sentences (no markdown, URLs or \
code), then a blank line, then the full detail as markdown if there is any, with \
sources as links.";

/// Spoken at once when the router escalates without saying anything first.
const ANSWER_FILLERS: &[&str] = &["Let me look into that.", "One moment.", "Let me check."];
const AGENT_FILLERS: &[&str] = &["On it.", "Working on it.", "Sure, give me a moment."];

/// A role the router can hand a request to.
pub enum Backend {
    Chat { client: Client, model: String },
    ClaudeCode { cc: ClaudeCode, model: Option<String> },
}

impl Backend {
    fn from_config(config: &Config, name: &str) -> anyhow::Result<Option<Self>> {
        let Some((_, role)) = config.roles.iter().find(|(n, _)| *n == name) else {
            return Ok(None);
        };
        let provider = &config.providers[&role.provider];
        let backend = match provider {
            Provider::ClaudeCode { .. } => Backend::ClaudeCode {
                cc: ClaudeCode::from_provider(provider)?,
                model: role.model.clone(),
            },
            _ => Backend::Chat {
                client: Client::from_provider(provider)?,
                model: role.model.clone().context("no model")?,
            },
        };
        Ok(Some(backend))
    }
}

pub struct Router {
    client: Client,
    model: String,
    answer: Option<Backend>,
    /// Falls back to `answer` when unset.
    agent: Option<Backend>,
    fillers: AtomicUsize,
}

/// What a reply produces, in order; always ends with `Done`.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyEvent {
    /// Spoken text, as it streams.
    Text(String),
    /// A sidebar document was created or changed.
    Document(Document),
    /// More text may be a while: speak what's buffered (e.g. the filler) now.
    Flush,
    Done,
}

impl Router {
    pub fn new(client: Client, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            answer: None,
            agent: None,
            fillers: AtomicUsize::new(0),
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
        let mut router = Self::new(client, model);
        // A broken escalation role shouldn't take the router down with it.
        for name in ["answer", "agent"] {
            let backend = Backend::from_config(config, name).unwrap_or_else(|e| {
                tracing::error!("[roles.{name}] unavailable: {e:#}");
                None
            });
            match name {
                "answer" => router.answer = backend,
                _ => router.agent = backend,
            }
        }
        Ok(router)
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
            answered: false,
            shown: false,
            start,
        };
        let messages = vec![Message::System(SYSTEM.into()), Message::User(text.into())];
        let result = match self
            .converse(&self.client, &self.model, messages, tools(), Some(MAX_TOKENS), &mut turn)
            .await
        {
            Ok(Some(esc)) => self.escalate(text, esc, &mut turn).await,
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            tracing::error!("reply: {e:#}");
            turn.say_sentence("Sorry, I couldn't reach the language model.");
        }
        if !turn.answered {
            // Models sometimes call `show` and say nothing despite the prompt.
            turn.say_sentence(if turn.shown {
                "I've put that in the sidebar."
            } else {
                "Sorry, I don't have an answer for that."
            });
        }
        tracing::info!("reply done in {:.2} s", start.elapsed().as_secs_f64());
        emit(ReplyEvent::Done);
    }

    /// Streams content into speech and runs tools, for up to `MAX_ROUNDS` calls.
    /// Stops early with the arguments of a valid `escalate` call.
    async fn converse(
        &self,
        client: &Client,
        model: &str,
        mut messages: Vec<Message>,
        tools: Vec<Tool>,
        max_tokens: Option<u32>,
        turn: &mut Turn<'_>,
    ) -> anyhow::Result<Option<EscalateArgs>> {
        for round in 0..MAX_ROUNDS {
            let mut req = ChatRequest::new(model, messages.clone());
            req.tools = tools.clone();
            req.max_tokens = max_tokens;
            if round + 1 == MAX_ROUNDS {
                req.tool_choice = Some(ToolChoice::None);
            }
            let mut stream = client.chat(&req).await?;
            let mut reply = None;
            let mut separated = !turn.spoken;
            while let Some(ev) = stream.next().await {
                match ev? {
                    Event::Text(t) => {
                        if !turn.answered && !t.trim().is_empty() {
                            tracing::info!(
                                "{model}: first text after {:.2} s",
                                turn.start.elapsed().as_secs_f64()
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
            turn.flush();
            if reply.tool_calls.is_empty() {
                return Ok(None);
            }
            messages.push(Message::from(&reply));
            let mut follow_up = !turn.answered;
            let mut escalation = None;
            for call in &reply.tool_calls {
                let (result, needs_answer) = match call.name.as_str() {
                    "escalate" => match call.parse_args::<EscalateArgs>() {
                        Ok(args) => {
                            escalation = Some(args);
                            ("Handed off.".into(), false)
                        }
                        Err(e) => (format!("Invalid arguments: {e}"), true),
                    },
                    _ => turn.tool(call),
                };
                follow_up |= needs_answer;
                messages.push(Message::Tool {
                    tool_call_id: call.id.clone(),
                    content: result,
                });
            }
            if escalation.is_some() {
                return Ok(escalation);
            }
            if !follow_up {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// Hands the request to `answer` or `agent`, after a filler line if the
    /// router hasn't said anything yet.
    async fn escalate(
        &self,
        text: &str,
        esc: EscalateArgs,
        turn: &mut Turn<'_>,
    ) -> anyhow::Result<()> {
        let agent = esc.role == "agent";
        let backend = if agent {
            self.agent.as_ref().or(self.answer.as_ref())
        } else {
            self.answer.as_ref()
        };
        tracing::info!("escalating to {}: {}", esc.role, esc.task);
        let Some(backend) = backend else {
            tracing::warn!("no [roles.{}] configured", esc.role);
            turn.say_sentence("Sorry, I can't do that yet: no model is set up for it.");
            turn.answered = true;
            return Ok(());
        };
        if !turn.answered {
            let fillers = if agent { AGENT_FILLERS } else { ANSWER_FILLERS };
            let i = self.fillers.fetch_add(1, Ordering::Relaxed) % fillers.len();
            turn.say_sentence(fillers[i]);
        }
        turn.flush();
        // From here on, only the escalated role's words count as the answer.
        turn.answered = false;
        let prompt = format!("The user said: \"{text}\"\nTask: {}", esc.task);
        match backend {
            Backend::Chat { client, model } => {
                let messages = vec![
                    Message::System(ESCALATED_SYSTEM.into()),
                    Message::User(prompt),
                ];
                // No `escalate` here, so a call to it gets "unknown tool".
                self.converse(client, model, messages, vec![show_tool()], None, turn)
                    .await?;
            }
            Backend::ClaudeCode { cc, model } => {
                let req = ClaudeRequest {
                    prompt,
                    system: Some(CLAUDE_SYSTEM.into()),
                    model: model.clone(),
                };
                let mut run = cc.run(&req).await?;
                while let Some(ev) = run.next().await {
                    match ev? {
                        ClaudeEvent::ToolUse { name } => tracing::info!("claude: tool {name}"),
                        ClaudeEvent::Text(_) => {}
                        ClaudeEvent::Result(r) => {
                            tracing::info!(
                                "claude: done after {:.1} s, ${:.4}",
                                turn.start.elapsed().as_secs_f64(),
                                r.cost_usd.unwrap_or(0.0)
                            );
                            if r.is_error {
                                bail!("Claude Code: {}", r.text);
                            }
                            turn.deliver(&r.text, &esc.task);
                        }
                    }
                }
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
    /// The current responder (router, or the role it escalated to) has said something.
    answered: bool,
    shown: bool,
    start: Instant,
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

#[derive(Debug, Deserialize)]
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
            self.answered = true;
        }
        (self.emit)(ReplyEvent::Text(text.into()));
    }

    fn flush(&self) {
        (self.emit)(ReplyEvent::Flush);
    }

    /// A whole sentence, separated from anything said before it.
    fn say_sentence(&mut self, text: &str) {
        if self.spoken {
            self.say(" ");
        }
        self.say(text);
    }

    /// Speaks the first paragraph of a finished answer and shows the rest.
    fn deliver(&mut self, text: &str, task: &str) {
        let text = text.trim();
        let (first, rest) = text.split_once("\n\n").unwrap_or((text, ""));
        let first = first.trim();
        let markdown_first = first.starts_with(['#', '-', '*', '`', '|', '>'])
            || first.split_once(". ").is_some_and(|(n, _)| n.parse::<u32>().is_ok());
        // Ignored the format: show it all and let the caller's fallback speak.
        let (spoken, detail) = if markdown_first || first.len() > 400 {
            ("", text)
        } else {
            (first, rest.trim())
        };
        if !detail.is_empty() {
            let title = heading(detail).unwrap_or_else(|| short_title(task));
            self.show(ShowArgs {
                title: Some(title),
                markdown: detail.into(),
                document_id: None,
                append: false,
            });
        }
        if !spoken.is_empty() {
            self.say_sentence(spoken);
        }
    }

    /// Runs a `show` (or unknown) tool call; returns its result and whether
    /// the model must answer it.
    fn tool(&mut self, call: &ToolCall) -> (String, bool) {
        tracing::info!("tool {} {}", call.name, call.arguments);
        match call.name.as_str() {
            "show" => match call.parse_args::<ShowArgs>() {
                Ok(args) => {
                    let id = self.show(args);
                    (format!("Shown in the sidebar as document {id}."), false)
                }
                Err(e) => (format!("Invalid arguments: {e}"), true),
            },
            other => (format!("Unknown tool `{other}`."), true),
        }
    }

    /// Creates or changes a document and emits it; returns its id.
    fn show(&mut self, args: ShowArgs) -> String {
        let doc = {
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
        };
        let id = doc.id.clone();
        (self.emit)(ReplyEvent::Document(doc));
        self.shown = true;
        id
    }
}

/// The first heading or line, for documents the model didn't title.
fn title_from(markdown: &str) -> String {
    heading(markdown).unwrap_or_else(|| {
        let line = markdown.lines().find(|l| !l.trim().is_empty()).unwrap_or("Document");
        short_title(line)
    })
}

/// The first line, if it's a heading.
fn heading(markdown: &str) -> Option<String> {
    let line = markdown.lines().find(|l| !l.trim().is_empty())?.trim();
    line.starts_with('#')
        .then(|| short_title(line.trim_start_matches('#')))
}

fn short_title(text: &str) -> String {
    text.trim().chars().take(80).collect()
}

fn show_tool() -> Tool {
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
    }
}

fn tools() -> Vec<Tool> {
    vec![
        show_tool(),
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
    async fn silent_escalation_says_a_filler_then_the_answer() {
        let (mut router, requests) = serve(vec![
            vec![call("escalate", json!({"role":"answer","task":"AI news"}))],
            vec![text("Nothing big today.")],
        ])
        .await;
        router.answer = Some(Backend::Chat {
            client: router.client.clone(),
            model: "big".into(),
        });
        let shared = Mutex::new(Shared::default());
        let events = respond(&router, &shared).await;

        assert_eq!(spoken(&events), "Let me look into that. Nothing big today.");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let second: serde_json::Value = serde_json::from_str(&requests[1]).unwrap();
        assert_eq!(second["model"], "big");
        assert!(second["messages"][1]["content"].as_str().unwrap().contains("Task: AI news"));
        let tools: Vec<_> = second["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].clone())
            .collect();
        assert_eq!(tools, vec![json!("show")]);
        assert!(second.get("max_tokens").is_none());
    }

    #[tokio::test]
    async fn spoken_filler_is_not_doubled() {
        let (mut router, _) = serve(vec![
            vec![text("Let me check."), call("escalate", json!({"role":"agent","task":"t"}))],
            vec![call("show", json!({"markdown":"# Result\n- x"}))],
            vec![text("It's in the sidebar.")],
        ])
        .await;
        // No agent: falls back to answer.
        router.answer = Some(Backend::Chat {
            client: router.client.clone(),
            model: "big".into(),
        });
        let shared = Mutex::new(Shared::default());
        let events = respond(&router, &shared).await;
        // The answer role showed silently, so it got a follow-up round.
        assert_eq!(spoken(&events), "Let me check. It's in the sidebar.");
        assert!(shared.lock().unwrap().documents.get("doc-1").is_some());
    }

    #[tokio::test]
    async fn escalation_without_a_role_apologises_without_filler() {
        let (router, requests) = serve(vec![vec![call(
            "escalate",
            json!({"role":"answer","task":"news"}),
        )]])
        .await;
        let shared = Mutex::new(Shared::default());
        let events = respond(&router, &shared).await;
        assert_eq!(spoken(&events), "Sorry, I can't do that yet: no model is set up for it.");
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn claude_code_result_is_spoken_and_shown() {
        use std::os::unix::fs::PermissionsExt;
        let (mut router, _) = serve(vec![vec![call(
            "escalate",
            json!({"role":"agent","task":"research X"}),
        )]])
        .await;
        let path = std::env::temp_dir().join(format!("vivus-router-claude-{}", std::process::id()));
        let result = json!({"type":"result","subtype":"success","is_error":false,
            "result":"X is a thing.\n\n## About X\n- [source](https://x.example)"});
        std::fs::write(&path, format!("#!/bin/sh\ncat >/dev/null\necho '{result}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        router.agent = Some(Backend::ClaudeCode {
            cc: ClaudeCode::new(path.to_str().unwrap(), vec![]),
            model: None,
        });
        let shared = Mutex::new(Shared::default());
        let events = respond(&router, &shared).await;
        let _ = std::fs::remove_file(&path);

        assert_eq!(spoken(&events), "On it. X is a thing.");
        // The filler is flushed to TTS before the agent runs.
        let filler = events.iter().position(|e| *e == ReplyEvent::Text("On it.".into()));
        assert_eq!(events.get(filler.unwrap() + 1), Some(&ReplyEvent::Flush));
        let shared = shared.lock().unwrap();
        let doc = shared.documents.get("doc-1").unwrap();
        assert_eq!(doc.title, "About X");
        assert!(doc.markdown.starts_with("## About X"));
    }

    #[test]
    fn deliver_shows_markdown_only_answers_whole() {
        let shared = Mutex::new(Shared::default());
        let events = Mutex::new(Vec::new());
        let emit = |e| events.lock().unwrap().push(e);
        let mut turn = Turn {
            shared: &shared,
            emit: &emit,
            spoken: false,
            answered: false,
            shown: false,
            start: Instant::now(),
        };
        turn.deliver("- a\n- b\n\nmore", "List things");
        assert!(!turn.answered);
        let shared = shared.lock().unwrap();
        let doc = shared.documents.get("doc-1").unwrap();
        assert_eq!(
            (doc.title.as_str(), doc.markdown.as_str()),
            ("List things", "- a\n- b\n\nmore")
        );
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
