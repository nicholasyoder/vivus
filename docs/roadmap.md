# Roadmap

Goal: talk to your Linux desktop like a capable person next to you. See
[vision.md](vision.md) for the vision and principles.

**How to use this file.** Each unchecked item is sized for roughly one working
session. Pick the first unchecked item in the current phase (unless there's a
reason to jump), do it, then check it off with the date and commit in the same
commit as the work. Design detail belongs in the linked topic docs, not here.
New work discovered along the way gets a new line item.

## Phase 1: Voice loop and sidebar ← current

Talking to the computer works smoothly for questions and basic app control,
using only standard protocols.

### Done

- [x] Project skeleton: Cargo workspace, D-Bus contract, `vivusctl`, Widgets sidebar (2026-11-20, `5bc6738`)
- [x] Voice input: PipeWire capture, Silero VAD, local streaming STT, live transcript in the sidebar (2026-11-23, `ffd138c`) — [speech.md](speech.md)
- [x] Local TTS (Kitten / Kokoro via sherpa-onnx) with PipeWire playback, fed one sentence at a time; `speaking` state (2026-11-23, `3d66e65`) — [speech.md](speech.md#tts)

### Voice

- [ ] TTS fallback for out-of-lexicon words (dropped silently today): neural G2P and/or a user lexicon — [speech.md](speech.md#out-of-lexicon-words)
- [ ] Echo cancellation (set up or auto-load PipeWire `module-echo-cancel`) and barge-in: speech during playback cancels TTS and LLM within ~200 ms — [speech.md](speech.md#barge-in)
- [ ] Semantic end-of-turn check on partials, so the silence threshold can shrink; start the router speculatively — [speech.md](speech.md#turn-detection)
- [ ] Remote STT provider (Groq Whisper, final-only) so `stt` has two providers
- [ ] Capture downmixes all channels (a stereo mic currently links only `FL`)

### Inference

- [ ] `vivus-inference`: OpenAI-compatible streaming chat client with tool calls — [inference.md](inference.md)
- [ ] Router: one streaming call with `speak` / `show` / `escalate` tools and the speech-vs-sidebar rules; `speak` streams into TTS — [inference.md](inference.md#routing)
- [ ] Escalation to `answer` and `agent`, including the Claude Code provider — [inference.md](inference.md#roles)
- [ ] Context providers: time/date, locale, active window, clipboard — [architecture.md](architecture.md#daemon-internals)
- [ ] Conversation memory: rolling history so follow-ups work
- [ ] `web_search` and `fetch_url` with a configurable provider; sourced sidebar documents — [inference.md](inference.md#web-access)

### Desktop control (standard protocols)

- [ ] App launching: parse `.desktop` files, fuzzy-match the spoken name, launch with an `xdg-activation` token
- [ ] Windows: list with `ext-foreign-toplevel-list-v1`; switch/close/minimize with `wlr-foreign-toplevel-management`; active window as context (`vivus-wayland`)
- [ ] Clipboard read/write via `wlr-data-control`
- [ ] Push-to-talk and toggle-listen through the `GlobalShortcuts` portal (`vivusctl` stays the fallback)

### Settings

- [ ] `org.vivus.Settings` D-Bus interface (`Get`/`Set`/`Changed`); daemon writes config with `toml_edit`, watches the file and reloads live — [settings.md](settings.md#d-bus)
- [ ] Audio device listing and hotplug from the PipeWire registry; switch input without restarting the daemon — [settings.md](settings.md#audio-devices)
- [ ] Sidebar settings page: input/output device pickers, mic level meter, end-of-turn silence — [settings.md](settings.md)
- [ ] Per-role provider and model pickers (`ListModels`: local models on disk, remote via `/models`), TTS voice — [settings.md](settings.md#roles-and-providers)
- [ ] Add/edit providers (base URL, API key) from the sidebar, once key storage is decided

### Sidebar and packaging

- [ ] Sidebar polish: document history UX, code blocks, images, show/hide behaviour
- [ ] systemd user unit for the daemon; `.desktop` entry for the sidebar; model download via `vivusctl` instead of `scripts/fetch-models.sh`
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

- Wake word as well as push-to-talk? Probably later and optional.
- Should the router have a small local fallback for offline use? (Speech is already local.)
- Where to store API keys: environment variables, Secret Service, or the config file? (Blocks editing providers in the sidebar.)
- How much conversation and sidebar history to keep, and where (documents are in memory only today).
- Which actions need spoken or visual confirmation, and how to keep screen or web text from triggering actions (treat it as data; confirm what it suggests).

## Decided

- Sidebar uses Qt Widgets + QSS, not QML, so Forest's QSS themes can style it (2026-11-20).
- Speech (VAD/STT/TTS) is local by default; LLM roles are remote by default (2026-11-23).
- STT interface streams partials and finals; local default is `parakeet-tdt-110m` (2026-11-23).
- Local TTS: `kitten-nano` (light, default) and `kokoro` v1.0 (standard), fp32 (int8 is ~2× slower on x86), lexicon frontend since sherpa-onnx 2.x has no espeak-ng (2026-11-23).
