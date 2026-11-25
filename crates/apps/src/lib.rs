//! Installed applications: desktop entries, matching a spoken name, launching.
//! See docs/desktop-control.md#app-launching.

mod entry;
mod find;
mod launch;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub use entry::App;
pub use find::{Match, name_score};
pub use launch::launch;

/// The visible applications, in XDG precedence order.
#[derive(Debug, Clone, Default)]
pub struct Apps {
    apps: Vec<App>,
    /// `mimeapps.list` files, highest precedence first.
    mimeapps: Vec<PathBuf>,
}

impl Apps {
    /// Scans `$XDG_DATA_HOME` and `$XDG_DATA_DIRS` (a few ms; cheap enough per request).
    pub fn load() -> Self {
        let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .split(':')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let data = data_dirs();
        let mut mimeapps = Vec::new();
        for dir in config_dirs() {
            for d in &desktops {
                mimeapps.push(dir.join(format!("{}-mimeapps.list", d.to_lowercase())));
            }
            mimeapps.push(dir.join("mimeapps.list"));
        }
        mimeapps.extend(data.iter().map(|d| d.join("applications/mimeapps.list")));
        let mut apps = Self::load_from(&data, &entry::locales(), &desktops);
        apps.mimeapps = mimeapps;
        apps
    }

    /// Reads `<dir>/applications` for each data dir; earlier dirs shadow later ones.
    pub fn load_from(data_dirs: &[PathBuf], locales: &[String], desktops: &[String]) -> Self {
        let mut seen = HashSet::new();
        let mut apps = Vec::new();
        for dir in data_dirs {
            let root = dir.join("applications");
            let mut files = Vec::new();
            collect(&root, &mut files);
            files.sort();
            for file in files {
                let Ok(rel) = file.strip_prefix(&root) else {
                    continue;
                };
                let id = rel.to_string_lossy().replace('/', "-");
                // A shadowed ID stays hidden even if the winning file isn't shown.
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&file) else {
                    continue;
                };
                if let Some(app) = entry::parse(id, file, &text, locales, desktops) {
                    apps.push(app);
                }
            }
        }
        Self {
            apps,
            mimeapps: Vec::new(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &App> {
        self.apps.iter()
    }

    pub fn get(&self, id: &str) -> Option<&App> {
        self.apps.iter().find(|a| a.id == id)
    }

    /// The app a window belongs to, from its Wayland app ID: the desktop ID,
    /// else `StartupWMClass`, else the ID's last component (`nautilus`).
    pub fn for_app_id(&self, app_id: &str) -> Option<&App> {
        if app_id.is_empty() {
            return None;
        }
        let eq = |s: &str| s.eq_ignore_ascii_case(app_id);
        self.apps
            .iter()
            .find(|a| eq(a.app_id()))
            .or_else(|| {
                self.apps
                    .iter()
                    .find(|a| a.startup_wm_class.as_deref().is_some_and(eq))
            })
            .or_else(|| {
                self.apps
                    .iter()
                    .find(|a| a.app_id().rsplit('.').next().is_some_and(eq))
            })
    }

    /// The default handler for a MIME type: `mimeapps.list`, else the first app
    /// that declares it.
    pub fn default_for(&self, mime: &str) -> Option<&App> {
        for file in &self.mimeapps {
            let Ok(text) = std::fs::read_to_string(file) else {
                continue;
            };
            let mut in_defaults = false;
            for line in text.lines().map(str::trim) {
                if line.starts_with('[') {
                    in_defaults = line == "[Default Applications]";
                } else if in_defaults
                    && let Some((k, v)) = line.split_once('=')
                    && k.trim() == mime
                    && let Some(app) = v.split(';').find_map(|id| self.get(id.trim()))
                {
                    return Some(app);
                }
            }
        }
        self.apps
            .iter()
            .find(|a| a.mime_types.iter().any(|m| m == mime))
    }

    fn with_category(&self, category: &str) -> Option<&App> {
        self.apps
            .iter()
            .find(|a| a.categories.iter().any(|c| c == category))
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "desktop") {
            out.push(path);
        }
    }
}

fn xdg_dirs(
    home_var: &str,
    home_default: &str,
    dirs_var: &str,
    dirs_default: &str,
) -> Vec<PathBuf> {
    let home = std::env::var_os(home_var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(home_default)));
    let dirs = std::env::var(dirs_var)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| dirs_default.into());
    home.into_iter()
        .chain(dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from))
        .collect()
}

fn data_dirs() -> Vec<PathBuf> {
    xdg_dirs(
        "XDG_DATA_HOME",
        ".local/share",
        "XDG_DATA_DIRS",
        "/usr/local/share:/usr/share",
    )
}

fn config_dirs() -> Vec<PathBuf> {
    xdg_dirs("XDG_CONFIG_HOME", ".config", "XDG_CONFIG_DIRS", "/etc/xdg")
}
