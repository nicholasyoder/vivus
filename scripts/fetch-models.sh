#!/usr/bin/env bash
# Download sherpa-onnx speech models into $VIVUS_MODELS (default ~/.local/share/vivus/models).
# Usage: fetch-models.sh [light|standard|all]
set -euo pipefail

base=https://github.com/k2-fsa/sherpa-onnx/releases/download
dest=${VIVUS_MODELS:-${XDG_DATA_HOME:-$HOME/.local/share}/vivus/models}
mkdir -p "$dest"

fetch_file() { # tag file
    [[ -e "$dest/$2" ]] && return
    echo "fetching $2"
    curl -fL --progress-bar -o "$dest/$2.part" "$base/$1/$2" && mv "$dest/$2.part" "$dest/$2"
}

fetch_archive() { # tag name
    [[ -d "$dest/$2" ]] && return
    echo "fetching $2"
    curl -fL --progress-bar "$base/$1/$2.tar.bz2" | tar -xj -C "$dest"
}

tier=${1:-standard}
fetch_file asr-models silero_vad_v5.onnx
case $tier in
    light|all)
        fetch_archive asr-models sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8
        fetch_archive tts-models kitten-nano-en-v0_8-fp32
        fetch_file tts-models lexicon-en-us.txt ;;&
    standard|all)
        fetch_archive asr-models sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25
        fetch_archive tts-models kokoro-multi-lang-v1_0 ;;
    light) ;;
    *) echo "unknown tier: $tier" >&2; exit 1 ;;
esac
echo "models in $dest"
