# CLAUDE.md

Read PLAN.md first; it holds the goals, phases and naming conventions.

- `dbus/org.vivus.Assistant.xml` is the contract. Change it first, then `crates/dbus` (client proxies), `crates/daemon/src/service.rs` (server) and the sidebar (generated from the XML by `qt_add_dbus_interface`).
- The daemon's D-Bus methods only enqueue `engine::Command`s; behaviour lives in the engine so in-flight work can be cancelled.
- Keep compositor-specific code behind backend interfaces (PLAN.md §2).
- All sherpa-onnx use stays in `crates/speech` (blocking; runs on the daemon's `vivus-speech` thread). `crates/audio` is PipeWire only.
- Test voice without a mic: create a virtual source (`pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=vivus_test media.class=Audio/Source/Virtual object.linger=true audio.position=[MONO] }'`), set `[audio] input = "vivus_test"`, play a WAV with `pw-cat --playback --target 0 -P '{ node.name=vivus-play }'` and `pw-link` it into `vivus_test:input_MONO`. Destroy the node afterwards.
- Comments: 1-3 lines, only the non-obvious why.
- Git: work on `develop`; `master` changes only at releases.
- Sidebar is Qt Widgets, not QML. Styling goes through QSS on object names (`sidebar/data/sidebar.qss`), so keep object names stable.
