//! Feeds a WAV through an STT model in 20 ms chunks, as live capture would.
//! Usage: stt_bench <model> <file.wav> [threads]
//! Reports compute per second of audio and the end-of-speech -> final latency.

use std::time::{Duration, Instant};

use vivus_speech::models::default_models_dir;
use vivus_speech::{SAMPLE_RATE, stt};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, model, wav, rest @ ..] = args.as_slice() else {
        eprintln!("usage: stt_bench <model> <file.wav> [threads]");
        std::process::exit(2);
    };
    let threads = rest.first().map_or(2, |t| t.parse().expect("threads"));

    let wave = sherpa_onnx::Wave::read(wav).expect("read wav");
    assert_eq!(wave.sample_rate(), SAMPLE_RATE, "need 16 kHz input");
    let audio = wave.samples();
    let audio_secs = audio.len() as f32 / SAMPLE_RATE as f32;

    let opts = stt::Options {
        num_threads: threads,
        ..Default::default()
    };
    let t = Instant::now();
    let mut rec = stt::create(&default_models_dir().unwrap(), model, &opts).expect("load model");
    let load = t.elapsed();

    let chunk = (SAMPLE_RATE / 50) as usize;
    let mut compute = Duration::ZERO;
    let mut worst_chunk = Duration::ZERO;
    let mut partials = 0;
    for c in audio.chunks(chunk) {
        let t = Instant::now();
        if rec.accept(c).is_some() {
            partials += 1;
        }
        let e = t.elapsed();
        compute += e;
        worst_chunk = worst_chunk.max(e);
    }
    let t = Instant::now();
    let text = rec.finish();
    let finish = t.elapsed();

    println!("model {model}, {threads} threads, {audio_secs:.1}s audio");
    println!("  load            {:>7.0} ms", load.as_secs_f64() * 1e3);
    println!(
        "  compute/audio   {:>7.3}  (<1 keeps up with live audio)",
        compute.as_secs_f32() / audio_secs
    );
    println!(
        "  worst chunk     {:>7.0} ms",
        worst_chunk.as_secs_f64() * 1e3
    );
    println!("  final latency   {:>7.0} ms", finish.as_secs_f64() * 1e3);
    println!("  partials        {partials}");
    println!("  text: {text}");
}
