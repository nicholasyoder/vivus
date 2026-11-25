//! The config file on disk: writing edits from `org.vivus.Settings` and
//! watching for hand edits. The engine decides what to apply.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use notify::Watcher;
use tokio::sync::mpsc::{self, UnboundedSender};
use vivus_core::settings::{Array, Value};
use zbus::zvariant;

use crate::engine::Command;

/// Writes beside the real file (through a symlink, so dotfile links survive),
/// then renames over it, so a reader never sees half a file.
pub fn write(path: &Path, text: &str) -> anyhow::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let dir = target.parent().context("config path has no directory")?;
    std::fs::create_dir_all(dir)?;
    let tmp = target.with_extension("toml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &target).with_context(|| format!("replacing {}", target.display()))
}

/// Sends `Command::ConfigFileChanged` when the file may have changed. Watches
/// the directory (and a symlink target's), since editors save by renaming.
pub fn watch(
    path: &Path,
    tx: UnboundedSender<Command>,
) -> anyhow::Result<notify::RecommendedWatcher> {
    let mut dirs = vec![
        path.parent()
            .context("config path has no directory")?
            .to_owned(),
    ];
    std::fs::create_dir_all(&dirs[0])?;
    if let Some(dir) = std::fs::canonicalize(path)
        .ok()
        .and_then(|t| t.parent().map(PathBuf::from))
        && dir != dirs[0]
    {
        dirs.push(dir);
    }
    let names: Vec<_> = [Some(path.to_owned()), std::fs::canonicalize(path).ok()]
        .into_iter()
        .flatten()
        .filter_map(|p| p.file_name().map(|n| n.to_owned()))
        .collect();

    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && !ev.kind.is_access()
            && ev
                .paths
                .iter()
                .any(|p| p.file_name().is_some_and(|n| names.iter().any(|m| m == n)))
        {
            let _ = raw_tx.send(());
        }
    })?;
    for dir in &dirs {
        watcher.watch(dir, notify::RecursiveMode::NonRecursive)?;
    }
    tokio::spawn(async move {
        while raw_rx.recv().await.is_some() {
            // A save is several events; read once they settle.
            tokio::time::sleep(Duration::from_millis(150)).await;
            while raw_rx.try_recv().is_ok() {}
            if tx.send(Command::ConfigFileChanged).is_err() {
                break;
            }
        }
    });
    Ok(watcher)
}

/// Settings that take effect only when the daemon restarts (speech threads,
/// audio graph and portal shortcuts are set up once).
pub fn needs_restart(path: &str) -> bool {
    ["audio.", "shortcuts.", "roles.stt.", "roles.tts."]
        .iter()
        .any(|p| path.starts_with(p))
        || path.starts_with("providers.")
            && (path.ends_with(".models_dir") || path.ends_with(".threads"))
}

/// D-Bus variant → TOML value: strings, booleans, numbers and arrays of them.
pub fn to_toml(v: &zvariant::Value<'_>) -> Result<Value, String> {
    use zvariant::Value as V;
    Ok(match v {
        V::Value(inner) => return to_toml(inner),
        V::Bool(b) => (*b).into(),
        V::Str(s) => s.as_str().into(),
        V::U8(n) => i64::from(*n).into(),
        V::I16(n) => i64::from(*n).into(),
        V::U16(n) => i64::from(*n).into(),
        V::I32(n) => i64::from(*n).into(),
        V::U32(n) => i64::from(*n).into(),
        V::I64(n) => (*n).into(),
        V::U64(n) => i64::try_from(*n).map_err(|_| "integer too large")?.into(),
        V::F64(n) => (*n).into(),
        V::Array(a) => Value::Array(a.iter().map(to_toml).collect::<Result<Array, _>>()?),
        other => {
            return Err(format!(
                "unsupported value type `{}`",
                other.value_signature()
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_conversion() {
        let v = zvariant::Value::from(vec!["a", "b"]);
        assert_eq!(to_toml(&v).unwrap().to_string(), r#"["a", "b"]"#);
        let nested = zvariant::Value::Value(Box::new(zvariant::Value::from(0.5)));
        assert_eq!(to_toml(&nested).unwrap().as_float(), Some(0.5));
        assert_eq!(
            to_toml(&zvariant::Value::from(7u32)).unwrap().as_integer(),
            Some(7)
        );
        assert!(
            to_toml(&zvariant::Value::from(
                zvariant::ObjectPath::from_static_str_unchecked("/a")
            ))
            .is_err()
        );
    }

    #[test]
    fn restart_paths() {
        assert!(needs_restart("audio.input"));
        assert!(needs_restart("providers.local-speech.threads"));
        assert!(!needs_restart("roles.router.model"));
        assert!(!needs_restart("debug.transcribe_only"));
    }
}
