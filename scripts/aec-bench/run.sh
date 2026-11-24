#!/usr/bin/env bash
# Echo-cancellation bench (docs/speech.md#echo-cancellation-quality).
#   run.sh scene.wav out.wav "<extra module args>|none" [reference position]
# Plays FL/FR of a scene.py scene into a virtual speaker and FC into a virtual
# mic, runs module-echo-cancel on them as vivus does (monitor mode), and records
# its output at 16 kHz mono. "none" records the mic without cancellation.
# Example (vivus's settings):
#   run.sh scene.wav out.wav "aec.args = { webrtc.noise_suppression = false }" "FL FR"
#   cargo run --release -p vivus-speech --example listen_file -- parakeet-tdt-0.6b out.wav > hyp.txt
#   python3 wer.py truth.txt hyp.txt
set -u
scene=$1 out=$2 extra=$3 refpos=${4:-MONO}
node_id() { pw-cli ls Node | awk -v n="\"$1\"" '/^\s*id /{id=$2} $0 ~ "node.name = "n {print id}' | tr -d ,; }
cleanup() {
    kill $(jobs -p) 2>/dev/null; sleep 0.3
    for n in aecb_spk aecb_mic; do id=$(node_id $n); [ -n "$id" ] && pw-cli destroy "$id" >/dev/null; done
}
trap cleanup EXIT
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=aecb_spk media.class=Audio/Sink object.linger=true audio.position=[FL FR] priority.session=0 priority.driver=0 }' >/dev/null
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=aecb_mic media.class=Audio/Source/Virtual object.linger=true audio.position=[MONO] priority.session=0 priority.driver=0 }' >/dev/null
if [ "$extra" != "none" ]; then
  pw-cli -m load-module libpipewire-module-echo-cancel "{ monitor.mode = true audio.channels = 1 audio.position = [ MONO ] $extra
    capture.props = { node.name = aecb-capture target.object = aecb_mic }
    source.props = { node.name = aecb-source priority.session = 0 }
    sink.props = { node.name = aecb-ref target.object = aecb_spk audio.position = [ $refpos ] } }" >/dev/null 2>&1 &
  src=aecb-source
else
  src=aecb_mic
fi
sleep 1
pw-record --target $src --rate 16000 --channels 1 -P '{ node.name=aecb-rec node.autoconnect=true }' "$out" &
sleep 0.5
pw-cat --playback --channels 3 --channel-map FL,FR,FC -P '{ node.name=aecb-play node.autoconnect=false }' "$scene" &
play=$!
sleep 0.3
pw-link aecb-play:output_FL aecb_spk:playback_FL
pw-link aecb-play:output_FR aecb_spk:playback_FR
pw-link aecb-play:output_FC aecb_mic:input_MONO
wait $play
sleep 1
