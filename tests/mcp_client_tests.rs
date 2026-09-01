//! Integration tests for the MCP client layer against a real `mock-mcp-server`
//! subprocess (not an in-process stub).

mod support;

use omcp::mcp::McpClient;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use serde_json::json;
use support::spawn_mock_mcp_server;

#[tokio::test]
async fn initialize_lists_and_calls_tools() {
    let server = spawn_mock_mcp_server(Some("test-token"));

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, "Bearer test-token".parse().unwrap());
    let client = McpClient::new(server.base_url.clone(), headers).unwrap();

    client.initialize(None).await.expect("initialize should succeed");

    let tools = client.list_tools().await.expect("tools/list should succeed");
    let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"template.list"));
    assert!(names.contains(&"time.now"));

    let result = client.call_tool("echo", json!({ "text": "hello" })).await.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(text, "hello");

    let templates = client.call_tool("template.list", json!({})).await.unwrap();
    let text = templates["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("resume"));
}

#[tokio::test]
async fn missing_auth_header_is_rejected() {
    let server = spawn_mock_mcp_server(Some("test-token"));
    let client = McpClient::new(server.base_url.clone(), HeaderMap::new()).unwrap();

    let err = client.initialize(None).await.unwrap_err();
    assert!(err.to_string().contains("401"), "unexpected error: {err}");
}

#[tokio::test]
async fn unknown_tool_call_returns_error_payload() {
    let server = spawn_mock_mcp_server(None);
    let client = McpClient::new(server.base_url.clone(), HeaderMap::new()).unwrap();
    client.initialize(None).await.unwrap();

    let result = client.call_tool("does.not.exist", json!({})).await.unwrap();
    assert_eq!(result["isError"], true);
}

#[tokio::test]
async fn stdio_transport_initializes_lists_and_calls_tools() {
    let client = McpClient::stdio(env!("CARGO_BIN_EXE_stdio-mock-mcp-server"), &[]).unwrap();
    client.initialize(None).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools[0].name, "echo");
    let result = client.call_tool("echo", json!({ "text": "stdio works" })).await.unwrap();
    assert_eq!(result["content"][0]["text"], "stdio works");
}
