#!/usr/bin/env bash
# Symlink this checkout's builds into ~/.local, so autostart always runs the
# latest build. Build first: cargo build --release, and the sidebar's CMake build.
# Usage: dev-link.sh [--debug]   (default links target/release)
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
profile=release
[[ ${1:-} == --debug ]] && profile=debug
bin=$HOME/.local/bin
apps=${XDG_DATA_HOME:-$HOME/.local/share}/applications
mkdir -p "$bin" "$apps"

link() { # target dir
    [[ -e $1 ]] || echo "warning: $1 not built yet" >&2
    ln -sfn "$1" "$2/"
    echo "$2/$(basename "$1") -> $1"
}

link "$repo/target/$profile/vivus-daemon" "$bin"
link "$repo/target/$profile/vivusctl" "$bin"
link "$repo/sidebar/build/vivus-sidebar" "$bin"
link "$repo/sidebar/data/org.vivus.Vivus.desktop" "$apps"

case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "note: $bin isn't on PATH; autostart won't find the binaries" >&2 ;;
esac
