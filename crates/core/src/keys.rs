//! Provider API keys. They never go in `config.toml`: lookup is the
//! environment, then the keyring (Secret Service), then `~/.config/vivus/.env`.
//! The daemon owns the keyring and hands its keys to [`set_keyring`], so this
//! lookup stays synchronous for the code that builds clients.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::RwLock;

use crate::config::{Config, Provider};

/// Where a provider's key comes from, as `org.vivus.Settings.ApiKeyStatus` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Environment,
    Keyring,
    File,
    Missing,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            KeySource::Environment => "environment",
            KeySource::Keyring => "keyring",
            KeySource::File => "file",
            KeySource::Missing => "missing",
        }
    }
}

/// Keyring keys by provider name.
static KEYRING: RwLock<BTreeMap<String, String>> = RwLock::new(BTreeMap::new());

/// Replaces the cached keyring contents (after loading it).
pub fn set_keyring(keys: BTreeMap<String, String>) {
    *KEYRING.write().unwrap() = keys;
}

/// Updates one provider's cached keyring key; `None` removes it.
pub fn set_keyring_key(provider: &str, key: Option<String>) {
    let mut keys = KEYRING.write().unwrap();
    match key {
        Some(k) => keys.insert(provider.to_owned(), k),
        None => keys.remove(provider),
    };
}

/// The variable a provider's key goes by in the environment and `.env`:
/// `api_key_env`, else derived from the name (`nano-gpt` → `NANO_GPT_API_KEY`).
/// `None` for kinds that take no key.
pub fn key_var(name: &str, provider: &Provider) -> Option<String> {
    let Provider::OpenaiCompatible { api_key_env, .. } = provider else {
        return None;
    };
    Some(api_key_env.clone().unwrap_or_else(|| {
        let name: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("{name}_API_KEY")
    }))
}

/// A provider's key and where it came from. The `.env` file is read on each
/// call, so hand edits apply without a restart.
pub fn api_key(name: &str, provider: &Provider) -> (Option<String>, KeySource) {
    let Some(var) = key_var(name, provider) else {
        return (None, KeySource::Missing);
    };
    if let Some(k) = std::env::var(&var).ok().filter(|v| !v.is_empty()) {
        return (Some(k), KeySource::Environment);
    }
    if let Some(k) = KEYRING.read().unwrap().get(name) {
        return (Some(k.clone()), KeySource::Keyring);
    }
    let from_file = Config::env_file_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| parse_env(&text).remove(&var))
        .filter(|v| !v.is_empty());
    match from_file {
        Some(k) => (Some(k), KeySource::File),
        None => (None, KeySource::Missing),
    }
}

/// Sets (`Some`) or removes `var` in the `.env` file at `path`, keeping its
/// other lines. Written mode 0600 via a temp file and rename.
pub fn write_env_var(path: &Path, var: &str, value: Option<&str>) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if value.is_none() {
                return Ok(());
            }
            String::new()
        }
        Err(e) => return Err(e),
    };
    let new = edit_env(&text, var, value);
    if new == text {
        return Ok(());
    }
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = target.with_extension("tmp");
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(new.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &target)
}

/// `text` with every assignment of `var` dropped, and `var=value` appended if set.
fn edit_env(text: &str, var: &str, value: Option<&str>) -> String {
    let mut out: String = text
        .lines()
        .filter(|line| parse_line(line).is_none_or(|(k, _)| k != var))
        .flat_map(|line| [line, "\n"])
        .collect();
    if let Some(v) = value {
        out.push_str(&format!("{var}={}\n", quote(v)));
    }
    out
}

/// Keys are usually plain tokens; quote anything a shell or our parser would split.
fn quote(v: &str) -> String {
    if v.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.:/+=".contains(c))
    {
        v.to_owned()
    } else {
        format!("'{v}'")
    }
}

/// `KEY=value` lines; `#` comments, optional `export ` and surrounding quotes.
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(parse_line)
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn parse_line(line: &str) -> Option<(&str, &str)> {
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
    Some((k, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_parsing() {
        let env = parse_env("# keys\nexport A=1\nB = \"two words\"\nC='x=y'\n\nnot a line\n=bad\n");
        assert_eq!(env.len(), 3);
        assert_eq!(env["A"], "1");
        assert_eq!(env["B"], "two words");
        assert_eq!(env["C"], "x=y");
    }

    #[test]
    fn env_file_editing() {
        let text = "# mine\nA=1\nexport B=old\n";
        assert_eq!(edit_env(text, "B", Some("new")), "# mine\nA=1\nB=new\n");
        assert_eq!(edit_env(text, "B", None), "# mine\nA=1\n");
        assert_eq!(edit_env("", "K", Some("a b")), "K='a b'\n");
        assert_eq!(edit_env("A=1", "K", None), "A=1\n");
    }

    #[test]
    fn writes_private_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vivus-keys-{}", std::process::id()));
        let path = dir.join(".env");
        write_env_var(&path, "K", Some("secret")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "K=secret\n");
        write_env_var(&path, "K", None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn derived_var_names() {
        let p = Provider::OpenaiCompatible {
            base_url: "x".into(),
            api_key_env: None,
        };
        assert_eq!(key_var("nano-gpt", &p).unwrap(), "NANO_GPT_API_KEY");
        let p = Provider::OpenaiCompatible {
            base_url: "x".into(),
            api_key_env: Some("NANOGPT_API_KEY".into()),
        };
        assert_eq!(key_var("nano-gpt", &p).unwrap(), "NANOGPT_API_KEY");
    }
}
