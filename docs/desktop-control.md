# Desktop control

Acting on the desktop through standard interfaces: global shortcuts,
launching apps, managing windows and the clipboard. Phase 2's in-app control is in
[screen-awareness.md](screen-awareness.md).

## App launching

The router has a `launch_app(name)` tool. The model passes the name as the user
said it. Matching happens in the daemon, so the model never needs the app list
(~140 entries here, too many tokens per request).

**Index** (`crates/apps`). On each call, the daemon reads every `.desktop` file
under `$XDG_DATA_HOME/applications` and `$XDG_DATA_DIRS/*/applications`. That
takes a few ms, so nothing is cached and newly installed apps show up at once.
It follows the spec's rules: earlier dirs shadow later ones by desktop ID, and
`Hidden`, `NoDisplay`, `OnlyShowIn`/`NotShowIn` (against
`XDG_CURRENT_DESKTOP`) and `TryExec` are honoured. Names are localized from
`LC_ALL`/`LC_MESSAGES`/`LANG`, and the untranslated name still matches. Apps
whose names are identical (e.g. native and Flatpak copies) count as one.

**Matching** (`find.rs`). Transcripts split and mishear names, so every field
is compared lowercased and without spaces ("fire fox", "pavu control"). Each
field has a weight: `Name` 1.0, desktop ID tail and `Exec` program name 0.95,
`GenericName` 0.9, `Keywords` 0.8. A field scores 1.0 for an exact match,
0.85 for a prefix (3+ chars), 0.8 if all the query's words appear in it, and
otherwise its edit-distance similarity × 0.9 if that's ≥ 0.7. Words like "my",
"the" and "app" are dropped first. Generic names (browser, file manager,
terminal, text editor, email, music/video player, image or PDF viewer,
calculator) go to the user's default handler from `mimeapps.list`, unless an
app is literally called that. The outcome is one of three:

- **Found**: launched, and Vivus says "Opening *Name*." itself if the model said
  nothing. No second LLM call, so "open the calculator" took ~0.5 s end to end.
- **Ambiguous**: scores within 0.05 of the best (e.g. "settings"). The tool
  result lists them and the model asks the user.
- **Not found**: the nearest few go back to the model, which either retries
  with one of them or tells the user.

**Launching** (`launch.rs`). `Exec` is split by the spec's quoting rules. File
and URL field codes are dropped; `%i`, `%c` and `%k` are expanded. `Path` sets
the working directory. `Terminal=true` apps go through `xdg-terminal-exec`,
then `$TERMINAL`, then the first `TerminalEmulator` app. When a systemd user
manager is running, the app runs as `systemd-run --user --scope` in
`app-vivus-<id>-<random>.scope` (the XDG app-scope naming). That way it
doesn't die with the daemon's unit once the daemon runs under systemd, and it
shows up like apps started by other launchers. Without one, the app is
spawned directly in its own process group. `DBusActivatable` apps with no
`Exec` aren't supported: none are installed here.

**Activation token** (`crates/wayland`). Before spawning, the daemon asks for
an `xdg-activation-v1` token with the app ID set, over a short-lived Wayland
connection. It passes the token as `XDG_ACTIVATION_TOKEN` and
`DESKTOP_STARTUP_ID`. We have no input serial or surface to offer, so a
compositor with strict focus-stealing prevention may decline to focus the
app. Biome issues the token. If there's no token, the app launches anyway.

Try matching without the daemon:
`cargo run -p vivus-apps --example find -- my browser` (add `--launch` to start it).

## Windows

The router has a `window(action, window?)` tool: `switch`, `close` or
`minimize`. As with `launch_app`, the model passes the window as the user said
it, and the daemon does the matching.

**Backend** (`crates/wayland/src/windows.rs`). This sits behind
`vivus_core::WindowBackend`, so a custom-protocol backend can replace it. One
Wayland connection stays open on the `vivus-wayland` thread. It keeps the list
current from `done` events. `wlr-foreign-toplevel-management` (v1-3) gives
title, app ID, state (active, minimized, maximized, fullscreen) and the
actions. Activating a minimized window unminimizes it first. If that protocol
is missing, `ext-foreign-toplevel-list-v1` gives a read-only list with no
active window. The two are never paired: neither protocol links its handles to
the other's, and matching them by creation order (as Forest's panel does)
depends on how the compositor behaves. The list is ordered by our own focus
clock: the active window first, then by when each window last gained focus.
Windows that were already open at startup count as least recent.

**Matching** (`crates/daemon/src/windows.rs`). An empty `window`, or one made
only of words like "this", "it" or "current window", means the active window.
Otherwise each window is scored, and the highest of these counts:

- 1.0 if `Apps::find` resolves the query to this window's app. This covers
  generic names ("my file manager") and misheard app names.
- The app's display name, scored by `vivus_apps::name_score` (the app-matching
  rules).
- The app ID's last part × 0.95.
- The title × 0.9.

A window is found from 0.6 up, and windows within 0.05 of the best are a tie.
For a tie, `switch` takes the most recent window that isn't active, as long as
all the tied windows belong to one app ("switch to Firefox" from a Firefox
window). `close` and `minimize` take the active window if it's tied, and
otherwise ask. A window is found by its desktop entry: the desktop ID, then
`StartupWMClass`, then the ID's last component. The sidebar (`vivus-sidebar`)
is left out.

**Silent actions.** A window that switches, closes or minimizes is feedback
enough, so a successful call says nothing and needs no second LLM call. All of
these took 0.5-1 s end to end on Biome. Misses and ties go back to the model as
tool results. A miss on `switch` suggests `launch_app`.

**Context.** Each router request gets the open windows in its system prompt,
up to 15, as `App: "title"` with active/minimized flags. Titles are cut to 80
characters and marked as data, never instructions. This lets the model answer
"what windows do I have open?" and pick the window for "switch to the one with
the weather".

**Pronunciations.** When a window gains focus, its title goes to the
pronunciation prefetch (only titles that gain focus, so a terminal that
retitles itself on every command doesn't flood the lookups).

Try it without the daemon: `cargo run -p vivus-wayland --example windows`
lists the windows and watches focus for 5 s, and
`-- <activate|close|minimize> <id>` acts on one.

## Clipboard

The router has `get_clipboard()` and `set_clipboard(text)`. There's no
clipboard in the system prompt: the model reads it only when asked about what
the user copied, so a copied password or private text never goes out with an
unrelated request. Whether a short summary should go into every request is up
to the 1e context-providers item.

**Backend** (`crates/wayland/src/clipboard.rs`). This sits behind
`vivus_core::ClipboardBackend`. A `wlr-data-control` connection stays open on
the `vivus-clipboard` thread and tracks the current selection offer and its
MIME types. We bind v1, so the primary selection (middle-click paste) stays
out of it. Biome, wlroots compositors, Hyprland and KWin have the protocol.
GNOME has no data-control, so there's no clipboard there. `ext-data-control-v1`
does the same job, but Biome doesn't offer it and the compositors that do also
offer the wlr protocol, so we don't use it yet.

- **Read.** Offers carrying `x-kde-passwordManagerHint` (KeePassXC, Bitwarden,
  KDE) come back as `Secret`, and their contents are never read. Otherwise it
  takes the first text type of `text/plain;charset=utf-8`, `UTF8_STRING`,
  `text/plain`, `STRING`, `TEXT`, receives it over a pipe, and stops at EOF,
  1 MiB or 1 s, whichever comes first. If there's no text type, it reports the
  MIME types instead ("something that isn't text (image/png)").
- **Write.** A data source offers the same text types and becomes the
  selection. While Vivus owns the clipboard, reads return its text directly.
  Each paste is written on its own thread so a slow reader can't stall the
  connection. Like any Wayland client, Vivus serves the copied text itself, so
  it's gone when the daemon quits unless a clipboard manager keeps a copy.

**Tools** (`crates/daemon/src/clipboard.rs`). `get_clipboard` hands the model
up to 4000 characters between `<<<` and `>>>` and tells it the text is data,
never instructions. It always needs an answer. `set_clipboard` is silent like
`launch_app`: Vivus says "Copied." itself if the model said nothing. Without a
prompt example, gpt-oss-120b escalated "translate what I copied into Spanish
and copy it" to the agent. With the example, the router does it alone:

| Request | Tools | End to end |
| --- | --- | --- |
| "what did I copy?" | `get_clipboard` | 1.15 s |
| "translate what I copied into Spanish and copy it to my clipboard" | `get_clipboard`, `set_clipboard` | 1.50 s |
| "copy the word banana to my clipboard" | `set_clipboard` | 0.58 s |
| "summarize my clipboard" | `get_clipboard` | 0.97 s |

Tested on Biome on 2026-11-24 with `vivusctl ask`. `wl-paste` read what Vivus
copied, and a `wl-copy --type x-kde-passwordManagerHint` offer read as
`Secret`.

Try it without the daemon: `cargo run -p vivus-wayland --example clipboard`
prints the clipboard, and `-- <text>` copies the text and keeps it on offer
for 30 s.

## Global shortcuts

The daemon binds its keys through `org.freedesktop.portal.GlobalShortcuts`
(`crates/daemon/src/shortcuts.rs`), so any desktop with that portal works
without compositor config. Triggers come from `[shortcuts]` in the config
(shortcuts-spec format; `""` leaves one unbound):

| Shortcut | Default | Does |
| --- | --- | --- |
| `push-to-talk` | `LOGO+space` | Press → `StartListening`, release → `StopListening` |
| `toggle-listen` | `LOGO+SHIFT+space` | `ToggleListen` (conversation mode) |
| `toggle-sidebar` | `LOGO+v` | `SidebarVisibilityRequested("toggle")` |

**Session.** The portal client has its own session-bus connection, since the
session and app ID belong to the connection. It first calls
`org.freedesktop.host.portal.Registry.Register("org.vivus.Vivus")`:
xdg-desktop-portal 1.19+ wants an app ID for unsandboxed apps. That fails
until `org.vivus.Vivus.desktop` is installed (see the 1d desktop-entry item),
and Biome's backend binds anyway. Then `CreateSession` and `BindShortcuts`,
each answered by a `Request` `Response` that we subscribe to before calling.
The bound triggers are logged; anything refused or already taken gets a
warning. If the portal restarts or closes the session, the daemon rebinds, and
a push-to-talk held at that moment is released.

**Backends.** Biome binds the preferred triggers straight away with no dialog,
skipping any already in use, and sends `Deactivated` on release, which
push-to-talk needs. KDE and GNOME show a dialog on first bind and keep what
the user picks, so the config values are only suggestions there. Without the
portal (sway, Hyprland without `xdg-desktop-portal-hyprland`'s shortcuts), the
log says so and `vivusctl toggle-listen` / `push-to-talk start|stop` bound in
the compositor are the fallback.

Tested on Biome 2026-11-24: holding `Meta+Space` listened until release and
then answered; `Shift+Meta+Space` started and stopped conversation mode;
`Meta+V` toggled the sidebar. The rebind after a portal restart hasn't been
exercised.
