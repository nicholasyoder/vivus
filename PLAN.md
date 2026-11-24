# Vivus: project plan

*Vivus* is Latin for "alive" or "living": the active, responsive part of the
desktop alongside forest and biome.

Naming conventions: Rust crates are `vivus-*`; the binaries are `vivus-daemon`,
`vivus-sidebar` and `vivusctl`; the D-Bus service is `org.vivus`; Wayland
protocols are prefixed `vivus_` (e.g. `vivus_toplevel_info_v1`); the config is
`~/.config/vivus/config.toml`.

## 1. Vision

Talk to your Linux desktop the way you'd talk to a capable person sitting next
to you, and get answers the way you'd expect them:

- **Voice in, voice out, for short things.** "What time is it?" gets a spoken answer and nothing else.
- **Voice plus a sidebar, for detailed things.** "What's the latest AI news?" gets a short spoken line ("let me pull that up in your sidebar"), then a spoken summary once results arrive. The full detail appears in an always-available sidebar you can read and scroll through.
- **Control of normal apps through speech.** Launch, switch, close, and later operate inside apps.
- **Awareness of what's on screen.** "What's this error?" resolves to the window you mean (the active window first, gaze later) and reads its text.

## 2. Principles

1. **Not tied to one compositor.** Use standard Wayland protocols (`ext-*`, then `wlr-*`) first. Anything missing gets a **new, documented protocol** that `vivus` defines and Biome implements as the reference. Any other compositor can support `vivus` by implementing the same protocols. Supporting only wlroots-based compositors is acceptable where nothing standard exists (e.g. window activation).
2. **Standalone first, Forest second.** `vivus` must be fully usable without Forest. A future Forest plugin is only a bridge: a D-Bus client that renders the sidebar in Forest's style. It must never be the only way to use `vivus`.
3. **Inference is configuration, not code.** Every model role points at whatever endpoint you configure: Groq, any OpenAI-compatible API, a local server, or Claude Code. LLM roles default to remote inference for speed; speech (VAD, STT, TTS) defaults to local, because it runs continuously and needs streaming (§4.2).
4. **Structured actions before clicking pixels.** Prefer app launching, compositor protocols, D-Bus, AT-SPI actions, CLIs and app APIs over screenshot-and-click. They're more reliable and let cheaper models do more of the work. Screenshot-and-click is the last fallback.
5. **Latency is a feature.** Time from the end of your speech to the first audio back matters more than how smart the model is for everyday requests. Stream everything and cancel instantly when you interrupt.
6. **Light by default, scale up when it helps.** It must run on an ordinary laptop with no GPU. Bigger local models (or remote ones) are a config choice, not a requirement. When lightness and voice quality conflict, voice quality wins, but pick the smallest model that gets there.

## 3. Architecture

```
┌──────────────────────────── vivus-daemon (Rust, tokio) ────────────────────────────┐
│                                                                                      │
│  Audio in ─► VAD ─► STT (streaming) ─► Router LLM ──┬─► speak()  ─► TTS ─► Audio out │
│  (PipeWire, echo-cancelled)               │         ├─► show()   ─► sidebar content  │
│        ▲                                  │         ├─► tool calls (actions)         │
│        │ interrupt: cancels TTS + LLM     │         └─► escalate() ─► Agent LLM      │
│                                           ▼                                          │
│                              Context providers: active window, clipboard, time, ...  │
│                                                                                      │
│  Backends:  wayland-std (ext/wlr protocols) │ vivus-protocol (custom) │ AT-SPI │ OCR │
│                                                                                      │
│  D-Bus API (org.vivus.*): state, transcript, sidebar documents, commands            │
└──────────────────────────────────────────────┬───────────────────────────────────────┘
                                               │ D-Bus
                     ┌─────────────────────────┴─────────────────────────┐
             vivus-sidebar                                   Forest plugin (later, optional)
             Qt6/C++ Widgets + QSS,                           Qt, Forest-themed, just a bridge
             wlr-layer-shell via layer-shell-qt
```

### 3.1 Components

| Component | Language | Purpose |
|---|---|---|
| `vivus-daemon` | Rust | Audio pipeline, STT/TTS clients, LLM orchestration, tools, compositor backends, D-Bus service. Runs without a UI. |
| `vivus-sidebar` | C++ / Qt6 | Standalone layer-shell panel. Renders sidebar documents (markdown, links, code, images), shows listening/thinking/speaking state, and offers a text input as a fallback. A pure D-Bus client. |
| `vivusctl` | Rust | Small CLI: `toggle-listen`, `push-to-talk start/stop`, `ask "<text>"`, `status`. Useful for scripting, compositors without a `GlobalShortcuts` portal, and testing. |
| `protocol/` | XML | Custom Wayland protocol(s) (Phase 2). Biome is the reference implementation. |
| Forest plugin | C++ / Qt | Later and optional. A bridge only; lives in the Forest repo. |

### 3.2 Repository layout (proposed)

```
vivus/
  Cargo.toml              # workspace
  crates/
    daemon/               # binary
    ctl/                  # CLI binary
    core/                 # pipeline, orchestration, config, tool registry
    audio/                # capture, VAD, playback, echo cancellation
    inference/            # provider clients: OpenAI-compatible, Groq, Claude Code, STT/TTS APIs
    wayland/              # standard-protocol backend
    protocol/             # bindings generated from ../protocol/*.xml (Phase 2)
  sidebar/                # CMake + Qt6 project
  protocol/               # *.xml custom protocols + docs
  dbus/                   # D-Bus interface XML (shared by the daemon, the sidebar and the Forest plugin)
  docs/
  PLAN.md
```

The only artifacts shared with Forest and Biome are the **protocol XML** and the
**D-Bus interface XML**, not code libraries. Biome implements the protocol, and
the Forest plugin implements a D-Bus client.

### 3.3 Daemon internals

- **Async runtime:** tokio. Every stage is a cancellable task. Interrupting (you start speaking while it's talking) cancels in-flight TTS, LLM streams and pending tool calls.
- **Audio:** PipeWire through the `pipewire` crate (so we can target the echo-cancel node directly). Echo cancellation through PipeWire's `module-echo-cancel` (document how to set it up, or load it automatically).
- **Speech runtime:** [`sherpa-onnx`](https://docs.rs/sherpa-onnx) for Silero VAD, local STT and local TTS. One CPU-friendly ONNX runtime covers every speech model in §4.2. Its build script downloads a prebuilt static library; packagers set `SHERPA_ONNX_LIB_DIR` instead.
- **Turn detection:** VAD silence threshold first, kept short because live partial transcripts make a wrong guess cheap. Then a semantic end-of-turn check on the partial text, so pauses mid-sentence don't cut you off, and so the router can start speculatively before you've definitely finished.
- **Context providers:** small functions that supply context for each request: current time and date, locale, active window (app ID and title), clipboard text, and later selected text, window text and gaze target. The router gets a compact summary; the agent can ask for the full content through tools.
- **Conversation memory:** a short rolling history so follow-ups work ("open it", "what about the second one?"). Sidebar documents are kept and can be referred back to.

## 4. Inference design

### 4.1 Roles

Configured in `~/.config/vivus/config.toml`. Each role names a provider plus a model:

| Role | Job | Typical choice |
|---|---|---|
| `stt` | Streaming speech-to-text | Local via sherpa-onnx (see §4.2); remote Groq Whisper / Deepgram as alternatives |
| `tts` | Low-latency speech | Local via sherpa-onnx (Kokoro, Kitten), or a cloud TTS |
| `router` | Every request: answers simple requests directly, calls quick tools, decides when to escalate | Fast small model (e.g. on Groq) |
| `answer` | Knowledge and web questions that need more quality than the router | Mid-tier model |
| `agent` | Multi-step tasks, computer control, heavy research | Frontier model through an OpenAI-compatible API or a headless Claude Code (`claude -p --output-format stream-json`) / Agent SDK |
| `vision` | Reading screenshots when AT-SPI text isn't available | Any vision-capable model |

```toml
[providers.groq]
kind = "openai-compatible"
base_url = "https://api.groq.com/openai/v1"
api_key_env = "GROQ_API_KEY"

[providers.claude-code]
kind = "claude-code"
command = "claude"

[roles.router]
provider = "groq"
model = "<fast-model>"

[roles.agent]
provider = "claude-code"
```

### 4.2 Speech: local and streaming by default

The STT interface is streaming: audio frames go in, **partial** and **final** transcripts come out. Partials drive the live transcript in the sidebar, the semantic end-of-turn check and speculative router calls. A provider that can only transcribe whole utterances (Groq Whisper) still fits: it just emits a final and no partials, at the cost of the faster end-of-turn.

Local STT either uses a native streaming model or re-decodes the growing utterance every ~300 ms with an offline model ("simulated streaming", which sherpa-onnx has examples for). Re-decoding is cheap for the small models and keeps the final transcript only one short decode behind the end of speech.

Model tiers (all through sherpa-onnx; exact choices to be benchmarked in Phase 1):

| Tier | STT | TTS | Target machine |
|---|---|---|---|
| Light | Moonshine (tiny/base) or a streaming Zipformer | Kitten TTS | Any laptop CPU |
| Standard (default) | Parakeet TDT 0.6B (int8), simulated streaming | Kokoro (82M) | 4+ cores, no GPU |
| Remote | Groq Whisper (final only), Deepgram (streaming) | Cloud TTS | Anything with a network |

Barge-in (interrupting by talking) needs VAD and STT running during playback, which depends on echo cancellation. Keeping speech local means that always-on listening costs no network round trips.

### 4.3 Routing: one model call with tools, not a separate classifier

The router gets the transcript plus context in one streaming call, with tools:

- `speak(text)`: what to say aloud. One or two sentences at most; no lists, URLs or code.
- `show(document)`: put markdown in the sidebar (new document, or append/replace an existing one).
- `escalate(role, task)`: hand the request to `answer` or `agent`. The router also calls `speak` with a short filler line ("let me pull that up") so there's immediate audio.
- Action tools: `launch_app`, `focus_window`, `close_window`, `web_search`, `fetch_url`, `get_clipboard`, `set_clipboard`, `get_time`, ...

Rules for the speech/sidebar split (system prompt and examples):
- Short factual answers are spoken only.
- Anything with lists, code, links, more than about 2 sentences, or something you'll want to re-read: a spoken summary plus a sidebar document.
- Never read code, URLs or long numbers aloud.
- When escalating: say the filler line immediately, and when the result arrives, give a spoken summary and the full detail in the sidebar.

Streaming: the text from `speak` is sent to TTS one sentence at a time while it's still being generated.

### 4.4 Web access

`web_search` and `fetch_url` tools with configurable providers (e.g. Brave, Tavily, SearXNG, or the search built into a provider). Results go into the sidebar with sources.

## 5. Phases

### Phase 1: Voice loop and sidebar (initial focus)

Goal: talking to the computer works smoothly for questions and basic app control,
using only standard protocols.

**Scope**
- Push-to-talk and toggle-listen through the `GlobalShortcuts` portal (Biome has a backend), with `vivusctl` as a fallback for any compositor (bind a key to run a command).
- Streaming STT (local by default, §4.2) with live partials, then the router, then TTS, with barge-in and echo cancellation.
- The sidebar: a layer-shell panel that shows/hides on demand, renders markdown documents, keeps a history of past documents, and shows the listening/thinking/speaking state.
- Knowledge Q&A, web search Q&A ("what's that song that starts with…", "latest AI news"), how-to questions ("how do I make a list in HTML").
- App control with standard protocols:
  - **Launch:** parse `.desktop` files (XDG dirs), fuzzy-match the spoken name, launch with an `xdg-activation` token.
  - **List/switch/close windows:** `ext-foreign-toplevel-list-v1` (list) and `wlr-foreign-toplevel-management-unstable-v1` (activate/close/minimize). Biome implements both.
  - **Active window as context:** the foreign-toplevel `activated` state gives the app ID and title.
- Clipboard read/write through `wlr-data-control` (Biome has it): "copy that", "what's on my clipboard", "summarize what I copied".

**Done when**
- "What time is it?" gives spoken audio quickly (target under ~1 s from the end of speech to the first audio, with remote inference).
- "What's the latest news about AI?" says a filler line immediately, then a spoken summary, with a sourced document in the sidebar.
- "Open my browser", "switch to my file manager", "close this" work reliably on Biome and on at least one other wlroots compositor (sway or Hyprland).
- Interrupting mid-reply stops it within ~200 ms.
- Every inference role can be switched between at least two providers using only the config file.
- The light tier holds a normal conversation on a 4-core laptop with no GPU.

### Phase 2: Screen awareness and deeper computer control

Goal: understand what's on screen and act inside apps.

**Scope**
- **Window content extraction**, in priority order:
  1. AT-SPI text and tree for the active window (GTK, Qt and Firefox work well; Chromium/Electron need accessibility turned on). Matching a Wayland window to its AT-SPI app needs the client PID; see the protocol below.
  2. A screenshot of a single window with `ext-image-copy-capture-v1` and `ext-foreign-toplevel-image-capture-source-v1` (Biome already has both), then OCR or the `vision` role. This alone makes a basic "what's this error?" possible without any new protocol.
- **"What's this error?"**: active window, then its text, then the router/answer model, with the explanation and fix in the sidebar.
- **Input injection** for typing and acting in apps: `zwp_virtual_keyboard_v1` and `zwlr_virtual_pointer_v1`, or libei. Biome has neither yet, so this is Biome work.
- **AT-SPI actions** (press a button, fill in a field, follow a menu path) as the main way to control apps, with screenshot-and-click as the fallback.
- **The `agent` role** handles multi-step tasks, with a confirmation step for destructive or outward-facing actions (sending, deleting, purchasing).
- **The custom protocol** (`vivus_*`, prototyped as a socket IPC behind a backend interface, then frozen as Wayland XML once stable) for what no standard covers:
  - Mapping a window to its client PID/app (for matching windows to AT-SPI apps)
  - Window geometry in global coordinates, stacking order, and the output the window is on (needed for gaze in Phase 3)
  - Possibly: reliable focus/raise without user-activation checks, and per-window "describe" metadata
  - It should be a **privileged** protocol, limited to trusted clients (Biome decides; e.g. matching the executable path or a security-context tag).

**Done when**
- "What's this error?" works for a terminal, a browser, and an IDE window.
- "Click Save", "type my email address here", "go to the next tab" work in common GTK/Qt/Firefox apps.
- Biome implements the custom protocol, and the protocol is documented well enough for a sway/Hyprland developer to implement it.

### Phase 3: Gaze tracking (optional enhancement)

Goal: use where you're looking to resolve "this/that/here" when there are several windows. Only ship this if it clearly improves on just using the active window.

**What the research says**
- Open-source webcam gaze trackers are off by about **2–5° of visual angle** (best recent results ~2°). At 60 cm that's 2–5 cm on screen: good enough to pick a window or pane, not a line of text.
- Gaze-guided assistants have been studied for AR glasses (GazePointAR, GazeGPT), and users rated pointing with their gaze as more natural. Nobody has built it for a desktop.

**Approach**
- A separate `vivus-gaze` process (camera access stays isolated; it can be turned off completely).
- MediaPipe face and iris landmarks plus a small per-user calibration model. Evaluate WebEyeTrack and EyeTheia.
- Calibrate silently from mouse clicks (you're almost always looking where you click) to correct drift.
- Keep a fixation history for the last 2–3 seconds and line it up with when you said "this/that". Turn it into a ranked list of candidate windows using the window geometry from the Phase 2 protocol.
- Hardware: a 1080p webcam at 30 fps or better, centred on top of the monitor, with even lighting. IR cameras or Tobii are the upgrade path.

**Done when:** in a blind A/B test with several windows open, gaze plus the active window resolves "this" correctly noticeably more often than the active window alone.

## 6. Compatibility matrix (target)

| Feature | Biome | wlroots (sway/Hyprland/…) | KDE | GNOME |
|---|---|---|---|---|
| Voice Q&A + sidebar | ✅ | ✅ | ✅ | ⚠️ no layer-shell (needs a normal window instead) |
| Push-to-talk (portal) | ✅ | depends on portal | ✅ | ✅ |
| Push-to-talk (`vivusctl` keybind) | ✅ | ✅ | ✅ | ✅ |
| Launch apps | ✅ | ✅ | ✅ | ✅ |
| Switch/close windows | ✅ | ✅ (wlr-foreign-toplevel) | ❌ (needs a KWin script backend) | ❌ (needs an extension) |
| Window screenshot | ✅ | depends on compositor | portal | portal |
| AT-SPI text/actions | ✅ | ✅ | ✅ | ✅ |
| Custom protocol (Phase 2+) | ✅ reference | if implemented | ❌ | ❌ |

KDE/GNOME backends are not a goal, but the backend interface should leave room for them.

## 7. Open questions

- ~~Sidebar: QML or Widgets?~~ Widgets (decided 2026-11-20), so Forest QSS themes can style it; it ships its own default QSS, overridable at `~/.config/vivus/sidebar.qss`.
- Wake word as well as push-to-talk? (Privacy plus false triggers vs. convenience.) Probably later and optional.
- Local fallback when offline: speech is local already; should the router have a small local fallback too?
- Storing API keys: environment variables, Secret Service (libsecret), or a config file?
- How much conversation history and sidebar history to keep, and where to store it.
- Security of action tools: which actions need spoken/visual confirmation, and how to avoid prompt injection from screen or web text causing actions (treat extracted text as data; confirm actions it suggests).

## 8. References

- Existing projects (none cover this whole scope): [Vigil](https://github.com/3L0935/Vigil) (offline LLM assistant with tools), [whisrs](https://docs.rs/crate/whisrs/0.1.19) (Wayland dictation), [Talon](https://talonvoice.com/docs/) (no Wayland support).
- Gaze research: [GazePointAR](https://arxiv.org/pdf/2404.08213), [GazeGPT](https://computationalimaging.org/?p=6731), [WebEyeTrack](https://arxiv.org/html/2508.19544v1), [webcam tracker thesis (~2°)](https://digitalcommons.unl.edu/computerscidiss/255), [UnitEye (C920 ~2.4°)](https://github.com/wgnrto/uniteye).
- Wayland input injection: [libei roadmap / compatibility notes](https://perrette.github.io/scribe/roadmap-libei/), [wdotool](https://github.com/cushycush/wdotool).
- Accessibility on Wayland: [GTK accessibility](https://wiki.gnome.org/Projects/GTK/Accessibility), [AT-SPI2](https://wiki.freedesktop.org/www/Accessibility/AT-SPI2/).
- Computer-use model capability: [OSWorld-Verified rankings](https://www.developersdigest.tech/models/best-for/computer-use). The best models complete about 86% of tasks (late 2026), so screenshot-and-click remains the fallback.
- Voice stack: [voice AI roundup (Aug 2026)](https://futureagi.com/blog/best-voice-ai-august-2026/). Kokoro/Piper for TTS, Parakeet/Whisper for STT, Pipecat as a reference for how to structure the pipeline.
