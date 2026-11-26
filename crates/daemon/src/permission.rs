//! Human-readable descriptions and voice yes/no matching for Claude Code
//! permission prompts.

use std::path::Path;

use serde_json::Value;

/// How a permission prompt is presented in the sidebar and spoken aloud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDisplay {
    pub tool: String,
    /// Short one-line description for the sidebar header.
    pub summary: String,
    /// Full details shown in the sidebar (e.g. the shell command or file path).
    pub detail: String,
    /// Short spoken question ("Claude wants to … — OK?").
    pub spoken: String,
}

/// Formats a tool call for the sidebar card and TTS.
pub fn describe(tool: &str, input: &Value) -> PermissionDisplay {
    let (summary, detail, clause) = match tool {
        "Bash" => {
            let cmd = input["command"].as_str().unwrap_or("").trim();
            let desc = input["description"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let first = first_line(cmd);
            let summary = match desc {
                Some(d) => format!("Run: {}", truncate(d, 80)),
                None if !first.is_empty() => format!("Run `{}`", truncate(first, 80)),
                None => "Run a shell command".into(),
            };
            let detail = if cmd.is_empty() {
                pretty(input)
            } else {
                cmd.to_owned()
            };
            let clause = match desc {
                Some(d) => spoken_desc(d),
                None => spoken_command(first),
            };
            (summary, detail, clause)
        }
        "Edit" | "MultiEdit" | "NotebookEdit" => {
            let path = input["file_path"]
                .as_str()
                .or_else(|| input["notebook_path"].as_str())
                .unwrap_or("");
            let summary = if path.is_empty() {
                "Edit a file".into()
            } else {
                format!("Edit {}", short_path(path))
            };
            let detail = if path.is_empty() {
                pretty(input)
            } else {
                path.to_owned()
            };
            let clause = format!("edit {}", spoken_path(path));
            (summary, detail, clause)
        }
        "Write" => {
            let path = input["file_path"].as_str().unwrap_or("");
            let summary = if path.is_empty() {
                "Write a file".into()
            } else {
                format!("Write {}", short_path(path))
            };
            let detail = if path.is_empty() {
                pretty(input)
            } else {
                path.to_owned()
            };
            let clause = format!("write {}", spoken_path(path));
            (summary, detail, clause)
        }
        "Read" => {
            let path = input["file_path"].as_str().unwrap_or("");
            let summary = if path.is_empty() {
                "Read a file".into()
            } else {
                format!("Read {}", short_path(path))
            };
            let detail = if path.is_empty() {
                pretty(input)
            } else {
                path.to_owned()
            };
            let clause = format!("read {}", spoken_path(path));
            (summary, detail, clause)
        }
        "WebFetch" => {
            let url = input["url"].as_str().unwrap_or("");
            let host = url_host(url);
            let summary = if url.is_empty() {
                "Fetch a web page".into()
            } else {
                format!("Fetch {}", truncate(url, 80))
            };
            let detail = if url.is_empty() {
                pretty(input)
            } else {
                url.to_owned()
            };
            let clause = match host {
                Some(h) => format!("fetch {h}"),
                None => "fetch a web page".into(),
            };
            (summary, detail, clause)
        }
        other => {
            let name = human_tool_name(other);
            (format!("Use {name}"), pretty(input), format!("use {name}"))
        }
    };
    let spoken = format!("Claude wants to {clause} — OK?");
    PermissionDisplay {
        tool: tool.to_owned(),
        summary,
        detail,
        spoken,
    }
}

/// Parses a short spoken yes/no answer. Returns `None` if the utterance isn't a
/// clear approval or denial (e.g. a new question or an unrelated sentence).
pub fn parse_spoken_answer(text: &str) -> Option<bool> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    if words.is_empty() || words.len() > 6 {
        return None;
    }
    let joined = words.join(" ");
    const YES: &[&str] = &[
        "yes",
        "yeah",
        "yep",
        "yup",
        "sure",
        "ok",
        "okay",
        "alright",
        "all right",
        "allow",
        "allow it",
        "approve",
        "approved",
        "go ahead",
        "do it",
        "yes please",
        "yes go ahead",
        "yes allow",
        "yes allow it",
        "yes do it",
        "yeah go ahead",
        "yeah do it",
        "sure go ahead",
        "ok go ahead",
        "okay go ahead",
        "fine",
        "fine by me",
        "that's fine",
        "sounds good",
        "proceed",
    ];
    const NO: &[&str] = &[
        "no",
        "nope",
        "nah",
        "deny",
        "deny it",
        "don't",
        "dont",
        "do not",
        "don't do it",
        "dont do it",
        "don't allow",
        "dont allow",
        "don't allow it",
        "dont allow it",
        "no don't",
        "no dont",
        "no don't do it",
        "no thanks",
        "no thank you",
        "stop",
        "cancel",
        "reject",
        "decline",
        "never mind",
        "nevermind",
        "abort",
    ];
    if YES.contains(&joined.as_str()) {
        return Some(true);
    }
    if NO.contains(&joined.as_str()) {
        return Some(false);
    }
    None
}

fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn short_path(path: &str) -> String {
    if let Ok(home) = std::env::var("HOME")
        && let Some(rest) = path.strip_prefix(&home)
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("~{rest}");
    }
    truncate(path, 80)
}

fn spoken_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| "a file".into())
}

fn spoken_desc(desc: &str) -> String {
    let d = desc.trim().trim_end_matches('.');
    let mut chars = d.chars();
    let lowered = match chars.next() {
        Some(c) if c.is_uppercase() => {
            // Lowercase leading word unless it's an acronym like "SSH" or "HTTP".
            let rest = chars.as_str();
            if rest.chars().next().is_some_and(char::is_uppercase) {
                d.to_owned()
            } else {
                format!("{}{rest}", c.to_lowercase())
            }
        }
        _ => d.to_owned(),
    };
    truncate(&lowered, 80)
}

fn spoken_command(cmd: &str) -> String {
    let program = cmd
        .split_whitespace()
        .find(|tok| !tok.contains('='))
        .unwrap_or("");
    let base = program.rsplit('/').next().unwrap_or(program);
    if base.is_empty() {
        "run a shell command".into()
    } else {
        format!("run {base}")
    }
}

fn url_host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then_some(host)
}

fn human_tool_name(tool: &str) -> String {
    if let Some(rest) = tool.strip_prefix("mcp__") {
        rest.replace("__", " ")
    } else {
        tool.to_owned()
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn describes_common_tools() {
        let bash = describe(
            "Bash",
            &json!({"command": "rm -rf /tmp/old", "description": "Remove temporary directory"}),
        );
        assert_eq!(bash.summary, "Run: Remove temporary directory");
        assert_eq!(bash.detail, "rm -rf /tmp/old");
        assert_eq!(
            bash.spoken,
            "Claude wants to remove temporary directory — OK?"
        );

        let bash_no_desc = describe("Bash", &json!({"command": "FOO=1 /usr/bin/systemctl restart foo"}));
        assert_eq!(bash_no_desc.spoken, "Claude wants to run systemctl — OK?");

        let edit = describe("Edit", &json!({"file_path": "/etc/hosts"}));
        assert_eq!(edit.summary, "Edit /etc/hosts");
        assert_eq!(edit.spoken, "Claude wants to edit hosts — OK?");

        let fetch = describe("WebFetch", &json!({"url": "https://example.com/a/b"}));
        assert_eq!(fetch.spoken, "Claude wants to fetch example.com — OK?");
    }

    #[test]
    fn parses_yes_and_no_answers() {
        for yes in ["yes", "Yeah.", "OK!", "Go ahead", "yes, allow it", "Sure."] {
            assert_eq!(parse_spoken_answer(yes), Some(true), "expected yes: {yes}");
        }
        for no in ["no", "Nope.", "Don't do it", "deny", "Cancel.", "no thanks"] {
            assert_eq!(parse_spoken_answer(no), Some(false), "expected no: {no}");
        }
        for other in [
            "",
            "what time is it",
            "yes actually open firefox instead",
            "tell me why it wants to do that",
        ] {
            assert_eq!(parse_spoken_answer(other), None, "expected None: {other}");
        }
    }
}
