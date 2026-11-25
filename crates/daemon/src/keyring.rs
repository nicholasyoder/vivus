//! API keys in the Secret Service (GNOME Keyring, KWallet, KeePassXC): one item
//! per provider, found by `application = vivus`, `provider = <name>`. Lookup
//! itself is `vivus_core::keys`; this loads its cache and writes keys.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use anyhow::Context;
use oo7::dbus::{Collection, Service};
use vivus_core::config::Provider;
use vivus_core::keys::{self, KeySource};

const APPLICATION: &str = "vivus";

/// How long startup waits for the keyring before going on without it; a
/// locked keyring's unlock prompt can take as long as the user likes.
const STARTUP_WAIT: Duration = Duration::from_secs(3);

fn attributes(provider: Option<&str>) -> HashMap<&str, &str> {
    let mut a = HashMap::from([("application", APPLICATION)]);
    if let Some(p) = provider {
        a.insert("provider", p);
    }
    a
}

/// The default collection, unlocked (which may prompt). `Err` without a Secret Service.
async fn collection() -> anyhow::Result<Collection<'static>> {
    let service = Service::new().await.context("no Secret Service")?;
    let collection = service.default_collection().await?;
    if collection.is_locked().await? {
        collection
            .unlock(None)
            .await
            .context("unlocking the keyring")?;
    }
    Ok(collection)
}

/// Fills the key cache from the keyring. Waits up to [`STARTUP_WAIT`]; a
/// slower load (an unlock prompt) still lands in the cache when it finishes.
pub async fn load() {
    let task = tokio::spawn(async {
        match read_all().await {
            Ok(found) => {
                tracing::info!("keyring: {} API key(s)", found.len());
                keys::set_keyring(found);
            }
            Err(e) => tracing::info!("keyring unavailable, keys come from .env: {e:#}"),
        }
    });
    if tokio::time::timeout(STARTUP_WAIT, task).await.is_err() {
        tracing::info!("keyring is slow to answer (locked?); continuing without it for now");
    }
}

async fn read_all() -> anyhow::Result<BTreeMap<String, String>> {
    let collection = collection().await?;
    let mut found = BTreeMap::new();
    for item in collection.search_items(&attributes(None)).await? {
        let attrs = item.attributes().await?;
        let Some(provider) = attrs.get("provider") else {
            continue;
        };
        let secret = item.secret().await?;
        match std::str::from_utf8(secret.as_bytes()) {
            Ok(k) => {
                found.insert(provider.clone(), k.to_owned());
            }
            Err(_) => tracing::warn!("keyring key for `{provider}` isn't text; ignored"),
        }
    }
    Ok(found)
}

/// Stores (`Some`) or removes `name`'s key: in the keyring, or `.env` without
/// one. Removing clears both, and storing in the keyring drops a `.env` copy,
/// so the status reports where the key really is. Returns where it went.
pub async fn store(
    name: &str,
    provider: &Provider,
    key: Option<&str>,
) -> anyhow::Result<KeySource> {
    let var = keys::key_var(name, provider).context("this provider takes no API key")?;
    let env_file = vivus_core::Config::env_file_path().context("HOME is unset")?;
    let collection = collection().await;
    if let Err(e) = &collection {
        tracing::info!("keyring unavailable, using {}: {e:#}", env_file.display());
    }
    let source = match (collection, key) {
        (Ok(c), Some(k)) => {
            let label = format!("Vivus API key: {name}");
            c.create_item(&label, &attributes(Some(name)), k, true, None)
                .await
                .context("saving to the keyring")?;
            keys::write_env_var(&env_file, &var, None)?;
            KeySource::Keyring
        }
        (Err(_), Some(k)) => {
            keys::write_env_var(&env_file, &var, Some(k))
                .with_context(|| format!("writing {}", env_file.display()))?;
            KeySource::File
        }
        (c, None) => {
            if let Ok(c) = c {
                for item in c.search_items(&attributes(Some(name))).await? {
                    item.delete(None)
                        .await
                        .context("removing from the keyring")?;
                }
            }
            keys::write_env_var(&env_file, &var, None)?;
            KeySource::Missing
        }
    };
    keys::set_keyring_key(
        name,
        (source == KeySource::Keyring).then(|| key.unwrap().to_owned()),
    );
    Ok(source)
}

/// What `SetApiKey` accepts: one line, no quotes (it may land in `.env`).
pub fn check_key(key: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !key.chars()
            .any(|c| c.is_control() || c == '\'' || c == '"' || c == ' '),
        "an API key can't contain spaces, quotes or line breaks"
    );
    Ok(())
}
