# Screen awareness and deeper control (Phase 2)

## Window content extraction

In priority order:

1. **AT-SPI** text and tree for the active window. GTK, Qt and Firefox work well; Chromium/Electron need accessibility turned on. Matching a Wayland window to its AT-SPI app needs the client PID (see the custom protocol below).
2. **Single-window screenshot** with `ext-image-copy-capture-v1` and `ext-foreign-toplevel-image-capture-source-v1` (Biome has both), then OCR or the `vision` role. This alone makes a basic "what's this error?" possible without any new protocol.

"What's this error?": active window → its text → `router`/`answer` → explanation
and fix in the sidebar.

## Acting in apps

- **AT-SPI actions** (press a button, fill in a field, follow a menu path) are the main way to control apps; screenshot-and-click is the fallback.
- **Input injection** for typing: `zwp_virtual_keyboard_v1` and `zwlr_virtual_pointer_v1`, or libei. Biome has neither yet.
- The **`agent` role** handles multi-step tasks, with a confirmation step for destructive or outward-facing actions (sending, deleting, purchasing).

## Custom protocol (`vivus_*`)

Prototyped as socket IPC behind a backend interface, then frozen as Wayland XML
in `protocol/` once stable. Covers what no standard does:

- Window → client PID/app (to match windows to AT-SPI apps)
- Window geometry in global coordinates, stacking order, and the output it's on (needed for gaze)
- Possibly: reliable focus/raise without user-activation checks, and per-window "describe" metadata

It's **privileged**: limited to trusted clients (Biome decides, e.g. by
executable path or a security-context tag). Biome is the reference
implementation; it must be documented well enough for other compositors.

## References

- Input injection: [libei roadmap / compatibility notes](https://perrette.github.io/scribe/roadmap-libei/), [wdotool](https://github.com/cushycush/wdotool)
- Accessibility: [GTK accessibility](https://wiki.gnome.org/Projects/GTK/Accessibility), [AT-SPI2](https://wiki.freedesktop.org/www/Accessibility/AT-SPI2/)
- Computer-use capability: [OSWorld-Verified rankings](https://www.developersdigest.tech/models/best-for/computer-use). The best models complete about 86% of tasks (late 2026), so structured actions stay first.
