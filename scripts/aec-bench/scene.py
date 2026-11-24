"""Builds a 3-channel 48 kHz echo-cancellation test scene: FL/FR are music for
a virtual speaker, FC is what the mic hears (your clips + that music through two
different simulated room paths, so a mono reference can't model it).
Clips come from [audio] record_utterances; their transcripts.tsv lines are the
reference text, so pick clips it got right.

Usage: scene.py [--echo-db -24] [--voice-db -8] [--lead 10] [--no-voice]
                out.wav truth.txt vocals.wav transcripts.tsv clip.wav...
vocals.wav is any speech (e.g. from tts_bench), mixed into the music as vocals.
Needs numpy."""
import argparse, os, sys, wave
import numpy as np
ap = argparse.ArgumentParser()
ap.add_argument("--echo-db", type=float, default=-24, help="music level at the mic (RMS dBFS)")
ap.add_argument("--voice-db", type=float, default=-8, help="voice level (99th-percentile dBFS)")
ap.add_argument("--lead", type=float, default=10, help="music-only seconds before the first clip")
ap.add_argument("--no-voice", action="store_true", help="echo only, to measure what's left of it")
ap.add_argument("out"); ap.add_argument("truth"); ap.add_argument("vocals"); ap.add_argument("tsv")
ap.add_argument("clips", nargs="+")
args = ap.parse_args()
SR = 48000
rng = np.random.default_rng(1)

def read(p):
    w = wave.open(p); sr = w.getframerate(); raw = w.readframes(w.getnframes())
    a = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768 if w.getsampwidth() == 2 else np.frombuffer(raw, dtype=np.float32)
    if w.getnchannels() > 1: a = a.reshape(-1, w.getnchannels()).mean(1)
    if sr != SR:
        t = np.arange(int(len(a) * SR / sr)) * sr / SR
        a = np.interp(t, np.arange(len(a)), a).astype(np.float32)
    return a

texts = {}
for line in open(args.tsv):
    f, _, text = line.rstrip("\n").split("\t")
    texts[f] = text
clips = [(read(c), texts[os.path.basename(c)]) for c in args.clips]

gap = int(2.5 * SR)
total = int(args.lead * SR) + sum(len(c) + gap for c, _ in clips)
voice = np.zeros(total, np.float32)
pos = int(args.lead * SR)
for c, _ in clips:
    voice[pos:pos + len(c)] = c; pos += len(c) + gap
voice *= 10 ** (args.voice_db / 20) / np.percentile(np.abs(voice[voice != 0]), 99)

# Music: 120 bpm, instruments panned so L and R differ.
t = np.arange(total) / SR
beat = 0.5
def env(period, decay, offset=0.0):
    ph = ((t - offset) % period)
    return np.exp(-ph / decay)
kick = np.sin(2 * np.pi * (50 + 80 * env(beat, 0.03)) * t) * env(beat, 0.15)
snare = rng.standard_normal(total) * env(2 * beat, 0.08, beat) * 0.5
hat = np.diff(rng.standard_normal(total + 1)) * env(beat / 2, 0.02) * 0.25
notes = [55, 55, 49, 52]  # A, A, F, G bass roots (Hz)
root = np.array(notes)[(t // 2).astype(int) % 4]
bass = 0.4 * np.sign(np.sin(2 * np.pi * np.cumsum(root) / SR)) * (0.6 + 0.4 * env(beat, 0.2))
pad = sum(np.sin(2 * np.pi * np.cumsum(root * m) / SR) for m in (4, 5, 6)) * 0.12
vox = read(args.vocals); vox = np.tile(vox, total // len(vox) + 1)[:total] * 0.9
L = 0.8 * kick + 0.3 * snare + 0.2 * hat + 0.5 * bass + 1.0 * pad + 0.7 * vox
R = 0.8 * kick + 0.5 * snare + 1.0 * hat + 0.5 * bass + 0.3 * pad + 0.5 * vox
peak = max(np.abs(L).max(), np.abs(R).max()); L /= peak / 0.7; R /= peak / 0.7

# Room: a different impulse response per speaker (delay, gain, decaying tail).
def rir(delay_ms, gain, seed):
    r = np.random.default_rng(seed); n = int(0.35 * SR)
    h = r.standard_normal(n) * np.exp(-np.arange(n) / (0.06 * SR)) * 0.08
    h[: int(delay_ms * SR / 1000)] = 0
    h[int(delay_ms * SR / 1000)] = 1.0
    return h * gain
def conv(x, h):
    n = len(x) + len(h); N = 1 << (n - 1).bit_length()
    return np.fft.irfft(np.fft.rfft(x, N) * np.fft.rfft(h, N), N)[: len(x)].astype(np.float32)
echo = conv(L, rir(4.0, 1.0, 2)) + conv(R, rir(7.5, 0.7, 3))
echo *= 10 ** (args.echo_db / 20) / np.sqrt((echo ** 2).mean())
voice *= 0.0 if args.no_voice else 1.0
mic = voice + echo + rng.standard_normal(total).astype(np.float32) * 10 ** (-60 / 20)
print(f"{len(clips)} clips, {total / SR:.0f} s; voice p99 {args.voice_db} dBFS, echo rms {args.echo_db} dBFS, mic peak {20*np.log10(np.abs(mic).max()):.1f} dBFS")
data = np.stack([L, R, mic], 1)
pcm = (np.clip(data, -1, 1) * 32767).astype(np.int16)
w = wave.open(args.out, "wb"); w.setnchannels(3); w.setsampwidth(2); w.setframerate(SR); w.writeframes(pcm.tobytes()); w.close()
open(args.truth, "w").write("\n".join(t for _, t in clips) + "\n")
