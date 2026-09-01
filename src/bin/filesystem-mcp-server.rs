//! A local MCP server that exposes a deliberately confirmation-gated filesystem.
//!
//! ```text
//! filesystem-mcp-server --start-dir /path/available/to/the/model [--port 8765]
//! ```

use serde_json::{json, Value};
use std::fs;
use std::io::{self, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

struct FileSystem {
    start_dir: PathBuf,
    working_dir: PathBuf,
}

fn main() {
    let mut port: u16 = 0;
    let mut start_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                port = args.next().expect("--port requires a value").parse().expect("--port must be a number");
            }
            "--start-dir" => start_dir = Some(PathBuf::from(args.next().expect("--start-dir requires a path"))),
            "-h" | "--help" => {
                println!("Usage: filesystem-mcp-server --start-dir <PATH> [--port <PORT>]");
                return;
            }
            other => {
                eprintln!("unrecognized argument '{other}'");
                std::process::exit(2);
            }
        }
    }

    let start_dir = start_dir.expect("--start-dir is required");
    let start_dir = fs::canonicalize(&start_dir).expect("--start-dir must be an existing directory");
    assert!(start_dir.is_dir(), "--start-dir must be a directory");

    let listener = TcpListener::bind(("127.0.0.1", port)).expect("failed to bind filesystem MCP server");
    let addr = listener.local_addr().expect("failed to read bound address");
    let server = tiny_http::Server::from_listener(listener, None).expect("failed to start server");
    let state = Mutex::new(FileSystem { working_dir: start_dir.clone(), start_dir });

    println!("LISTENING {addr}");
    io::stdout().flush().ok();

    for mut request in server.incoming_requests() {
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body).ok();
        let parsed: Value = serde_json::from_str(&body).unwrap_or(json!({}));
        let method = parsed.get("method").and_then(Value::as_str).unwrap_or("");
        let response = handle(method, &parsed, &state);
        match response {
            Some(payload) => {
                let response = tiny_http::Response::from_string(payload.to_string()).with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
                );
                request.respond(response).ok();
            }
            None => {
                request.respond(tiny_http::Response::empty(202)).ok();
            }
        }
    }
}

fn handle(method: &str, request: &Value, state: &Mutex<FileSystem>) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => Some(ok(id, json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "filesystem-mcp-server", "version": env!("CARGO_PKG_VERSION") }
        }))),
        "notifications/initialized" => None,
        "tools/list" => Some(ok(id, json!({ "tools": tools() }))),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            Some(ok(id, call_tool(name, &arguments, state)))
        }
        _ => Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "method not found" } })),
    }
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn tools() -> Value {
    json!([
        tool("get-start-dir", "Returns the configured filesystem root the model initially has access to.", json!({})),
        tool("pwd", "Returns the server's current working directory.", json!({})),
        tool("cd", "Changes the server's working directory.", json!({ "path": { "type": "string" } })),
        tool("ask-rm", "Requests deletion of a file or directory. Always requires manual confirmation.", json!({ "path": { "type": "string" }, "recursive": { "type": "boolean", "default": false } })),
        tool("ls", "Lists direct children of a directory, or the current directory when path is omitted.", json!({ "path": { "type": "string" } })),
        tool("cat", "Reads a UTF-8 text file and returns its contents.", json!({ "path": { "type": "string" } })),
        tool("edit", "Writes content to a file, replacing existing content or creating it.", json!({ "path": { "type": "string" }, "content": { "type": "string" } })),
        tool("append", "Appends content to a file, creating it when absent.", json!({ "path": { "type": "string" }, "content": { "type": "string" } })),
        tool("rg", "Runs ripgrep from the current directory or an optional path.", json!({ "pattern": { "type": "string" }, "path": { "type": "string" } })),
        tool("confirm", "Prompts the local user for a yes/no decision.", json!({ "question": { "type": "string" } })),
        tool("suggest", "Shows a suggested approach and asks the local user for a free-form response.", json!({ "suggestion": { "type": "string" } }))
    ])
}

fn tool(name: &str, description: &str, properties: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties }
    })
}

fn call_tool(name: &str, args: &Value, state: &Mutex<FileSystem>) -> Value {
    if name == "get-start-dir" {
        let state = state.lock().unwrap();
        return text(&state.start_dir.display().to_string());
    }
    if name == "confirm" {
        return text(if ask_yes_no(&required(args, "question")) { "yes" } else { "no" });
    }
    if name == "suggest" {
        return text(&ask_text(&required(args, "suggestion")));
    }

    let mut state = state.lock().unwrap();
    match file_operation(name, args, &mut state) {
        Ok(output) => text(&output),
        Err(error) => json!({ "isError": true, "content": [{ "type": "text", "text": error }] }),
    }
}

fn file_operation(name: &str, args: &Value, state: &mut FileSystem) -> Result<String, String> {
    let path = match name {
        "pwd" => state.working_dir.clone(),
        "cd" | "ask-rm" | "cat" | "edit" | "append" => resolve_path(state, &required(args, "path"))?,
        "ls" => match args.get("path").and_then(Value::as_str) {
            Some(path) => resolve_path(state, path)?,
            None => state.working_dir.clone(),
        },
        "rg" => match args.get("path").and_then(Value::as_str) {
            Some(path) => resolve_path(state, path)?,
            None => state.working_dir.clone(),
        },
        _ => return Err(format!("unknown tool: {name}")),
    };

    let inside_start_dir = path.starts_with(&state.start_dir);
    let description = format!("{name} {}", path.display());
    if !ask_yes_no(&format!("Allow MCP filesystem server to {description}?")) {
        return Err("operation denied by user".to_string());
    }
    if !inside_start_dir && !ask_yes_no(&format!("{} is outside the configured start directory {}. Allow it?", path.display(), state.start_dir.display())) {
        return Err("outside-root operation denied by user".to_string());
    }

    match name {
        "pwd" => Ok(path.display().to_string()),
        "cd" => {
            if !path.is_dir() {
                return Err(format!("not a directory: {}", path.display()));
            }
            state.working_dir = fs::canonicalize(&path).map_err(|e| e.to_string())?;
            Ok(state.working_dir.display().to_string())
        }
        "ls" => list_dir(&path),
        "cat" => fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display())),
        "edit" => write_file(&path, &required(args, "content"), false),
        "append" => write_file(&path, &required(args, "content"), true),
        "ask-rm" => remove_path(&path, args.get("recursive").and_then(Value::as_bool).unwrap_or(false)),
        "rg" => ripgrep(&required(args, "pattern"), &path),
        _ => Err(format!("unknown tool: {name}")),
    }
}

fn resolve_path(state: &FileSystem, raw: &str) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(raw);
    let joined = if candidate.is_absolute() { candidate } else { state.working_dir.join(candidate) };
    if joined.exists() {
        fs::canonicalize(&joined).map_err(|e| format!("{}: {e}", joined.display()))
    } else {
        let parent = joined.parent().ok_or_else(|| "path has no parent".to_string())?;
        let parent = fs::canonicalize(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        Ok(parent.join(joined.file_name().ok_or_else(|| "path has no file name".to_string())?))
    }
}

fn required<'a>(args: &'a Value, name: &str) -> String {
    args.get(name).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn list_dir(path: &Path) -> Result<String, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .map(|entry| entry.map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries
        .into_iter()
        .map(|entry| {
            let suffix = if entry.path().is_dir() { "/" } else { "" };
            format!("{}{}", entry.file_name().to_string_lossy(), suffix)
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn write_file(path: &Path, content: &str, append: bool) -> Result<String, String> {
    if append {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
        file.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
    } else {
        fs::write(path, content).map_err(|e| e.to_string())?;
    }
    Ok(format!("wrote {}", path.display()))
}

fn remove_path(path: &Path, recursive: bool) -> Result<String, String> {
    if !ask_yes_no(&format!("Remove {}? This cannot be undone.", path.display())) {
        return Err("removal denied by user".to_string());
    }
    if path.is_dir() {
        if !recursive {
            return Err("refusing to remove a directory without recursive=true".to_string());
        }
        fs::remove_dir_all(path).map_err(|e| e.to_string())?;
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(format!("removed {}", path.display()))
}

fn ripgrep(pattern: &str, path: &Path) -> Result<String, String> {
    let output = Command::new("rg")
        .arg("--line-number")
        .arg("--no-heading")
        .arg(pattern)
        .arg(path)
        .output()
        .map_err(|e| format!("failed to start rg: {e}"))?;
    if output.status.code() == Some(1) {
        return Ok(String::new());
    }
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn ask_yes_no(question: &str) -> bool {
    loop {
        eprint!("\n[filesystem-mcp] {question} [yes/no]: ");
        io::stderr().flush().ok();
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "yes" | "y" => return true,
            "no" | "n" | "" => return false,
            _ => eprintln!("Please answer yes or no."),
        }
    }
}

fn ask_text(suggestion: &str) -> String {
    eprint!("\n[filesystem-mcp] Suggested approach: {suggestion}\nYour response: ");
    io::stderr().flush().ok();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    answer.trim().to_string()
}

fn text(value: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": value }] })
}
