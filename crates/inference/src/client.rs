//! OpenAI-compatible `/chat/completions` over HTTP with SSE streaming.

use std::collections::VecDeque;
use std::time::Duration;

use serde_json::Value;
use vivus_core::config::Provider;

use crate::chat::{Assembler, ChatRequest, Event, Reply, WireChunk};
use crate::sse::SseParser;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("API key ${0} is not set (environment or ~/.config/vivus/.env)")]
    MissingKey(String),
    #[error("provider is not an openai-compatible provider")]
    WrongKind,
    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("HTTP {status}: {message}")]
    Status { status: u16, message: String },
    #[error("provider error: {0}")]
    Api(String),
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl Client {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // Per read, not total: replies can stream for a long time.
            .read_timeout(Duration::from_secs(60))
            .build()
            .expect("building HTTP client");
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
        }
    }

    /// Builds a client for an `openai-compatible` provider; the key comes from
    /// the environment or `~/.config/vivus/.env`.
    pub fn from_provider(provider: &Provider) -> Result<Self, Error> {
        let Provider::OpenaiCompatible {
            base_url,
            api_key_env,
        } = provider
        else {
            return Err(Error::WrongKind);
        };
        let api_key = match api_key_env {
            Some(var) => Some(
                vivus_core::config::secret(var).ok_or_else(|| Error::MissingKey(var.clone()))?,
            ),
            None => None,
        };
        Ok(Self::new(base_url, api_key))
    }

    /// Starts a streaming completion. Dropping the stream cancels the request.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatStream, Error> {
        let mut body = serde_json::to_value(req).expect("request serializes");
        body["stream"] = true.into();
        body["stream_options"] = serde_json::json!({"include_usage": true});

        let mut rb = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        if let Some(key) = &self.api_key {
            rb = rb.bearer_auth(key);
        }
        let resp = rb.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Status {
                status: status.as_u16(),
                message: error_message(&text),
            });
        }
        // Some servers ignore `stream` and answer with one JSON body.
        let json_body = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("json"));
        Ok(ChatStream {
            resp: Some(resp),
            json_body,
            body: Vec::new(),
            sse: SseParser::default(),
            asm: Some(Assembler::default()),
            queue: VecDeque::new(),
        })
    }

    /// Runs a completion to the end and returns the assembled reply.
    pub async fn complete(&self, req: &ChatRequest) -> Result<Reply, Error> {
        let mut stream = self.chat(req).await?;
        while let Some(ev) = stream.next().await {
            if let Event::Done(reply) = ev? {
                return Ok(reply);
            }
        }
        unreachable!("a stream always ends with Done or an error")
    }
}

pub struct ChatStream {
    resp: Option<reqwest::Response>,
    json_body: bool,
    body: Vec<u8>,
    sse: SseParser,
    asm: Option<Assembler>,
    queue: VecDeque<Event>,
}

impl ChatStream {
    /// The next event; `None` after [`Event::Done`] or an error.
    pub async fn next(&mut self) -> Option<Result<Event, Error>> {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Some(Ok(ev));
            }
            let resp = self.resp.as_mut()?;
            let result = match resp.chunk().await {
                Ok(Some(bytes)) if self.json_body => {
                    self.body.extend_from_slice(&bytes);
                    Ok(())
                }
                Ok(Some(bytes)) => {
                    let events = self.sse.push(&bytes);
                    events.into_iter().try_for_each(|d| self.data(&d))
                }
                Ok(None) => {
                    let tail = if self.json_body {
                        Some(String::from_utf8_lossy(&self.body).into_owned())
                    } else {
                        self.sse.finish()
                    };
                    let r = tail.map_or(Ok(()), |d| self.data(&d));
                    self.end();
                    r
                }
                Err(e) => Err(e.into()),
            };
            if let Err(e) = result {
                self.resp = None;
                self.asm = None;
                self.queue.clear();
                return Some(Err(e));
            }
        }
    }

    fn data(&mut self, data: &str) -> Result<(), Error> {
        let data = data.trim();
        if data == "[DONE]" {
            self.end();
            return Ok(());
        }
        let Some(asm) = self.asm.as_mut() else {
            return Ok(());
        };
        let chunk: WireChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(%e, data, "skipping unparseable stream chunk");
                return Ok(());
            }
        };
        if let Some(err) = &chunk.error {
            return Err(Error::Api(value_message(err)));
        }
        asm.chunk(chunk, &mut self.queue);
        Ok(())
    }

    fn end(&mut self) {
        self.resp = None;
        if let Some(asm) = self.asm.take() {
            asm.finish(&mut self.queue);
        }
    }
}

/// Pulls `error.message` out of an error body when there is one.
fn error_message(body: &str) -> String {
    match serde_json::from_str::<Value>(body) {
        Ok(v) => value_message(v.get("error").unwrap_or(&v)),
        Err(_) => body.chars().take(500).collect(),
    }
}

fn value_message(err: &Value) -> String {
    match err {
        Value::String(s) => s.clone(),
        v => v
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| v.to_string(), str::to_owned),
    }
}
