# Roadmap

Goal: talk to your Linux desktop like a capable person next to you. See
[vision.md](vision.md) for the vision and principles.

**How to use this file.** Each unchecked item is sized for roughly one working
session. Pick the first unchecked item in the current sub-phase (unless there's a
reason to jump), do it, then check it off with the date and commit in the same
commit as the work. Checked items stay where they are. Design detail belongs in the linked topic docs, not here.
New work discovered along the way gets a new line item.

## Phase 1: Voice loop and sidebar ← current

Talking to the computer works smoothly for questions and basic app control,
using only standard protocols. Sub-phases are ordered by need: what makes
everyday use work comes first, then smarter answers, then polish.

### 1a: Working loop

- [x] Project skeleton: Cargo workspace, D-Bus contract, `vivusctl`, Widgets sidebar (2026-11-20, `5bc6738`)
- [x] `vivus-inference`: OpenAI-compatible streaming chat client with tool calls (2026-11-23, `696b45d`) — [inference.md](inference.md)
- [x] Smoke-test the client against Groq and nano-gpt with real keys (`--example chat`) (2026-11-23, `658a656`) — [inference.md](inference.md#smoke-test-2026-11-23)
- [x] Voice input: PipeWire capture, Silero VAD, local streaming STT, live transcript in the sidebar (2026-11-23, `ffd138c`) — [speech.md](speech.md)
- [x] Local TTS (Kitten / Kokoro via sherpa-onnx) with PipeWire playback, fed one sentence at a time; `speaking` state (2026-11-23, `3d66e65`) — [speech.md](speech.md#tts)
- [x] Router: one streaming call; content is speech and streams into TTS; `show` / `escalate` tools; the speech-vs-sidebar rules (numbers as words until normalization lands) (2026-11-24, `cabb729`) — [inference.md](inference.md#routing)
- [x] Escalation to `answer` and `agent`, including the Claude Code provider; canned filler when the router escalates silently (2026-11-24, `0e169fb`) — [inference.md](inference.md#escalation)

### 1b: Speaking well

- [x] TTS text normalization: numbers, times, dates, units, currency, acronyms, abbreviations, URLs (all dropped today); log remaining OOV words (2026-11-24, `0d464b7`) — [speech.md](speech.md#out-of-lexicon-words)
- [x] Own phonemizer: lexicon lookup in Rust, `phoneme_codepoints` to sherpa; ARPAbet pronunciation file (learned + user entries) converted per model; `-s`/`-ed`/`-ing` suffix rules (Kokoro lacks many inflections) (2026-11-24, `342b76f`) — [speech.md](speech.md#out-of-lexicon-words)
- [x] LLM pronunciation lookup for OOV words, started while the reply streams; cached in the learned lexicon; idle prefetch of app names and transcripts (2026-11-24, `aeae4c1`) — [speech.md](speech.md#out-of-lexicon-words)
- [ ] Echo cancellation (set up or auto-load PipeWire `module-echo-cancel`) and barge-in: speech during playback cancels TTS and LLM within ~200 ms — [speech.md](speech.md#barge-in)
- [ ] Capture downmixes all channels (a stereo mic currently links only `FL`)

### 1c: Basic desktop control

- [ ] App launching: parse `.desktop` files, fuzzy-match the spoken name, launch with an `xdg-activation` token
- [ ] Windows: list with `ext-foreign-toplevel-list-v1`; switch/close/minimize with `wlr-foreign-toplevel-management`; active window as context (`vivus-wayland`); prefetch pronunciations of window titles
- [ ] Push-to-talk and toggle-listen through the `GlobalShortcuts` portal (`vivusctl` stays the fallback)
- [ ] Clipboard read/write via `wlr-data-control`

### 1d: Everyday setup

- [ ] systemd user unit for the daemon; `.desktop` entry for the sidebar; model download via `vivusctl` instead of `scripts/fetch-models.sh`
- [ ] `org.vivus.Settings` D-Bus interface (`Get`/`Set`/`Changed`); daemon writes config with `toml_edit`, watches the file and reloads live — [settings.md](settings.md#d-bus)
- [ ] Audio device listing and hotplug from the PipeWire registry; switch input without restarting the daemon — [settings.md](settings.md#audio-devices)
- [ ] Sidebar settings page: input/output device pickers, mic level meter, end-of-turn silence — [settings.md](settings.md)
- [ ] Per-role provider and model pickers (`ListModels`: local models on disk, remote via `/models`), TTS voice — [settings.md](settings.md#roles-and-providers)

### 1e: Smarter answers

- [ ] Conversation memory: rolling history so follow-ups work
- [ ] Context providers: time/date, locale, active window, clipboard — [architecture.md](architecture.md#daemon-internals)
- [ ] `web_search` and `fetch_url` with a configurable provider; sourced sidebar documents — [inference.md](inference.md#web-access)
- [ ] Long escalations: `thinking` state after the filler, a spoken progress line for agent runs past ~15 s — [inference.md](inference.md#escalation)

### 1f: Polish

- [ ] Semantic end-of-turn check on partials, so the silence threshold can shrink; start the router speculatively — [speech.md](speech.md#turn-detection)
- [ ] Local neural G2P as the offline / over-deadline fallback for OOV words — [speech.md](speech.md#out-of-lexicon-words)
- [ ] Remote STT provider (Groq Whisper, final-only) so `stt` has two providers
- [ ] Sidebar polish: document history UX, code blocks, images, show/hide behaviour
- [ ] Add/edit providers (base URL, API key) from the sidebar, once key storage is decided
- [ ] Benchmark the light tier on a 4-core laptop with no GPU; settle the default STT/TTS models — [speech.md](speech.md#tiers)

### Phase 1 is done when

- [ ] "What time is it?" gets spoken audio in under ~1 s from the end of speech (remote inference)
- [ ] "What's the latest news about AI?" says a filler line at once, then a spoken summary, with a sourced sidebar document
- [ ] "Open my browser", "switch to my file manager", "close this" work on Biome and on sway or Hyprland
- [ ] Interrupting mid-reply stops it within ~200 ms
- [ ] Every inference role can be switched between at least two providers using only the config file
- [ ] The light tier holds a normal conversation on a 4-core laptop with no GPU

## Phase 2: Screen awareness and deeper control

Understand what's on screen and act inside apps. Design: [screen-awareness.md](screen-awareness.md).

- [ ] AT-SPI text and tree for the active window
- [ ] Single-window screenshot (`ext-image-copy-capture-v1`) plus OCR or the `vision` role
- [ ] "What's this error?": active window → its text → explanation and fix in the sidebar
- [ ] Custom protocol prototype (socket IPC behind a backend interface): window → client PID, global geometry, stacking, output; Biome side
- [ ] Freeze the custom protocol as Wayland XML in `protocol/`, documented for other compositors
- [ ] Input injection (virtual keyboard/pointer or libei); needs Biome work
- [ ] AT-SPI actions (press, fill in, menu paths), screenshot-and-click as fallback
- [ ] `agent` role for multi-step tasks, with confirmation for destructive or outward-facing actions

**Done when:** "what's this error?" works for a terminal, a browser and an IDE;
"click Save", "type my email address here", "go to the next tab" work in common
GTK/Qt/Firefox apps; Biome implements the custom protocol and it's documented
well enough for a sway/Hyprland developer.

## Phase 3: Gaze tracking (optional)

Resolve "this/that" with where you're looking. Only ship it if it clearly beats
the active window alone. Design and research: [gaze.md](gaze.md).

- [ ] `vivus-gaze` prototype (MediaPipe face and iris landmarks); measure accuracy on the dev webcam
- [ ] Silent calibration from mouse clicks
- [ ] Fixation history aligned with "this/that" in the transcript → ranked candidate windows
- [ ] Blind A/B test against active-window-only

## Open questions

- Is ~1 s from end of speech to audio realistic for normal replies? Measure once the loop runs.

- Wake word as well as push-to-talk? Probably later and optional.
- Should the router have a small local fallback for offline use? (Speech is already local.)
- Where to store API keys long-term: Secret Service, or keep env + `~/.config/vivus/.env` (current interim)? (Blocks editing providers in the sidebar.)
- How much conversation and sidebar history to keep, and where (documents are in memory only today).
- Which actions need spoken or visual confirmation, and how to keep screen or web text from triggering actions (treat it as data; confirm what it suggests).

## Decided

- Phase 1 is split into need-ordered sub-phases instead of categories; speech quality comes next, smarter answers (memory, context, web, long escalations) after basic desktop control and setup (2026-11-24).
- The router's spoken reply is its streamed text content, not a `speak` tool: providers send tool arguments in one final chunk (2026-11-23).
- OpenAI-compatible client is our own `reqwest` + SSE code, not `async-openai`, for quirk tolerance and raw tool-argument deltas (2026-11-23).
- Inference before the rest of the voice items, to get the live STT → LLM → TTS loop working first (2026-11-23).
- OOV pronunciations come mainly from LLM lookup, cached; user entries override. A first sentence may wait a few seconds for a lookup (2026-11-23).

- Sidebar uses Qt Widgets + QSS, not QML, so Forest's QSS themes can style it (2026-11-20).
- Speech (VAD/STT/TTS) is local by default; LLM roles are remote by default (2026-11-23).
- STT interface streams partials and finals; local default is `parakeet-tdt-110m` (2026-11-23).
- Local TTS: `kitten-nano` (light, default) and `kokoro` v1.0 (standard), fp32 (int8 is ~2× slower on x86), lexicon frontend since sherpa-onnx 2.x has no espeak-ng (2026-11-23).
