//! JSON-lines MCP fixture used by integration tests.

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let request: Value = match line.and_then(|line| serde_json::from_str(&line).map_err(io::Error::other)) {
            Ok(request) => request,
            Err(_) => break,
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let Some(id) = request.get("id") else { continue };
        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2025-06-18",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "stdio-mock", "version": "1" }
            }),
            "tools/list" => json!({ "tools": [{
                "name": "echo", "description": "Returns supplied text.",
                "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } }
            }]}),
            "tools/call" => json!({ "content": [{ "type": "text", "text": request["params"]["arguments"]["text"] }] }),
            _ => continue,
        };
        writeln!(stdout, "{}", json!({ "jsonrpc": "2.0", "id": id, "result": result })).ok();
        stdout.flush().ok();
    }
}
