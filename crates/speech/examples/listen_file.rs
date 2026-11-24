//! Runs WAVs through the full listener (VAD + STT) as live capture would and
//! prints each final. Any sample rate; resampled to 16 kHz.
//! Usage: listen_file [--hold] [--pad secs] <model> <file.wav>...
//! --hold: push-to-talk, finish right after the clip; --pad: silence around it (default 1).

use vivus_speech::models::default_models_dir;
use vivus_speech::{Event, Listener, Mode, SAMPLE_RATE, VadOptions, stt};

fn resample(samples: &[f32], from: i32) -> Vec<f32> {
    if from == SAMPLE_RATE {
        return samples.to_vec();
    }
    let ratio = from as f64 / SAMPLE_RATE as f64;
    let n = (samples.len() as f64 / ratio) as usize;
    (0..n)
        .map(|i| {
            let x = i as f64 * ratio;
            let j = x as usize;
            let f = (x - j as f64) as f32;
            let a = samples[j];
            let b = *samples.get(j + 1).unwrap_or(&a);
            a + (b - a) * f
        })
        .collect()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let hold = args.first().is_some_and(|a| a == "--hold");
    if hold {
        args.remove(0);
    }
    let mut pad = 1.0;
    if args.first().is_some_and(|a| a == "--pad") {
        pad = args[1].parse().expect("pad seconds");
        args.drain(..2);
    }
    let [model, wavs @ ..] = args.as_slice() else {
        eprintln!("usage: listen_file [--hold] [--pad secs] <model> <file.wav>...");
        std::process::exit(2);
    };
    let dir = default_models_dir().unwrap();
    let rec = stt::create(&dir, model, &Default::default()).expect("load model");
    let mut listener = Listener::new(&dir, rec, &VadOptions::default()).expect("load vad");

    let silence = vec![0.0; (pad * SAMPLE_RATE as f32) as usize];
    for wav in wavs {
        let wave = sherpa_onnx::Wave::read(wav).expect("read wav");
        let audio = resample(wave.samples(), wave.sample_rate());
        let audio: Vec<f32> = [&silence[..], &audio, &silence[..]].concat();
        let mut finals = Vec::new();
        let mut on = |e| {
            if let Event::Final(t) = e {
                finals.push(t);
            }
        };
        listener.set_mode(if hold { Mode::Hold } else { Mode::Vad });
        for chunk in audio.chunks(320) {
            listener.push(chunk, &mut on);
        }
        listener.finish(&mut on);
        println!("{wav}: {finals:?}");
    }
}
