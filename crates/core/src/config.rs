//! `~/.config/vivus/config.toml`. See `config.example.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    #[error("provider name `{0}` may only use letters, digits, `-` and `_`")]
    BadProviderName(String),
    #[error("provider `{provider}`: base_url `{url}` must start with http:// or https://")]
    BadBaseUrl { provider: String, url: String },
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
    #[serde(default)]
    pub roles: Roles,
    #[serde(default)]
    pub audio: Audio,
    #[serde(default)]
    pub shortcuts: Shortcuts,
    #[serde(default)]
    pub debug: Debug,
}

/// Triggers requested from the `GlobalShortcuts` portal, in the shortcuts-spec
/// format (`LOGO+space`). Empty leaves one unbound. Backends with a dialog
/// treat these as suggestions.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shortcuts {
    /// Hold to talk; release to send.
    pub push_to_talk: String,
    /// Start or stop conversation mode (end of turn by silence).
    pub toggle_listen: String,
    pub toggle_sidebar: String,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            push_to_talk: "LOGO+space".into(),
            toggle_listen: "LOGO+SHIFT+space".into(),
            toggle_sidebar: "LOGO+v".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Debug {
    /// Show what was heard but don't answer it (no LLM, no speech), for
    /// testing recognition quickly.
    pub transcribe_only: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    /// Keep capturing (and discarding) while not listening, so the mic never
    /// suspends: onboard inputs thump on wake-up. The mic-in-use indicator stays on.
    pub keep_mic_open: bool,
    /// Other apps' volume while we speak or the user talks, relative to their own (0.3 is about
    /// -10 dB); 1 leaves them alone.
    pub duck_volume: f32,
    /// Speech playback volume as mixers show it (cubic, like pavucontrol), 0 to 1.
    pub speech_volume: f32,
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
            keep_mic_open: true,
            duck_volume: 0.3,
            speech_volume: 1.0,
            record_utterances: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Provider {
    OpenaiCompatible {
        base_url: String,
        /// The variable the key goes by in the environment and `.env`; defaults
        /// to `<NAME>_API_KEY`. Keys never go in this file (see `keys`).
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

/// Roles a `[roles.*]` table can name.
pub const ROLES: &[&str] = &["stt", "tts", "router", "answer", "agent", "vision"];

impl Provider {
    /// Whether this kind of provider can serve `role` (what's implemented, not
    /// what the API could do: no remote STT/TTS yet).
    pub fn serves(&self, role: &str) -> bool {
        match self {
            Provider::LocalSpeech { .. } => matches!(role, "stt" | "tts"),
            Provider::OpenaiCompatible { .. } => {
                matches!(role, "router" | "answer" | "agent" | "vision")
            }
            // The router needs streamed tool calls, which `claude -p` doesn't give us.
            Provider::ClaudeCode { .. } => matches!(role, "answer" | "agent"),
        }
    }
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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
    pub fn get(&self, role: &str) -> Option<&Role> {
        self.iter().find(|(n, _)| *n == role).map(|(_, r)| r)
    }

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

#[derive(Debug, Clone, Deserialize, Serialize)]
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
        Self::parse(&Self::read(path)?, path)
    }

    /// The file's text; empty if it doesn't exist.
    pub fn read(path: &Path) -> Result<String, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(t) => Ok(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(source) => Err(ConfigError::Io {
                path: path.into(),
                source,
            }),
        }
    }

    /// `path` is only for error messages.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.into(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    /// For `org.vivus.Settings.Get`.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("config serializes")
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (name, provider) in &self.providers {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(ConfigError::BadProviderName(name.clone()));
            }
            if let Provider::OpenaiCompatible { base_url, .. } = provider
                && !(base_url.starts_with("https://") || base_url.starts_with("http://"))
            {
                return Err(ConfigError::BadBaseUrl {
                    provider: name.clone(),
                    url: base_url.clone(),
                });
            }
        }
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
    fn provider_checks() {
        let bad_url = "[providers.x]\nkind = \"openai-compatible\"\nbase_url = \"x.com\"\n";
        assert!(matches!(
            Config::parse(bad_url, Path::new("c")),
            Err(ConfigError::BadBaseUrl { .. })
        ));
        let bad_name = "[providers.\"a b\"]\nkind = \"claude-code\"\n";
        assert!(matches!(
            Config::parse(bad_name, Path::new("c")),
            Err(ConfigError::BadProviderName(_))
        ));
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
