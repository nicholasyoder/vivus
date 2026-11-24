# Inference

## Roles

Configured in `~/.config/vivus/config.toml` (see `config.example.toml`). Each
role names a provider plus a model; switching providers is a config edit only.

| Role | Job | Typical choice |
|---|---|---|
| `stt` | Streaming speech-to-text | Local via sherpa-onnx ([speech.md](speech.md)); Groq Whisper / Deepgram remote |
| `tts` | Low-latency speech | Local via sherpa-onnx (Kokoro, Kitten), or a cloud TTS |
| `router` | Every request: answers simple ones directly, calls quick tools, decides when to escalate | Fast small model (e.g. on Groq) |
| `answer` | Knowledge and web questions needing more quality than the router | Mid-tier model |
| `agent` | Multi-step tasks, computer control, heavy research | Frontier model via an OpenAI-compatible API, or headless Claude Code (`claude -p --output-format stream-json`) / Agent SDK |
| `vision` | Reading screenshots when AT-SPI text isn't available | Any vision-capable model |

Provider kinds today: `openai-compatible` (`base_url`, `api_key_env`),
`claude-code` (`command`), `local-speech` (`models_dir`, `threads`).

## Providers and client

Keys on hand for development: Groq (free tier, good for small fast models, but
already used elsewhere, so don't make it the only option) and
[nano-gpt](https://docs.nano-gpt.com/introduction) (OpenAI-compatible, hosts
most models). Defaults and tests should work with either.

### Client (`crates/inference`)

A small `reqwest` + SSE client of our own rather than `async-openai`: generated
SDK types reject the quirks below, and we need raw tool-argument deltas.

- `Client::from_provider` reads the key from `api_key_env`; `chat()` returns a
  `ChatStream` of `Event`s (`Text`, `Reasoning`, `ToolCallStart`,
  `ToolCallArgs`, then `Done(Reply)` with the assembled turn and usage).
  `complete()` drains it.
- Cancelling is dropping the stream: the connection closes and the provider
  stops generating.
- `partial::StringField` decodes a string field from tool arguments while they
  stream (escapes split across chunks are held back), so `speak`'s `text` can
  go to TTS sentence by sentence.
- Tolerated quirks: SSE comments and CRLF, no `[DONE]`, usage-only final chunk,
  a JSON body despite `stream: true`, `error` objects mid-stream, tool calls
  without `index` or reusing one index with new ids, `id`/`name` repeated per
  chunk, arguments as an object instead of a string, empty arguments,
  `reasoning` / `reasoning_content`.
- Timeouts: 10 s connect, 60 s between reads, no total limit.

Try a configured role: `cargo run -p vivus-inference --example chat -- [--role answer] <prompt>`.
Tested against a local mock server only so far; no provider key was available
in the dev session (2026-11-23).

## Routing

One model call with tools, not a separate classifier. The router gets the
transcript plus context in one streaming call, with tools:

- `speak(text)`: what to say aloud. One or two sentences at most; no lists, URLs or code.
- `show(document)`: put markdown in the sidebar (new document, or append/replace an existing one).
- `escalate(role, task)`: hand the request to `answer` or `agent`. The router also calls `speak` with a short filler line ("let me pull that up") so there's immediate audio.
- Action tools: `launch_app`, `focus_window`, `close_window`, `web_search`, `fetch_url`, `get_clipboard`, `set_clipboard`, `get_time`, ...

Speech/sidebar rules (system prompt and examples):

- Short factual answers are spoken only.
- Lists, code, links, more than about two sentences, or anything you'll want to re-read: a spoken summary plus a sidebar document.
- Never read code, URLs or long numbers aloud.
- When escalating: filler line immediately; when the result arrives, a spoken summary and the full detail in the sidebar.

The text from `speak` goes to TTS one sentence at a time while it's still being
generated.

## Web access

`web_search` and `fetch_url` tools with configurable providers (e.g. Brave,
Tavily, SearXNG, or a provider's built-in search). Results go into the sidebar
with sources.
