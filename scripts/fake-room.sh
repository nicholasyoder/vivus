#!/usr/bin/env bash
# A room without speakers or a mic, for testing echo cancellation and barge-in:
# whatever plays to the sink `vivus_spk` leaks into the source `vivus_mic`, like
# speakers into a mic. Runs until Ctrl-C, then removes the nodes.
#   scripts/fake-room.sh [echo_volume]     (default 0.5, about 20 dB below speech)
# Config: [audio] input = "vivus_mic", output = "vivus_spk". Then, for example:
#   pw-cat --playback --target vivus_spk music.wav                # "other audio"
#   pw-cat --playback --target 0 -P '{ node.name=near node.autoconnect=false }' you.wav &
#   pw-link near:output_FL vivus_mic:input_FL                    # "you" talking
set -eu
volume=${1:-0.5}

node_id() { pw-cli ls Node | awk -v n="\"$1\"" '/^\s*id /{id=$2} $0 ~ "node.name = "n {print id}' | tr -d ,; }
cleanup() {
    kill $(jobs -p) 2>/dev/null || true
    for n in vivus_spk vivus_mic; do
        id=$(node_id $n); [ -n "$id" ] && pw-cli destroy "$id" >/dev/null
    done
}
trap cleanup EXIT

pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=vivus_spk media.class=Audio/Sink object.linger=true audio.position=[FL FR] }' >/dev/null
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=vivus_mic media.class=Audio/Source/Virtual object.linger=true audio.position=[FL FR] }' >/dev/null
# autoconnect=false: otherwise WirePlumber sends the echo to the real speakers.
pw-loopback -n vivus-room-echo -d 0.01 -C vivus_spk \
    -i '{ stream.capture.sink=true node.name=vivus-room-echo-in }' \
    -o '{ node.name=vivus-room-echo-out node.autoconnect=false }' &
sleep 0.7
pw-link vivus-room-echo-out:output_FL vivus_mic:input_FL
pw-link vivus-room-echo-out:output_FR vivus_mic:input_FR
wpctl set-volume "$(node_id vivus-room-echo-out)" "$volume"
echo "fake room up: vivus_spk -> vivus_mic at volume $volume (Ctrl-C to remove)"
wait
