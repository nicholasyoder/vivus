//! Minimal Streamable-HTTP MCP server for Claude Code's
//! `--permission-prompt-tool`. Bound to `127.0.0.1:0` with a random per-run
//! bearer token (passed via `VIVUS_PERMISSION_TOKEN` so it isn't in `argv`),
//! forwarding each `tools/call` over a channel and waiting for an Allow/Deny.

use std::fmt::Write as _;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

/// MCP server name in `--mcp-config` (never `"stdio"`, which Claude Code reserves).
const SERVER_NAME: &str = "vivus";
const TOOL_NAME: &str = "approve";
/// Passed to `--permission-prompt-tool`.
pub const PERMISSION_TOOL_NAME: &str = "mcp__vivus__approve";
/// Environment variable carrying the bearer token; referenced by `--mcp-config`.
pub const TOKEN_ENV: &str = "VIVUS_PERMISSION_TOKEN";
/// How long Claude Code will wait for the user to answer one prompt.
const TOOL_TIMEOUT_MS: u64 = 5 * 60 * 1000;
/// Cap on incoming HTTP body size.
const MAX_BODY: usize = 4 * 1024 * 1024;

/// A tool call Claude Code asked permission to run.
#[derive(Debug)]
pub struct PermissionPrompt {
    pub tool_name: String,
    pub input: Value,
    pub tool_use_id: Option<String>,
    reply: oneshot::Sender<PermissionDecision>,
}

impl PermissionPrompt {
    /// Approve the tool call with its original input unchanged.
    pub fn allow(self) {
        let _ = self.reply.send(PermissionDecision::Allow);
    }

    /// Reject the tool call; `message` is returned to Claude Code as the reason.
    pub fn deny(self, message: impl Into<String>) {
        let _ = self.reply.send(PermissionDecision::Deny(message.into()));
    }
}

#[derive(Debug)]
enum PermissionDecision {
    Allow,
    Deny(String),
}

/// A running loopback MCP server; dropping it stops accepting connections.
pub(crate) struct PermissionServer {
    port: u16,
    token: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for PermissionServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PermissionServer {
    pub async fn start() -> std::io::Result<(Self, mpsc::UnboundedReceiver<PermissionPrompt>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let token = random_token();
        let (tx, rx) = mpsc::unbounded_channel();
        let expected_auth: Arc<str> = format!("Bearer {token}").into();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let auth = expected_auth.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, &auth, &tx).await;
                });
            }
        });
        Ok((Self { port, token, task }, rx))
    }

    /// JSON passed to `--mcp-config`. References `TOKEN_ENV` so the secret isn't
    /// exposed in the child's command line.
    pub fn mcp_config_json(&self) -> String {
        json!({
            "mcpServers": {
                SERVER_NAME: {
                    "type": "http",
                    "url": format!("http://127.0.0.1:{}/mcp", self.port),
                    "headers": {
                        "Authorization": format!("Bearer ${{{TOKEN_ENV}}}")
                    },
                    "timeout": TOOL_TIMEOUT_MS,
                }
            }
        })
        .to_string()
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_ok();
    if !ok {
        // Fallback if /dev/urandom is somehow unavailable: mix pid + time.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes[..16].copy_from_slice(&t.to_le_bytes());
    }
    let mut hex = String::with_capacity(32);
    for b in bytes {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

async fn handle_connection(
    stream: TcpStream,
    expected_auth: &str,
    tx: &mpsc::UnboundedSender<PermissionPrompt>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    loop {
        let Some(req) = read_request(&mut reader).await? else {
            return Ok(());
        };
        let keep_alive = !req
            .connection
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case("close"));
        let authorized = req.authorization.as_deref() == Some(expected_auth);
        let response = if !authorized {
            HttpResponse::empty(401, "Unauthorized")
        } else {
            match req.method.as_str() {
                "POST" => handle_post(&req.body, tx).await,
                // Streamable HTTP: no server-initiated SSE stream; 405 tells client not to open one.
                "GET" => HttpResponse::empty(405, "Method Not Allowed"),
                "DELETE" => HttpResponse::empty(200, "OK"),
                _ => HttpResponse::empty(405, "Method Not Allowed"),
            }
        };
        write_response(reader.get_mut(), response, keep_alive).await?;
        if !keep_alive {
            return Ok(());
        }
    }
}

struct HttpRequest {
    method: String,
    authorization: Option<String>,
    connection: Option<String>,
    body: Vec<u8>,
}

struct HttpResponse {
    status: u16,
    reason: &'static str,
    content_type: Option<&'static str>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn empty(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            content_type: None,
            body: Vec::new(),
        }
    }

    fn json(value: &Value) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: Some("application/json"),
            body: value.to_string().into_bytes(),
        }
    }
}

async fn read_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<HttpRequest>> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    let method = line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut content_length: usize = 0;
    let mut chunked = false;
    let mut authorization = None;
    let mut connection = None;
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.to_owned());
            } else if name.eq_ignore_ascii_case("connection") {
                connection = Some(value.to_owned());
            }
        }
    }
    let body = if chunked {
        read_chunked(reader).await?
    } else if content_length > 0 {
        if content_length > MAX_BODY {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request body too large",
            ));
        }
        let mut buf = vec![0u8; content_length];
        reader.read_exact(&mut buf).await?;
        buf
    } else {
        Vec::new()
    };
    Ok(Some(HttpRequest {
        method,
        authorization,
        connection,
        body,
    }))
}

async fn read_chunked(reader: &mut BufReader<TcpStream>) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        let size_str = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if size == 0 {
            // Trailing CRLF.
            line.clear();
            let _ = reader.read_line(&mut line).await;
            break;
        }
        if out.len().saturating_add(size) > MAX_BODY {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "chunked body too large",
            ));
        }
        let start = out.len();
        out.resize(start + size, 0);
        reader.read_exact(&mut out[start..]).await?;
        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf).await?;
    }
    Ok(out)
}

async fn write_response(
    stream: &mut TcpStream,
    res: HttpResponse,
    keep_alive: bool,
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", res.status, res.reason);
    if let Some(ct) = res.content_type {
        let _ = write!(head, "Content-Type: {ct}\r\n");
    }
    let _ = write!(
        head,
        "Content-Length: {}\r\nConnection: {}\r\n\r\n",
        res.body.len(),
        if keep_alive { "keep-alive" } else { "close" }
    );
    stream.write_all(head.as_bytes()).await?;
    if !res.body.is_empty() {
        stream.write_all(&res.body).await?;
    }
    stream.flush().await
}

async fn handle_post(
    body: &[u8],
    tx: &mpsc::UnboundedSender<PermissionPrompt>,
) -> HttpResponse {
    let Ok(msg) = serde_json::from_slice::<Value>(body) else {
        return HttpResponse::empty(400, "Bad Request");
    };
    let Some(id) = msg.get("id").cloned() else {
        // Notification (e.g. `notifications/initialized`).
        return HttpResponse::empty(202, "Accepted");
    };
    let method = msg["method"].as_str().unwrap_or_default();
    let reply = match method {
        "initialize" => {
            let version = msg["params"]["protocolVersion"]
                .as_str()
                .unwrap_or("2025-03-26");
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": SERVER_NAME, "version": "0.1.0" },
                }
            })
        }
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [{
                    "name": TOOL_NAME,
                    "description": "Ask the user to approve or deny a tool call.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "tool_name": { "type": "string" },
                            "input": { "type": "object" },
                            "tool_use_id": { "type": "string" },
                        },
                        "required": ["tool_name", "input"],
                    },
                }]
            }
        }),
        "tools/call" => {
            let args = &msg["params"]["arguments"];
            let tool_name = args["tool_name"].as_str().unwrap_or("unknown").to_owned();
            let input = args.get("input").cloned().unwrap_or_else(|| json!({}));
            let tool_use_id = args["tool_use_id"].as_str().map(str::to_owned);
            let decision = ask_user(tx, tool_name, input.clone(), tool_use_id).await;
            let payload = match decision {
                PermissionDecision::Allow => json!({
                    "behavior": "allow",
                    "updatedInput": input,
                }),
                PermissionDecision::Deny(message) => json!({
                    "behavior": "deny",
                    "message": message,
                }),
            };
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": payload.to_string() }]
                }
            })
        }
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("method not found: {method}") }
        }),
    };
    HttpResponse::json(&reply)
}

async fn ask_user(
    tx: &mpsc::UnboundedSender<PermissionPrompt>,
    tool_name: String,
    input: Value,
    tool_use_id: Option<String>,
) -> PermissionDecision {
    let (reply_tx, reply_rx) = oneshot::channel();
    let prompt = PermissionPrompt {
        tool_name,
        input,
        tool_use_id,
        reply: reply_tx,
    };
    if tx.send(prompt).is_err() {
        return PermissionDecision::Deny("No permission handler available.".into());
    }
    reply_rx
        .await
        .unwrap_or_else(|_| PermissionDecision::Deny("Permission prompt was cancelled.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn post(url: &str, auth: Option<&str>, body: Value) -> (reqwest::StatusCode, String) {
        let client = reqwest::Client::new();
        let mut req = client.post(url).json(&body);
        if let Some(a) = auth {
            req = req.header("Authorization", a);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        (status, text)
    }

    #[tokio::test]
    async fn auth_handshake_and_permission_decisions() {
        let (server, mut rx) = PermissionServer::start().await.unwrap();
        let url = format!("http://127.0.0.1:{}/mcp", server.port);
        let auth = format!("Bearer {}", server.token());

        // Missing or wrong bearer token is rejected.
        let (st, _) = post(&url, None, json!({"jsonrpc":"2.0","id":1,"method":"ping"})).await;
        assert_eq!(st, 401);
        let (st, _) = post(
            &url,
            Some("Bearer wrong"),
            json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
        )
        .await;
        assert_eq!(st, 401);

        // Initialize + notification + tools/list.
        let (st, body) = post(
            &url,
            Some(&auth),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
        )
        .await;
        assert_eq!(st, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["protocolVersion"], "2025-11-25");

        let (st, _) = post(
            &url,
            Some(&auth),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        assert_eq!(st, 202);

        let (st, body) = post(
            &url,
            Some(&auth),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
        assert_eq!(st, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["tools"][0]["name"], "approve");

        // Allow echoes updatedInput unchanged.
        let call = tokio::spawn({
            let (url, auth) = (url.clone(), auth.clone());
            async move {
                post(
                    &url,
                    Some(&auth),
                    json!({
                        "jsonrpc": "2.0",
                        "id": 3,
                        "method": "tools/call",
                        "params": {
                            "name": "approve",
                            "arguments": {
                                "tool_name": "Bash",
                                "input": {"command": "rm -rf /tmp/x"},
                                "tool_use_id": "toolu_1"
                            }
                        }
                    }),
                )
                .await
            }
        });
        let prompt = rx.recv().await.unwrap();
        assert_eq!(prompt.tool_name, "Bash");
        assert_eq!(prompt.input["command"], "rm -rf /tmp/x");
        assert_eq!(prompt.tool_use_id.as_deref(), Some("toolu_1"));
        prompt.allow();
        let (st, body) = call.await.unwrap();
        assert_eq!(st, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        let decision: Value =
            serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(decision["behavior"], "allow");
        assert_eq!(decision["updatedInput"]["command"], "rm -rf /tmp/x");

        // Deny carries the message.
        let call = tokio::spawn({
            let (url, auth) = (url.clone(), auth.clone());
            async move {
                post(
                    &url,
                    Some(&auth),
                    json!({
                        "jsonrpc": "2.0",
                        "id": 4,
                        "method": "tools/call",
                        "params": {
                            "name": "approve",
                            "arguments": {
                                "tool_name": "Edit",
                                "input": {"file_path": "/etc/hosts"}
                            }
                        }
                    }),
                )
                .await
            }
        });
        let prompt = rx.recv().await.unwrap();
        prompt.deny("User denied it.");
        let (st, body) = call.await.unwrap();
        assert_eq!(st, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        let decision: Value =
            serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(decision["behavior"], "deny");
        assert_eq!(decision["message"], "User denied it.");
    }
}
