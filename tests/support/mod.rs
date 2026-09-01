//! Shared helpers for omcp's integration tests: spawns the `mock-mcp-server`
//! binary as a real subprocess (a genuine external MCP server, not an in-test
//! stub) and sets up isolated `$HOME` directories for config-file-based tests.
//!
//! Not every test binary in `tests/` uses every helper here, since each
//! `tests/*.rs` file is compiled as its own crate; that's expected.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// A running `mock-mcp-server` subprocess. Killed automatically on drop.
pub struct MockMcpServer {
    pub base_url: String,
    child: Child,
}

impl Drop for MockMcpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawns `mock-mcp-server` on an ephemeral port, optionally requiring a bearer
/// token, and waits for it to report the address it bound.
pub fn spawn_mock_mcp_server(require_bearer: Option<&str>) -> MockMcpServer {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mock-mcp-server"));
    cmd.arg("--port").arg("0");
    if let Some(token) = require_bearer {
        cmd.arg("--require-bearer").arg(token);
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mock-mcp-server");

    let stdout = child.stdout.take().expect("mock-mcp-server stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("failed to read mock-mcp-server startup line");
    let addr = line
        .trim()
        .strip_prefix("LISTENING ")
        .unwrap_or_else(|| panic!("unexpected mock-mcp-server startup output: {line:?}"));

    MockMcpServer { base_url: format!("http://{addr}/mcp"), child }
}

/// An isolated fake `$HOME` for tests that exercise `~/.mcp/config` without
/// touching the real user's home directory. Removed automatically on drop.
pub struct TempHome {
    pub path: PathBuf,
}

impl TempHome {
    pub fn new() -> Self {
        let unique = format!(
            "omcp-test-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(path.join(".mcp")).expect("failed to create temp home");
        Self { path }
    }

    pub fn mcp_dir(&self) -> PathBuf {
        self.path.join(".mcp")
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Returns true if a local Ollama instance is reachable, so real-model tests
/// can be skipped gracefully in environments (like CI) without one.
pub fn ollama_is_reachable(host: &str) -> bool {
    let url = format!("{}/api/tags", host.trim_end_matches('/'));
    match std::process::Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "-m", "3", &url])
        .output()
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim() == "200",
        Err(_) => false,
    }
}

/// Spawns an in-process fake Ollama `/api/chat` endpoint that replies with the
/// given JSON responses in order (one per request received), for
/// deterministic tests of the tool-calling round trip without a real model.
pub fn spawn_mock_ollama_server(responses: Vec<serde_json::Value>) -> String {
    use std::net::TcpListener;
    use std::sync::Mutex;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock ollama server");
    let addr = listener.local_addr().unwrap();
    let server = tiny_http::Server::from_listener(listener, None).expect("start mock ollama server");
    let base_url = format!("http://{addr}");
    let remaining = Mutex::new(responses.into_iter());

    std::thread::spawn(move || {
        for mut request in server.incoming_requests() {
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).ok();

            let next = remaining.lock().unwrap().next();
            let response = match next {
                Some(payload) => tiny_http::Response::from_string(payload.to_string()).with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
                ),
                None => tiny_http::Response::from_string("no more scripted responses").with_status_code(500),
            };
            request.respond(response).ok();
        }
    });

    base_url
}

/// Like `spawn_mock_ollama_server`, but also returns a shared log of every
/// request body received (parsed as JSON), so tests can assert on what the
/// client actually sent (e.g. the `options.num_ctx` value).
pub fn spawn_mock_ollama_server_capturing(
    responses: Vec<serde_json::Value>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>) {
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock ollama server");
    let addr = listener.local_addr().unwrap();
    let server = tiny_http::Server::from_listener(listener, None).expect("start mock ollama server");
    let base_url = format!("http://{addr}");
    let remaining = Mutex::new(responses.into_iter());
    let received = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();

    std::thread::spawn(move || {
        for mut request in server.incoming_requests() {
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).ok();
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
                received_clone.lock().unwrap().push(parsed);
            }

            let next = remaining.lock().unwrap().next();
            let response = match next {
                Some(payload) => tiny_http::Response::from_string(payload.to_string()).with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
                ),
                None => tiny_http::Response::from_string("no more scripted responses").with_status_code(500),
            };
            request.respond(response).ok();
        }
    });

    (base_url, received)
}
