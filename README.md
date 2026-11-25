# vivus

A voice-driven assistant for Wayland desktops. See the [roadmap](docs/roadmap.md) for status and [docs/](docs/) for design.

## Layout

| Path | What |
|---|---|
| `crates/daemon` | `vivus-daemon`: audio, inference, tools, D-Bus service |
| `crates/ctl` | `vivusctl`: CLI for keybinds and scripting |
| `crates/core` | Config, state, sidebar documents, orchestration |
| `crates/dbus` | D-Bus names and client proxies (mirrors `dbus/*.xml`) |
| `crates/audio` | PipeWire mic capture |
| `crates/speech` | Local VAD and STT via sherpa-onnx; turns audio into utterances |
| `crates/inference`, `crates/wayland` | LLM clients and compositor backend (stubs for now) |
| `sidebar/` | `vivus-sidebar`: Qt6 Widgets layer-shell panel, a pure D-Bus client, styled with QSS |
| `dbus/` | D-Bus interface XML, the contract shared with every UI |
| `protocol/` | Custom Wayland protocols (Phase 2) |

## Build

Rust 1.85+, `pipewire-devel` and `clang-devel`; for the sidebar, Qt 6.8+ with
`layer-shell-qt-devel`. The first build downloads a prebuilt sherpa-onnx static
library (set `SHERPA_ONNX_LIB_DIR` to use your own).

```sh
cargo build
cmake -S sidebar -B sidebar/build && cmake --build sidebar/build
cargo run -p vivusctl -- models fetch    # light tier; or: standard, all, model names
cargo run -p vivusctl -- models list
```

`vivusctl models` downloads into `~/.local/share/vivus/models` (or
`$VIVUS_MODELS`); pass `--dir` if the config sets another `models_dir`.

## Install

```sh
cargo install --path crates/daemon
cargo install --path crates/ctl
cmake -S sidebar -B sidebar/build -DCMAKE_INSTALL_PREFIX=~/.local
cmake --build sidebar/build && cmake --install sidebar/build
```

This puts `vivus-daemon` and `vivusctl` in `~/.cargo/bin`, and `vivus-sidebar`
plus the `org.vivus.Vivus.desktop` entry under `~/.local`. The entry is the
app ID the daemon registers with the shortcuts portal and the sidebar's
Wayland app ID; launching it shows the sidebar.

To start vivus with the session, add both to your desktop's autostart (either
order; the sidebar connects once the daemon is up):

```sh
vivus-daemon
vivus-sidebar
```

There's no systemd unit. The daemon logs to stderr (`RUST_LOG=debug` for
more), so redirect it if your autostart drops output.

## Try it

```sh
cargo run -p vivus-daemon &
sidebar/build/vivus-sidebar &
cargo run -p vivusctl -- ask "hello"   # shows a placeholder document in the sidebar
cargo run -p vivusctl -- toggle-listen # talk; live transcript in the sidebar
cargo run -p vivusctl -- status
```

Benchmark STT models on a WAV (16 kHz mono):

```sh
cargo run --release -p vivus-speech --example stt_bench -- parakeet-tdt-110m file.wav 2
```

Config goes in `~/.config/vivus/config.toml`; start from `config.example.toml`.
