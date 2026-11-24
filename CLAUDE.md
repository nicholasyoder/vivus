# CLAUDE.md

Start with [docs/roadmap.md](docs/roadmap.md): it says where the project is and
what's next. Topic docs in `docs/` hold the design detail
([vision](docs/vision.md), [architecture](docs/architecture.md),
[inference](docs/inference.md), [speech](docs/speech.md),
[screen-awareness](docs/screen-awareness.md), [gaze](docs/gaze.md), [settings](docs/settings.md)).

## Working on roadmap items

- One roadmap item per session, usually the first unchecked one in the current sub-phase.
- When it's done, check it off with the date and short commit hash, in the same commit as the work (amend or follow up with the hash if needed). Leave it in its section; don't move finished items.
- Keep the roadmap concise: one line per item. Design notes, findings and benchmarks go in the relevant topic doc (create one if none fits) and the item links to it.
- New work discovered along the way becomes a new roadmap item, not a paragraph.
- Settled decisions go in the roadmap's "Decided" list; unresolved ones in "Open questions".

## Code

- `dbus/org.vivus.Assistant.xml` is the contract. Change it first, then `crates/dbus` (client proxies), `crates/daemon/src/service.rs` (server) and the sidebar (generated from the XML by `qt_add_dbus_interface`).
- The daemon's D-Bus methods only enqueue `engine::Command`s; behaviour lives in the engine so in-flight work can be cancelled.
- Keep compositor-specific code behind backend interfaces ([vision.md](docs/vision.md) principle 1).
- All sherpa-onnx use stays in `crates/speech` (blocking; runs on the daemon's `vivus-speech` and `vivus-tts` threads). `crates/audio` is PipeWire only.
- Sidebar is Qt Widgets, not QML. Styling goes through QSS on object names (`sidebar/data/sidebar.qss`), so keep object names stable.
- Comments: 1-3 lines, only the non-obvious why.
- Testing voice without a mic: see [docs/speech.md](docs/speech.md#testing-without-a-microphone).

## Git

Claude owns git here: commit and push when it makes sense, without asking.
Work on `develop`; `master` changes only at releases. Major features go on a
branch with a PR into `develop` once a remote exists.
