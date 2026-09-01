pub mod config;
pub mod endpoint;
pub mod hosts;
pub mod mcp;
pub mod meta;
pub mod ollama;
pub mod settings;

use anyhow::{Context, Result};
use endpoint::EndpointConfig;
use mcp::McpClient;
use meta::Meta;
use ollama::{mcp_tool_to_ollama, Message, OllamaClient};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

/// Connects to an MCP server and performs the `initialize` handshake.
pub async fn connect(mcp_url: &str, meta: &Meta) -> Result<McpClient> {
    connect_endpoint(&EndpointConfig::new(mcp_url.to_string()), meta).await
}

pub async fn connect_endpoint(endpoint: &EndpointConfig, meta: &Meta) -> Result<McpClient> {
    let client = if let Some(command) = endpoint.value.strip_prefix("stdio:") {
        let parts: Vec<String> = command.split_whitespace().map(str::to_string).collect();
        let executable = parts.first().context("stdio MCP command is empty")?;
        McpClient::stdio_with_body(executable, &parts[1..], endpoint.body.clone())?
    } else {
        let headers = meta.build_headers()?;
        let mut headers = headers;
        headers.extend(endpoint.header_map()?);
        let timeout = match endpoint.timeout_seconds {
            Some(seconds) => settings::seconds_to_duration(seconds)?,
            None => settings::timeout()?,
        };
        McpClient::http(endpoint.value.clone(), headers, endpoint.body.clone(), timeout)?
    };
    client
        .initialize(meta.protocol_version.as_deref())
        .await
        .with_context(|| format!("failed to initialize MCP session with '{}'", endpoint.value))?;
    Ok(client)
}

/// Restricts `tools` to `allowed` (matched by name) when an allow-list is given;
/// returns all tools unchanged otherwise.
pub fn filter_tools(tools: Vec<mcp::Tool>, allowed: Option<&[String]>) -> Vec<mcp::Tool> {
    match allowed {
        None => tools,
        Some(names) => tools.into_iter().filter(|t| names.iter().any(|n| n == &t.name)).collect(),
    }
}

struct RoutedHost {
    namespace: String,
    client: McpClient,
    tools: Vec<mcp::Tool>,
}

async fn connect_hosts(endpoints: Vec<EndpointConfig>, meta: &Meta) -> Result<Vec<RoutedHost>> {
    let mut hosts = Vec::new();
    for (index, endpoint) in endpoints.into_iter().enumerate() {
        let endpoint = if endpoint.value.starts_with("http://") || endpoint.value.starts_with("https://") || endpoint.value.starts_with("stdio:") {
            endpoint
        } else {
            hosts::load(&endpoint.value)?.unwrap_or(endpoint)
        };
        let base_namespace = endpoint.namespace.clone().unwrap_or_else(|| namespace_for(&endpoint.value, index));
        let namespace = if hosts.iter().any(|host: &RoutedHost| host.namespace == base_namespace) {
            format!("{}-{}", base_namespace, index + 1)
        } else {
            base_namespace
        };
        let client = connect_endpoint(&endpoint, meta).await?;
        let tools = client.list_tools().await?;
        hosts.push(RoutedHost { namespace, client, tools });
    }
    Ok(hosts)
}

fn namespace_for(value: &str, index: usize) -> String {
    let source = value.strip_prefix("stdio:").unwrap_or(value);
    let name = source.rsplit('/').next().unwrap_or(source).split_whitespace().next().unwrap_or("mcp");
    let sanitized: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    if sanitized.is_empty() { format!("mcp{}", index + 1) } else { sanitized }
}

fn router_tools(hosts: &[RoutedHost], allowed: Option<&[String]>) -> Vec<serde_json::Value> {
    let mut tools = vec![
        serde_json::json!({"type":"function","function":{"name":"mcp.search_namespaces","description":"Search connected MCP namespaces.","parameters":{"type":"object","properties":{"query":{"type":"string"}}}}}),
        serde_json::json!({"type":"function","function":{"name":"mcp.search_tools","description":"Search available MCP tools by name or description.","parameters":{"type":"object","properties":{"query":{"type":"string"},"namespace":{"type":"string"}}}}}),
    ];
    for host in hosts {
        for tool in filter_tools(host.tools.clone(), allowed) {
            let mut value = mcp_tool_to_ollama(&tool);
            value["function"]["name"] = serde_json::Value::from(format!("{}.{}", host.namespace, tool.name));
            tools.push(value);
        }
    }
    tools
}

/// Runs a chat session bridging an Ollama model and an MCP server.
/// `allowed_tools`, when set, restricts which MCP tools are offered to the model.
/// `seed_prompt`, when set, is `(role, text)` for an extra message (e.g. a
/// "user"-role prompt) pushed onto the conversation before the loop starts.
/// `message`, when set, sends that single message non-interactively (printing
/// just the answer) and returns, instead of starting an interactive REPL.
pub async fn run_chat(
    label: &str,
    mcp_url: &str,
    model: &str,
    model_host: &str,
    meta: Meta,
    allowed_tools: Option<&[String]>,
    seed_prompt: Option<(&str, &str)>,
    message: Option<&str>,
) -> Result<()> {
    let mcp_client = connect(mcp_url, &meta).await?;
    let tools = mcp_client.list_tools().await?;
    let tools = filter_tools(tools, allowed_tools);
    let ollama_tools: Vec<_> = tools.iter().map(mcp_tool_to_ollama).collect();
    let mut model_endpoint = EndpointConfig::new(model_host.to_string());
    if let Some(params) = meta.model_params.clone() {
        model_endpoint.body = serde_json::json!({ "options": params });
    }
    let ollama = OllamaClient::new(model_endpoint, model)?;

    let mut history = Vec::new();
    if let Some(system_prompt) = &meta.system_prompt {
        history.push(Message::system(system_prompt.clone()));
    }
    if let Some((role, text)) = seed_prompt {
        history.push(Message::with_role(role, text));
    }

    if let Some(message) = message {
        history.push(Message::user(message));
        return answer_turn(&ollama, &mcp_client, &ollama_tools, &mut history).await;
    }

    println!(
        "Connected to '{label}' ({} tool(s) available). Model: {model} @ {model_host}",
        tools.len()
    );
    println!("Type your message and press Enter. Ctrl+D to exit.\n");

    // rustyline gives proper line editing (arrow keys, backspace, history)
    // instead of raw stdin reads, which just echo escape codes like `^[[C`.
    let mut editor = DefaultEditor::new().context("failed to initialize the input editor")?;
    loop {
        let line = match editor.readline("> ") {
            Ok(line) => line,
            Err(ReadlineError::Eof) | Err(ReadlineError::Interrupted) => break,
            Err(err) => return Err(err.into()),
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        editor.add_history_entry(line).ok();
        history.push(Message::user(line));
        answer_turn(&ollama, &mcp_client, &ollama_tools, &mut history).await?;
    }
    Ok(())
}

/// Runs a chat session using independently configured MCP and model endpoints.
pub async fn run_chat_endpoints(
    mcp_endpoint: EndpointConfig,
    model_endpoint: EndpointConfig,
    model: &str,
    meta: Meta,
    allowed_tools: Option<&[String]>,
    mut history: Vec<Message>,
    message: Option<&str>,
) -> Result<()> {
    let mcp_client = connect_endpoint(&mcp_endpoint, &meta).await?;
    let tools = filter_tools(mcp_client.list_tools().await?, allowed_tools);
    let ollama_tools: Vec<_> = tools.iter().map(mcp_tool_to_ollama).collect();
    let model_endpoint_name = model_endpoint.value.clone();
    let ollama = OllamaClient::new(model_endpoint, model)?;

    if let Some(system_prompt) = &meta.system_prompt {
        history.insert(0, Message::system(system_prompt.clone()));
    }
    if let Some(message) = message {
        history.push(Message::user(message));
        return answer_turn(&ollama, &mcp_client, &ollama_tools, &mut history).await;
    }

    println!(
        "Connected to '{}' ({} tool(s) available). Model: {} @ {}",
        mcp_endpoint.value,
        tools.len(),
        model,
        model_endpoint_name
    );
    println!("Type your message and press Enter. Ctrl+D to exit.\n");
    let mut editor = DefaultEditor::new().context("failed to initialize the input editor")?;
    loop {
        let line = match editor.readline("> ") {
            Ok(line) => line,
            Err(ReadlineError::Eof) | Err(ReadlineError::Interrupted) => break,
            Err(err) => return Err(err.into()),
        };
        let line = line.trim();
        if line.is_empty() { continue; }
        editor.add_history_entry(line).ok();
        history.push(Message::user(line));
        answer_turn(&ollama, &mcp_client, &ollama_tools, &mut history).await?;
    }
    Ok(())
}

fn parse_added_host(spec: &str) -> Result<EndpointConfig> {
    let mut tokens = spec.split_whitespace();
    if tokens.next() != Some("-host") { return Err(anyhow::anyhow!("/add-host requires '-host <URL-or-name>'")); }
    let host = tokens.next().context("/add-host requires a host value")?;
    let mut endpoint = hosts::load(host)?.unwrap_or_else(|| EndpointConfig::new(host.to_string()));
    while let Some(flag) = tokens.next() {
        let value = tokens.next().with_context(|| format!("{flag} requires JSON"))?;
        match flag {
            "-headers" => endpoint.add_headers_json(value)?,
            "-body" => endpoint.add_body_json(value)?,
            _ => return Err(anyhow::anyhow!("/add-host supports only -host, -headers, and -body")),
        }
    }
    Ok(endpoint)
}

pub async fn run_chat_hosts(
    endpoints: Vec<EndpointConfig>,
    model_endpoint: EndpointConfig,
    model: &str,
    meta: Meta,
    allowed_tools: Option<&[String]>,
    mut history: Vec<Message>,
    message: Option<&str>,
) -> Result<()> {
    let mut hosts = connect_hosts(endpoints, &meta).await?;
    let model_endpoint_name = model_endpoint.value.clone();
    let ollama = OllamaClient::new(model_endpoint, model)?;
    if let Some(system_prompt) = &meta.system_prompt { history.insert(0, Message::system(system_prompt.clone())); }
    if let Some(message) = message {
        history.push(Message::user(message));
        return answer_routed_turn(&ollama, &mut hosts, allowed_tools, &mut history).await;
    }
    let tool_count: usize = hosts.iter().map(|host| filter_tools(host.tools.clone(), allowed_tools).len()).sum();
    println!("Connected to {} MCP host(s) ({} tool(s) available). Model: {} @ {}", hosts.len(), tool_count, model, model_endpoint_name);
    println!("Type your message and press Enter. Ctrl+D to exit. Use /add-host -host <URL-or-name> to attach another server.\n");
    let mut editor = DefaultEditor::new().context("failed to initialize the input editor")?;
    loop {
        let line = match editor.readline("> ") { Ok(line) => line, Err(ReadlineError::Eof) | Err(ReadlineError::Interrupted) => break, Err(err) => return Err(err.into()) };
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Some(spec) = line.strip_prefix("/add-host ") {
            let endpoint = parse_added_host(spec)?;
            let index = hosts.len();
            let base_namespace = namespace_for(&endpoint.value, index);
            let namespace = if hosts.iter().any(|host| host.namespace == base_namespace) { format!("{}-{}", base_namespace, index + 1) } else { base_namespace };
            let client = connect_endpoint(&endpoint, &meta).await?;
            let tools = client.list_tools().await?;
            hosts.push(RoutedHost { namespace: namespace.clone(), client, tools });
            println!("Added MCP host '{namespace}'.");
            continue;
        }
        editor.add_history_entry(line).ok();
        history.push(Message::user(line));
        answer_routed_turn(&ollama, &mut hosts, allowed_tools, &mut history).await?;
    }
    Ok(())
}

async fn answer_routed_turn(ollama: &OllamaClient, hosts: &mut [RoutedHost], allowed: Option<&[String]>, history: &mut Vec<Message>) -> Result<()> {
    for _ in 0..8 {
        let reply = ollama.chat(history, &router_tools(hosts, allowed)).await?;
        let calls = reply.tool_calls.clone().unwrap_or_default();
        history.push(reply.clone());
        if calls.is_empty() { println!("{}", reply.content); return Ok(()); }
        for call in calls {
            let name = &call.function.name;
            let result = if name == "mcp.search_namespaces" {
                let query = call.function.arguments.get("query").and_then(serde_json::Value::as_str).unwrap_or("").to_ascii_lowercase();
                let names: Vec<_> = hosts.iter().filter(|host| query.is_empty() || host.namespace.to_ascii_lowercase().contains(&query)).map(|host| host.namespace.clone()).collect();
                Ok(serde_json::json!(names))
            } else if name == "mcp.search_tools" {
                let query = call.function.arguments.get("query").and_then(serde_json::Value::as_str).unwrap_or("").to_ascii_lowercase();
                let namespace = call.function.arguments.get("namespace").and_then(serde_json::Value::as_str);
                let matches: Vec<_> = hosts.iter().filter(|host| namespace.map_or(true, |ns| ns == host.namespace)).flat_map(|host| host.tools.iter().map(move |tool| (host, tool))).filter(|(_, tool)| query.is_empty() || tool.name.to_ascii_lowercase().contains(&query) || tool.description.as_deref().unwrap_or("").to_ascii_lowercase().contains(&query)).map(|(host, tool)| serde_json::json!({"name":format!("{}.{}",host.namespace,tool.name),"description":tool.description})).collect();
                Ok(serde_json::json!(matches))
            } else {
                let (namespace, tool) = name.split_once('.').ok_or_else(|| anyhow::anyhow!("tool '{name}' has no MCP namespace"))?;
                let host = hosts.iter().find(|host| host.namespace == namespace).ok_or_else(|| anyhow::anyhow!("unknown MCP namespace '{namespace}'"))?;
                host.client.call_tool(tool, call.function.arguments.clone()).await
            };
            let content = result.map(|v| v.to_string()).unwrap_or_else(|err| format!("error: {err}"));
            history.push(Message::tool_result(name, content));
        }
    }
    Err(anyhow::anyhow!("MCP interaction exceeded 8 tool-call rounds"))
}

/// Runs the model/tool-call loop for the most recently added user message,
/// printing the model's final reply. Allows several rounds of tool calls
/// before the model settles on a plain-text answer.
async fn answer_turn(
    ollama: &OllamaClient,
    mcp_client: &McpClient,
    ollama_tools: &[serde_json::Value],
    history: &mut Vec<Message>,
) -> Result<()> {
    for _ in 0..8 {
        let reply = ollama.chat(history, ollama_tools).await?;
        let tool_calls = reply.tool_calls.clone().unwrap_or_default();
        history.push(reply.clone());

        if tool_calls.is_empty() {
            println!("{}", reply.content);
            break;
        }

        for call in tool_calls {
            let result = mcp_client
                .call_tool(&call.function.name, call.function.arguments.clone())
                .await;
            let content = match result {
                Ok(value) => serde_json::to_string(&value).unwrap_or_default(),
                Err(err) => format!("error: {err}"),
            };
            history.push(Message::tool_result(call.function.name, content));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> mcp::Tool {
        mcp::Tool { name: name.to_string(), description: None, input_schema: json!({}) }
    }

    #[test]
    fn filter_tools_returns_all_when_no_allowlist() {
        let tools = vec![tool("a"), tool("b")];
        assert_eq!(filter_tools(tools, None).len(), 2);
    }

    #[test]
    fn filter_tools_restricts_to_allowlist() {
        let tools = vec![tool("a"), tool("b"), tool("c")];
        let allowed = vec!["b".to_string()];
        let filtered = filter_tools(tools, Some(&allowed));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "b");
    }

    #[test]
    fn filter_tools_empty_allowlist_yields_no_tools() {
        let tools = vec![tool("a")];
        let allowed: Vec<String> = vec![];
        assert!(filter_tools(tools, Some(&allowed)).is_empty());
    }
}
