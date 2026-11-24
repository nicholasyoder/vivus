#!/usr/bin/env bash
# Records a real-room scene for the echo-cancellation bench: what the speakers
# play (the sink's monitor) and the raw mic, in one stream so they stay in sync.
# Output is run.sh's scene format: FL/FR = speakers, FC = mic (L/R averaged, as
# vivus's capture does).
#   record-room.sh out.wav [mic node] [sink node]      Ctrl-C stops.
# Defaults are the default source and sink. Stop vivus first, so it neither
# ducks the music nor holds the mic.
set -eu
out=$1
name() { wpctl inspect "$1" | grep -oP 'node.name = "\K[^"]+'; }
mic=${2:-$(name @DEFAULT_AUDIO_SOURCE@)}
sink=${3:-$(name @DEFAULT_AUDIO_SINK@)}
raw=$(mktemp --suffix=.wav)
warmup=4

pw-record --rate 48000 --channels 4 --channel-map FL,FR,RL,RR \
    -P '{ node.name=vivus-room-rec node.autoconnect=false }' "$raw" &
rec=$!
# Background jobs of a script ignore SIGINT, so pass Ctrl-C on as TERM.
trap 'kill -TERM $rec 2>/dev/null || true' INT TERM
sleep 0.5
pw-link "$sink:monitor_FL" vivus-room-rec:input_FL
pw-link "$sink:monitor_FR" vivus-room-rec:input_FR
if pw-link -o | grep -qx "$mic:capture_MONO"; then
    pw-link "$mic:capture_MONO" vivus-room-rec:input_RL
    pw-link "$mic:capture_MONO" vivus-room-rec:input_RR
else
    pw-link "$mic:capture_FL" vivus-room-rec:input_RL
    pw-link "$mic:capture_FR" vivus-room-rec:input_RR
fi
# A suspended onboard input can open with a full-scale DC thump that takes
# seconds to settle; wait it out and trim it.
echo "recording $sink + $mic; mic settling..."
sleep $warmup
echo "go: start the music and talk (Ctrl-C to stop)"
wait $rec || true
wait $rec 2>/dev/null || true
ffmpeg -loglevel error -y -ss $((warmup + 1)) -i "$raw" -af "pan=3.0|c0=c0|c1=c1|c2=0.5*c2+0.5*c3" -c:a pcm_s16le "$out"
rm -f "$raw"
echo "wrote $out"
