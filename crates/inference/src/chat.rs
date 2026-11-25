//! Chat completion request types and the assembly of streamed deltas.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};

#[derive(Debug, Clone, Default, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Vec::is_empty", serialize_with = "ser_tools")]
    pub tools: Vec<Tool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// `low` / `medium` / `high` for reasoning models that take it (Groq's
    /// gpt-oss, OpenAI o-series); others may reject the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    System(String),
    User(String),
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = match self {
            Message::System(c) => json!({"role": "system", "content": c}),
            Message::User(c) => json!({"role": "user", "content": c}),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let mut v = json!({"role": "assistant", "content": content});
                if !tool_calls.is_empty() {
                    v["tool_calls"] = tool_calls.iter().map(ToolCall::to_wire).collect();
                }
                v
            }
            Message::Tool {
                tool_call_id,
                content,
            } => json!({"role": "tool", "tool_call_id": tool_call_id, "content": content}),
        };
        v.serialize(s)
    }
}

impl From<&Reply> for Message {
    /// The assistant turn to append to the history before sending tool results.
    fn from(r: &Reply) -> Self {
        Message::Assistant {
            content: (!r.content.is_empty()).then(|| r.content.clone()),
            tool_calls: r.tool_calls.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

fn ser_tools<S: Serializer>(tools: &[Tool], s: S) -> Result<S::Ok, S::Error> {
    let v: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({"type": "function", "function": {
                "name": t.name, "description": t.description, "parameters": t.parameters,
            }})
        })
        .collect();
    v.serialize(s)
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Function(String),
}

impl Serialize for ToolChoice {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            ToolChoice::Auto => s.serialize_str("auto"),
            ToolChoice::None => s.serialize_str("none"),
            ToolChoice::Required => s.serialize_str("required"),
            ToolChoice::Function(name) => {
                json!({"type": "function", "function": {"name": name}}).serialize(s)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON text as the model produced it; may be invalid.
    pub arguments: String,
}

impl ToolCall {
    pub fn parse_args<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        // Some models send an empty string for a tool with no parameters.
        let a = self.arguments.trim();
        serde_json::from_str(if a.is_empty() { "{}" } else { a })
    }

    fn to_wire(&self) -> Value {
        json!({"id": self.id, "type": "function",
               "function": {"name": self.name, "arguments": self.arguments}})
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

impl From<String> for FinishReason {
    fn from(s: String) -> Self {
        match s.as_str() {
            "stop" | "end_turn" => Self::Stop,
            "length" | "max_tokens" => Self::Length,
            "tool_calls" | "function_call" | "tool_use" => Self::ToolCalls,
            "content_filter" => Self::ContentFilter,
            _ => Self::Other(s),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
}

/// The whole assistant turn, assembled from the stream.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reply {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<FinishReason>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Text(String),
    /// Thinking tokens (`reasoning` / `reasoning_content`), for models that expose them.
    Reasoning(String),
    /// `index` is the call's position in [`Reply::tool_calls`], not the provider's index.
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolCallArgs {
        index: usize,
        delta: String,
    },
    /// Always the last event of a successful stream.
    Done(Reply),
}

// Wire format. Everything optional: providers differ in what they omit.

#[derive(Deserialize)]
pub(crate) struct WireChunk {
    #[serde(default)]
    choices: Vec<WireChoice>,
    usage: Option<Usage>,
    pub(crate) error: Option<Value>,
}

#[derive(Deserialize)]
struct WireChoice {
    // Non-streaming responses carry `message` with the same shape.
    #[serde(alias = "message")]
    delta: Option<WireDelta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireDelta {
    content: Option<String>,
    #[serde(alias = "reasoning_content")]
    reasoning: Option<String>,
    tool_calls: Option<Vec<WireToolCall>>,
}

#[derive(Deserialize)]
struct WireToolCall {
    index: Option<usize>,
    id: Option<String>,
    function: Option<WireFunction>,
}

#[derive(Deserialize)]
struct WireFunction {
    name: Option<String>,
    // Usually a string fragment; a few providers send a complete object.
    arguments: Option<Value>,
}

struct Slot {
    provider_index: Option<usize>,
    started: bool,
    pending_args: String,
}

/// Turns wire chunks into [`Event`]s while building the [`Reply`].
#[derive(Default)]
pub(crate) struct Assembler {
    reply: Reply,
    slots: Vec<Slot>,
}

impl Assembler {
    pub(crate) fn chunk(&mut self, chunk: WireChunk, out: &mut VecDeque<Event>) {
        if let Some(u) = chunk.usage {
            self.reply.usage = Some(u);
        }
        // Only the first choice; we never ask for n > 1.
        let Some(choice) = chunk.choices.into_iter().next() else {
            return;
        };
        if let Some(r) = choice.finish_reason {
            self.reply.finish_reason = Some(r.into());
        }
        let Some(delta) = choice.delta else { return };
        if let Some(t) = delta.reasoning.filter(|t| !t.is_empty()) {
            out.push_back(Event::Reasoning(t));
        }
        if let Some(t) = delta.content.filter(|t| !t.is_empty()) {
            self.reply.content.push_str(&t);
            out.push_back(Event::Text(t));
        }
        for tc in delta.tool_calls.into_iter().flatten() {
            self.tool_call(tc, out);
        }
    }

    fn tool_call(&mut self, tc: WireToolCall, out: &mut VecDeque<Event>) {
        let id = tc.id.filter(|s| !s.is_empty());
        let i = self.slot_for(tc.index, id.as_deref());
        let call = &mut self.reply.tool_calls[i];
        let slot = &mut self.slots[i];
        // Some servers repeat id and name in every chunk; keep the first.
        if let Some(id) = id
            && call.id.is_empty()
        {
            call.id = id;
        }
        let (name, args) = match tc.function {
            Some(f) => (f.name.filter(|s| !s.is_empty()), f.arguments),
            None => (None, None),
        };
        if let Some(name) = name
            && call.name.is_empty()
        {
            call.name = name;
        }
        let args = match args {
            Some(Value::String(s)) => s,
            Some(Value::Null) | None => String::new(),
            Some(v) => v.to_string(),
        };
        call.arguments.push_str(&args);
        if !slot.started {
            slot.pending_args.push_str(&args);
            if !call.name.is_empty() {
                slot.started = true;
                out.push_back(Event::ToolCallStart {
                    index: i,
                    id: call.id.clone(),
                    name: call.name.clone(),
                });
                if !slot.pending_args.is_empty() {
                    let delta = std::mem::take(&mut slot.pending_args);
                    out.push_back(Event::ToolCallArgs { index: i, delta });
                }
            }
        } else if !args.is_empty() {
            out.push_back(Event::ToolCallArgs {
                index: i,
                delta: args,
            });
        }
    }

    fn slot_for(&mut self, index: Option<usize>, id: Option<&str>) -> usize {
        let calls = &self.reply.tool_calls;
        let differs = |i: usize| id.is_some_and(|id| !calls[i].id.is_empty() && calls[i].id != id);
        let found = match index {
            Some(pi) => self
                .slots
                .iter()
                .rposition(|s| s.provider_index == Some(pi))
                .filter(|&i| !differs(i)),
            // No index: a new id starts a new call, otherwise continue the last one.
            None => self.slots.len().checked_sub(1).filter(|&i| !differs(i)),
        };
        found.unwrap_or_else(|| {
            self.slots.push(Slot {
                provider_index: index,
                started: false,
                pending_args: String::new(),
            });
            self.reply.tool_calls.push(ToolCall::default());
            self.slots.len() - 1
        })
    }

    pub(crate) fn finish(mut self, out: &mut VecDeque<Event>) {
        // Calls that never got a name are unusable; drop them rather than guess.
        let mut i = 0;
        self.reply.tool_calls.retain(|c| {
            let keep = !c.name.is_empty();
            if !keep {
                tracing::warn!(index = i, "dropping tool call without a name");
            }
            i += 1;
            keep
        });
        for (n, c) in self.reply.tool_calls.iter_mut().enumerate() {
            if c.id.is_empty() {
                c.id = format!("call_{n}");
            }
        }
        out.push_back(Event::Done(self.reply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[Value]) -> Vec<Event> {
        let mut a = Assembler::default();
        let mut out = VecDeque::new();
        for c in chunks {
            a.chunk(serde_json::from_value(c.clone()).unwrap(), &mut out);
        }
        a.finish(&mut out);
        out.into()
    }

    fn reply(events: &[Event]) -> &Reply {
        match events.last() {
            Some(Event::Done(r)) => r,
            e => panic!("no Done: {e:?}"),
        }
    }

    #[test]
    fn openai_style_tool_call_deltas() {
        let ev = run(&[
            json!({"choices":[{"delta":{"role":"assistant","content":null}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"speak","arguments":""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"text\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Hi\"}"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"show","arguments":"{}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}),
        ]);
        assert_eq!(
            ev[0],
            Event::ToolCallStart {
                index: 0,
                id: "a".into(),
                name: "speak".into()
            }
        );
        assert_eq!(
            ev[1],
            Event::ToolCallArgs {
                index: 0,
                delta: "{\"text\":".into()
            }
        );
        let r = reply(&ev);
        assert_eq!(r.tool_calls.len(), 2);
        assert_eq!(r.tool_calls[0].arguments, "{\"text\":\"Hi\"}");
        assert_eq!(r.tool_calls[1].name, "show");
        assert_eq!(r.finish_reason, Some(FinishReason::ToolCalls));
        assert_eq!(r.usage.unwrap().completion_tokens, 5);
    }

    #[test]
    fn quirks_missing_index_repeated_name_object_args() {
        let ev = run(&[
            // No index, distinct ids (Gemini-style), object arguments.
            json!({"choices":[{"delta":{"tool_calls":[{"id":"x","function":{"name":"a","arguments":{"k":1}}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"id":"y","function":{"name":"b","arguments":"{"}}]}}]}),
            // Name and id repeated alongside an args fragment.
            json!({"choices":[{"delta":{"tool_calls":[{"id":"y","function":{"name":"b","arguments":"}"}}]}}]}),
            // An index with a new id is a new call.
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"z","function":{"name":"c"}}]}}]}),
        ]);
        let r = reply(&ev);
        let names: Vec<_> = r.tool_calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(r.tool_calls[0].arguments, "{\"k\":1}");
        assert_eq!(r.tool_calls[1].arguments, "{}");
        assert_eq!(r.tool_calls[2].parse_args::<Value>().unwrap(), json!({}));
    }

    #[test]
    fn args_before_name_are_buffered() {
        let ev = run(&[
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"f","arguments":":1}"}}]}}]}),
        ]);
        assert_eq!(
            ev[0],
            Event::ToolCallStart {
                index: 0,
                id: String::new(),
                name: "f".into()
            }
        );
        assert_eq!(
            ev[1],
            Event::ToolCallArgs {
                index: 0,
                delta: "{\"a\":1}".into()
            }
        );
        assert_eq!(reply(&ev).tool_calls[0].id, "call_0");
    }

    #[test]
    fn non_streaming_body_and_reasoning() {
        let ev = run(&[
            json!({"choices":[{"message":{"role":"assistant","content":"Hello","reasoning_content":"hmm"},"finish_reason":"stop"}]}),
        ]);
        assert_eq!(ev[0], Event::Reasoning("hmm".into()));
        assert_eq!(ev[1], Event::Text("Hello".into()));
        assert_eq!(reply(&ev).finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn request_serialization() {
        let call = ToolCall {
            id: "a".into(),
            name: "speak".into(),
            arguments: "{}".into(),
        };
        let mut req = ChatRequest::new(
            "m",
            vec![
                Message::System("s".into()),
                Message::Assistant {
                    content: None,
                    tool_calls: vec![call],
                },
                Message::Tool {
                    tool_call_id: "a".into(),
                    content: "ok".into(),
                },
            ],
        );
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("tools").is_none());
        assert_eq!(
            v["messages"][1]["tool_calls"][0]["function"]["name"],
            "speak"
        );
        req.tools.push(Tool {
            name: "t".into(),
            description: "d".into(),
            parameters: json!({"type":"object"}),
        });
        req.tool_choice = Some(ToolChoice::Function("t".into()));
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["tools"][0]["function"]["name"], "t");
        assert_eq!(v["tool_choice"]["function"]["name"], "t");
    }
}
