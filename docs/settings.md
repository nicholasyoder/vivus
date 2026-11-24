# Settings

The sidebar gets a settings page so common choices don't need a text editor.
`~/.config/vivus/config.toml` stays the single source of truth: the sidebar
edits it through the daemon, and anything the page can't edit is still
available in the file.

## Principles

- **The daemon owns the config.** The sidebar is a pure D-Bus client, so it
  never touches the file itself. It reads and writes settings over D-Bus, and
  the daemon validates, persists and applies them.
- **Writes keep the file readable.** Use `toml_edit` so comments, ordering and
  unknown-to-the-UI keys survive a round trip.
- **Apply live where it's cheap.** Switching the input device or a role's
  provider/model applies on the next utterance or turn without restarting the
  daemon. Anything that can't be applied live (e.g. speech thread count) is
  marked "applies after restart" in the UI.
- **External edits count too.** The daemon watches the file and reloads it, so
  hand edits and UI edits converge. An invalid file is reported and ignored,
  and the last good config is kept.

## D-Bus

A new `org.vivus.Settings` interface on `/org/vivus` (contract first, as usual):

- `Get() → s`: the effective config as JSON, with defaults filled in.
- `Set(s path, v value)`: set one key (e.g. `audio.input`, `roles.stt.model`). It
  validates before writing, and an invalid value returns a D-Bus error with
  the reason.
- `Changed(as paths)`: emitted after any write or reload.
- `ListAudioDevices() → a(ssbs)`: id, description, is-default, direction
  (`source` / `sink`). `AudioDevicesChanged` is emitted on hotplug.
- `ListModels(s role) → a(ss)`: model id, label. For local speech this lists
  what's on disk under the models dir; for OpenAI-compatible providers it
  queries `/models`.

As with `org.vivus.Assistant`, the methods only enqueue engine commands.

## Audio devices

`crates/audio` enumerates PipeWire nodes from the registry
(`Audio/Source`, later `Audio/Sink` for TTS playback) and tracks add/remove.
Choosing a device sets `audio.input` / `audio.output` to the node name. "Default"
means unset, which follows the system default. The echo-cancel source shows
up as a normal device but is labelled as such. A level meter next to the input
picker shows whether the mic is actually working.

## Roles and providers

Per role (stt, tts, router, answer, agent, vision): a provider picker filtered
to the providers that can serve that role (e.g. only `local-speech` and remote
STT providers for `stt`), then a model picker from `ListModels`, plus
role-specific extras (TTS voice). Adding or editing providers (base URL, API key)
comes later and depends on how API keys are stored (see the roadmap's open
questions).

## Later

- Compositor backend override (auto-detect `wayland-std` vs `vivus-protocol`),
  once Phase 2 adds the second backend.
- Shortcuts, once the `GlobalShortcuts` portal integration exists (the portal
  owns the binding UI; the sidebar just links to it).
