//! Needs models from scripts/fetch-models.sh; skips (passes) without them.

use vivus_speech::{Event, Listener, Mode, SAMPLE_RATE, VadOptions, default_models_dir, stt};

const WAV: &str = "sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25/test_wavs/0.wav";

/// The clip trimmed to its speech, then `pad` seconds of silence either side.
fn run(model: &str, mode: Mode, pad: f32) -> Option<Vec<Event>> {
    let dir = default_models_dir()?;
    let wave = sherpa_onnx::Wave::read(dir.join(WAV).to_str()?)?;
    let rec = stt::create(&dir, model, &Default::default()).ok()?;
    let mut listener = Listener::new(&dir, rec, &VadOptions::default()).ok()?;
    listener.set_mode(mode);

    let mut events = Vec::new();
    let silence = vec![0.0; (pad * SAMPLE_RATE as f32) as usize];
    let samples = wave.samples();
    let loud = |s: &&f32| s.abs() > 0.02;
    let start = samples.iter().position(|s| loud(&s))?;
    let end = samples.len() - samples.iter().rev().position(|s| loud(&s))?;
    let audio: Vec<f32> = [&silence[..], &samples[start..end], &silence[..]].concat();
    for chunk in audio.chunks(320) {
        listener.push(chunk, &mut |e| events.push(e));
    }
    listener.finish(&mut |e| events.push(e));
    Some(events)
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
    for model in ["parakeet-tdt-110m", "nemotron-streaming-0.6b"] {
        let Some(events) = run(model, Mode::Vad, 1.0) else {
            eprintln!("skipping {model}: models not installed");
            continue;
        };
        assert_eq!(events[..2], [Event::SpeechPossible, Event::SpeechStarted], "{model}");
        assert!(events.iter().any(|e| matches!(e, Event::Partial(_))), "{model}: no partials");
        let text = finals(&events).join(" ").to_lowercase();
        assert!(text.starts_with("after early nightfall"), "{model}: {text}");
        assert!(text.contains("brothels"), "{model}: {text}");
    }
}

/// Push-to-talk with speech right up to both edges: no word may be clipped.
#[test]
fn hold_mode_is_one_utterance() {
    for model in ["parakeet-tdt-110m", "nemotron-streaming-0.6b"] {
        let Some(events) = run(model, Mode::Hold, 0.0) else {
            continue;
        };
        let finals = finals(&events);
        assert_eq!(finals.len(), 1, "{model}: {finals:?}");
        let text = finals[0].to_lowercase();
        assert!(text.starts_with("after early"), "{model}: {text}");
        assert!(text.trim_end_matches('.').ends_with("brothels"), "{model}: {text}");
    }
}
