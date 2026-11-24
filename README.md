# vivus

A voice-driven assistant for Wayland desktops. See [PLAN.md](PLAN.md).

## Layout

| Path | What |
|---|---|
| `crates/daemon` | `vivus-daemon`: audio, inference, tools, D-Bus service |
| `crates/ctl` | `vivusctl`: CLI for keybinds and scripting |
| `crates/core` | Config, state, sidebar documents, orchestration |
| `crates/dbus` | D-Bus names and client proxies (mirrors `dbus/*.xml`) |
| `crates/audio`, `crates/inference`, `crates/wayland` | Pipeline pieces (stubs for now) |
| `sidebar/` | `vivus-sidebar`: Qt6 Widgets layer-shell panel, a pure D-Bus client, styled with QSS |
| `dbus/` | D-Bus interface XML, the contract shared with every UI |
| `protocol/` | Custom Wayland protocols (Phase 2) |

## Build

Rust 1.85+ and, for the sidebar, Qt 6.8+ with `layer-shell-qt-devel`.

```sh
cargo build
cmake -S sidebar -B sidebar/build && cmake --build sidebar/build
```

## Try it

```sh
cargo run -p vivus-daemon &
sidebar/build/vivus-sidebar &
cargo run -p vivusctl -- ask "hello"   # shows a placeholder document in the sidebar
cargo run -p vivusctl -- status
```

Config goes in `~/.config/vivus/config.toml`; start from `config.example.toml`.
