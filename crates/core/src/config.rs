//! `~/.config/vivus/config.toml`. See `config.example.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("role `{role}` references unknown provider `{provider}`")]
    UnknownProvider { role: String, provider: String },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
    #[serde(default)]
    pub roles: Roles,
    #[serde(default)]
    pub audio: Audio,
    #[serde(default)]
    pub debug: Debug,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Debug {
    /// Show what was heard but don't answer it (no LLM, no speech), for
    /// testing recognition quickly.
    pub transcribe_only: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Audio {
    /// PipeWire node to capture from. Default source if unset.
    pub input: Option<String>,
    /// PipeWire node to play speech to. Default sink if unset.
    pub output: Option<String>,
    /// Seconds of silence that end an utterance in conversation mode.
    pub end_of_turn_silence: f32,
    /// Load PipeWire's echo canceller so the mic stays open while speaking
    /// (barge-in). It removes everything the output sink plays, music included.
    pub echo_cancel: bool,
    /// Other apps' volume while we speak or the user talks, relative to their own (0.3 is about
    /// -10 dB); 1 leaves them alone.
    pub duck_volume: f32,
    /// Save each utterance as heard (after echo cancellation) with its
    /// transcript, under `~/.local/share/vivus/recordings`.
    pub record_utterances: bool,
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            end_of_turn_silence: 0.4,
            echo_cancel: true,
            duck_volume: 0.3,
            record_utterances: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Provider {
    OpenaiCompatible {
        base_url: String,
        /// Name of the variable holding the key, so keys stay out of this file.
        /// Looked up in the environment, then in `.env` next to the config.
        api_key_env: Option<String>,
    },
    ClaudeCode {
        #[serde(default = "default_claude_command")]
        command: String,
        /// Appended after `-p --output-format stream-json`. Headless runs can't
        /// ask for permission, so this is where tools get allowed.
        #[serde(default = "default_claude_args")]
        args: Vec<String>,
    },
    /// Speech models run in-process via sherpa-onnx.
    LocalSpeech {
        /// Defaults to `~/.local/share/vivus/models`.
        models_dir: Option<PathBuf>,
        #[serde(default = "default_speech_threads")]
        threads: i32,
    },
}

fn default_speech_threads() -> i32 {
    2
}

fn default_claude_command() -> String {
    "claude".into()
}

fn default_claude_args() -> Vec<String> {
    vec!["--allowedTools".into(), "WebSearch,WebFetch".into()]
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roles {
    pub stt: Option<Role>,
    pub tts: Option<Role>,
    pub router: Option<Role>,
    pub answer: Option<Role>,
    pub agent: Option<Role>,
    pub vision: Option<Role>,
}

impl Roles {
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &Role)> {
        [
            ("stt", &self.stt),
            ("tts", &self.tts),
            ("router", &self.router),
            ("answer", &self.answer),
            ("agent", &self.agent),
            ("vision", &self.vision),
        ]
        .into_iter()
        .filter_map(|(name, role)| role.as_ref().map(|r| (name, r)))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub provider: String,
    pub model: Option<String>,
    /// e.g. a TTS voice name.
    pub voice: Option<String>,
}

impl Config {
    /// `$XDG_CONFIG_HOME/vivus/config.toml`, falling back to `~/.config`.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("vivus").join("config.toml"))
    }

    /// `.env` beside the config file.
    pub fn env_file_path() -> Option<PathBuf> {
        Some(Self::default_path()?.with_file_name(".env"))
    }

    /// A missing file yields the empty default config.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.into(),
                    source,
                });
            }
        };
        let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.into(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (name, role) in self.roles.iter() {
            if !self.providers.contains_key(&role.provider) {
                return Err(ConfigError::UnknownProvider {
                    role: name.into(),
                    provider: role.provider.clone(),
                });
            }
        }
        Ok(())
    }
}

/// A secret such as an API key: the environment wins, then `~/.config/vivus/.env`.
/// The file is read on each call so edits apply without a restart.
pub fn secret(var: &str) -> Option<String> {
    let from_env = std::env::var(var).ok().filter(|v| !v.is_empty());
    from_env.or_else(|| {
        let text = std::fs::read_to_string(Config::env_file_path()?).ok()?;
        parse_env(&text).remove(var).filter(|v| !v.is_empty())
    })
}

/// `KEY=value` lines; `#` comments, optional `export ` and surrounding quotes.
fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (k, v) = line.split_once('=')?;
            let k = k.trim();
            if k.is_empty() || k.starts_with('#') {
                return None;
            }
            let v = v.trim();
            let v = [('"', '"'), ('\'', '\'')]
                .iter()
                .find_map(|&(a, b)| v.strip_prefix(a)?.strip_suffix(b))
                .unwrap_or(v);
            Some((k.to_owned(), v.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let text = include_str!("../../../config.example.toml");
        let config: Config = toml::from_str(text).unwrap();
        config.validate().unwrap();
        assert!(config.roles.router.is_some());
    }

    #[test]
    fn env_file_parsing() {
        let env = parse_env("# keys\nexport A=1\nB = \"two words\"\nC='x=y'\n\nnot a line\n=bad\n");
        assert_eq!(env.len(), 3);
        assert_eq!(env["A"], "1");
        assert_eq!(env["B"], "two words");
        assert_eq!(env["C"], "x=y");
    }

    #[test]
    fn unknown_provider_rejected() {
        let config: Config = toml::from_str("[roles.router]\nprovider = \"nope\"").unwrap();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::UnknownProvider { .. })
        ));
    }
}
