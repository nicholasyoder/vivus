# CLAUDE.md

Read PLAN.md first; it holds the goals, phases and naming conventions.

- `dbus/org.vivus.Assistant.xml` is the contract. Change it first, then `crates/dbus` (client proxies), `crates/daemon/src/service.rs` (server) and the sidebar (generated from the XML by `qt_add_dbus_interface`).
- The daemon's D-Bus methods only enqueue `engine::Command`s; behaviour lives in the engine so in-flight work can be cancelled.
- Keep compositor-specific code behind backend interfaces (PLAN.md §2).
- Comments: 1-3 lines, only the non-obvious why.
- Git: work on `develop`; `master` changes only at releases.
- Sidebar is Qt Widgets, not QML. Styling goes through QSS on object names (`sidebar/data/sidebar.qss`), so keep object names stable.
