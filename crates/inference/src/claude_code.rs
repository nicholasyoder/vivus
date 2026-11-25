//! Headless Claude Code as a provider: `claude -p --output-format stream-json`,
//! one JSON object per line. The prompt goes in on stdin.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use vivus_core::config::Provider;

#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    #[error("provider is not a claude-code provider")]
    WrongKind,
    #[error("starting `{command}`: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("reading Claude Code output: {0}")]
    Io(#[from] std::io::Error),
    #[error("Claude Code exited without a result ({0})")]
    NoResult(String),
}

#[derive(Debug, Clone)]
pub struct ClaudeCode {
    command: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
}

/// One run's input.
#[derive(Debug, Clone, Default)]
pub struct ClaudeRequest {
    pub prompt: String,
    /// Passed as `--append-system-prompt`.
    pub system: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeEvent {
    /// A text block of an assistant message (whole blocks, not deltas).
    Text(String),
    ToolUse {
        name: String,
    },
    /// The final answer; always the last event.
    Result(ClaudeResult),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeResult {
    pub text: String,
    pub is_error: bool,
    pub cost_usd: Option<f64>,
}

impl ClaudeCode {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            cwd: std::env::var_os("HOME").map(PathBuf::from),
        }
    }

    pub fn from_provider(provider: &Provider) -> Result<Self, ClaudeError> {
        match provider {
            Provider::ClaudeCode { command, args } => Ok(Self::new(command, args.clone())),
            _ => Err(ClaudeError::WrongKind),
        }
    }

    /// Starts a run. Dropping the returned `ClaudeRun` kills the process.
    pub async fn run(&self, req: &ClaudeRequest) -> Result<ClaudeRun, ClaudeError> {
        let mut cmd = Command::new(&self.command);
        cmd.args(["-p", "--output-format", "stream-json", "--verbose"]);
        if let Some(model) = &req.model {
            cmd.args(["--model", model]);
        }
        if let Some(system) = &req.system {
            cmd.args(["--append-system-prompt", system]);
        }
        cmd.args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().map_err(|source| ClaudeError::Spawn {
            command: self.command.clone(),
            source,
        })?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(req.prompt.as_bytes()).await?;
        drop(stdin);
        let stdout = child.stdout.take().expect("piped stdout");
        Ok(ClaudeRun {
            child,
            lines: BufReader::new(stdout).lines(),
            pending: VecDeque::new(),
            done: false,
        })
    }
}

pub struct ClaudeRun {
    child: Child,
    lines: Lines<BufReader<ChildStdout>>,
    pending: VecDeque<ClaudeEvent>,
    done: bool,
}

impl ClaudeRun {
    /// The next event; `None` after the `Result`.
    pub async fn next(&mut self) -> Option<Result<ClaudeEvent, ClaudeError>> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                if matches!(ev, ClaudeEvent::Result(_)) {
                    self.done = true;
                }
                return Some(Ok(ev));
            }
            if self.done {
                return None;
            }
            match self.lines.next_line().await {
                Ok(Some(line)) => parse_line(&line, &mut self.pending),
                Ok(None) => {
                    self.done = true;
                    let status = self.child.wait().await;
                    let why = match status {
                        Ok(s) => s.to_string(),
                        Err(e) => e.to_string(),
                    };
                    return Some(Err(ClaudeError::NoResult(why)));
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(e.into()));
                }
            }
        }
    }
}

/// Unknown event types and malformed lines are skipped.
fn parse_line(line: &str, out: &mut VecDeque<ClaudeEvent>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return;
    };
    match v["type"].as_str() {
        Some("assistant") => {
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = block["text"].as_str().filter(|t| !t.is_empty()) {
                            out.push_back(ClaudeEvent::Text(t.into()));
                        }
                    }
                    Some("tool_use") => out.push_back(ClaudeEvent::ToolUse {
                        name: block["name"].as_str().unwrap_or_default().into(),
                    }),
                    _ => {}
                }
            }
        }
        Some("result") => out.push_back(ClaudeEvent::Result(ClaudeResult {
            text: v["result"].as_str().unwrap_or_default().into(),
            is_error: v["is_error"].as_bool().unwrap_or(v["subtype"] != "success"),
            cost_usd: v["total_cost_usd"].as_f64(),
        })),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stream_json() {
        let mut out = VecDeque::new();
        for line in [
            r#"{"type":"system","subtype":"init","tools":["Bash"]}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Searching."},{"type":"tool_use","name":"WebSearch","input":{}}]}}"#,
            "not json",
            r#"{"type":"rate_limit_event"}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.","total_cost_usd":0.01}"#,
        ] {
            parse_line(line, &mut out);
        }
        assert_eq!(
            Vec::from(out),
            vec![
                ClaudeEvent::Text("Searching.".into()),
                ClaudeEvent::ToolUse {
                    name: "WebSearch".into()
                },
                ClaudeEvent::Result(ClaudeResult {
                    text: "Done.".into(),
                    is_error: false,
                    cost_usd: Some(0.01)
                }),
            ]
        );
    }

    /// A fake `claude` that echoes its stdin as the result, then lingers.
    #[tokio::test]
    async fn runs_a_command_and_reads_the_result() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("vivus-fake-claude-{}", std::process::id()));
        std::fs::write(
            &path,
            "#!/bin/sh\nread -r p\nprintf '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"%s\"}\\n' \"$p\"\nsleep 5\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cc = ClaudeCode::new(path.to_str().unwrap(), vec![]);
        let mut run = cc
            .run(&ClaudeRequest {
                prompt: "hello\n".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let ev = run.next().await.unwrap().unwrap();
        assert!(matches!(ev, ClaudeEvent::Result(r) if r.text == "hello"));
        assert!(run.next().await.is_none());
        let _ = std::fs::remove_file(path);
    }
}
