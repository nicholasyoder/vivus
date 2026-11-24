# Architecture

## Overview

```
┌──────────────────────────── vivus-daemon (Rust, tokio) ────────────────────────────┐
│                                                                                      │
│  Audio in ─► VAD ─► STT (streaming) ─► Router LLM ──┬─► content  ─► TTS ─► Audio out │
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

## Naming

Rust crates are `vivus-*`; binaries are `vivus-daemon`, `vivus-sidebar` and
`vivusctl`; the D-Bus service is `org.vivus` at `/org/vivus`; Wayland protocols
are prefixed `vivus_` (e.g. `vivus_toplevel_info_v1`); config is
`~/.config/vivus/config.toml` (see `config.example.toml`); models live in
`~/.local/share/vivus/models`.

## Components

| Component | Language | Purpose |
|---|---|---|
| `vivus-daemon` | Rust | Audio pipeline, speech, LLM orchestration, tools, compositor backends, D-Bus service. Runs without a UI. |
| `vivus-sidebar` | C++ / Qt6 Widgets | Layer-shell panel: sidebar documents, state, live transcript, text input. A pure D-Bus client. |
| `vivusctl` | Rust | CLI: `toggle-listen`, `push-to-talk start/stop`, `ask`, `cancel`, `quit`, `toggle-sidebar`, `status`. For keybinds on compositors without a `GlobalShortcuts` portal, scripting and testing. |
| Forest plugin | C++ / Qt | Later and optional; a bridge in the Forest repo. |

## Repository layout

```
crates/
  core/        config, assistant state, sidebar documents (later: orchestration, tool registry)
  dbus/        D-Bus names and client proxies, mirrors dbus/*.xml
  daemon/      vivus-daemon: D-Bus service, engine, router, voice and TTS workers
  ctl/         vivusctl
  audio/       PipeWire capture (later playback)
  speech/      sherpa-onnx VAD, STT (later TTS); utterance listener
  inference/   LLM provider clients (OpenAI-compatible streaming)
  wayland/     standard-protocol compositor backend (stub)
sidebar/       CMake + Qt6 Widgets project; data/sidebar.qss is the default style
dbus/          D-Bus interface XML: the contract shared with every UI
protocol/      custom Wayland protocols (Phase 2)
scripts/       fetch-models.sh
docs/          roadmap and design docs
```

The only things shared with Forest and Biome are the protocol XML and the D-Bus
interface XML, never code libraries.

## D-Bus contract

`dbus/org.vivus.Assistant.xml` is the source of truth. Change it first, then
`crates/dbus` (client proxies), `crates/daemon/src/service.rs` (server) and the
sidebar (its proxy is generated from the XML by `qt_add_dbus_interface`).

- `org.vivus.Assistant`: `State` property (idle / listening / thinking / speaking, changes via `PropertiesChanged`); `ToggleListen`, `StartListening`/`StopListening` (push-to-talk), `Ask`, `Cancel`, `ToggleSidebar`; signals `Transcript(role, text, is_final)` and `SidebarVisibilityRequested(action)`.
- `org.vivus.Documents`: `List`, `Get`; signal `DocumentChanged(id, title, markdown)`.

## Daemon internals

- **Engine.** D-Bus methods only enqueue `engine::Command`s; the engine task owns all behaviour so in-flight work can be cancelled. Every stage should be a cancellable task: interrupting cancels in-flight TTS, LLM streams and pending tool calls.
- **Threads.** tokio for the engine and D-Bus; a `vivus-capture` thread runs the PipeWire loop; a `vivus-speech` thread runs the blocking sherpa-onnx work. Audio and control messages share one channel into the speech thread, so ordering holds (a push-to-talk release is processed after the audio before it). Details: [speech.md](speech.md).
- **Context providers.** Small functions that supply per-request context: time and date, locale, active window (app ID, title), clipboard, later selected text, window text and gaze target. The router gets a compact summary; the agent can fetch full content through tools.
- **Conversation memory.** A short rolling history so follow-ups work ("open it", "what about the second one?"). Sidebar documents are kept and can be referred back to.

## Compatibility (target)

| Feature | Biome | wlroots (sway/Hyprland/…) | KDE | GNOME |
|---|---|---|---|---|
| Voice Q&A + sidebar | ✅ | ✅ | ✅ | ⚠️ no layer-shell (normal window instead) |
| Push-to-talk (portal) | ✅ | depends on portal | ✅ | ✅ |
| Push-to-talk (`vivusctl` keybind) | ✅ | ✅ | ✅ | ✅ |
| Launch apps | ✅ | ✅ | ✅ | ✅ |
| Switch/close windows | ✅ | ✅ (wlr-foreign-toplevel) | ❌ (needs a KWin script backend) | ❌ (needs an extension) |
| Window screenshot | ✅ | depends on compositor | portal | portal |
| AT-SPI text/actions | ✅ | ✅ | ✅ | ✅ |
| Custom protocol (Phase 2+) | ✅ reference | if implemented | ❌ | ❌ |

KDE/GNOME backends are not a goal, but the backend interface should leave room for them.
