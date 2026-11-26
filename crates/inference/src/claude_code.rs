//! Headless Claude Code as a provider: `claude -p --output-format stream-json`,
//! one JSON object per line. The prompt goes in on stdin. Runs happen in a
//! workspace directory of their own, with the home directory added for access.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::mpsc;
use vivus_core::config::Provider;

use crate::permission_server::{PERMISSION_TOOL_NAME, PermissionPrompt, PermissionServer, TOKEN_ENV};

#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    #[error("provider is not a claude-code provider")]
    WrongKind,
    #[error("starting `{command}`: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("preparing workspace {path}: {source}")]
    Workspace {
        path: PathBuf,
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
    /// Serve a loopback `--permission-prompt-tool` MCP endpoint and yield
    /// `ClaudeEvent::Permission` whenever Claude Code asks for approval.
    pub permission_prompts: bool,
}

#[derive(Debug)]
pub enum ClaudeEvent {
    /// A text block of an assistant message (whole blocks, not deltas).
    Text(String),
    ToolUse {
        name: String,
    },
    /// Claude Code asked the user to approve a tool call; call `allow` or `deny`.
    Permission(PermissionPrompt),
    /// The final answer; always the last event.
    Result(ClaudeResult),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeResult {
    pub text: String,
    pub is_error: bool,
    pub cost_usd: Option<f64>,
}

/// Written into a new workspace; never overwritten, so the user can edit it.
const WORKSPACE_CLAUDE_MD: &str = "\
# Vivus workspace

This is the working directory for Vivus's agent: requests the user spoke to their
desktop assistant, run headless with no one at the keyboard. How to format the
reply is in the system prompt.

- The user's home directory is accessible too (`--add-dir`); this folder is yours.
- Keep reusable helper scripts in `scripts/`, each with a one-line comment saying
  what it does. Check there before writing a new one.
- Permissions run in auto mode; anything auto mode won't decide is sent to the
  user to approve or deny. If the user denies an action or it is blocked, don't
  hunt for a workaround: say what happened and stop.
";

impl ClaudeCode {
    /// Runs in the current directory; see [`ClaudeCode::from_provider`] for the workspace.
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            cwd: None,
        }
    }

    /// Creates the provider's workspace (and its `CLAUDE.md`) if it's missing.
    pub fn from_provider(provider: &Provider) -> Result<Self, ClaudeError> {
        let Provider::ClaudeCode {
            command,
            args,
            workspace,
        } = provider
        else {
            return Err(ClaudeError::WrongKind);
        };
        let mut cc = Self::new(command, args.clone());
        if let Some(dir) = workspace {
            prepare_workspace(dir).map_err(|source| ClaudeError::Workspace {
                path: dir.clone(),
                source,
            })?;
            cc.cwd = Some(dir.clone());
        }
        Ok(cc)
    }

    /// Starts a run. Dropping the returned `ClaudeRun` kills the process.
    pub async fn run(&self, req: &ClaudeRequest) -> Result<ClaudeRun, ClaudeError> {
        let (perm_server, perm_rx) = if req.permission_prompts {
            let (server, rx) = PermissionServer::start().await?;
            (Some(server), Some(rx))
        } else {
            (None, None)
        };
        let mut cmd = Command::new(&self.command);
        cmd.args(["-p", "--output-format", "stream-json", "--verbose"]);
        if let Some(model) = &req.model {
            cmd.args(["--model", model]);
        }
        if let Some(system) = &req.system {
            cmd.args(["--append-system-prompt", system]);
        }
        if let Some(home) = std::env::var_os("HOME") {
            cmd.arg("--add-dir").arg(home);
        }
        if let Some(server) = &perm_server {
            cmd.args([
                "--mcp-config",
                &server.mcp_config_json(),
                "--permission-prompt-tool",
                PERMISSION_TOOL_NAME,
            ]);
            cmd.env(TOKEN_ENV, server.token());
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
            _perm_server: perm_server,
            perm_rx,
        })
    }
}

fn prepare_workspace(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("scripts"))?;
    let md = dir.join("CLAUDE.md");
    if !md.exists() {
        std::fs::write(md, WORKSPACE_CLAUDE_MD)?;
    }
    Ok(())
}

pub struct ClaudeRun {
    child: Child,
    lines: Lines<BufReader<ChildStdout>>,
    pending: VecDeque<ClaudeEvent>,
    done: bool,
    _perm_server: Option<PermissionServer>,
    perm_rx: Option<mpsc::UnboundedReceiver<PermissionPrompt>>,
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
            let line = match &mut self.perm_rx {
                Some(rx) => tokio::select! {
                    biased;
                    Some(prompt) = rx.recv() => return Some(Ok(ClaudeEvent::Permission(prompt))),
                    line = self.lines.next_line() => line,
                },
                None => self.lines.next_line().await,
            };
            match line {
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
        assert_eq!(out.len(), 3);
        assert!(matches!(&out[0], ClaudeEvent::Text(t) if t == "Searching."));
        assert!(matches!(&out[1], ClaudeEvent::ToolUse { name } if name == "WebSearch"));
        assert!(matches!(
            &out[2],
            ClaudeEvent::Result(ClaudeResult {
                text,
                is_error: false,
                cost_usd: Some(c)
            }) if text == "Done." && (*c - 0.01).abs() < 1e-9
        ));
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

    /// The workspace is created with its CLAUDE.md, kept on later runs, and is the run's cwd.
    #[tokio::test]
    async fn runs_in_a_prepared_workspace() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("vivus-ws-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fake = root.join("fake-claude");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '{\"type\":\"result\",\"is_error\":false,\"result\":\"%s\"}\\n' \"$PWD\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ws = root.join("workspace");
        let provider = Provider::ClaudeCode {
            command: fake.to_str().unwrap().into(),
            args: vec![],
            workspace: Some(ws.clone()),
        };
        let cc = ClaudeCode::from_provider(&provider).unwrap();
        assert!(ws.join("scripts").is_dir());
        std::fs::write(ws.join("CLAUDE.md"), "edited").unwrap();
        ClaudeCode::from_provider(&provider).unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("CLAUDE.md")).unwrap(), "edited");

        let mut run = cc.run(&ClaudeRequest::default()).await.unwrap();
        let ev = run.next().await.unwrap().unwrap();
        assert!(matches!(ev, ClaudeEvent::Result(r) if r.text == ws.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(root);
    }

    /// With `permission_prompts`, ClaudeRun starts the loopback MCP server, passes
    /// `--mcp-config` + `--permission-prompt-tool` + `VIVUS_PERMISSION_TOKEN`,
    /// yields `ClaudeEvent::Permission`, and returns the decision to the child.
    #[tokio::test]
    async fn permission_prompt_round_trip() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("vivus-perm-claude-{}", std::process::id()));
        // Fake `claude` that extracts the MCP URL from `--mcp-config`, calls
        // `tools/call`, and prints the returned behavior as the final result.
        let script = r#"#!/usr/bin/env python3
import json, os, sys, urllib.request
args = sys.argv[1:]
cfg = json.loads(args[args.index("--mcp-config") + 1])
tool = args[args.index("--permission-prompt-tool") + 1]
assert tool == "mcp__vivus__approve", tool
srv = cfg["mcpServers"]["vivus"]
url = srv["url"]
token = os.environ["VIVUS_PERMISSION_TOKEN"]
body = json.dumps({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/call",
    "params": {
        "name": "approve",
        "arguments": {
            "tool_name": "Bash",
            "input": {"command": "echo hi"},
            "tool_use_id": "toolu_test",
        },
    },
}).encode()
req = urllib.request.Request(
    url,
    data=body,
    headers={"Content-Type": "application/json", "Authorization": f"Bearer {token}"},
)
with urllib.request.urlopen(req) as resp:
    res = json.loads(resp.read().decode())
decision = json.loads(res["result"]["content"][0]["text"])
print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": decision["behavior"]}))
"#;
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cc = ClaudeCode::new(path.to_str().unwrap(), vec![]);
        let mut run = cc
            .run(&ClaudeRequest {
                prompt: "go".into(),
                permission_prompts: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let ev = run.next().await.unwrap().unwrap();
        let ClaudeEvent::Permission(prompt) = ev else {
            panic!("expected permission prompt, got {ev:?}");
        };
        assert_eq!(prompt.tool_name, "Bash");
        assert_eq!(prompt.input["command"], "echo hi");
        prompt.allow();
        let ev = run.next().await.unwrap().unwrap();
        assert!(matches!(ev, ClaudeEvent::Result(r) if r.text == "allow"));
        let _ = std::fs::remove_file(path);
    }

    /// End-to-end smoke test against the real `claude` CLI in manual mode:
    /// verifies that our loopback `--permission-prompt-tool` receives a Bash
    /// prompt and that allowing/denying it drives Claude Code as expected.
    #[tokio::test]
    #[ignore = "invokes the real `claude` CLI"]
    async fn live_permission_prompt_with_claude_cli() {
        let cc = ClaudeCode::new(
            "claude",
            vec![
                "--permission-mode".into(),
                "manual".into(),
                "--setting-sources".into(),
                "".into(),
            ],
        );
        let mut run = cc
            .run(&ClaudeRequest {
                prompt: "Run `rm -f /tmp/opencode/vivus-live-perm-nonexistent` with Bash and report whether it was allowed or denied.".into(),
                model: Some("haiku".into()),
                permission_prompts: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut saw_prompt = false;
        let mut result_text = String::new();
        while let Some(ev) = run.next().await {
            match ev.unwrap() {
                ClaudeEvent::Permission(p) => {
                    assert_eq!(p.tool_name, "Bash");
                    saw_prompt = true;
                    p.deny("User said no.");
                }
                ClaudeEvent::Result(r) => {
                    assert!(!r.is_error, "claude error: {}", r.text);
                    result_text = r.text;
                }
                _ => {}
            }
        }
        assert!(saw_prompt, "expected a permission prompt");
        assert!(
            result_text.to_lowercase().contains("denied")
                || result_text.to_lowercase().contains("declined")
                || result_text.to_lowercase().contains("user said no"),
            "unexpected result: {result_text}"
        );
    }
}
