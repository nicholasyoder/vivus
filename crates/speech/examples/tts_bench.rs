//! Synthesizes text sentence by sentence, as the daemon does, and writes a WAV.
//! Usage: tts_bench <model> [voice] [threads] [out.wav] [text]
//! Reports time to the first sentence's audio and compute per second of audio.

use std::time::{Duration, Instant};

use vivus_speech::models::default_models_dir;
use vivus_speech::{SentenceSplitter, Synthesizer, tts};

const TEXT: &str = "It's twenty past three in the afternoon. The forecast says light rain \
    this evening, so you might want an umbrella. Anything else?";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(model) = args.get(1) else {
        eprintln!("usage: tts_bench <model> [voice] [threads] [out.wav] [text]");
        std::process::exit(2);
    };
    let voice = args
        .get(2)
        .map(String::as_str)
        .filter(|v| !v.is_empty() && *v != "-");
    let threads = args.get(3).map_or(2, |t| t.parse().expect("threads"));
    let out = args.get(4).map_or("tts_bench.wav", String::as_str);
    let text = args.get(5).map_or(TEXT, String::as_str);

    let opts = tts::Options {
        num_threads: threads,
        pronunciations: vivus_speech::default_pronunciation_files(),
        ..Default::default()
    };
    let t = Instant::now();
    let synth = Synthesizer::create(&default_models_dir().unwrap(), model, voice, &opts)
        .expect("load model");
    let load = t.elapsed();
    let rate = synth.sample_rate();
    // The daemon warms up at load: the first inference is much slower.
    let t = Instant::now();
    synth.synthesize("Ready.");
    let warmup = t.elapsed();

    let mut splitter = SentenceSplitter::new();
    let mut sentences = splitter.push(text);
    sentences.extend(splitter.finish());

    let mut audio = Vec::new();
    let mut compute = Duration::ZERO;
    let mut first = None;
    for s in &sentences {
        let t = Instant::now();
        let samples = synth.synthesize(s);
        let e = t.elapsed();
        let spoken = synth.normalize(s);
        println!(
            "  {:>5.0} ms for {:.1}s: {spoken}",
            e.as_secs_f64() * 1e3,
            samples.len() as f32 / rate as f32
        );
        let ph = synth.phonemize(&spoken);
        println!("           /{}/", ph.phonemes);
        if !ph.missing.is_empty() {
            println!("           no pronunciation: {}", ph.missing.join(", "));
        }
        audio.extend(samples);
        compute += e;
        first.get_or_insert(compute);
    }
    let audio_secs = audio.len() as f32 / rate as f32;
    sherpa_onnx::write(out, &audio, rate as i32);

    println!(
        "model {model}, {threads} threads, {} sentences, {audio_secs:.1}s audio at {rate} Hz",
        sentences.len()
    );
    println!("  load            {:>7.0} ms", load.as_secs_f64() * 1e3);
    println!("  warm-up         {:>7.0} ms", warmup.as_secs_f64() * 1e3);
    println!(
        "  first sentence  {:>7.0} ms",
        first.unwrap_or_default().as_secs_f64() * 1e3
    );
    println!(
        "  compute/audio   {:>7.3}  (<1 keeps up with playback)",
        compute.as_secs_f32() / audio_secs
    );
    println!("  wrote {out}");
}
