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
