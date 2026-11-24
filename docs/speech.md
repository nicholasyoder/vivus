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

engine reply text ─► vivus-tts thread: SentenceSplitter ─► Synthesizer (one sentence at a time)
   └─► PipeWire playback (vivus-playback thread, 24 kHz mono) ─► SpeakingFinished ─► engine
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
| Light (default) | `parakeet-tdt-110m` (int8, ~130 MB): re-decoded, adds punctuation | `kitten-nano` (Kitten v0.8 nano, fp32, ~75 MB) | Any laptop CPU |
| Standard | `nemotron-streaming-0.6b` (int8, 160 ms chunks, ~650 MB): native streaming | `kokoro` (Kokoro v1.0, fp32, ~380 MB) | 4+ cores, no GPU |
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

TTS, same machine, `tts_bench`, 2 threads, after warm-up. "First sentence" is
"It's twenty past three in the afternoon." (time to first audio).

| Model | Compute / audio time | First sentence | Warm-up (first call) |
|---|---|---|---|
| kitten-nano fp32 | 0.09 | 330 ms | 75 ms |
| kitten-nano int8 | 0.20 | 790 ms | 175 ms |
| kokoro fp32 | 0.40 | 780 ms | 400 ms |
| kokoro int8 | 0.81 | 1570 ms | 790 ms |

int8 is about twice as slow as fp32 on this x86 CPU, so the catalog uses fp32.
More threads help little (Kokoro fp32 at 4 threads: ~0.35). Kitten speaks at a
noticeably slower pace than Kokoro (12.3 s vs 6.6 s for the same text). Output
of both round-trips through parakeet word for word. Kokoro's first sentence is
too slow for the ~1 s target on a weak CPU, which is why it's the standard tier.

```sh
cargo run --release -p vivus-speech --example stt_bench -- <model> file.wav [threads]
cargo run --release -p vivus-speech --example tts_bench -- <model> [voice|-] [threads] [out.wav] [text]
```

Other sherpa-onnx models worth evaluating: Moonshine (tiny/base, quantized),
Nemotron 3.5 streaming (multilingual), Parakeet TDT 0.6B v3 (more accurate, heavier).

## Turn detection

Today: VAD silence threshold (`[audio] end_of_turn_silence`, default 0.4 s),
the main end-of-turn latency knob. Next: a semantic end-of-turn check on the
partial text so mid-sentence pauses don't cut you off, which lets the threshold
shrink, and lets the router start speculatively before you've definitely finished.

## TTS

- **Synthesizer** (`crates/speech/src/tts.rs`): sherpa-onnx `OfflineTts`, one
  sentence per call. Models and voice tables are in `models.rs`; `[roles.tts]
  voice` picks the speaker (Kokoro: `af_heart`, `bm_george`, …, English voices
  only; Kitten: `bella`, `jasper`, `luna`, `bruno`, `rosie`, `hugo`, `kiki`,
  `leo`). Unset `tts` means `kitten-nano`. The daemon synthesizes a short
  warm-up phrase at load because the first inference is several times slower.
- **No espeak-ng**: sherpa-onnx 2.x dropped it (GPL), so models need a lexicon.
  Kokoro's archive ships `lexicon-us-en.txt`/`lexicon-gb-en.txt`; we pass both,
  the voice's own first (sherpa takes a comma-separated list, first entry wins),
  because the US one lacks many plurals ("minutes", "degrees", "dogs"). Kitten's
  archive has none; it uses the shared espeak-IPA `lexicon-en-us.txt` from the
  `tts-models` release, stored in the models dir, after our small
  `crates/speech/data/extra-kitten.txt` (it lacks "or" and "her"!), which is
  copied beside it at load. Loading costs ~30 ms (Kitten) and ~500 ms (Kokoro).
  **Out-of-lexicon words are dropped silently** (sherpa logs "OOV word
  skipped"): "Open Hyprland and zorbleflax the Kubernetes pods." is spoken as
  "Open and the pods." with both models. See
  [Out-of-lexicon words](#out-of-lexicon-words).
- **Sentence splitting** (`SentenceSplitter`): streamed deltas in, complete
  sentences out. Splits on `.!?…` followed by whitespace, and on newlines; skips
  abbreviations and initials; past 120 bytes it splits at a clause boundary so a
  long first sentence doesn't delay audio.
- **Playback** (`crates/audio/src/playback.rs`): F32 mono at the model rate, inactive
  except while playing. `play` queues, `end` reports `on_finished` after
  PipeWire's `drained`, `stop` drops everything at once. `[audio] output` targets a node.
- **Daemon** (`crates/daemon/src/speaker.rs`): own `vivus-tts` thread so STT keeps
  running during synthesis (needed for barge-in). Each reply has a generation;
  `stop()` bumps it under a lock, so audio from a cancelled reply never reaches
  playback. State is `speaking` until `SpeakingFinished`. Until echo cancellation
  lands, the mic is paused while speaking in conversation mode; starting to
  listen or `Cancel` stops speech.

### Out-of-lexicon words

Sherpa's lexicon frontend drops every word it can't find, and does no text
normalization. Tested 2026-11-23 on "It is 3:45 PM on Nov 23, 2026. Your GPU
uses 12.5 GB, about 40% of the RAM. Dr. Smith emailed you about the API at
vivus.org.": both models drop every number, `40%` and `GB`; Kitten also drops
`GPU` and `vivus`, Kokoro `API` and `emailed`. It also drops words with quotes
or brackets attached (`"Hello`, `(world)`), `foo/bar`, and a possessive or
contraction it lacks (`Smith's`; Kokoro lacks `that's`, `what's`, `here's`).
Plan, in layers:

1. **Normalization** (done 2026-11-24, `crates/speech/src/normalize.rs`): text
   → text before sherpa, inside `Synthesizer::synthesize`, so the router can
   write digits and symbols. Regex passes, in order: emails and URLs (host only,
   `vivus dot org`), file names; symbol cleanup (quotes, brackets, markdown,
   emoji, `&` `+` `=`); currency (`$5.99`, `€1.4K`, `£3.50`); dates (ISO,
   `11/24/2026` US order, `Nov 23, 2026`, `23rd March`, weekday abbreviations);
   times (`3:45 PM`, `7am`, `15:30`); decades and years after `in`/`since`/…;
   signs, ranges, `%`, `°C`, units (`km/h` → "kilometers an hour"), ordinals,
   simple fractions; version numbers; every other number (long or
   zero-padded ones digit by digit); abbreviations (`Dr.`, `vs.`, `e.g.`,
   `etc.`). Then two lexicon-aware passes: all-caps words of 2–6 letters are
   spelled as letters unless the lexicon knows them (`US`, `IT`, `OS`, … are
   always spelled), and unknown words are respelled: contractions expanded
   (`that's` → "that is", `should've` → "should have"), possessives of known
   words lose the `'s` (`NVIDIA's` → "NVIDIA"), camelCase and two-word
   compounds split (`GitHub` → "Git Hub", `Firefox` → "Fire fox"), metric
   prefixes split (`kilometers` → "kilo meters"). Whatever is still unknown is
   logged at info (`tts: not in lexicon, skipped: …`); `tts_bench` prints it per
   sentence. Our check matches sherpa's own OOV log on the test sentences.
   On a ~25-sentence sweep of typical replies, what's left is names
   (`Hyprland`, `Kubernetes`, `nginx`, `Spotify`, `LibreOffice`) and, for
   Kokoro, inflected forms neither of its lexicons has (`opens`,
   `notifications`, `compositors`): the phonemizer needs `-s`/`-ed`/`-ing`
   suffix rules.
2. **Own phonemizer.** `phoneme_codepoints` replaces the whole sentence's input,
   so per-word fixes mean phonemizing ourselves: load the lexicon in Rust, look
   up each word, emit codepoints (punctuation and pauses included). This also
   detects OOV words *before* synthesis, so nothing is dropped and re-said.
   Pronunciations are stored as ARPAbet with stress (`HYPRLAND HH AY1 P ER0 L AE2
   N D`): LLMs produce it reliably, it validates against a small symbol set, and
   the ARPAbet → espeak-IPA (Kitten) and → misaki (Kokoro) tables can be learned
   from words both lexicons share. One file of learned entries plus user
   entries, which win.
3. **LLM lookup.** Scan the reply as it streams and request pronunciations for
   OOV words immediately (fast model, batched), so they're usually back before
   their sentence is synthesized; later sentences have the previous sentence's
   playback as slack. A first sentence may wait a few seconds for a lookup
   rather than speak a word wrong. Results are cached forever, and at idle we
   prefetch app names (`.desktop`), window titles and words from transcripts.
   LLMs know real-world pronunciations ("nginx", "Kubernetes") that spelling
   rules miss.
4. **Local neural G2P** (small CMUdict-trained ARPAbet model) when offline or
   past the deadline; a later LLM answer replaces its guess in the cache.

The reply model could also annotate unusual names inline (misaki has a syntax
for it), but it can't know what our lexicon lacks; an option, not the mechanism.

Testing without speakers: create a null sink, set `[audio] output` to it, and
record its monitor with `pw-record --target <sink> -P '{ stream.capture.sink=true }'`
(then transcribe the recording with `stt_bench`).

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
