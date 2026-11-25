//! Matching a spoken app name. Transcripts split and mishear names ("fire
//! fox", "lever office"), so names are compared without spaces and with an
//! edit-distance fallback. Generic names ("my browser") go to the default handler.

use crate::{App, Apps};

#[derive(Debug, PartialEq)]
pub enum Match<'a> {
    Found(&'a App),
    /// Several apps fit about equally well, best first.
    Ambiguous(Vec<&'a App>),
    /// Nothing close enough; the nearest few, if any.
    NotFound(Vec<&'a App>),
}

/// Words that say nothing about which app.
const FILLER: &[&str] = &[
    "a",
    "an",
    "the",
    "my",
    "app",
    "application",
    "program",
    "open",
    "launch",
    "start",
    "run",
    "please",
    "up",
];

enum Handler {
    Mime(&'static str),
    Category(&'static str),
}
use Handler::{Category, Mime};

/// Generic names (compacted) → how to find the user's app for them, in order.
const GENERIC: &[(&[&str], &[Handler])] = &[
    (
        &["browser", "webbrowser", "internet", "internetbrowser"],
        &[
            Mime("x-scheme-handler/https"),
            Mime("text/html"),
            Category("WebBrowser"),
        ],
    ),
    (
        &[
            "filemanager",
            "files",
            "filebrowser",
            "fileexplorer",
            "explorer",
        ],
        &[Mime("inode/directory"), Category("FileManager")],
    ),
    (
        &["terminal", "console", "terminalemulator"],
        &[Category("TerminalEmulator")],
    ),
    (
        &["texteditor", "editor", "notepad"],
        &[Mime("text/plain"), Category("TextEditor")],
    ),
    (
        &["email", "mail", "emailclient", "mailclient"],
        &[Mime("x-scheme-handler/mailto"), Category("Email")],
    ),
    (
        &["musicplayer", "music", "audioplayer"],
        &[Mime("audio/mpeg"), Mime("audio/flac")],
    ),
    (
        &["videoplayer", "mediaplayer", "video", "videos"],
        &[Mime("video/mp4")],
    ),
    (
        &["imageviewer", "photoviewer", "photos", "pictures", "images"],
        &[Mime("image/png"), Mime("image/jpeg")],
    ),
    (
        &["pdfviewer", "pdfreader", "documentviewer"],
        &[Mime("application/pdf")],
    ),
    (&["calculator"], &[Category("Calculator")]),
];

const ENOUGH: f32 = 0.6;
/// Scores this close to the best are a tie.
const TIE: f32 = 0.05;

impl Apps {
    pub fn find(&self, query: &str) -> Match<'_> {
        let words = words(query);
        let words: Vec<&str> = words
            .iter()
            .map(String::as_str)
            .filter(|w| !FILLER.contains(w))
            .collect();
        let compact = words.concat();
        if compact.is_empty() {
            return Match::NotFound(Vec::new());
        }
        let mut scored: Vec<(f32, &App)> = Vec::new();
        for app in &self.apps {
            // Native and Flatpak copies of one app usually share a name.
            if scored.iter().any(|(_, a)| a.name == app.name) {
                continue;
            }
            let s = score(app, &words, &compact);
            if s > 0.3 {
                scored.push((s, app));
            }
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        let best = scored.first().map_or(0.0, |s| s.0);
        if best < 0.95
            && let Some((_, handlers)) = GENERIC
                .iter()
                .find(|(names, _)| names.contains(&compact.as_str()))
            && let Some(app) = handlers.iter().find_map(|h| match h {
                Mime(m) => self.default_for(m),
                Category(c) => self.with_category(c),
            })
        {
            return Match::Found(app);
        }
        if best < ENOUGH {
            return Match::NotFound(scored.iter().take(3).map(|s| s.1).collect());
        }
        let tied: Vec<&App> = scored
            .iter()
            .take_while(|(s, _)| *s >= best - TIE)
            .map(|s| s.1)
            .take(5)
            .collect();
        // An exact name beats near misses even if they're close.
        if tied.len() == 1 || (best >= 1.0 && scored[1].0 < 1.0) {
            Match::Found(tied[0])
        } else {
            Match::Ambiguous(tied)
        }
    }
}

/// How well a name or title fits a spoken query, 0 to 1, with the same rules
/// as app matching (filler words dropped, spaces ignored, edit distance).
pub fn name_score(field: &str, query: &str) -> f32 {
    let words = words(query);
    let words: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| !FILLER.contains(w))
        .collect();
    let compact = words.concat();
    if compact.is_empty() {
        return 0.0;
    }
    field_score(field, &words, &compact)
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// How well `app` fits the query, 0 to 1.
fn score(app: &App, query: &[&str], compact: &str) -> f32 {
    let mut fields: Vec<(String, f32)> = vec![(app.name.clone(), 1.0)];
    fields.extend(app.name_c.iter().map(|n| (n.clone(), 1.0)));
    fields.extend(app.generic_name.iter().map(|n| (n.clone(), 0.9)));
    fields.extend(app.keywords.iter().map(|k| (k.clone(), 0.8)));
    // `org.gnome.Nautilus` → `Nautilus`; `brave-browser` stays whole.
    let id_tail = app.app_id().rsplit('.').next().unwrap_or("");
    fields.push((id_tail.to_owned(), 0.95));
    if let Some(program) = program(&app.exec) {
        fields.push((program, 0.95));
    }
    fields
        .iter()
        .map(|(field, weight)| field_score(field, query, compact) * weight)
        .fold(0.0, f32::max)
}

fn field_score(field: &str, query: &[&str], compact: &str) -> f32 {
    let fwords = words(field);
    let fcompact = fwords.concat();
    if fcompact.is_empty() {
        return 0.0;
    }
    if fcompact == compact {
        return 1.0;
    }
    if compact.chars().count() >= 3 && fcompact.starts_with(compact) {
        return 0.85;
    }
    if query.iter().all(|q| fwords.iter().any(|f| f == q)) {
        return 0.8;
    }
    let sim = similarity(compact, &fcompact);
    if sim >= 0.7 { sim * 0.9 } else { 0.0 }
}

/// The program's file name, skipping `env VAR=...`.
fn program(exec: &str) -> Option<String> {
    let mut parts = exec.split_whitespace();
    let mut first = parts.next()?;
    if first.ends_with("/env") || first == "env" {
        first = parts.find(|p| !p.contains('=') && !p.starts_with('-'))?;
    }
    Some(first.rsplit('/').next()?.trim_matches('"').to_owned())
}

/// 1 minus the edit distance over the longer length.
fn similarity(a: &str, b: &str) -> f32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = if ca == cb {
                prev
            } else {
                1 + prev.min(cur).min(row[j])
            };
            prev = cur;
        }
    }
    1.0 - row[b.len()] as f32 / a.len().max(b.len()) as f32
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn app(id: &str, name: &str, extra: &str) -> App {
        let text = format!("[Desktop Entry]\nType=Application\nName={name}\n{extra}");
        let mut a = crate::entry::parse(id.into(), PathBuf::new(), &text, &[], &[]).unwrap();
        a.id = id.into();
        a
    }

    fn apps() -> Apps {
        Apps {
            apps: vec![
                app(
                    "firefox.desktop",
                    "Firefox",
                    "GenericName=Web Browser\nExec=firefox %u\nCategories=Network;WebBrowser;\nMimeType=text/html;x-scheme-handler/https;",
                ),
                app(
                    "brave-browser.desktop",
                    "Brave Web Browser",
                    "Exec=/usr/bin/brave-browser-stable %U\nCategories=Network;WebBrowser;",
                ),
                app(
                    "org.gnome.Nautilus.desktop",
                    "Files",
                    "Exec=nautilus --new-window %U\nMimeType=inode/directory;",
                ),
                app(
                    "libreoffice-writer.desktop",
                    "LibreOffice Writer",
                    "Exec=libreoffice --writer %U",
                ),
                app(
                    "libreoffice-calc.desktop",
                    "LibreOffice Calc",
                    "Exec=libreoffice --calc %U",
                ),
                app(
                    "org.pulseaudio.pavucontrol.desktop",
                    "Volume Control",
                    "GenericName=Volume Control\nExec=pavucontrol",
                ),
                app(
                    "com.microsoft.VSCode.desktop",
                    "Visual Studio Code",
                    "Exec=env FOO=1 /usr/share/code/code %F\nKeywords=vscode;",
                ),
                app(
                    "gimp.desktop",
                    "GNU Image Manipulation Program",
                    "Exec=gimp %U",
                ),
                app(
                    "qterminal.desktop",
                    "QTerminal",
                    "Exec=qterminal\nCategories=System;TerminalEmulator;",
                ),
            ],
            mimeapps: Vec::new(),
        }
    }

    fn found(q: &str) -> String {
        match apps().find(q) {
            Match::Found(a) => a.id.clone(),
            other => panic!("{q}: {other:?}"),
        }
    }

    #[test]
    fn matches_names_as_heard() {
        assert_eq!(found("Firefox"), "firefox.desktop");
        assert_eq!(found("fire fox"), "firefox.desktop");
        assert_eq!(found("firfox"), "firefox.desktop");
        assert_eq!(found("open the brave app"), "brave-browser.desktop");
        assert_eq!(found("nautilus"), "org.gnome.Nautilus.desktop");
        assert_eq!(found("pavu control"), "org.pulseaudio.pavucontrol.desktop");
        assert_eq!(found("code"), "com.microsoft.VSCode.desktop");
        assert_eq!(found("VS Code"), "com.microsoft.VSCode.desktop");
        assert_eq!(found("gimp"), "gimp.desktop");
        assert_eq!(found("writer"), "libreoffice-writer.desktop");
    }

    #[test]
    fn generic_names_use_defaults() {
        assert_eq!(found("my browser"), "firefox.desktop");
        assert_eq!(found("file manager"), "org.gnome.Nautilus.desktop");
        assert_eq!(found("terminal"), "qterminal.desktop");
    }

    #[test]
    fn ties_and_misses() {
        match apps().find("libreoffice") {
            Match::Ambiguous(a) => assert_eq!(a.len(), 2),
            other => panic!("{other:?}"),
        }
        assert!(matches!(apps().find("blender"), Match::NotFound(_)));
        assert!(matches!(apps().find("the"), Match::NotFound(v) if v.is_empty()));
    }
}
