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

- `Client::from_provider` reads the key named by `api_key_env` from the
  environment, else `~/.config/vivus/.env` (read per lookup, so edits apply live); `chat()` returns a
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

Try a configured role: `cargo run -p vivus-inference --example chat -- [--role answer] <prompt>`
(keys in the environment or `~/.config/vivus/.env`). It prints time to first
event, to first spoken text, and the number of argument chunks.

### Smoke test (2026-11-23)

Asked to answer through a `speak` tool:

| Provider | Model | First event | First `speak` text | Total |
|---|---|---|---|---|
| Groq | `openai/gpt-oss-20b` | 0.2–0.4 s (reasoning) | 0.5 s | 0.5 s |
| nano-gpt | `deepseek/deepseek-v4.1-flash-1017` | 2.3–2.9 s | same | same |

- Tool calls, escaped quotes in arguments, `finish_reason` and usage all come
  through on both.
- **Neither streams tool arguments**: the whole call arrives in one chunk at the
  end (also `openai/gpt-5.4-mini` via nano-gpt). Plain `content` does stream on
  both. So `speak` as a tool can't start TTS early; see [Routing](#routing).
- Groq omits `index` on tool-call deltas (handled) and sends gpt-oss reasoning
  as `reasoning` with a `channel` field.
- nano-gpt's first token takes ~2 s even for plain text: fine for `answer`,
  too slow for `router`.
- Groq free tier: `qwen/qwen3.8-27b` rejects requests over 1000 output tokens
  per minute, so set `max_tokens` on the router.

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
generated. But the providers tested buffer tool arguments until the call is
complete ([smoke test](#smoke-test-2026-11-23)), so streaming only works if the
spoken reply is plain `content` and the tools are for `show`, `escalate` and
actions. Still open; see the roadmap.

## Web access

`web_search` and `fetch_url` tools with configurable providers (e.g. Brave,
Tavily, SearXNG, or a provider's built-in search). Results go into the sidebar
with sources.
