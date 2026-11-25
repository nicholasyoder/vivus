//! Needs models from scripts/fetch-models.sh; skips (passes) without them.

use vivus_speech::{
    Event, Listener, Mode, Recorder, SAMPLE_RATE, VadOptions, default_models_dir, stt,
};

const WAV: &str = "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8/test_wavs/0.wav";
const MODELS: [&str; 3] = [
    "parakeet-tdt-110m",
    "parakeet-tdt-0.6b",
    "nemotron-streaming-0.6b",
];

/// The clip trimmed to its speech (cut to `secs` if given), then `pad` seconds
/// of silence either side.
fn run(model: &str, mode: Mode, pad: f32, secs: Option<f32>) -> Option<Vec<Event>> {
    run_with(model, mode, pad, secs, None)
}

fn run_with(
    model: &str,
    mode: Mode,
    pad: f32,
    secs: Option<f32>,
    recorder: Option<Recorder>,
) -> Option<Vec<Event>> {
    let dir = default_models_dir()?;
    let wave = sherpa_onnx::Wave::read(dir.join(WAV).to_str()?)?;
    let rec = stt::create(&dir, model, &Default::default()).ok()?;
    let mut listener = Listener::new(&dir, rec, &VadOptions::default()).ok()?;
    listener.set_mode(mode);
    listener.set_recorder(recorder);

    let mut events = Vec::new();
    let silence = vec![0.0; (pad * SAMPLE_RATE as f32) as usize];
    let samples = wave.samples();
    let loud = |s: &&f32| s.abs() > 0.02;
    let start = samples.iter().position(|s| loud(&s))?;
    let end = samples.len() - samples.iter().rev().position(|s| loud(&s))?;
    let end = secs.map_or(end, |s| start + (s * SAMPLE_RATE as f32) as usize);
    let audio: Vec<f32> = [&silence[..], &samples[start..end], &silence[..]].concat();
    for chunk in audio.chunks(320) {
        listener.push(chunk, &mut |e| events.push(e));
    }
    listener.finish(&mut |e| events.push(e));
    Some(events)
}

/// Lowercase words only, since not every model punctuates.
fn words(text: &str) -> String {
    let text: String = text
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn finals(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Final(t) if !t.is_empty() => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn vad_mode_segments_and_transcribes() {
    for model in MODELS {
        let Some(events) = run(model, Mode::Vad, 1.0, None) else {
            eprintln!("skipping {model}: models not installed");
            continue;
        };
        assert_eq!(
            events[..2],
            [Event::SpeechPossible, Event::SpeechStarted],
            "{model}"
        );
        assert!(
            events.iter().any(|e| matches!(e, Event::Partial(_))),
            "{model}: no partials"
        );
        let text = words(&finals(&events).join(" "));
        assert!(text.starts_with("well i dont wish"), "{model}: {text}");
        assert!(text.contains("portrait"), "{model}: {text}");
    }
}

/// Push-to-talk with speech right up to both edges: no word may be clipped.
#[test]
fn hold_mode_is_one_utterance() {
    for model in MODELS {
        let Some(events) = run(model, Mode::Hold, 0.0, None) else {
            continue;
        };
        let finals = finals(&events);
        assert_eq!(finals.len(), 1, "{model}: {finals:?}");
        let text = words(finals[0]);
        assert!(text.starts_with("well"), "{model}: {text}");
        assert!(text.ends_with("old portrait"), "{model}: {text}");
    }
}

/// A word shorter than the VAD's `min_speech` ("wait" over a reply) is still
/// heard, rather than dismissed as `NotSpeech`.
#[test]
fn short_word_is_speech() {
    for model in MODELS {
        let Some(events) = run(model, Mode::Vad, 1.0, Some(0.2)) else {
            continue;
        };
        assert!(!events.contains(&Event::NotSpeech), "{model}: {events:?}");
        let text = words(&finals(&events).join(" "));
        assert_eq!(text, "well", "{model}: {events:?}");
    }
}

#[test]
fn records_what_it_heard() {
    let out = std::env::temp_dir().join(format!("vivus-rec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let model = "parakeet-tdt-110m";
    let recorder = Recorder::new(&out, model).unwrap();
    if run_with(model, Mode::Hold, 0.0, None, Some(recorder)).is_none() {
        return;
    }
    let index = std::fs::read_to_string(out.join("transcripts.tsv")).unwrap();
    let [line] = index.lines().collect::<Vec<_>>()[..] else {
        panic!("{index}");
    };
    let [file, m, text] = line.split('\t').collect::<Vec<_>>()[..] else {
        panic!("{line}");
    };
    assert_eq!(m, model);
    assert!(words(text).ends_with("old portrait"), "{text}");
    let wave = sherpa_onnx::Wave::read(out.join(file).to_str().unwrap()).unwrap();
    assert_eq!(wave.sample_rate(), SAMPLE_RATE);
    let secs = wave.samples().len() as f32 / SAMPLE_RATE as f32;
    assert!((3.0..10.0).contains(&secs), "{secs} s");
    std::fs::remove_dir_all(&out).unwrap();
}
