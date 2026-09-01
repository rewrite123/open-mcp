//! `mock-mcp-server` - a tiny standalone MCP server (Streamable HTTP transport)
//! used to exercise omcp/mcp in integration tests without hitting a real
//! service. Also handy for manual testing:
//!
//! ```text
//! mock-mcp-server --port 8765 --require-bearer test-token
//! ```
//!
//! Exposes three fixed tools: `echo`, `template.list`, and `time.now`.

use serde_json::{json, Value};
use std::net::TcpListener;

fn usage() -> &'static str {
    "Usage: mock-mcp-server [--port <PORT>] [--require-bearer <TOKEN>]"
}

fn main() {
    let mut port: u16 = 0;
    let mut require_bearer: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                port = args
                    .next()
                    .expect("--port requires a value")
                    .parse()
                    .expect("--port must be a number");
            }
            "--require-bearer" => {
                require_bearer = Some(args.next().expect("--require-bearer requires a value"));
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return;
            }
            other => {
                eprintln!("unrecognized argument '{other}'\n{}", usage());
                std::process::exit(2);
            }
        }
    }

    let listener = TcpListener::bind(("127.0.0.1", port)).expect("failed to bind mock server");
    let addr = listener.local_addr().expect("failed to read bound address");
    let server = tiny_http::Server::from_listener(listener, None).expect("failed to start server");

    // Tests parse this line to discover the ephemeral port that was bound.
    println!("LISTENING {addr}");
    use std::io::Write;
    std::io::stdout().flush().ok();

    for mut request in server.incoming_requests() {
        if let Some(expected) = &require_bearer {
            let expected_header = format!("Bearer {expected}");
            let authorized = request.headers().iter().any(|h| {
                h.field.as_str().as_str().eq_ignore_ascii_case("authorization") && h.value.as_str() == expected_header
            });
            if !authorized {
                let response = tiny_http::Response::from_string("unauthorized").with_status_code(401);
                request.respond(response).ok();
                continue;
            }
        }

        let mut body = String::new();
        request.as_reader().read_to_string(&mut body).ok();
        let parsed: Value = serde_json::from_str(&body).unwrap_or(json!({}));
        let method = parsed.get("method").and_then(Value::as_str).unwrap_or("");
        let id = parsed.get("id").cloned();

        match handle(method, &parsed) {
            Some(payload) => {
                let response = tiny_http::Response::from_string(payload.to_string()).with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
                );
                request.respond(response).ok();
            }
            None => {
                // Notification (e.g. notifications/initialized): no id, no body expected.
                let _ = id;
                request.respond(tiny_http::Response::empty(202)).ok();
            }
        }
    }
}

fn handle(method: &str, request: &Value) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "serverInfo": { "name": "mock-mcp-server", "version": env!("CARGO_PKG_VERSION") },
            }
        })),
        "notifications/initialized" => None,
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": tools() }
        })),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": call_tool(name, &arguments)
            }))
        }
        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("method not found: {method}") }
        })),
    }
}

fn tools() -> Value {
    json!([
        {
            "name": "echo",
            "description": "Echoes back the provided text.",
            "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] },
        },
        {
            "name": "template.list",
            "description": "Lists saved document templates.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "time.now",
            "description": "Returns a fixed timestamp (deterministic for tests).",
            "inputSchema": { "type": "object", "properties": {} },
        },
    ])
}

fn call_tool(name: &str, arguments: &Value) -> Value {
    match name {
        "echo" => {
            let text = arguments.get("text").and_then(Value::as_str).unwrap_or("");
            json!({ "content": [{ "type": "text", "text": text }] })
        }
        "template.list" => json!({
            "content": [{ "type": "text", "text": "[{\"name\":\"resume\"},{\"name\":\"invoice\"}]" }]
        }),
        "time.now" => json!({ "content": [{ "type": "text", "text": "2026-01-01T00:00:00Z" }] }),
        other => json!({ "isError": true, "content": [{ "type": "text", "text": format!("unknown tool: {other}") }] }),
    }
}
