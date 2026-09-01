//! End-to-end test against a real local Ollama instance plus a real
//! `mock-mcp-server` subprocess. Not run by default (requires a running
//! Ollama with the target model pulled); run explicitly with:
//!
//! ```text
//! cargo test --test real_ollama_tests -- --ignored
//! ```
//!
//! Override the model with `OMCP_TEST_MODEL` (default: granite4.2:latest) and
//! the host with `OMCP_TEST_OLLAMA_HOST` (default: http://localhost:11434).

mod support;

use std::process::Command;
use support::spawn_mock_mcp_server;

#[test]
#[ignore = "requires a locally running Ollama instance"]
fn real_ollama_calls_mock_mcp_tool_end_to_end() {
    let host = std::env::var("OMCP_TEST_OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
    if !support::ollama_is_reachable(&host) {
        eprintln!("skipping: no Ollama reachable at {host}");
        return;
    }
    let model = std::env::var("OMCP_TEST_MODEL").unwrap_or_else(|_| "granite4.2:latest".to_string());

    let server = spawn_mock_mcp_server(None);

    // -message answers non-interactively, no stdin piping needed.
    let output = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .arg("-host")
        .arg(&server.base_url)
        .arg("-model")
        .arg(&model)
        .arg("-model-host")
        .arg(&host)
        .arg("-tools")
        .arg("template.list")
        .arg("-prompt")
        .arg("Every request is already authenticated via API key. Never ask for login credentials.")
        .arg("-message")
        .arg("List the current templates.")
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(stdout.contains("resume"), "expected a mention of the 'resume' template, got: {stdout}");
}
