//! Needs models from scripts/fetch-models.sh; skips (passes) without them.

use vivus_speech::{Event, Listener, Mode, SAMPLE_RATE, VadOptions, default_models_dir, stt};

const WAV: &str = "sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25/test_wavs/0.wav";

fn run(model: &str, mode: Mode) -> Option<Vec<Event>> {
    let dir = default_models_dir()?;
    let wave = sherpa_onnx::Wave::read(dir.join(WAV).to_str()?)?;
    let rec = stt::create(&dir, model, &Default::default()).ok()?;
    let mut listener = Listener::new(&dir, rec, &VadOptions::default()).ok()?;
    listener.set_mode(mode);

    let mut events = Vec::new();
    let silence = vec![0.0; SAMPLE_RATE as usize];
    let audio: Vec<f32> = [&silence[..], wave.samples(), &silence[..]].concat();
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
        let Some(events) = run(model, Mode::Vad) else {
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

#[test]
fn hold_mode_is_one_utterance() {
    let Some(events) = run("parakeet-tdt-110m", Mode::Hold) else {
        return;
    };
    let finals = finals(&events);
    assert_eq!(finals.len(), 1, "{finals:?}");
    assert!(finals[0].to_lowercase().contains("brothels"));
}
