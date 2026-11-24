//! Starting an app the way desktop launchers do: in its own systemd scope (so
//! stopping the daemon's unit doesn't take it down) with the activation token
//! in its environment.

use std::process::{Command, Stdio};

use crate::entry::{exec_args, find_program};
use crate::{App, Apps};

/// Starts `app` detached. `token` is an `xdg-activation` token for it, if the
/// compositor gave one.
pub fn launch(apps: &Apps, app: &App, token: Option<&str>) -> std::io::Result<()> {
    let mut argv = exec_args(app);
    if argv.is_empty() {
        return Err(std::io::Error::other("empty Exec"));
    }
    if app.terminal {
        argv.splice(0..0, terminal(apps).ok_or_else(|| std::io::Error::other("no terminal emulator"))?);
    }
    if systemd_user() && find_program("systemd-run").is_some() {
        let unit = format!("--unit=app-vivus-{}-{}", escape_unit(app.app_id()), random());
        let scope = ["systemd-run", "--user", "--scope", "--quiet", "--collect", &unit];
        argv.splice(0..0, scope.map(str::to_owned));
    }
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(dir) = &app.path {
        cmd.current_dir(dir);
    }
    if let Some(t) = token {
        cmd.env("XDG_ACTIVATION_TOKEN", t).env("DESKTOP_STARTUP_ID", t);
    }
    use std::os::unix::process::CommandExt;
    let mut child = cmd.spawn()?;
    tracing::info!("launched {} ({})", app.id, argv.join(" "));
    // Reaped on a thread so it doesn't linger as a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// The command prefix that runs a program in a terminal.
fn terminal(apps: &Apps) -> Option<Vec<String>> {
    if find_program("xdg-terminal-exec").is_some() {
        return Some(vec!["xdg-terminal-exec".into()]);
    }
    let term = std::env::var("TERMINAL")
        .ok()
        .filter(|t| find_program(t).is_some())
        .or_else(|| {
            let app = apps.with_category("TerminalEmulator")?;
            exec_args(app).into_iter().next()
        })?;
    // `-e` works for nearly all of them; GNOME's and wezterm's want other forms.
    let flag = match term.rsplit('/').next() {
        Some("gnome-terminal" | "kgx" | "ptyxis") => "--",
        Some("wezterm") => "start",
        _ => "-e",
    };
    Some(vec![term, flag.into()])
}

/// A user manager to put the scope in.
fn systemd_user() -> bool {
    std::env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|d| std::path::Path::new(&d).join("systemd/private").exists())
}

/// systemd's unit-name escaping, as the XDG app-scope convention uses (`-` → `\x2d`).
fn escape_unit(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | ':' | '_' | '.' => c.to_string(),
            c => format!("\\x{:02x}", c as u32 & 0xff),
        })
        .collect()
}

fn random() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("{:x}{:x}", std::process::id(), nanos)
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_unit_names() {
        assert_eq!(super::escape_unit("brave-browser"), "brave\\x2dbrowser");
        assert_eq!(super::escape_unit("org.gnome.Files"), "org.gnome.Files");
    }
}
