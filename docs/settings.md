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

- `Get() → s`: the effective config as JSON, with defaults filled in (unset
  options are `null`).
- `Set(s path, v value)`: set one key (e.g. `audio.input`, `roles.stt.model`). It
  validates before writing, and an invalid value returns `InvalidArgs` with
  the reason (the deserializer's message, e.g. ``unknown field `bogus` ``).
- `Reset(s path)`: remove a key from the file so its default applies ("Default"
  in a picker is unset, not a value).
- `Changed(as paths)`: emitted after any write or reload, with the dotted leaf
  paths whose effective value changed (diffed from the JSON, so a reload that
  only touches comments emits nothing).
- `ListAudioDevices() → a(ssbs)`: id, description, is-default, direction
  (`source` / `sink`). `AudioDevicesChanged` is emitted on hotplug.
- `ListModels(s role) → a(ss)`: model id, label. For local speech this lists
  what's on disk under the models dir; for OpenAI-compatible providers it
  queries `/models`.

As with `org.vivus.Assistant`, the methods only enqueue engine commands.
`Set`/`Reset` carry a oneshot reply, so the call returns once the engine has
accepted or refused the edit. `vivusctl settings get|set|reset` wraps these.

### Writing and reloading

- `Set` re-reads the file, edits that text with `toml_edit` (the old value's
  trailing comment is kept; missing tables are created), and parses the result
  as a `Config` before anything is written. If the file already has a syntax
  error, the edit is refused until it's fixed.
- Writes go to a temp file beside the real one and rename over it. A symlinked
  config (dotfiles) is followed, so the link survives.
- The daemon watches the config's directory (and a symlink target's), since
  editors save by renaming. Events are settled for 150 ms, then the file is
  re-read. Text equal to our own last write is ignored. Invalid text is logged
  and the last good config kept.
- Applied live: `roles.*` and `providers.*` (the router is rebuilt on the next
  turn), `debug.*`, and the mic settings `audio.input`, `audio.echo_cancel`,
  `audio.keep_mic_open` (see [Audio devices](#audio-devices)). After a
  restart, for now: the other `audio.*` keys, `shortcuts.*`, `roles.stt.*`,
  `roles.tts.*` and local-speech `models_dir`/`threads`
  (`settings::needs_restart`). The daemon logs which. The settings-page item
  moves the rest of `audio.*` to live.

## Audio devices

`vivus_audio::Devices` watches the PipeWire registry on its own thread
(`vivus-devices`): nodes of class `Audio/Source`, `Audio/Source/Virtual` and
`Audio/Sink`, plus the `default` metadata's `default.audio.source` /
`default.audio.sink` for the is-default flag. It reports the whole list once
the initial scan is done (two core syncs: globals, then the metadata
properties that binding sends) and again whenever the list changes, so the
daemon emits `AudioDevicesChanged` only for real changes. Our own echo-cancel
nodes (`vivus-*`) are left out. `vivusctl settings devices` prints the list.

Choosing a device sets `audio.input` / `audio.output` to the node name. "Default"
means unset, which follows the system default.

**Live input switching.** A change to `audio.input`, `audio.echo_cancel`,
`audio.keep_mic_open` or `audio.output` (the canceller's reference) reopens the
mic: the old stream and its echo-cancel module are dropped first (node names
would clash), then a new one is spawned and set active if we were listening.
Speech playback still picks up `audio.output` only after a restart.

**Hotplug.** If the configured input disappears, WirePlumber moves the stream
to the default source. When it shows up again in the device list, the daemon
reopens the mic so it goes back to the chosen device (checked with a lingering
`support.null-audio-sink` node created and destroyed via `pw-cli`).

Still to come with the settings page: speech volume sets the playback stream's
volume (what a mixer shows, so the two agree); `duck_volume` is how far other
apps go down while speaking. A level meter next to the input picker shows
whether the mic is actually working.

**Sidebar types.** `a(ssbs)` needs a Qt type name for `qdbusxml2cpp`, so the
XML annotates it as `AudioDeviceList`, declared in `sidebar/src/dbustypes.h`
and registered at startup.

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
