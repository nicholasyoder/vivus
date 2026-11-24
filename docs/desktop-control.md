# Desktop control

Acting on the desktop through standard interfaces: launching apps, and later
windows and the clipboard. Phase 2's in-app control is in
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
