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
- `ListProviders(s role) → as`, `ListModels(s role, s provider) → a(ss)`,
  `ListVoices(s provider, s model) → a(ss)`: what the role pickers offer (see
  [Roles and providers](#roles-and-providers)).
- `SetApiKey(s provider, s key)`, `ApiKeyStatus(s provider) → s`: see
  [API keys](#api-keys).

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
  an utterance in progress), `audio.speech_volume`, `audio.duck_volume`, and
  speech models (see [Reloading speech](#reloading-speech)). Only
  `shortcuts.*` waits for a restart (`settings::needs_restart`); the daemon
  logs it.

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

The window's Models group has a row per role (stt, tts, router, answer,
agent, vision): a provider picker, then a model picker, plus the TTS voice.
Keys are set in their own group (see [API keys](#api-keys)); adding and
editing providers comes later (see [Providers](#providers)).

- **Providers.** `ListProviders(role)` returns the configured providers whose
  kind can serve the role (`Provider::serves`): `local-speech` for stt/tts,
  `openai-compatible` for the LLM roles, `claude-code` for answer and agent
  only (the router needs streamed tool calls). The first picker entry is the
  unset role: "Default (local)" for stt/tts, "None" for the rest (agent then
  falls back to answer).
- **Models.** `ListModels(role, provider)` gives `(id, label)`; an empty id is
  the unset model, labelled with the default it stands for. Local speech
  lists catalog models that are on disk (`installed()`), with labels from the
  catalog. `claude-code` offers its aliases (haiku, sonnet, opus). An
  `openai-compatible` provider is asked for `GET /models`, cached for 5
  minutes per provider, with speech, embedding and guard models dropped by
  id. That list can be long (nano-gpt: ~600) and may lack a model the
  provider still serves, so its picker is editable: type to filter, or enter
  any id. A configured model missing from the list stays shown.
- **Voices.** `ListVoices(provider, model)` lists a TTS model's voices with
  readable labels (`af_heart` → "Heart (American, female)"; Kitten's
  `expr-voice-*` aliases are skipped).
- **Edits.** A model or voice belongs to its provider or model, so changing
  the provider resets `model` (and `voice`) before setting `provider`, and
  changing the TTS model resets `voice` first. Each step is a config the
  daemon accepts; the speech reload waits for them to settle. Choosing the
  first provider entry resets the whole `[roles.<role>]` table. `Reset` of a
  key under a missing table is a no-op (creating an empty `[roles.tts]` would
  be invalid: a role needs a provider).
- **Checks.** A `Set`/`Reset` under `roles.stt` or `roles.tts` is refused
  unless the model is known and downloaded and the voice belongs to it, so
  `vivusctl settings set roles.tts.voice bella` with Kokoro fails with the
  known voices instead of breaking speech later.
- `vivusctl settings providers|models|voices` print the same lists.

### Reloading speech

A change to what `roles.stt` or `roles.tts` resolves to (provider, model,
voice, or the local-speech provider's `models_dir`/`threads`) reloads that
side 300 ms after the last such change, so a reset-then-set pair loads once.
Each load carries a generation and an older result is dropped. A config that
can't load (a hand edit naming a missing model) is logged and the old models
are kept.

- **TTS** loads beside the old speaker, which keeps talking until the new
  one is ready; then a reply still playing is cut (the new stream wouldn't
  report it finishing) and the old speaker is dropped. Kokoro takes about 2 s
  to load, Kitten under 1 s.
- **STT** has to close the mic first, since the new capture's echo-cancel
  nodes would clash with the old one's. The mic is off for the load (about
  1 s for parakeet-110m), then listening resumes in the same mode.
  `audio.record_utterances` reloads it the same way.

## API keys

Keys stay out of `config.toml`, which people share and keep in dotfiles. The
settings window sets them; nothing has to be edited by hand.

- **Store.** The Secret Service (GNOME Keyring, KWallet, KeePassXC) through
  `oo7` (0.5: newer ones need zbus 6), one item per provider labelled
  "Vivus API key: <name>" with attributes `application = vivus`,
  `provider = <name>` (`secret-tool search application vivus` shows them).
  Without a Secret Service (common on sway or Hyprland), the daemon writes
  `~/.config/vivus/.env` itself, mode 0600, through a temp file and rename,
  keeping the file's other lines.
- **Variable.** A provider's key goes by `api_key_env` in the environment and
  `.env`, else a name derived from the provider's (`nano-gpt` →
  `NANO_GPT_API_KEY`), so a fresh provider needs no extra config.
- **Lookup order** (`vivus_core::keys::api_key`): the environment, then the
  keyring, then `.env`. An environment variable wins, so scripted setups keep
  working; the window says so instead of offering Remove. A provider with
  `api_key_env` set fails to build a client without a key; one without may
  run keyless (a local server).
- **Keyring cache.** Client construction is synchronous, so the daemon loads
  the keyring's vivus items into a cache in `vivus-core` at startup
  (`keyring::load`, waiting up to 3 s: a locked keyring prompts to unlock, and
  a later answer still fills the cache) and updates it on every `SetApiKey`.
  Edits made in Seahorse while the daemon runs apply after a restart. `.env`
  is read on each lookup.
- **D-Bus.** `SetApiKey(provider, key)` stores the key (an empty key removes
  it); it's trimmed, and refused if it has spaces, quotes or control
  characters. Storing in the keyring also drops a `.env` copy, and removing
  clears both, so the status tells where the key really is.
  `ApiKeyStatus(provider)` returns `environment`, `keyring`, `file` or
  `missing`. A key is never sent back out.
- **Applying.** Setting a key drops the router (rebuilt on the next turn) and
  that provider's `/models` cache entry. Pronunciation lookups keep the
  client they started with until the speaker reloads.
- **Window.** An "API keys" group lists each `openai-compatible` provider
  with a masked field whose placeholder says where the current key lives,
  plus Save and Remove. After a save the roles on that provider list their
  models again, which tests the key (a failure shows in the model picker's
  tooltip).
- **CLI.** `vivusctl settings key <provider>` prints the status; `--set`
  reads a key from stdin without echo (so it stays out of shell history),
  `--remove` deletes it.

## Providers

Planned. The window gets a Providers group listing each provider with its
kind, base URL and key status.

- **Built-ins.** `openai` (`https://api.openai.com/v1`), `nano-gpt` and
  `groq` exist without any config, merged under the file's `[providers]`. A
  file entry with the same name overrides one. Built-ins can't be removed,
  but without a key they cost nothing, and the role pickers mark them
  "(no key)". `local-speech` and `claude-code` become built-ins too, so a
  fresh config needs no `[providers]` table at all.
- **Adding.** Name, base URL and optional key, for `openai-compatible` only.
  A provider table has to be written in one step (`kind` and `base_url` are
  both required), so `Set` also takes a dictionary (`a{sv}`) and writes it
  as a table. Adding is `Set("providers.<name>", {kind, base_url})`, then
  `SetApiKey`.
- **Removing** is `Reset("providers.<name>")`, which config validation
  already refuses while a role uses the provider. Removing also deletes the
  provider's key.

## Later

- Compositor backend override (auto-detect `wayland-std` vs `vivus-protocol`),
  once Phase 2 adds the second backend.
- Shortcuts, once the `GlobalShortcuts` portal integration exists (the portal
  owns the binding UI; the sidebar just links to it).
