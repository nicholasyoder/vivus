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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Audio {
    /// PipeWire node to capture from, e.g. the echo-cancel source. Default source if unset.
    pub input: Option<String>,
    /// Seconds of silence that end an utterance in conversation mode.
    pub end_of_turn_silence: f32,
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            input: None,
            end_of_turn_silence: 0.4,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Provider {
    OpenaiCompatible {
        base_url: String,
        /// Name of the env var holding the key, so keys stay out of the file.
        api_key_env: Option<String>,
    },
    ClaudeCode {
        #[serde(default = "default_claude_command")]
        command: String,
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
    fn unknown_provider_rejected() {
        let config: Config = toml::from_str("[roles.router]\nprovider = \"nope\"").unwrap();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::UnknownProvider { .. })
        ));
    }
}
