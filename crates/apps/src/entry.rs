//! Desktop entry files (freedesktop Desktop Entry Specification 1.5).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One launchable application from a `.desktop` file.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    /// Desktop file ID, e.g. `org.gnome.Nautilus.desktop`.
    pub id: String,
    /// `Name`, localized.
    pub name: String,
    /// `Name` before localization, when it differs.
    pub name_c: Option<String>,
    pub generic_name: Option<String>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    pub mime_types: Vec<String>,
    pub exec: String,
    pub icon: Option<String>,
    /// Working directory.
    pub path: Option<PathBuf>,
    pub terminal: bool,
    pub file: PathBuf,
}

impl App {
    /// The ID without `.desktop`, which is also the expected Wayland app ID.
    pub fn app_id(&self) -> &str {
        self.id.strip_suffix(".desktop").unwrap_or(&self.id)
    }
}

/// The keys of the `[Desktop Entry]` group, values still escaped.
fn group(text: &str) -> HashMap<&str, &str> {
    let mut keys = HashMap::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if inside {
                break;
            }
            inside = line == "[Desktop Entry]";
        } else if inside
            && !line.starts_with('#')
            && let Some((k, v)) = line.split_once('=')
        {
            keys.entry(k.trim()).or_insert(v.trim());
        }
    }
    keys
}

/// Locale suffixes to try for `Key[...]`, most specific first.
pub fn locales() -> Vec<String> {
    let var = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default();
    let base = var.split(['.', '@']).next().unwrap_or("");
    if base.is_empty() || base == "C" || base == "POSIX" {
        return Vec::new();
    }
    let mut out = vec![base.to_owned()];
    if let Some((lang, _)) = base.split_once('_') {
        out.push(lang.to_owned());
    }
    out
}

/// Parses a desktop file; `None` if it isn't a visible, runnable application
/// for `desktops` (`XDG_CURRENT_DESKTOP`, split on `:`).
pub fn parse(
    id: String,
    file: PathBuf,
    text: &str,
    locales: &[String],
    desktops: &[String],
) -> Option<App> {
    let g = group(text);
    let get = |k: &str| g.get(k).map(|v| unescape(v));
    let localized = |k: &str| {
        locales
            .iter()
            .find_map(|l| g.get(format!("{k}[{l}]").as_str()))
            .map(|v| unescape(v))
            .or_else(|| get(k))
    };
    let flag = |k: &str| g.get(k).is_some_and(|v| *v == "true");
    let list = |v: Option<String>| v.map(|v| split_list(&v)).unwrap_or_default();

    if g.get("Type") != Some(&"Application") || flag("Hidden") || flag("NoDisplay") {
        return None;
    }
    let shown_in = |k: &str| list(get(k)).iter().any(|d| desktops.contains(d));
    if g.contains_key("OnlyShowIn") && !shown_in("OnlyShowIn") {
        return None;
    }
    if shown_in("NotShowIn") {
        return None;
    }
    if let Some(try_exec) = get("TryExec")
        && find_program(&try_exec).is_none()
    {
        return None;
    }
    let name = localized("Name")?;
    let name_c = get("Name").filter(|n| *n != name);
    Some(App {
        id,
        name,
        name_c,
        generic_name: localized("GenericName"),
        keywords: list(localized("Keywords")),
        categories: list(get("Categories")),
        mime_types: list(get("MimeType")),
        exec: get("Exec")?,
        icon: get("Icon"),
        path: get("Path").filter(|p| !p.is_empty()).map(PathBuf::from),
        terminal: flag("Terminal"),
        file,
    })
}

/// Undoes the spec's string escapes (`\s`, `\n`, `\t`, `\r`, `\\`). `\;` is
/// kept for [`split_list`].
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn split_list(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(';') => cur.push(';'),
                Some(o) => {
                    cur.push('\\');
                    cur.push(o);
                }
                None => {}
            },
            ';' => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_owned());
                }
                cur.clear();
            }
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    out
}

/// Splits an `Exec` value into argv and expands field codes, with no files or URLs.
pub fn exec_args(app: &App) -> Vec<String> {
    let mut args = Vec::new();
    for word in split_exec(&app.exec) {
        match word.as_str() {
            "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            "%i" => {
                if let Some(icon) = &app.icon {
                    args.extend(["--icon".to_owned(), icon.clone()]);
                }
            }
            _ => args.push(expand_codes(&word, app)),
        }
    }
    args
}

/// Field codes inside a longer argument (not allowed by the spec, but seen).
fn expand_codes(word: &str, app: &App) -> String {
    let mut out = String::new();
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('c') => out.push_str(&app.name),
            Some('k') => out.push_str(&app.file.to_string_lossy()),
            _ => {}
        }
    }
    out
}

/// The spec's quoting: double quotes, inside which `\` escapes `"`, `` ` ``, `$` and `\`.
fn split_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            '\\' if quoted => match chars.next() {
                Some(e @ ('"' | '`' | '$' | '\\')) => cur.push(e),
                Some(o) => {
                    cur.push('\\');
                    cur.push(o);
                }
                None => cur.push('\\'),
            },
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    args.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        args.push(cur);
    }
    args
}

/// An absolute path, or the first match on `PATH`.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let executable = |p: &Path| {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        let p = PathBuf::from(name);
        return executable(&p).then_some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| executable(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(text: &str) -> Option<App> {
        parse(
            "t.desktop".into(),
            "/x/t.desktop".into(),
            text,
            &["de_DE".into(), "de".into()],
            &["sway".into()],
        )
    }

    #[test]
    fn parses_localized_fields() {
        let a = app(
            "[Desktop Entry]\nType=Application\nName=Files\nName[de]=Dateien\n\
                     Keywords=folder;manager\\;x;\nExec=nautilus --new-window %U\n\
                     [Desktop Action new]\nName=Other\n",
        )
        .unwrap();
        assert_eq!(a.name, "Dateien");
        assert_eq!(a.name_c.as_deref(), Some("Files"));
        assert_eq!(a.keywords, ["folder", "manager;x"]);
        assert_eq!(exec_args(&a), ["nautilus", "--new-window"]);
    }

    #[test]
    fn skips_hidden_and_other_desktops() {
        let base = "[Desktop Entry]\nType=Application\nName=X\nExec=x\n";
        assert!(app(base).is_some());
        assert!(app(&format!("{base}NoDisplay=true\n")).is_none());
        assert!(app(&format!("{base}OnlyShowIn=GNOME;KDE;\n")).is_none());
        assert!(app(&format!("{base}OnlyShowIn=sway;\n")).is_some());
        assert!(app(&format!("{base}NotShowIn=sway;\n")).is_none());
        assert!(app(&format!("{base}TryExec=/nonexistent/x\n")).is_none());
        assert!(app("[Desktop Entry]\nType=Link\nName=X\nURL=u\n").is_none());
    }

    #[test]
    fn splits_exec_quoting() {
        let mut a = app("[Desktop Entry]\nType=Application\nName=Pad\nIcon=pad\nExec=x\n").unwrap();
        a.exec = r#"sh -c "echo \"hi\" \$HOME" --name=%c %i %%u"#.into();
        assert_eq!(
            exec_args(&a),
            [
                "sh",
                "-c",
                "echo \"hi\" $HOME",
                "--name=Pad",
                "--icon",
                "pad",
                "%u"
            ]
        );
        a.exec = r#"x "" a"#.into();
        assert_eq!(exec_args(&a), ["x", "", "a"]);
    }
}
