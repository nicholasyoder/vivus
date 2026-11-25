//! What the settings pickers offer (`ListProviders`, `ListModels`,
//! `ListVoices`), and checks that a speech role's model and voice can load.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};
use vivus_core::Config;
use vivus_core::config::{Provider, ROLES};
use vivus_dbus::Choice;
use vivus_speech::{SttModel, TtsModel};

use crate::speaker::DEFAULT_TTS_MODEL;
use crate::voice::{DEFAULT_STT_MODEL, LocalSpeech, local_provider, resolve_local};

/// `/models` answers by provider, so opening the settings window doesn't hit
/// every provider each time.
pub type ModelCache = Arc<Mutex<HashMap<String, (Instant, Vec<String>)>>>;
const MODEL_CACHE_TTL: Duration = Duration::from_secs(300);

/// Configured providers that can serve `role`.
pub fn providers(config: &Config, role: &str) -> anyhow::Result<Vec<String>> {
    ensure!(ROLES.contains(&role), "unknown role `{role}`");
    Ok(config
        .providers
        .iter()
        .filter(|(_, p)| p.serves(role))
        .map(|(name, _)| name.clone())
        .collect())
}

/// `""` is the role's default provider (`None`).
fn provider<'a>(
    config: &'a Config,
    name: &'a str,
) -> anyhow::Result<Option<(&'a str, &'a Provider)>> {
    if name.is_empty() {
        return Ok(None);
    }
    let p = config
        .providers
        .get(name)
        .with_context(|| format!("no provider `{name}`"))?;
    Ok(Some((name, p)))
}

/// TTS voices of `model` (`""`: the default model), default first.
pub fn voices(config: &Config, provider_name: &str, model: &str) -> anyhow::Result<Vec<Choice>> {
    local_provider("tts", provider(config, provider_name)?)?;
    let model = TtsModel::find(if model.is_empty() {
        DEFAULT_TTS_MODEL
    } else {
        model
    })?;
    let labels = model.voice_labels();
    let default = labels
        .iter()
        .find(|(n, _)| *n == model.default_voice)
        .map_or(model.default_voice.to_owned(), |(_, l)| l.clone());
    let mut out = vec![(String::new(), format!("Default: {default}"))];
    out.extend(labels.into_iter().map(|(n, l)| (n.to_owned(), l)));
    Ok(out)
}

/// Models `provider_name` offers for `role`. Remote lists are fetched, hence async.
pub async fn models(
    config: &Config,
    role: &str,
    provider_name: &str,
    cache: &ModelCache,
) -> anyhow::Result<Vec<Choice>> {
    ensure!(ROLES.contains(&role), "unknown role `{role}`");
    let p = provider(config, provider_name)?;
    if let Some((name, p)) = p {
        ensure!(p.serves(role), "provider `{name}` can't serve {role}");
    }
    match p {
        None | Some((_, Provider::LocalSpeech { .. })) => {
            let (dir, _) = local_provider(role, p)?;
            local_models(role, &dir)
        }
        Some((_, Provider::ClaudeCode { .. })) => Ok([
            ("", "Default"),
            ("haiku", "Haiku"),
            ("sonnet", "Sonnet"),
            ("opus", "Opus"),
        ]
        .map(|(id, label)| (id.to_owned(), label.to_owned()))
        .into()),
        Some((name, p @ Provider::OpenaiCompatible { base_url, .. })) => {
            let key = format!("{name} {base_url}");
            let cached = cache
                .lock()
                .unwrap()
                .get(&key)
                .filter(|(at, _)| at.elapsed() < MODEL_CACHE_TTL)
                .map(|(_, ids)| ids.clone());
            let ids = match cached {
                Some(ids) => ids,
                None => {
                    let client = vivus_inference::Client::from_provider(name, p)?;
                    let ids = client
                        .models()
                        .await
                        .with_context(|| format!("listing {name}'s models"))?;
                    cache
                        .lock()
                        .unwrap()
                        .insert(key, (Instant::now(), ids.clone()));
                    ids
                }
            };
            Ok(ids
                .into_iter()
                .filter(|id| chat_model(id))
                .map(|id| (id.clone(), id))
                .collect())
        }
    }
}

/// Drops `provider`'s cached model list (its key changed, so it may differ or now work).
pub fn forget_models(cache: &ModelCache, provider: &str) {
    let prefix = format!("{provider} ");
    cache.lock().unwrap().retain(|k, _| !k.starts_with(&prefix));
}

/// `/models` also lists speech and embedding models, which no chat role can use.
fn chat_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    ![
        "whisper",
        "tts",
        "orpheus",
        "playai",
        "embed",
        "transcribe",
        "prompt-guard",
    ]
    .iter()
    .any(|w| id.contains(w))
}

/// Catalog models on disk, default first (listed even if it isn't downloaded).
fn local_models(role: &str, dir: &std::path::Path) -> anyhow::Result<Vec<Choice>> {
    let entry = |name: &str, label: &str| (name.to_owned(), label.to_owned());
    let (default, mut out): (_, Vec<Choice>) = match role {
        "stt" => (
            SttModel::find(DEFAULT_STT_MODEL)?.label,
            vivus_speech::models::STT_MODELS
                .iter()
                .filter(|m| m.installed(dir))
                .map(|m| entry(m.name, m.label))
                .collect(),
        ),
        "tts" => (
            TtsModel::find(DEFAULT_TTS_MODEL)?.label,
            vivus_speech::models::TTS_MODELS
                .iter()
                .filter(|m| m.installed(dir))
                .map(|m| entry(m.name, m.label))
                .collect(),
        ),
        _ => bail!("local speech can't serve {role}"),
    };
    out.insert(0, (String::new(), format!("Default: {default}")));
    Ok(out)
}

/// `role` (stt or tts) resolved, with its model known and on disk and its voice known.
pub fn check_local(config: &Config, role: &str) -> anyhow::Result<LocalSpeech> {
    let fetch =
        |name: &str| format!("{name} isn't downloaded (run `vivusctl models fetch {name}`)");
    let r = config.roles.get(role);
    match role {
        "stt" => {
            let l = resolve_local(config, role, r, DEFAULT_STT_MODEL)?;
            let m = SttModel::find(&l.model)?;
            ensure!(m.installed(&l.models_dir), fetch(m.name));
            Ok(l)
        }
        "tts" => {
            let l = resolve_local(config, role, r, DEFAULT_TTS_MODEL)?;
            let m = TtsModel::find(&l.model)?;
            if let Some(v) = &l.voice {
                m.speaker_id(v)?;
            }
            ensure!(m.installed(&l.models_dir), fetch(m.name));
            Ok(l)
        }
        _ => bail!("{role} isn't a speech role"),
    }
}

/// Refuses an edit of `path` that would leave its speech role unable to load.
pub fn check_edit(config: &Config, path: &str) -> anyhow::Result<()> {
    for role in ["stt", "tts"] {
        let table = format!("roles.{role}");
        if path == table || path.starts_with(&format!("{table}.")) {
            check_local(config, role)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        Config::parse(text, std::path::Path::new("c.toml")).unwrap()
    }

    const PROVIDERS: &str = r#"
[providers.groq]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9"
[providers.cc]
kind = "claude-code"
[providers.local]
kind = "local-speech"
"#;

    #[test]
    fn providers_by_role() {
        let c = config(PROVIDERS);
        assert_eq!(providers(&c, "stt").unwrap(), ["local", "local-speech"]);
        assert_eq!(
            providers(&c, "router").unwrap(),
            ["groq", "nano-gpt", "openai"]
        );
        assert_eq!(
            providers(&c, "agent").unwrap(),
            ["cc", "claude-code", "groq", "nano-gpt", "openai"]
        );
        assert!(providers(&c, "nope").is_err());
    }

    #[tokio::test]
    async fn fixed_model_lists() {
        let c = config(PROVIDERS);
        let cache = ModelCache::default();
        let cc = models(&c, "agent", "cc", &cache).await.unwrap();
        assert_eq!(cc[0].0, "");
        assert!(cc.iter().any(|(id, _)| id == "opus"));
        assert!(models(&c, "router", "cc", &cache).await.is_err());
        assert!(models(&c, "router", "", &cache).await.is_err());
        // An empty dir still offers the default.
        let local = local_models("tts", std::path::Path::new("/nonexistent")).unwrap();
        assert_eq!(
            local,
            [(String::new(), "Default: Kitten nano (light)".into())]
        );
    }

    #[test]
    fn voice_list() {
        let c = config(PROVIDERS);
        let v = voices(&c, "local", "kokoro").unwrap();
        assert_eq!(
            v[0],
            (String::new(), "Default: Heart (American, female)".into())
        );
        assert!(voices(&c, "groq", "").is_err());
        assert!(voices(&c, "", "nope").is_err());
    }

    #[test]
    fn edit_checks() {
        let c = config(&format!(
            "{PROVIDERS}[roles.tts]\nprovider = \"local\"\nmodel = \"kokoro\"\nvoice = \"bella\"\n"
        ));
        let err = check_edit(&c, "roles.tts.voice").unwrap_err();
        assert!(err.to_string().contains("unknown voice"), "{err}");
        check_edit(&c, "roles.stt.model").ok(); // depends on what's downloaded
        check_edit(&c, "audio.input").unwrap();
        assert!(chat_model("llama-3.3-70b") && !chat_model("whisper-large-v3"));
    }
}
