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

Providers today: `[providers.<name>]` entries, each an OpenAI-compatible API
(`base_url`, `api_key_env`); `claude-code`, always there, configured by
`[claude_code]` (`command`, `args`); and local speech, which stt and tts use
when they name no provider, configured by `[speech]` (`models_dir`,
`threads`). See [settings.md](settings.md#providers).

## Providers and client

Keys on hand for development: Groq (free tier, good for small fast models, but
already used elsewhere, so don't make it the only option) and
[nano-gpt](https://docs.nano-gpt.com/introduction) (OpenAI-compatible, hosts
most models). Defaults and tests should work with either.

### Client (`crates/inference`)

A small `reqwest` + SSE client of our own rather than `async-openai`: generated
SDK types reject the quirks below, and we need raw tool-argument deltas.

- `Client::from_provider(name, provider)` gets its key from `vivus_core::keys`
  (environment, keyring, `.env`; see [settings.md](settings.md#api-keys)); `chat()` returns a
  `ChatStream` of `Event`s (`Text`, `Reasoning`, `ToolCallStart`,
  `ToolCallArgs`, then `Done(Reply)` with the assembled turn and usage).
  `complete()` drains it.
- Cancelling is dropping the stream: the connection closes and the provider
  stops generating.
- `partial::StringField` decodes a string field from tool arguments while they
  stream (escapes split across chunks are held back). The providers tested
  don't stream arguments, so it's for those that do (e.g. a long `show`).
- Tolerated quirks: SSE comments and CRLF, no `[DONE]`, usage-only final chunk,
  a JSON body despite `stream: true`, `error` objects mid-stream, tool calls
  without `index` or reusing one index with new ids, `id`/`name` repeated per
  chunk, arguments as an object instead of a string, empty arguments,
  `reasoning` / `reasoning_content`.
- Timeouts: 10 s connect, 60 s between reads, no total limit.
- `ChatRequest::reasoning_effort` is sent only when set. Pronunciation lookups
  set `low`, since gpt-oss otherwise spends its whole `max_tokens` budget
  reasoning about ARPAbet ([speech.md](speech.md#out-of-lexicon-words)).

Try a configured role: `cargo run -p vivus-inference --example chat -- [--role answer] <prompt>`
(keys in the environment or `~/.config/vivus/.env`; only the daemon reads the keyring). Content is printed as
speech, with a `show` tool offered; it prints time to first event, to first
spoken text, and the number of tool-argument chunks.

### Smoke test (2026-11-23)

Asked to answer through a `speak(text)` tool (the design at the time):

| Provider | Model | First event | First `speak` text | Total |
|---|---|---|---|---|
| Groq | `openai/gpt-oss-20b` | 0.2–0.4 s (reasoning) | 0.5 s | 0.5 s |
| nano-gpt | `deepseek/deepseek-v4.1-flash-1017` | 2.3–2.9 s | same | same |

- Tool calls, escaped quotes in arguments, `finish_reason` and usage all come
  through on both.
- **Neither streams tool arguments**: the whole call arrives in one chunk at the
  end (also `openai/gpt-5.4-mini` via nano-gpt). Plain `content` does stream on
  both. So speech goes in content, not a `speak` tool; see [Routing](#routing).
- Groq omits `index` on tool-call deltas (handled) and sends gpt-oss reasoning
  as `reasoning` with a `channel` field.
- nano-gpt's first token takes ~2 s even for plain text: fine for `answer`,
  too slow for `router`.
- Groq free tier: `qwen/qwen3.8-27b` rejects requests over 1000 output tokens
  per minute, so set `max_tokens` on the router.

## Routing

One model call with tools, not a separate classifier. The router gets the
transcript plus context in one streaming call.

**The reply's text content is what gets spoken**, one or two sentences at most,
no lists, URLs or code. It streams into TTS one sentence at a time. Speech is
not a tool because providers send tool arguments in one final chunk
([smoke test](#smoke-test-2026-11-23)). Tools:

- `show(document)`: put markdown in the sidebar (new document, or append/replace an existing one).
- `escalate(role, task)`: hand the request to `answer` or `agent`. The router first writes a short filler line ("let me pull that up") as content, so there's immediate audio; models put content before tool calls.
- Action tools: `launch_app`, `focus_window`, `close_window`, `web_search`, `fetch_url`, `get_clipboard`, `set_clipboard`, `get_time`, ...

Speech/sidebar rules (system prompt and examples):

- Short factual answers are spoken only.
- Lists, code, links, more than about two sentences, or anything you'll want to re-read: a spoken summary plus a sidebar document.
- Never read code, URLs or long numbers aloud.
- When escalating: filler line immediately; when the result arrives, a spoken summary and the full detail in the sidebar.

After action tools run, their results go back in a follow-up call, and that
call's content is spoken too ("Opened Firefox"). If a model puts sidebar-style
material (lists, code) in content anyway, the prompt and examples are the fix;
TTS normalization drops what it can't say. Numbers, times, units and symbols
can be written normally: [normalization](speech.md#out-of-lexicon-words) turns
them into words, so the prompt no longer asks for them spelled out.

In the example run (2026-11-23), both gpt-oss-20b and DeepSeek called `show`
for a packing list **with no spoken content**, despite the prompt asking for a
summary. The router needs a fallback: send the `show` result back and speak the
follow-up's content, or say a canned line ("I've put that in the sidebar").

### Implementation (`crates/daemon/src/router.rs`)

- The engine runs each reply as a tokio task tagged with a generation; its
  `ReplyEvent`s (`Text`, `Document`, `Done`) come back as `Command::Reply` and
  stale generations are ignored. Cancel, listening again or a new request
  aborts the task, which drops the stream and closes the request.
- Text deltas go straight to the speaker (which splits sentences); the state
  turns `speaking` at the first non-blank text.
- Up to three rounds. A follow-up call is made only if nothing has been spoken
  yet, or a tool needs an answer (`escalate`, bad arguments, unknown tool); the
  last round sets `tool_choice: none`. If still nothing was said: "I've put
  that in the sidebar" after a `show`, else an apology. Provider errors are
  spoken as "couldn't reach the language model".
- `escalate` ends the router's call; see [Escalation](#escalation).
- `max_tokens = 1000` (Groq free tier limit). Earlier exchanges come from
  [Conversation memory](#conversation-memory); other context (time, locale)
  is its own roadmap item.

### Live run (2026-11-24, Groq `openai/gpt-oss-20b`, Kokoro)

| Request | Router | Spoken first text | TTS of first sentence |
|---|---|---|---|
| "what's the capital of Australia" | one call | 0.45 s | 0.44 s |
| "packing list for a weekend hike" | silent `show`, then follow-up | 1.06 s | 0.81 s |
| "latest news about AI" | silent `escalate`, then follow-up | 1.10 s | 0.71 s |

gpt-oss-20b still calls tools without a spoken line first despite the prompt
and examples, so the follow-up round costs about 0.5 s. For `escalate` this is
now a canned filler instead ([Escalation](#escalation)).

### Escalation

- A valid `escalate(role, task)` ends the router's call with no follow-up round.
  If the router hasn't spoken, a canned filler is said at once ("Let me look
  into that." for `answer`, "On it." for `agent`, rotating through a few).
- The splitter only ends a sentence once the next whitespace arrives, so the
  router emits `ReplyEvent::Flush` after each model call and after the filler;
  the speaker then speaks its buffered tail. Without it the filler waited for
  the agent's answer (fixed 2026-11-24: "On it." synthesized 1.1 s after the
  request instead of after a 24 s agent run).
- `agent` falls back to `answer` when unset; with neither configured, an
  apology is spoken and no filler. A role that fails to build (missing key) is
  logged and treated as unset, so the router still works.
- The escalated role gets `The user said: "…"\nTask: …`.
- **OpenAI-compatible** `answer`/`agent`: the same speech-plus-`show` loop as the
  router, with a prompt that skips the filler and admits it has no web access
  yet, only `show` as a tool, and no `max_tokens`.
- **Claude Code** (`crates/inference/src/claude_code.rs`): `claude -p
  --output-format stream-json --verbose [--model M] --append-system-prompt …`
  plus the provider's `args`, prompt on stdin, run in `$HOME`, killed on drop
  (cancel). Headless runs can't ask for permission, so the provider's `args`
  allow tools (default `--allowedTools WebSearch,WebFetch`). The final `result`
  is split: the first paragraph is spoken, the rest becomes a sidebar document
  (titled by its heading, else the task). If the first paragraph looks like
  markdown or runs over 400 characters, all of it goes to the sidebar.

Live run (2026-11-24; router gpt-oss-20b on Groq, `answer` DeepSeek v4.1 flash
on nano-gpt, `agent` Claude Code with its default model):

| Request | Route | Filler | Answer |
|---|---|---|---|
| "latest news about AI this week" | `answer` | 0.61 s | first text 3.3 s; says it has no web access, tips in the sidebar |
| "research reviews … best robot vacuum under \$300" | `agent` | 0.68 s | 32 s, five web searches, \$0.47; spoken pick, sourced sidebar document |

- gpt-oss-20b sent "check which Rust toolchains are installed" to `answer`, and
  answered "compare Rust web frameworks, with sources" itself with made-up
  numbers. A rule saying what `agent` is for, plus an example, got the vacuum request
  routed right; small routers will still guess wrong sometimes.
- Claude Code's default model is costly for voice requests; set
  `model = "sonnet"` (or similar) on `[roles.agent]`.
- The state stays `speaking` from the filler until the escalated reply ends,
  and a long agent run is silent after the filler.

## Conversation memory

Follow-ups ("and its population?", "add a stove to that list") need the
conversation so far. Options considered:

- **Whole session**: simplest, but the prompt grows without bound, every
  request gets slower and dearer, and Groq's free tier (8000 tokens per minute)
  runs out within a few requests.
- **Summarize older turns** with an extra LLM call: keeps long-range context,
  but adds a call, latency or background work, and a failure mode. Not needed
  for voice, where follow-ups refer to the last few exchanges.
- **Retrieval** over past conversations: long-term memory, a different feature.
- **Sliding window** (chosen): the last few exchanges, bounded three ways.

Implementation (`crates/daemon/src/conversation.rs`):

- An `Exchange` is the request, the reply text, silent actions' results
  ("Closed Firefox.", via `ReplyEvent::Acted`), the sidebar documents it
  touched, and whether it was interrupted. The engine records one when a reply
  ends or is cancelled (barge-in, `Cancel`), with the text generated so far;
  talking over a reply that's still being spoken marks it interrupted too.
- Sent as plain user/assistant turns: spoken text only, no replayed tool
  calls (no tool-call ids to keep valid, no tool names the escalated roles
  don't have, nothing for the model to imitate aloud). A silent reply's
  content is its actions' results; an interrupted one ends in "… (interrupted)".
- Documents go in the system prompt instead: the conversation's last five by id
  and title (so `show` can append to one), plus the newest one's text, cut to
  1500 characters, so "what's the third item?" works.
- Bounds: `[conversation] max_turns` (10) exchanges; 4000 characters of request
  and reply text in all (about 1000 tokens), dropping the oldest first; and a
  fresh conversation after `forget_after_minutes` (10) without a request.
- `answer` gets the same turns and document section; Claude Code gets them as
  a transcript at the top of its prompt.
- History lives in the daemon's memory only; a restart forgets it.

Later (Phase 2): compaction of older turns into summaries, and long-term
memory (files or a small database of facts and past conversations) that the
router can search with a tool. The summarizing and indexing run while idle, so
they never sit in front of a reply.

Live run (2026-11-25, gpt-oss-120b on Groq): "packing list for a weekend hike"
then "add a camping stove to it" appended to the same document (`document_id`);
"capital of Peru" then "how many people live there?" answered for Lima. Seven
requests in a minute then hit Groq's free-tier 8000 TPM limit (each request was
about 2000 tokens with the tools and window list), spoken as "couldn't reach
the language model"; the history budget was halved to 4000 characters after it.

## Web access

`web_search` and `fetch_url` tools with configurable providers (e.g. Brave,
Tavily, SearXNG, or a provider's built-in search). Results go into the sidebar
with sources.
