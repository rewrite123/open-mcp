//! Integration tests for ordered model/MCP endpoint scopes.

mod support;

use std::process::{Command, Stdio};
use support::{spawn_mock_mcp_server, spawn_mock_ollama_server_capturing, TempHome};

fn response(text: &str) -> serde_json::Value {
    serde_json::json!({ "message": { "role": "assistant", "content": text }, "done": true })
}

#[test]
fn mcp_scopes_headers_and_bodies_when_host_precedes_model() {
    let mcp = spawn_mock_mcp_server(None);
    let (model, received) = spawn_mock_ollama_server_capturing(vec![response("ok")]);
    let output = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .args(["-host", &mcp.base_url, "-headers", r#"{"X-Mcp":"one"}"#, "-body", r#"{"mcp_default":true}"#])
        .args(["-model", &model, "-headers", r#"{"X-Model":"two"}"#, "-body", r#"{"model":"provider-model","options":{"num_ctx":32768}}"#])
        .args(["-message", "hello"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let requests = received.lock().unwrap();
    assert_eq!(requests[0]["options"]["num_ctx"], 32768);
    assert_eq!(requests[0]["model"], "provider-model");
}

#[test]
fn mcp_scopes_headers_bodies_and_messages_when_model_precedes_host() {
    let mcp = spawn_mock_mcp_server(None);
    let (model, received) = spawn_mock_ollama_server_capturing(vec![response("ok")]);
    let output = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .args(["-model", &model, "-body", r#"{"options":{"temperature":0.2}}"#])
        .args(["-messages", r#"[{"role":"system","content":"seed"}]"#])
        .args(["-host", &mcp.base_url, "-headers", r#"{"X-Mcp":"one"}"#, "-body", r#"{"client":"test"}"#])
        .args(["-message", "hello"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let requests = received.lock().unwrap();
    assert_eq!(requests[0]["options"]["temperature"], 0.2);
    assert_eq!(requests[0]["messages"][0]["content"], "seed");
    assert_eq!(requests[0]["messages"][1]["content"], "hello");
}

#[test]
fn ordinary_model_name_defaults_to_local_ollama_endpoint() {
    let mcp = spawn_mock_mcp_server(None);
    let mut child = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .args(["-host", &mcp.base_url, "-model", "granite4.2:latest"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("granite4.2:latest @ http://localhost:11434"));
}

#[test]
fn rejects_scoped_fields_before_a_selector() {
    let output = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .args(["-headers", r#"{"X":"y"}"#, "-host", "http://x", "-model", "m"])
        .output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must appear after -host or -model"));
}

#[test]
fn profile_config_applies_model_body_messages_and_mcp_body() {
    let mcp = spawn_mock_mcp_server(None);
    let (model, received) = spawn_mock_ollama_server_capturing(vec![response("ok")]);
    let home = TempHome::new();
    std::fs::write(home.mcp_dir().join("config"), format!(
        "name scoped\n    mcp {}\n    mcp_headers {{\"X-Mcp\":\"yes\"}}\n    mcp_body {{\"client\":\"profile\"}}\n    model ignored\n    model_endpoint {}\n    model_headers {{\"X-Model\":\"yes\"}}\n    model_body {{\"options\":{{\"num_ctx\":8192}}}}\n    messages [{{\"role\":\"system\",\"content\":\"profile seed\"}}]\n",
        mcp.base_url, model
    )).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_omcp"))
        .args(["chat", "scoped", "hello"]).env("HOME", &home.path).output().unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let requests = received.lock().unwrap();
    assert_eq!(requests[0]["options"]["num_ctx"], 8192);
    assert_eq!(requests[0]["messages"][0]["content"], "profile seed");
}

#[test]
fn multiple_hosts_are_namespaced_and_catalog_tools_are_always_present() {
    let first = spawn_mock_mcp_server(None);
    let second = spawn_mock_mcp_server(None);
    let (model, received) = spawn_mock_ollama_server_capturing(vec![response("ok")]);
    let output = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .args(["-model", &model, "-host", &first.base_url, "-host", &second.base_url, "-message", "hello"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let requests = received.lock().unwrap();
    let names: Vec<_> = requests[0]["tools"].as_array().unwrap().iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"mcp.search_namespaces"));
    assert!(names.contains(&"mcp.search_tools"));
    assert!(names.contains(&"mcp.echo"));
    assert!(names.contains(&"mcp-2.echo"));
}

#[test]
fn nsname_sets_the_namespace_for_its_single_host() {
    let first = spawn_mock_mcp_server(None);
    let second = spawn_mock_mcp_server(None);
    let (model, received) = spawn_mock_ollama_server_capturing(vec![response("ok")]);
    let output = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .args(["-model", &model, "-host", &first.base_url, "-nsname", "documents", "-host", &second.base_url, "-nsname", "files", "-message", "hello"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let requests = received.lock().unwrap();
    let names: Vec<_> = requests[0]["tools"].as_array().unwrap().iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"documents.echo"));
    assert!(names.contains(&"files.echo"));
    assert!(!names.contains(&"mcp.echo"));
}

#[test]
fn nsname_must_follow_its_host_immediately() {
    let output = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .args(["-host", "http://example.test/mcp", "-headers", r#"{"X":"y"}"#, "-nsname", "late", "-model", "m"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("immediately after its -host"));
}

#[test]
fn timeout_must_be_scoped_and_valid() {
    let invalid = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .args(["-timeout", "five", "-host", "http://example.test/mcp", "-model", "m"])
        .output().unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("-timeout must be"));

    let unscoped = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .args(["-timeout", "60", "-host", "http://example.test/mcp", "-model", "m"])
        .output().unwrap();
    assert!(!unscoped.status.success());
    assert!(String::from_utf8_lossy(&unscoped.stderr).contains("must appear after -host or -model"));
}

#[test]
fn profile_config_supports_multiple_structured_mcp_hosts() {
    let first = spawn_mock_mcp_server(None);
    let second = spawn_mock_mcp_server(None);
    let (model, received) = spawn_mock_ollama_server_capturing(vec![response("ok")]);
    let home = TempHome::new();
    std::fs::write(home.mcp_dir().join("config"), format!(
        "name multi\n    mcp {}\n    mcp_hosts [{{\"host\":\"{}\",\"body\":{{\"one\":true}}}},{{\"host\":\"{}\",\"body\":{{\"two\":true}}}}]\n    model ignored\n    model_endpoint {}\n",
        first.base_url, first.base_url, second.base_url, model
    )).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_omcp"))
        .args(["chat", "multi", "hello"]).env("HOME", &home.path).output().unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(received.lock().unwrap().len(), 1);
}

#[test]
fn named_host_from_mcp_hosts_file_is_resolved_by_cli() {
    let mcp = spawn_mock_mcp_server(None);
    let (model, received) = spawn_mock_ollama_server_capturing(vec![response("ok")]);
    let home = TempHome::new();
    std::fs::write(
        home.mcp_dir().join("hosts"),
        format!("name demo\n    host {}\n    headers {{\"X-From-Hosts\":\"yes\"}}\n    body {{\"source\":\"hosts-file\"}}\n", mcp.base_url),
    ).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mcp"))
        .args(["-model", &model, "-host", "demo", "-message", "hello"])
        .env("HOME", &home.path)
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(received.lock().unwrap().len(), 1);
}
