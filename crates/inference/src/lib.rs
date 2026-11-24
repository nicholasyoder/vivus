//! Provider clients: OpenAI-compatible, Claude Code, STT/TTS APIs.
//!
//! The OpenAI-compatible client is a small `reqwest` + SSE implementation
//! rather than a generated SDK, so deserialization stays lenient towards
//! provider quirks. See `docs/inference.md`.

mod chat;
mod client;
pub mod partial;
mod sse;

pub use chat::{ChatRequest, Event, FinishReason, Message, Reply, Tool, ToolCall, ToolChoice, Usage};
pub use client::{ChatStream, Client, Error};
