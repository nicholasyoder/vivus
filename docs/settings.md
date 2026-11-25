# Settings

The sidebar app has a settings window so common choices don't need a text
editor. `~/.config/vivus/config.toml` stays the single source of truth: the
window edits it through the daemon, and anything it can't edit is still
available in the file ("Open config file").

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
- `MeterInput()`: send `InputLevel(d peak, b clipped)` about 20 times a second
  for the next 5 s, keeping the mic open meanwhile (see [Level meter](#level-meter)).
- `GetInputGain() → d`, `SetInputGain(d)`, `InputGainChanged(d)`: the mic's
  volume (see [Input gain](#input-gain)).
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
  turn), `debug.*`, the mic settings `audio.input`, `audio.echo_cancel`,
  `audio.keep_mic_open` (see [Audio devices](#audio-devices)), and
  `audio.output`, `audio.end_of_turn_silence` (the VAD is recreated, dropping
  an utterance in progress), `audio.speech_volume`, `audio.duck_volume`.
  After a restart, for now: `audio.record_utterances`, `shortcuts.*`,
  `roles.stt.*`, `roles.tts.*` and local-speech `models_dir`/`threads`
  (`settings::needs_restart`). The daemon logs which.

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

**Live output switching.** A change to `audio.output` recreates the playback
stream on the new sink (its `PlaybackSender` is shared, so the TTS thread's
clone follows). A reply being spoken is cancelled first, since the new stream
won't report the old one finishing.

**Live input switching.** A change to `audio.input`, `audio.echo_cancel`,
`audio.keep_mic_open` or `audio.output` (the canceller's reference) reopens the
mic: the old stream and its echo-cancel module are dropped first (node names
would clash), then a new one is spawned and set active if we were listening.
Speech playback still picks up `audio.output` only after a restart.

**Hotplug.** If the configured input disappears, WirePlumber moves the stream
to the default source. When it shows up again in the device list, the daemon
reopens the mic so it goes back to the chosen device (checked with a lingering
`support.null-audio-sink` node created and destroyed via `pw-cli`).

**Speech volume.** `audio.speech_volume` (0 to 1, default 1) is set as the
playback stream's `channelVolumes`, cubed, so a mixer shows the same
percentage. It's re-applied each time the stream starts, so it wins over a
volume WirePlumber restored for the app; changing the app's volume in a mixer
lasts until the next reply. `audio.duck_volume` changes apply at once, even
mid-duck.

## Input gain

The gain slider sets the PipeWire volume of the source the mic stream
records from (`audio.input` if it's connected, else the system default), 0 to
1 on the cubic scale mixers show. It isn't a config key: WirePlumber already
saves a volume per device and route, and a second copy in our config would
fight it. `vivusctl settings gain [value]` gets or sets it.

- **Writing.** For a sound card's source, the volume is set on the card's
  active input `Route` (index and profile device from the node's
  `card.profile.device`), with `save: true`, which is what pavucontrol and
  `wpctl set-volume` do. That moves the hardware mixer (`Capture`, and the
  `Mic Boost` steps above it) and WirePlumber writes it to
  `~/.local/state/wireplumber/default-routes`. Setting `channelVolumes` in
  the node's Props instead is accepted but ignored by ALSA nodes (checked
  2026-11-24). Sources without a card (virtual, filters) get node Props.
- **Reading.** `vivus-devices` subscribes to each source's Props and reports
  `channelVolumes` (loudest channel, cube root) on every change, so the slider
  follows a mixer, alsamixer, or another app's automatic gain control.
  `card.profile.device` isn't in the registry's global props, so it comes from
  the node's info (only events whose change mask includes props).
- **Why not alsamixer.** A change made only in alsamixer is overwritten when
  WirePlumber re-applies its saved route volume (device reopened, port change,
  suspend). Apps with WebRTC AGC (Chrome, Discord, Zoom) also push the mic
  toward 100% while they use it; pipewire-pulse's `block-source-volume` quirk
  stops that per app.

## Level meter

The meter shows what STT hears: the capture stream after echo cancellation,
not the raw mic. Capture computes the peak of each 50 ms block on its own
thread while metering is on, and the engine turns each into an `InputLevel`
signal. Metering is a lease, not on/off: each `MeterInput` call extends it to
5 s from now, so a client that crashes or forgets can't leave the mic open
and the bus busy. The settings window calls it every 2 s while shown.
`clipped` means a sample reached 0.99; the window turns the meter red for
1.5 s (the gain slider sits right below it). Metering holds the stream open like listening does (relevant only
with `keep_mic_open = false`).

## Settings window

A normal toplevel window, not a page inside the sidebar: the sidebar is a
narrow layer-shell panel meant for answers, a form wants more room, and the
window should stay open while you talk to test a change. It opens from the
sidebar's Settings button or `vivus-sidebar --settings`, and closing it
doesn't quit the app. It shares the sidebar's process and QSS (`#VivusSettings`
and the object names listed in `sidebar.qss`).

- Gain applies 100 ms after the slider stops, so you can watch the meter
  while dragging; it follows outside changes except mid-drag.
- Device pickers: "System default (…)" is the unset value (`Reset`); a
  configured device that's unplugged stays listed as "(not connected)".
- Sliders and the silence spin box write after a 250 ms pause, so dragging
  doesn't rewrite the file on every step but you still hear the change.
- Every `Changed` re-reads `Get`, so hand edits show up while it's open. A
  refused `Set` shows the daemon's reason and puts the control back.
- Disabled, with a note, while the daemon isn't running.

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
