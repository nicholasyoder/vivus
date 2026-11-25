//! `vivusctl models`: download the local speech models from sherpa-onnx
//! releases. Directory names must match `crates/speech/src/models.rs`
//! (checked by a test).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

const RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";

/// A release file: a single `.onnx`/`.txt`, or a `.tar.bz2` holding one
/// directory of the same name.
struct Asset {
    tag: &'static str,
    name: &'static str,
    archive: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tier {
    Light,
    Standard,
    /// Only fetched by `all` or by name.
    Extra,
}

struct Model {
    name: &'static str,
    what: &'static str,
    tier: Tier,
    assets: &'static [Asset],
}

const fn file(tag: &'static str, name: &'static str) -> Asset {
    Asset { tag, name, archive: false }
}
const fn archive(tag: &'static str, name: &'static str) -> Asset {
    Asset { tag, name, archive: true }
}

const MODELS: &[Model] = &[
    Model {
        name: "silero-vad",
        what: "voice activity detection",
        tier: Tier::Light,
        assets: &[file("asr-models", "silero_vad_v5.onnx")],
    },
    Model {
        name: "parakeet-tdt-110m",
        what: "STT",
        tier: Tier::Light,
        assets: &[archive(
            "asr-models",
            "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8",
        )],
    },
    Model {
        name: "kitten-nano",
        what: "TTS",
        tier: Tier::Light,
        assets: &[
            archive("tts-models", "kitten-nano-en-v0_8-fp32"),
            file("tts-models", "lexicon-en-us.txt"),
        ],
    },
    Model {
        name: "parakeet-tdt-0.6b",
        what: "STT",
        tier: Tier::Standard,
        assets: &[archive("asr-models", "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8")],
    },
    Model {
        name: "kokoro",
        what: "TTS",
        tier: Tier::Standard,
        assets: &[archive("tts-models", "kokoro-multi-lang-v1_0")],
    },
    Model {
        name: "nemotron-streaming-0.6b",
        what: "STT",
        tier: Tier::Extra,
        assets: &[archive(
            "asr-models",
            "sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25",
        )],
    },
];

/// `$VIVUS_MODELS`, else `$XDG_DATA_HOME/vivus/models`, as the daemon defaults to.
pub fn default_dir() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("VIVUS_MODELS").filter(|v| !v.is_empty()) {
        return Ok(dir.into());
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("no home directory")?;
    Ok(data.join("vivus/models"))
}

fn installed(dir: &Path, m: &Model) -> bool {
    m.assets.iter().all(|a| dir.join(a.name).exists())
}

pub fn list(dir: &Path) {
    println!("models in {}", dir.display());
    for m in MODELS {
        let tier = match m.tier {
            Tier::Light => "light",
            Tier::Standard => "standard",
            Tier::Extra => "extra",
        };
        let mark = if installed(dir, m) { "installed" } else { "-" };
        println!("  {:<25} {:<9} {:<26} {mark}", m.name, tier, m.what);
    }
}

/// `wanted` holds tiers (`light`, `standard`, `all`) and model names.
/// The VAD comes with every tier.
fn select(wanted: &[String]) -> anyhow::Result<Vec<&'static Model>> {
    let mut out: Vec<&Model> = Vec::new();
    for w in wanted {
        let picked: Vec<&Model> = match w.as_str() {
            "light" => MODELS.iter().filter(|m| m.tier == Tier::Light).collect(),
            "standard" => MODELS
                .iter()
                .filter(|m| m.tier == Tier::Standard || m.name == "silero-vad")
                .collect(),
            "all" => MODELS.iter().collect(),
            name => match MODELS.iter().find(|m| m.name == name) {
                Some(m) => vec![m],
                None => {
                    let known: Vec<_> = MODELS.iter().map(|m| m.name).collect();
                    bail!(
                        "unknown model or tier `{name}` (tiers: light, standard, all; models: {})",
                        known.join(", ")
                    );
                }
            },
        };
        for m in picked {
            if !out.iter().any(|o| o.name == m.name) {
                out.push(m);
            }
        }
    }
    Ok(out)
}

pub async fn fetch(dir: &Path, wanted: &[String]) -> anyhow::Result<()> {
    let models = select(wanted)?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let client = reqwest::Client::new();
    for m in models {
        for a in m.assets {
            if dir.join(a.name).exists() {
                continue;
            }
            fetch_asset(&client, dir, a)
                .await
                .with_context(|| format!("fetching {}", a.name))?;
        }
    }
    println!("models in {}", dir.display());
    Ok(())
}

async fn fetch_asset(client: &reqwest::Client, dir: &Path, a: &Asset) -> anyhow::Result<()> {
    let file = if a.archive { format!("{}.tar.bz2", a.name) } else { a.name.into() };
    let url = format!("{RELEASES}/{}/{file}", a.tag);
    let part = dir.join(format!(".{file}.part"));
    download(client, &url, &part, a.name).await?;

    if !a.archive {
        std::fs::rename(&part, dir.join(a.name))?;
        return Ok(());
    }
    eprintln!("unpacking {}", a.name);
    let (dir, name) = (dir.to_owned(), a.name);
    tokio::task::spawn_blocking(move || unpack(&part, &dir, name)).await?
}

/// Streams to `dest`, overwriting a stale partial download.
async fn download(client: &reqwest::Client, url: &str, dest: &Path, label: &str) -> anyhow::Result<()> {
    let mut resp = client.get(url).send().await?.error_for_status()?;
    let total = resp.content_length();
    let mut out = std::fs::File::create(dest)?;
    let (mut got, mut shown) = (0u64, u64::MAX);
    let mut stderr = std::io::stderr();
    while let Some(chunk) = resp.chunk().await? {
        out.write_all(&chunk)?;
        got += chunk.len() as u64;
        let mb = got >> 20;
        if mb != shown {
            shown = mb;
            match total {
                Some(t) => write!(stderr, "\r{label}: {mb} / {} MB", t >> 20)?,
                None => write!(stderr, "\r{label}: {mb} MB")?,
            }
        }
    }
    writeln!(stderr)?;
    out.sync_all()?;
    Ok(())
}

/// Unpacks into a hidden staging directory first, so an interrupted run never
/// leaves a half-filled model directory that looks installed.
fn unpack(archive: &Path, dir: &Path, name: &str) -> anyhow::Result<()> {
    let staging = dir.join(format!(".{name}.unpack"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    let reader = std::io::BufReader::new(std::fs::File::open(archive)?);
    tar::Archive::new(bzip2::read::BzDecoder::new(reader)).unpack(&staging)?;
    let inner = staging.join(name);
    if !inner.is_dir() {
        bail!("archive has no {name}/ directory");
    }
    std::fs::rename(&inner, dir.join(name))?;
    std::fs::remove_dir_all(&staging)?;
    std::fs::remove_file(archive)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> Vec<&'static str> {
        MODELS.iter().flat_map(|m| m.assets).map(|a| a.name).collect()
    }

    #[test]
    fn covers_the_speech_catalog() {
        use vivus_speech::models::{KITTEN_LEXICON, STT_MODELS, TTS_MODELS, VAD_MODEL};
        let have = assets();
        let need = STT_MODELS
            .iter()
            .map(|m| m.dir)
            .chain(TTS_MODELS.iter().map(|m| m.dir))
            .chain([VAD_MODEL, KITTEN_LEXICON]);
        for dir in need {
            assert!(have.contains(&dir), "no download for {dir}");
        }
        for m in STT_MODELS.iter().map(|m| m.name).chain(TTS_MODELS.iter().map(|m| m.name)) {
            assert!(MODELS.iter().any(|d| d.name == m), "no download named {m}");
        }
    }

    #[test]
    fn tiers() {
        let names = |w: &[&str]| -> Vec<&str> {
            let w: Vec<String> = w.iter().map(|s| s.to_string()).collect();
            select(&w).unwrap().iter().map(|m| m.name).collect()
        };
        assert_eq!(names(&["light"]), ["silero-vad", "parakeet-tdt-110m", "kitten-nano"]);
        assert_eq!(names(&["standard"]), ["silero-vad", "parakeet-tdt-0.6b", "kokoro"]);
        assert_eq!(names(&["light", "kokoro", "kitten-nano"]).len(), 4);
        assert_eq!(names(&["all"]).len(), MODELS.len());
        assert!(select(&["nope".into()]).is_err());
    }
}
