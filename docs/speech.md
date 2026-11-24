# Speech

All speech models run in-process through [`sherpa-onnx`](https://docs.rs/sherpa-onnx)
(Silero VAD, STT, TTS): one CPU-friendly ONNX runtime for every tier. Its build
script downloads a prebuilt static library; packagers set `SHERPA_ONNX_LIB_DIR`
instead. All sherpa-onnx use stays in `crates/speech`.

## Pipeline

```
PipeWire capture (vivus-capture thread, 16 kHz mono, inactive until listening)
   └─► channel ─► vivus-speech thread: Listener = Silero VAD + SpeechRecognizer
                     └─► Event::{SpeechStarted, Partial, Final} ─► engine ─► D-Bus Transcript
```

- **Capture** (`crates/audio`): fixed F32 / 16 kHz / mono format so PipeWire's adapter resamples and downmixes. `[audio] input` targets a specific node (e.g. the echo-cancel source); otherwise the default source.
- **Listener** (`crates/speech/src/listener.rs`), two modes:
  - *Vad* (toggle-listen, conversation): VAD splits utterances. ~0.5 s of preroll is kept so the first syllable isn't lost.
  - *Hold* (push-to-talk): everything until release is one utterance, so pauses don't split it.
- **STT interface** (`SpeechRecognizer`): feed samples, get partials when the text changes, `finish()` for the final. A provider that only does whole utterances (Groq Whisper) just emits a final with no partials.
- **Two STT implementations:** native streaming (sherpa `OnlineRecognizer`) and re-decode (an offline model re-run on the growing utterance every 0.3 s of new audio).

## Tiers

| Tier | STT | TTS | Target machine |
|---|---|---|---|
| Light (default) | `parakeet-tdt-110m` (int8, ~130 MB): re-decoded, adds punctuation | Kitten TTS | Any laptop CPU |
| Standard | `nemotron-streaming-0.6b` (int8, 160 ms chunks, ~650 MB): native streaming | Kokoro (82M) | 4+ cores, no GPU |
| Remote | Groq Whisper (final only), Deepgram (streaming) | Cloud TTS | Anything with a network |

Model names map to sherpa-onnx release archives in `crates/speech/src/models.rs`;
`scripts/fetch-models.sh light|standard|all` downloads them.

### Benchmarks

Dev machine (Ryzen, 16 cores), `stt_bench`, 2 threads. "Final" is end of speech
to final text.

| Model | Compute / audio time | Final, 6.6 s utterance | Final, 16.7 s utterance |
|---|---|---|---|
| parakeet-tdt-110m | 0.20–0.48 (grows with length) | 110 ms | 290 ms |
| nemotron-streaming-0.6b | 0.47 (flat) | ~0 ms | ~0 ms |

Both transcribe the test clips correctly. Re-decoding costs more the longer you
speak, but commands are short; Nemotron's cost is flat but higher overall. No
silence padding is needed before finishing a streaming utterance: the VAD's
min-silence window has already been fed in. Still to do: benchmark a 4-core laptop.

```sh
cargo run --release -p vivus-speech --example stt_bench -- <model> file.wav [threads]
```

Other sherpa-onnx models worth evaluating: Moonshine (tiny/base, quantized),
Nemotron 3.5 streaming (multilingual), Parakeet TDT 0.6B v3 (more accurate, heavier).

## Turn detection

Today: VAD silence threshold (`[audio] end_of_turn_silence`, default 0.4 s),
the main end-of-turn latency knob. Next: a semantic end-of-turn check on the
partial text so mid-sentence pauses don't cut you off, which lets the threshold
shrink, and lets the router start speculatively before you've definitely finished.

## TTS

Not built yet. Plan: sherpa-onnx `OfflineTts` (Kitten for light, Kokoro for
standard), PipeWire playback in `crates/audio`, synthesized one sentence at a
time as the router streams `speak` text, so first audio starts after the first
sentence.

## Barge-in

Interrupting by talking needs VAD and STT running during playback, which needs
echo cancellation so the assistant doesn't hear itself: PipeWire's
`module-echo-cancel`, either documented or loaded automatically, with capture
targeting its source. Speech detected during playback cancels TTS and the LLM
stream (target ~200 ms). Local speech means always-on listening costs no
network round trips.

## Testing without a microphone

Create a virtual source, point capture at it, and play a WAV into it:

```sh
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=vivus_test media.class=Audio/Source/Virtual object.linger=true audio.position=[MONO] }'
# config: [audio] input = "vivus_test"
pw-cat --playback --target 0 -P '{ node.name=vivus-play node.autoconnect=false }' file.wav &
pw-link vivus-play:output_MONO vivus_test:input_MONO   # or output_FL
pw-cli destroy <vivus_test node id>                    # afterwards
```

Run the daemon under `dbus-run-session` to keep it off your real session bus.
`crates/speech/tests/listener.rs` covers the listener offline (skips without models).

## References

[Voice AI roundup (Aug 2026)](https://futureagi.com/blog/best-voice-ai-august-2026/);
[Parakeet vs Whisper vs Nemotron](https://openwhispr.com/blog/parakeet-vs-whisper-vs-nemotron);
[Voxtral Realtime](https://arxiv.org/pdf/2602.11298); Pipecat as a reference
for pipeline structure.
