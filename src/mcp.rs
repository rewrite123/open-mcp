//! Minimal MCP (Model Context Protocol) client using the Streamable HTTP
//! transport: JSON-RPC 2.0 messages POSTed to a single endpoint, with
//! responses returned either as a plain JSON body or as a `text/event-stream`.
//!
//! Handles the `initialize` handshake (including protocol version
//! negotiation), `tools/list`, and `tools/call`.

use anyhow::{anyhow, bail, Context, Result};
use crate::endpoint::merge_json;
use reqwest::header::{HeaderMap, ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

/// Protocol versions this client understands, newest first. The first one is
/// sent as our preference during `initialize`.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub struct McpClient {
    transport: Transport,
    next_id: AtomicI64,
    /// Protocol version negotiated with the server during `initialize`.
    pub protocol_version: Mutex<Option<String>>,
}

enum Transport {
    Http {
        http: reqwest::Client,
        endpoint: String,
        headers: HeaderMap,
        body: Value,
        session_id: Mutex<Option<String>>,
    },
    Stdio {
        _child: Mutex<Child>,
        stdin: Mutex<ChildStdin>,
        stdout: Mutex<BufReader<ChildStdout>>,
        body: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Value,
}

impl McpClient {
    pub fn new(endpoint: impl Into<String>, extra_headers: HeaderMap) -> Result<Self> {
        Self::http(endpoint, extra_headers, json!({}), crate::settings::timeout()?)
    }

    pub fn http(endpoint: impl Into<String>, headers: HeaderMap, body: Value, timeout: Option<std::time::Duration>) -> Result<Self> {
        let mut builder = reqwest::Client::builder();
        if let Some(timeout) = timeout { builder = builder.timeout(timeout); }
        let http = builder.build().context("failed to build HTTP client")?;
        Ok(Self {
            transport: Transport::Http {
                http,
                endpoint: endpoint.into(),
                headers,
                body,
                session_id: Mutex::new(None),
            },
            next_id: AtomicI64::new(1),
            protocol_version: Mutex::new(None),
        })
    }

    /// Starts a JSON-lines stdio MCP server. Its stdout must contain only
    /// JSON-RPC messages; diagnostics belong on stderr.
    pub fn stdio(command: &str, arguments: &[String]) -> Result<Self> {
        Self::stdio_with_body(command, arguments, json!({}))
    }

    pub fn stdio_with_body(command: &str, arguments: &[String], body: Value) -> Result<Self> {
        let mut child = Command::new(command)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to start stdio MCP server '{command}'"))?;
        let stdin = child.stdin.take().context("stdio MCP server did not expose stdin")?;
        let stdout = child.stdout.take().context("stdio MCP server did not expose stdout")?;
        Ok(Self {
            transport: Transport::Stdio {
                _child: Mutex::new(child),
                stdin: Mutex::new(stdin),
                stdout: Mutex::new(BufReader::new(stdout)),
                body,
            },
            next_id: AtomicI64::new(1),
            protocol_version: Mutex::new(None),
        })
    }

    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Performs the MCP `initialize` handshake and sends the required
    /// `notifications/initialized` follow-up. Returns the server's `serverInfo`.
    pub async fn initialize(&self, preferred_version: Option<&str>) -> Result<Value> {
        let requested_version = preferred_version.unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0]);
        let params = json!({
            "protocolVersion": requested_version,
            "capabilities": {},
            "clientInfo": { "name": "omcp", "version": env!("CARGO_PKG_VERSION") },
        });

        let result = self.request("initialize", params).await?;

        let server_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(requested_version)
            .to_string();
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&server_version.as_str()) {
            eprintln!(
                "warning: server negotiated MCP protocol version '{server_version}', which this \
                 client does not explicitly support (supported: {SUPPORTED_PROTOCOL_VERSIONS:?}); \
                 continuing on a best-effort basis"
            );
        }
        *self.protocol_version.lock().unwrap() = Some(server_version);

        // Notification: no id, no response body expected.
        self.notify("notifications/initialized", json!({})).await?;

        Ok(result)
    }

    pub async fn list_tools(&self) -> Result<Vec<Tool>> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .cloned()
            .ok_or_else(|| anyhow!("tools/list response missing 'tools' field"))?;
        Ok(serde_json::from_value(tools)?)
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let params = json!({ "name": name, "arguments": arguments });
        self.request("tools/call", params).await
    }

    /// Sends a JSON-RPC request and returns the `result` field, or an error if
    /// the server responded with a JSON-RPC error object.
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let envelope = self.send_request(&body).await?;

        if let Some(error) = envelope.get("error") {
            bail!("MCP server returned an error for '{method}': {error}");
        }
        envelope
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("MCP response for '{method}' missing 'result' field"))
    }

    /// Sends a JSON-RPC notification (no `id`, no response expected/parsed).
    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.send_notification(&body).await
    }

    async fn send_request(&self, body: &Value) -> Result<Value> {
        match &self.transport {
            Transport::Http { http, endpoint, headers, body: defaults, session_id } => {
                let mut payload = merge_json(defaults.clone(), body.clone());
                payload["jsonrpc"] = body["jsonrpc"].clone();
                payload["id"] = body["id"].clone();
                payload["method"] = body["method"].clone();
                payload["params"] = body["params"].clone();
                let mut req = http.post(endpoint).headers(headers.clone()).header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json, text/event-stream").json(&payload);
                if let Some(id) = session_id.lock().unwrap().clone() { req = req.header("Mcp-Session-Id", id); }
                let response = req.send().await.with_context(|| format!("failed to reach MCP server at {endpoint}"))?;
                if let Some(id) = response.headers().get("Mcp-Session-Id").and_then(|v| v.to_str().ok()) {
                    *session_id.lock().unwrap() = Some(id.to_string());
                }
                if !response.status().is_success() {
                    let status = response.status();
                    bail!("MCP server returned HTTP {status}: {}", response.text().await.unwrap_or_default());
                }
                self.decode_response(response).await
            }
            Transport::Stdio { stdin, stdout, body: defaults, .. } => {
                let payload = rpc_payload(defaults.clone(), body);
                write_stdio(&mut stdin.lock().unwrap(), &payload)?;
                read_stdio(&mut stdout.lock().unwrap())
            }
        }
    }

    async fn send_notification(&self, body: &Value) -> Result<()> {
        match &self.transport {
            Transport::Http { http, endpoint, headers, body: defaults, session_id } => {
                let mut payload = merge_json(defaults.clone(), body.clone());
                payload["jsonrpc"] = body["jsonrpc"].clone();
                payload["method"] = body["method"].clone();
                payload["params"] = body["params"].clone();
                let mut req = http.post(endpoint).headers(headers.clone()).header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json, text/event-stream").json(&payload);
                if let Some(id) = session_id.lock().unwrap().clone() { req = req.header("Mcp-Session-Id", id); }
                let response = req.send().await.with_context(|| format!("failed to reach MCP server at {endpoint}"))?;
                if !response.status().is_success() && response.status().as_u16() != 202 {
                    let status = response.status();
                    bail!("MCP server rejected notification '{:?}': {status}", body["method"]);
                }
                Ok(())
            }
            Transport::Stdio { stdin, body: defaults, .. } => {
                let payload = rpc_payload(defaults.clone(), body);
                write_stdio(&mut stdin.lock().unwrap(), &payload)
            }
        }
    }

    /// Parses a response body as either a single JSON object or an SSE stream,
    /// returning the first JSON-RPC message found.
    async fn decode_response(&self, response: reqwest::Response) -> Result<Value> {
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = response.text().await.context("failed to read MCP response body")?;

        if content_type.contains("text/event-stream") {
            for line in text.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(data) {
                        return Ok(value);
                    }
                }
            }
            bail!("no JSON-RPC message found in SSE response");
        }

        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse MCP response as JSON: {text}"))
    }
}

fn rpc_payload(defaults: Value, request: &Value) -> Value {
    let mut payload = merge_json(defaults, request.clone());
    for field in ["jsonrpc", "id", "method", "params"] {
        if let Some(value) = request.get(field) {
            payload[field] = value.clone();
        }
    }
    payload
}

fn write_stdio(stdin: &mut ChildStdin, body: &Value) -> Result<()> {
    serde_json::to_writer(&mut *stdin, body).context("failed to encode stdio MCP message")?;
    stdin.write_all(b"\n").context("failed to write stdio MCP newline")?;
    stdin.flush().context("failed to flush stdio MCP request")
}

fn read_stdio(stdout: &mut BufReader<ChildStdout>) -> Result<Value> {
    loop {
        let mut line = String::new();
        let bytes = stdout.read_line(&mut line).context("failed to read stdio MCP response")?;
        if bytes == 0 {
            bail!("stdio MCP server closed stdout before responding");
        }
        if line.trim().is_empty() {
            continue;
        }
        return serde_json::from_str(&line)
            .with_context(|| format!("stdio MCP server wrote non-JSON output: {}", line.trim()));
    }
}
