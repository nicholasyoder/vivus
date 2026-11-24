//! Needs models from scripts/fetch-models.sh; skips (passes) without them.

use vivus_speech::{Error, Synthesizer, default_models_dir};

fn synth(model: &str, voice: Option<&str>) -> Option<Result<Synthesizer, Error>> {
    let dir = default_models_dir()?;
    match Synthesizer::create(&dir, model, voice, &Default::default()) {
        Err(Error::MissingFiles(p)) => {
            eprintln!("skipping {model}: {} missing", p.display());
            None
        }
        r => Some(r),
    }
}

#[test]
fn every_model_speaks() {
    for model in vivus_speech::models::TTS_MODELS {
        let Some(s) = synth(model.name, None) else { continue };
        let s = s.unwrap();
        assert_eq!(s.sample_rate(), 24_000);
        let secs = s.synthesize("Hello there.").len() as f32 / 24_000.0;
        assert!((0.3..5.0).contains(&secs), "{}: {secs}s of audio", model.name);
        assert!(s.synthesize("  ").is_empty());
    }
}

#[test]
fn voices_resolve() {
    let Some(s) = synth("kokoro", Some("bm_george")) else { return };
    assert!(s.is_ok());
    let Some(s) = synth("kitten-nano", Some("nope")) else { return };
    assert!(matches!(s, Err(Error::UnknownVoice(..))));
}

/// Typical reply text loses no words with any model or lexicon order.
#[test]
fn normalized_text_is_in_lexicon() {
    let text = "It is 3:45 PM on Nov 23, 2026. Your GPU uses 12.5 GB, about 40% of the RAM. \
        Tell her it's 21°C or 70°F, wind 15 km/h, and that's $5.99. What's next? \
        Dr. Smith emailed you about the API at vivus.org.";
    for (model, voice) in [("kitten-nano", None), ("kokoro", None), ("kokoro", Some("bm_george"))] {
        let Some(s) = synth(model, voice) else { continue };
        let s = s.unwrap();
        let spoken = s.normalize(text);
        let missing = s.phonemize(&spoken).missing;
        assert_eq!(missing, ["vivus"], "{model}: {spoken}");
    }
}

/// Inflections neither Kokoro lexicon has come from suffix rules.
#[test]
fn suffix_rules_fill_gaps() {
    let Some(s) = synth("kokoro", None) else { return };
    let s = s.unwrap();
    let ph = s.phonemize("It opens notifications for the compositors.");
    assert!(ph.missing.is_empty(), "{:?}", ph.missing);
}
