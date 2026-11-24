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

- **Capture** (`crates/audio`): fixed F32 / 16 kHz / mono format so PipeWire's adapter resamples and downmixes. Both streams declare the `MONO` channel position: without it WirePlumber linked the one port to the first channel only (left mic, left speaker). `[audio] input` targets a specific node; otherwise the default source. With `[audio] echo_cancel` (default on), capture goes through the echo canceller ([Barge-in](#barge-in)).
- **Listener** (`crates/speech/src/listener.rs`), two modes:
  - *Vad* (toggle-listen, conversation): VAD splits utterances. ~0.5 s of preroll is kept so the first syllable isn't lost. A burst too short for the VAD is transcribed and kept if it's a word ([Barge-in](#barge-in)).
  - *Hold* (push-to-talk): everything until release is one utterance, so pauses don't split it.
- **STT interface** (`SpeechRecognizer`): feed samples, get partials when the text changes, `finish()` for the final. A provider that only does whole utterances (Groq Whisper) just emits a final with no partials.
- **Two STT implementations:** native streaming (sherpa `OnlineRecognizer`) and re-decode (an offline model re-run on the growing utterance every 0.3 s of new audio, or less often when decodes get slow).

## Tiers

| Tier | STT | TTS | Target machine |
|---|---|---|---|
| Light (default) | `parakeet-tdt-110m` (int8, ~130 MB): re-decoded, adds punctuation | `kitten-nano` (Kitten v0.8 nano, fp32, ~75 MB) | Any laptop CPU |
| Standard | `parakeet-tdt-0.6b` (TDT 0.6B v2, int8, ~660 MB): re-decoded, adds punctuation | `kokoro` (Kokoro v1.0, fp32, ~380 MB) | 4+ cores, no GPU |
| Remote | Groq Whisper (final only), Deepgram (streaming) | Cloud TTS | Anything with a network |

Also in the catalog: `nemotron-streaming-0.6b` (int8, 160 ms chunks, ~650 MB),
native streaming. It was the standard tier until it proved worse in real use:
no punctuation, and words at the edges still went missing after the padding
fix below. Model names map to sherpa-onnx release archives in
`crates/speech/src/models.rs`; `scripts/fetch-models.sh light|standard|all`
downloads them (Nemotron only with `all`).

### Benchmarks

Dev machine (Ryzen, 16 cores), `stt_bench`. "Final" is end of speech to final
text.

| Model | Threads | Compute / audio time | Final, 6.6 s utterance | Final, 16.7 s utterance |
|---|---|---|---|---|
| parakeet-tdt-110m | 2 | 0.20–0.37 | 110 ms | 310 ms |
| parakeet-tdt-0.6b | 2 | 0.45–0.50 | 410 ms | 1010 ms |
| parakeet-tdt-0.6b | 4 | 0.40–0.46 | 270 ms | 670 ms |
| nemotron-streaming-0.6b | 2 | 0.48 (flat) | ~150 ms | ~150 ms |

All transcribe the test clips correctly. A re-decode model's final costs one
full decode, ~60 ms per second of speech for the 0.6B at 2 threads, so short
commands land around 150–250 ms. Partials wait at least twice the last decode's
time, which caps partial decoding near half real time; without that the 0.6B
fell behind live audio (1.7×) on the long clip. Still to do: benchmark a 4-core laptop.

The streaming model needs silence around each utterance or it clips words at
the edges, worst in push-to-talk, where speech runs right up to key release.
Every stream starts with 0.1 s of zeros (without them a word spoken at once,
such as "Yes", is lost) and ends with 0.32 s (two 160 ms chunks, so the last
word clears the encoder's lookahead). With less tail, "...call Sarah tomorrow"
came out as "...call Sarah to". The tail is the ~150 ms "Final" above.

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
# full listener (VAD + STT), any sample rate; --hold mimics push-to-talk
cargo run --release -p vivus-speech --example listen_file -- [--hold] [--pad secs] <model> file.wav...
```

Other sherpa-onnx models worth evaluating: Moonshine (tiny/base, quantized),
Parakeet TDT 0.6B v3 (multilingual) and the newer `-ultra`/`-redux` 0.6B archives
(undocumented so far).

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
- **Loudness**: each sentence is scaled so its voiced part (20 ms frames
  within 25 dB of the loudest) has an RMS of -14 dBFS, about -16 LUFS, with a
  soft limiter above 0.7. Raw output was -25 LUFS (Kokoro) and -20 (Kitten),
  drowned out by music (usually -8 to -14). Kokoro gains ~9 dB, Kitten ~3;
  both still round-trip through parakeet word for word. The stream's volume in
  a mixer (WirePlumber remembers it) adjusts it further.
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
  PipeWire's `drained`, `stop` drops everything at once; `pause`/`resume`
  hold the queue (barge-in). `[audio] output` targets a node.
- **Ducking** (`crates/audio/src/duck.rs`): while we're actually speaking
  (playback has audio; 1 s of silence, e.g. waiting on an escalation after
  the filler, brings them back up until speech resumes), every
  other app's output stream (also while the user talks: from `SpeechPossible`
  or the push-to-talk key to the utterance's end, which helps echo
  cancellation) (not our `vivus-*` nodes; registry globals carry no
  process id to tell them apart) is turned down to `[audio] duck_volume` (default
  0.3, about -10 dB; 1 turns it off). It scales the streams' `softVolumes`,
  not `channelVolumes`, which mixers show and WirePlumber saves per app, so a
  user's volumes are never overwritten; changing an app's volume while ducked
  is picked up and re-ducked. Soft and channel volumes don't always match (a
  `pw-loopback` stream had channel 0.125, soft 1.0), so it scales whatever
  soft volume the stream had. `Quit` stops speech first, so ducked streams are
  restored while everything is connected. If the daemon is killed mid-reply,
  other streams stay ducked until their volume is changed or they restart.
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
- **Short words** (2026-11-24): a one-word interrupt like "wait" can stay under
  the main VAD's 250 ms, more so with its onset damaged by the canceller, and
  was dismissed as `NotSpeech`. Now the burst (with preroll) is transcribed
  first; if it holds a real word (not empty, a stray letter or a filler like
  "um") it becomes `SpeechStarted` + `Final`, so it interrupts and is sent on
  as the user's utterance. Same outside replies: a lone "yes" of 150–250 ms
  used to produce nothing. Costs one decode of under a second of audio.
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
- Tried in a real room with music playing (2026-11-24): cancellation,
  interrupting and ducking work; `min_speech_possible` kept at 64 ms.

### Echo cancellation quality

Real use (2026-11-24, [recorded](#recording-utterances)): without the canceller
transcripts were near perfect; with it they were garbled, and with music on it
barely worked. The recordings sounded garbled with words missing.

Bench (`scripts/aec-bench/`): 13 of those clean recordings (103 words) mixed
with synthetic stereo music (drums, bass, pad, TTS "vocals", instruments panned)
through two different simulated speaker-to-mic paths, played through the real
module (monitor mode, virtual nodes), output transcribed by `parakeet-tdt-0.6b`.
Word error rate, music at about the voice's level (−24 dBFS RMS at the mic):

| Setup | WER, 2.5 s music lead-in | WER, 10 s lead-in | Music 10 dB down (ducked) |
|---|---|---|---|
| No canceller (music vocals get transcribed) | 41% | 39% | 16% |
| Mono reference, noise suppression on (was the default) | 95% | | 70% |
| Mono reference, noise suppression off | 84% | | |
| Stereo reference, noise suppression on | 56% | | |
| Stereo reference, noise suppression off (now) | 46% | 60% | 21% |

- **Stereo reference.** A mono downmix can't model two speakers' different
  paths, so WebRTC AEC3's suppressor mutes nearly everything while music plays:
  echo-only output was −87 dB with mono, −44 dB with stereo (input −24 dB),
  and the voice goes with it. Mic and output stay mono.
- **No noise suppression.** PipeWire enables WebRTC's at its "high" level by
  default; it smears consonants, and Parakeet copes with room noise anyway.
  The high-pass filter makes no difference (identical output).
- **Ducking while the user talks.** Other apps duck on `SpeechPossible` (or the
  push-to-talk key) until the utterance ends, not only while we speak.
- **Remaining limits.** Even with no echo at all, music in the reference takes
  the WER from 3% to 11%: the suppressor trims speech it can't rule out as
  echo, and PipeWire exposes no suppressor tuning. Which words overlap the
  loudest music matters a lot (46% vs 60% for the same setup, music shifted).
  With music playing the raw mic transcribes better than any canceller output,
  but STT stays on the cancelled signal: without it, podcasts, videos and
  sung lyrics are transcribed as the user. The canceller restarts unconverged whenever
  capture starts (its nodes suspend while we're not listening), so the first
  seconds of push-to-talk over music are worst.
- **Mic gain.** The real recordings peaked at 0 dBFS with up to 4% of samples
  clipped (ALC897 capture at +30 dB). Clipping makes the echo path nonlinear,
  which a linear canceller can't follow; aim for peaks around −6 dBFS.
- **Wake-up thump.** The dev machine's onboard input (ALC897) opens with a DC
  thump: full scale for ~1.5 s, settled after ~4 s. PipeWire suspends idle
  nodes after 5 s and we only capture while listening, so speech right after
  listening starts (push-to-talk especially) is clipped. Workaround: a
  WirePlumber rule with `session.suspend-timeout-seconds = 0` for that source.
- **Real-room scenes.** `scripts/aec-bench/record-room.sh` records the sink's
  monitor and the raw mic in one stream (run.sh's scene format) while you read
  `prompts.txt` over music, so settings can be compared on the real room
  instead of the simulated one. The reference channels are the monitor, taken
  before the sink volume, so they don't change with it; only the mic does.
  Results (2026-11-24, dev machine, after lowering the mic gain; `wer.py`
  compares numbers as words; music in the mic between sentences, median):

  | Take | Music in mic | No canceller | Old (mono ref, NS on) | New (stereo ref, NS off) |
  |---|---|---|---|---|
  | quieter | −38.5 dB | 2.6% | 14.6% | 2.6% |
  | normal | −33.7 dB | 2.0% | 25.2% | 3.3% |
  | louder | −27.1 dB | 1.3% | 76.2% | 4.0% |

  The real room is far kinder than the simulated one: the new settings cost
  at most a few words in 151 against no canceller. Still unmeasured on real audio: talking over our own TTS.

Local speech means always-on listening costs no network round trips.

## Recording utterances

`[audio] record_utterances = true` makes the listener save exactly what it fed
the recognizer for each utterance (after echo cancellation, preroll included;
also one-word bursts it kept) to `~/.local/share/vivus/recordings/<UTC
time>.wav`, 16 kHz mono, and append `file, model, transcript` to
`transcripts.tsv` there. `[debug] transcribe_only = true` skips the reply (no
LLM call, no speech), so test phrases can follow each other without waiting.
Use them to hear what the mic chain delivers and to re-run other models on
real speech:

```sh
cd ~/.local/share/vivus/recordings
cargo run --release -p vivus-speech --example listen_file -- --hold --pad 0 <model> *.wav
```

In real use (2026-11-24) the transcripts were noticeably worse than on clean
clips, and better with `echo_cancel = false`; the canceller's WebRTC noise
suppression is a suspect.

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
