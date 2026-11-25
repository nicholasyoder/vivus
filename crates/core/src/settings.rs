//! Editing `config.toml` one key at a time (`org.vivus.Settings`), keeping the
//! file's comments and layout, and working out which settings a change touched.

use std::path::Path;

use serde_json::Value as Json;
use toml_edit::{DocumentMut, Item, Table, TableLike};

use crate::config::{Config, ConfigError};

pub use toml_edit::{Array, Value};

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("invalid settings path `{0}`")]
    BadPath(String),
    #[error("`{0}` is not a table")]
    NotATable(String),
    #[error("the config file has syntax errors; fix it first: {0}")]
    Syntax(#[from] toml_edit::TomlError),
    /// The edited file doesn't deserialize, e.g. a wrong type or unknown key.
    #[error("{0}")]
    Rejected(String),
    #[error(transparent)]
    Invalid(#[from] ConfigError),
}

/// Sets (`Some`) or removes (`None`) the dotted key `path` in the config text.
/// Returns the new text and the config it parses to; nothing is written.
pub fn edit(
    text: &str,
    file: &Path,
    path: &str,
    value: Option<Value>,
) -> Result<(String, Config), SettingsError> {
    let keys: Vec<&str> = path.split('.').collect();
    if keys.iter().any(|k| k.trim().is_empty()) {
        return Err(SettingsError::BadPath(path.into()));
    }
    let mut doc: DocumentMut = text.parse()?;
    let (last, parents) = keys.split_last().unwrap();
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for (i, key) in parents.iter().enumerate() {
        // Removing under a missing table: nothing to do, and creating it empty
        // could make it invalid (a role needs a provider).
        if value.is_none() && !table.contains_key(key) {
            return finish(doc, file);
        }
        let item = table.entry(key).or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        });
        table = item
            .as_table_like_mut()
            .ok_or_else(|| SettingsError::NotATable(keys[..=i].join(".")))?;
    }
    match (value, table.get_mut(last)) {
        // Keep the old value's surrounding whitespace and trailing comment.
        (Some(mut v), Some(Item::Value(old))) => {
            *v.decor_mut() = old.decor().clone();
            *old = v;
        }
        (Some(v), _) => {
            table.insert(last, Item::Value(v));
        }
        (None, _) => {
            table.remove(last);
        }
    }
    finish(doc, file)
}

fn finish(doc: DocumentMut, file: &Path) -> Result<(String, Config), SettingsError> {
    let text = doc.to_string();
    let config = Config::parse(&text, file).map_err(|e| match e {
        // Without the file path and line, which say nothing to a settings UI.
        ConfigError::Parse { source, .. } => SettingsError::Rejected(source.message().into()),
        e => e.into(),
    })?;
    Ok((text, config))
}

/// Dotted leaf paths whose effective value differs (arrays count as leaves).
pub fn changed_paths(old: &Config, new: &Config) -> Vec<String> {
    let old = serde_json::to_value(old).expect("config serializes");
    let new = serde_json::to_value(new).expect("config serializes");
    let mut out = Vec::new();
    diff(&old, &new, String::new(), &mut out);
    out
}

fn diff(old: &Json, new: &Json, prefix: String, out: &mut Vec<String>) {
    let join = |k: &str| {
        if prefix.is_empty() {
            k.to_owned()
        } else {
            format!("{prefix}.{k}")
        }
    };
    match (old, new) {
        (Json::Object(a), Json::Object(b)) => {
            for (k, va) in a {
                diff(va, b.get(k).unwrap_or(&Json::Null), join(k), out);
            }
            for (k, vb) in b.iter().filter(|(k, _)| !a.contains_key(*k)) {
                diff(&Json::Null, vb, join(k), out);
            }
        }
        // A table appearing or vanishing (a role or provider): report its leaves.
        (Json::Object(a), Json::Null) => {
            for (k, va) in a {
                diff(va, &Json::Null, join(k), out);
            }
        }
        (Json::Null, Json::Object(b)) => {
            for (k, vb) in b {
                diff(&Json::Null, vb, join(k), out);
            }
        }
        _ if old != new => out.push(prefix),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# my config
[audio]
# seconds
end_of_turn_silence = 0.4 # short
echo_cancel = true
";

    fn set(text: &str, path: &str, v: Option<Value>) -> Result<(String, Config), SettingsError> {
        edit(text, Path::new("config.toml"), path, v)
    }

    #[test]
    fn keeps_comments() {
        let (text, config) = set(FILE, "audio.end_of_turn_silence", Some(0.6.into())).unwrap();
        assert_eq!(text, FILE.replace("0.4", "0.6"));
        assert_eq!(config.audio.end_of_turn_silence, 0.6);
    }

    #[test]
    fn creates_tables() {
        let (text, config) = set("", "audio.input", Some("mic".into())).unwrap();
        assert_eq!(text, "[audio]\ninput = \"mic\"\n");
        assert_eq!(config.audio.input.as_deref(), Some("mic"));
    }

    #[test]
    fn reset_removes() {
        let (text, config) = set(FILE, "audio.echo_cancel", None).unwrap();
        assert!(!text.contains("echo_cancel"));
        assert!(config.audio.echo_cancel); // the default
    }

    #[test]
    fn reset_under_missing_table() {
        let (text, _) = set(FILE, "roles.tts.voice", None).unwrap();
        assert_eq!(text, FILE);
        let text = "[providers.l]\nkind = \"local-speech\"\n[roles.tts]\nprovider = \"l\"\n";
        let (text, config) = set(text, "roles.tts", None).unwrap();
        assert!(!text.contains("roles") && config.roles.tts.is_none());
    }

    #[test]
    fn integer_for_float() {
        let (_, config) = set(FILE, "audio.end_of_turn_silence", Some(1.into())).unwrap();
        assert_eq!(config.audio.end_of_turn_silence, 1.0);
    }

    #[test]
    fn rejects_bad_edits() {
        assert!(matches!(
            set(FILE, "audio.nope", Some(true.into())),
            Err(SettingsError::Rejected(m)) if m.starts_with("unknown field `nope`")
        ));
        assert!(matches!(
            set(FILE, "audio.echo_cancel", Some("yes".into())),
            Err(SettingsError::Rejected(_))
        ));
        assert!(matches!(
            set(FILE, "roles.router.provider", Some("missing".into())),
            Err(SettingsError::Invalid(ConfigError::UnknownProvider { .. }))
        ));
        assert!(matches!(
            set(FILE, "audio.echo_cancel.x", Some(true.into())),
            Err(SettingsError::NotATable(p)) if p == "audio.echo_cancel"
        ));
        assert!(matches!(
            set(FILE, "audio..x", None),
            Err(SettingsError::BadPath(_))
        ));
        assert!(matches!(
            set("[audio", "audio.input", None),
            Err(SettingsError::Syntax(_))
        ));
    }

    #[test]
    fn diff_paths() {
        let old = Config::default();
        let (_, new) = set(FILE, "audio.input", Some("mic".into())).unwrap();
        assert_eq!(changed_paths(&old, &new), ["audio.input"]);
        assert!(changed_paths(&new, &new).is_empty());

        let text = "[providers.l]\nkind = \"local-speech\"\n[roles.stt]\nprovider = \"l\"\n";
        let (_, new) = set(text, "roles.stt.model", Some("m".into())).unwrap();
        let mut paths = changed_paths(&old, &new);
        paths.sort();
        assert_eq!(
            paths,
            [
                "providers.l.kind",
                "providers.l.threads",
                "roles.stt.model",
                "roles.stt.provider",
            ]
        );
    }
}
