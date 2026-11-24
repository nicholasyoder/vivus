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

- **Capture** (`crates/audio`): fixed F32 / 16 kHz / mono format so PipeWire's adapter resamples and downmixes. `[audio] input` targets a specific node; otherwise the default source. With `[audio] echo_cancel` (default on), capture goes through the echo canceller ([Barge-in](#barge-in)).
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
  We phonemize ourselves ([Out-of-lexicon words](#out-of-lexicon-words), layer
  2) and hand sherpa `phoneme_codepoints`, so sherpa loads no lexicon. Kokoro's
  archive ships `lexicon-us-en.txt`/`lexicon-gb-en.txt`; we load both, the
  voice's own first, because the US one lacks many plurals ("minutes",
  "degrees", "dogs"). Kitten's archive has none; it uses the shared espeak-IPA
  `lexicon-en-us.txt` from the `tts-models` release, stored in the models dir.
  Note that sherpa's own loader lets the *last* comma-separated file win, so
  before this, US voices got the British entry for every word both lexicons
  have. Model load is ~360 ms (Kitten) and ~820 ms (Kokoro), half what it was
  when sherpa parsed the lexicons too.
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
  playback. State is `speaking` until `SpeakingFinished`. With echo cancellation the
  mic stays open while speaking ([Barge-in](#barge-in)); without it, it's
  paused while speaking in conversation mode. Starting to listen or `Cancel`
  stops speech.

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
   (`should've` → "should have"; `'s` is left to the phonemizer's suffix
   rules), camelCase and two-word
   compounds split (`GitHub` → "Git Hub", `Firefox` → "Fire fox"), metric
   prefixes split (`kilometers` → "kilo meters"). Whatever is still unknown is
   logged at info (`tts: no pronunciation, skipped: …`); `tts_bench` prints it
   per sentence.
   On a ~25-sentence sweep of typical replies, what's left is names
   (`Hyprland`, `Kubernetes`, `nginx`, `Spotify`, `LibreOffice`) and, for
   Kokoro, inflected forms neither of its lexicons has (`opens`,
   `notifications`, `compositors`): the phonemizer needs `-s`/`-ed`/`-ing`
   suffix rules.
2. **Own phonemizer** (done 2026-11-24, `crates/speech/src/lexicon.rs`,
   `arpabet.rs`). `phoneme_codepoints` replaces the whole sentence's input, so
   per-word fixes mean phonemizing ourselves. `Lexicon::phonemize` tokenizes
   like sherpa's lexicon frontend (one space between words, `,.;:!?—…` and
   brackets/quotes kept as tokens), tries multi-word keys first (the espeak
   lexicon has homograph phrases like "the record"), then the word, then
   hyphen parts. Unknown words are reported in `missing` *before* synthesis.
   Codepoints the model's `tokens.txt` lacks are filtered out.
   - **Sources**, first one wins: user pronunciations
     (`~/.config/vivus/pronunciations.txt`), learned ones
     (`~/.local/share/vivus/pronunciations.txt`, for the LLM lookup), the
     model's lexicon files, then `crates/speech/data/pronunciations.txt`
     (built-in gap fillers: "or", "her" for Kitten). Pronunciation files are
     CMUdict-format ARPAbet (`HYPRLAND  HH AY1 P ER0 L AE2 N D`, `#`
     comments); bad lines are logged and skipped.
   - **Suffix rules** for a word with no entry: `-s`/`-es`/`-ies`, `'s`,
     `-ed`/`-ied` (incl. "used" → use, "stopped" → stop), `-ing` ("making" →
     make, "being" → be, "running" → run). The ending follows the base's last
     phoneme (s/z/ᵻz, t/d/ᵻd; GB `ɪ`), and American sets flap a final t
     ("getting" ɡˈɛɾɪŋ). Words ending in `ss`/`us` don't take `-s`.
   - **Articles**: "the" is ði before a vowel, ðə otherwise; "a" is ɐ, except
     a capital A next to a single letter or at the end ("A P I", "plan A").
     Every lexicon has "a" as the letter name, and Kokoro's US "the" is always ði.
   - **ARPAbet tables** (`arpabet.rs`) to espeak IPA, misaki US and misaki GB
     (misaki writes diphthongs as letters: `A` eɪ, `I` aɪ, `O` oʊ, `Q` əʊ, `W`
     aʊ, `Y` ɔɪ). Stress goes right before the vowel, as in both lexicons.
     Rules that measurably helped: US flap (`T` after a vowel or `R`, before an
     unstressed vowel), GB non-rhotic `R` and `ɒ` for `AA`, espeak `ɐ` for a
     word-initial `AH0`, `oː` before `R` and `ɚɹ` before a vowel, `ᵻ`/`ɪ` in
     final `-ted`/`-ded`, and secondary stress marked only for misaki US.
     Checked against CMUdict's first pronunciation of every word it shares with
     each lexicon:

     | Set | Shared words | Exact | Same but stress | Char similarity |
     |---|---|---|---|---|
     | espeak (Kitten) | 117k | 45% | 6% | 88% |
     | misaki US | 49k | 62% | 7% | 94% |
     | misaki GB | 50k | 49% | 5% | 92% |

     Most of the rest are real dictionary disagreements (names, reduced vowels
     `ə`/`ɪ`/`ᵻ`/`ᵊ`, which syllable gets secondary stress), not table errors.
     A syllabic `ᵊn`/`ᵊl` rule for misaki made it worse.
   - Checked by ear via STT: three sentences with contractions, a possessive,
     plurals, `-ed` and `-ing` forms round-trip through parakeet word for word
     with Kitten, Kokoro US and GB. User entries for "Hyprland", "Kubernetes",
     "nginx" come back as "Hyperland", "Kubernetes", "Engine X".
3. **LLM lookup** (done 2026-11-24, `crates/daemon/src/pronounce.rs`). The
   speaker splits the reply into sentences as it streams (on the engine side,
   not the TTS thread), and each complete sentence's missing words go to the
   router's model at once, one request per sentence, concurrently. The TTS
   thread waits for a sentence's pending words up to 3 s, so the first sentence
   pays for the lookup and later ones mostly wait during playback. The prompt
   asks for CMUdict ARPAbet as a JSON object; answers are validated by the
   ARPAbet parser, added to the shared lexicon and appended to the learned file
   (uppercase, CMUdict style). Possessives are looked up by their base, hyphen
   parts separately, words with digits not at all; a word is tried once per
   session. `reasoning_effort = "low"` keeps gpt-oss from spending its 1000
   tokens thinking (it hit the limit otherwise); a provider that rejects it with
   a 400 is retried without, and not sent it again.
   - **Prefetch** (background, 25 words per request, 10 s apart): `Name=` of
     installed `.desktop` files at startup, and each final user transcript.
     A reply needing a prefetched word promotes it. Window titles come with
     window listing.
   - **Measured** (Groq `openai/gpt-oss-120b`): 3 words in 0.5–0.6 s, 7 in
     ~2 s, 25 app names in 1.7 s. In a daemon run, "Hyprland and Zellij run fine
     with Ghostty." waited 580 ms before synthesis; through parakeet it comes
     back as "Hyperland and Zelie run fine with Ghosty" (Kokoro and Kitten).
     The model is good on well-known names and weak on compounds it can't
     place (`LibreOffice` → `L IH1 B R IY0 OW0 F IH2 S`); a user entry fixes
     those.
4. **Local neural G2P** (small CMUdict-trained ARPAbet model) when offline or
   past the deadline; a later LLM answer replaces its guess in the cache.

The reply model could also annotate unusual names inline (misaki has a syntax
for it), but it can't know what our lexicon lacks; an option, not the mechanism.

Testing without speakers: create a null sink, set `[audio] output` to it, and
record its monitor with `pw-record --target <sink> -P '{ stream.capture.sink=true }'`
(then transcribe the recording with `stt_bench`).

## Barge-in

Interrupting by talking needs VAD and STT running during playback, which needs
echo cancellation so the assistant doesn't hear itself (done 2026-11-24).

- **Echo cancellation** (`crates/audio/src/capture.rs`): the capture thread
  loads `libpipewire-module-echo-cancel` (WebRTC AEC) into its own PipeWire
  context, so it needs no setup and disappears with the daemon. It runs in
  **monitor mode**: the reference is the monitor of the output sink
  (`[audio] output`, else the default sink) rather than a sink apps must play
  into. So it cancels *everything* the speakers play (music, videos, calls), not
  just our TTS, and nothing gets rerouted. Mono, to halve the cost. Capture
  targets `vivus-echo-cancel-source`. While our stream is inactive the module's
  nodes suspend and the mic is closed. If the module fails to load, the daemon
  falls back to pausing the mic while speaking. `[audio] echo_cancel = false`
  turns it off, e.g. when `input` is already an echo-cancelled source.
- **Two-stage detection** (`Listener`): a second Silero instance with
  `min_speech` 64 ms emits `SpeechPossible`, which **pauses** playback (the
  queue is held). The normal VAD (250 ms) confirming it as `SpeechStarted`
  cancels the reply: TTS, the LLM stream, and the thinking phase too. If the
  burst ends first (200 ms of silence) it emits `NotSpeech` and playback
  resumes, so a cough or a door costs a short hiccup, not the reply. Only in
  conversation (VAD) mode; in push-to-talk, pressing the key interrupts.
- **Measured** with `scripts/fake-room.sh` (echo leaks from a virtual sink into
  a virtual mic ~20 dB below the voice; no real acoustics), Kokoro/Kitten
  speech as "you":
  - Echo alone, TTS or synthetic music: −60 to −80 dB after cancellation; no
    `SpeechPossible` during a reply with music playing.
  - Talking over TTS or music: transcribed correctly except the first word,
    which the canceller damages while both talk ("Stop" → "Snob", "Rob").
    With the echo as loud as the voice it fails: speech over music is
    suppressed entirely.
  - Speech onset to `SpeechPossible`, offline on three clips: 140–280 ms
    (100 ms more with a 100 ms `min_speech`; a lower threshold doesn't help:
    Silero needs ~100 ms of a word first). End to end, onset to silent
    speakers: 330 ms with a 100 ms `min_speech` (480 ms with no pause stage).
    PipeWire and the canceller add little.
  - A 200 ms speech fragment paused playback for 380 ms, then it resumed.
- Not yet tried in a real room (speaker distortion and reverb make cancellation
  worse than this loopback) or with real music with vocals.

Local speech means always-on listening costs no network round trips.

## Testing without a microphone

Create a virtual source, point capture at it, and play a WAV into it:

```sh
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=vivus_test media.class=Audio/Source/Virtual object.linger=true audio.position=[MONO] }'
# config: [audio] input = "vivus_test"
pw-cat --playback --target 0 -P '{ node.name=vivus-play node.autoconnect=false }' file.wav &
pw-link vivus-play:output_MONO vivus_test:input_MONO   # or output_FL
pw-cli destroy <vivus_test node id>                    # afterwards
```

For echo and barge-in, `scripts/fake-room.sh` adds a sink `vivus_spk` whose
output leaks into a source `vivus_mic` (usage in the script).

Run the daemon under `dbus-run-session` to keep it off your real session bus.
`crates/speech/tests/listener.rs` covers the listener offline (skips without models).

## References

[Voice AI roundup (Aug 2026)](https://futureagi.com/blog/best-voice-ai-august-2026/);
[Parakeet vs Whisper vs Nemotron](https://openwhispr.com/blog/parakeet-vs-whisper-vs-nemotron);
[Voxtral Realtime](https://arxiv.org/pdf/2602.11298); Pipecat as a reference
for pipeline structure.
