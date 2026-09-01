//! One-off MCP chat client with ordered endpoint scopes.
//!
//! `-headers` and `-body` attach to the most recently declared `-host` or
//! `-model`. Models that are not URLs use Ollama at localhost:11434.

use anyhow::{anyhow, bail, Context, Result};
use omcp::endpoint::{parse_json, EndpointConfig};
use omcp::meta::Meta;
use omcp::ollama::Message;
use std::path::PathBuf;

fn usage() -> &'static str {
    "Usage: mcp -host <MCP_URL|stdio:COMMAND> [-nsname <NAME>] [-headers <JSON>] [-body <JSON>] [-timeout <SECONDS>]\n\
     \x20         -model <MODEL_NAME|MODEL_URL> [-headers <JSON>] [-body <JSON>] [-timeout <SECONDS>] [-messages <JSON>]\n\
     \x20         [-tools <NAME,NAME,...>] [-message <TEXT>] [-meta <PATH>]\n\
     \x20         [-prompt <TEXT>] [-prompt-type system|user|assistant] [-protocol-version <VERSION>]\n\
     \x20\n\
     \x20-headers and -body are repeatable JSON objects and belong to the most recent\n\
     \x20-host or model selector. -messages belongs to the most recent model selector."
}

enum Scope { Host, Model }

struct Parsed {
    hosts: Vec<EndpointConfig>,
    model: Option<EndpointConfig>,
    model_name: Option<String>,
    messages: Vec<Message>,
    tools: Option<Vec<String>>,
    message: Option<String>,
    meta_path: Option<String>,
    prompt: Option<String>,
    prompt_type: String,
    protocol_version: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let parsed = parse_args(std::env::args().skip(1))?;
    let mut meta = match parsed.meta_path {
        Some(path) => Meta::load(&expand_tilde(&path))?,
        None => Meta::default(),
    };
    if let Some(version) = parsed.protocol_version { meta.protocol_version = Some(version); }
    let tools = parsed.tools.or_else(|| meta.allowed_tools.clone());
    let mut messages = parsed.messages;
    if let Some(prompt) = parsed.prompt {
        messages.push(Message::with_role(parsed.prompt_type, prompt));
    }
    omcp::run_chat_hosts(
        parsed.hosts,
        parsed.model.context("-model is required")?,
        &parsed.model_name.context("-model is required")?,
        meta,
        tools.as_deref(),
        messages,
        parsed.message.as_deref(),
    ).await
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Parsed> {
    let mut parsed = Parsed {
        hosts: Vec::new(), model: None, model_name: None, messages: Vec::new(), tools: None,
        message: None, meta_path: None, prompt: None, prompt_type: "system".to_string(), protocol_version: None,
    };
    let mut scope: Option<Scope> = None;
    let mut namespace_is_next = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-host" => {
                let value = next_value(&mut args, "-host")?;
                parsed.hosts.push(omcp::hosts::load(&value)?.unwrap_or_else(|| EndpointConfig::new(value)));
                scope = Some(Scope::Host);
                namespace_is_next = true;
            }
            "-model" => {
                let value = next_value(&mut args, "-model")?;
                parsed.model_name = Some(if is_url(&value) { String::new() } else { value.clone() });
                parsed.model = Some(EndpointConfig::new(if is_url(&value) { value } else { "http://localhost:11434".to_string() }));
                scope = Some(Scope::Model);
                namespace_is_next = false;
            }
            "-nsname" => {
                if !matches!(scope, Some(Scope::Host)) || !namespace_is_next {
                    bail!("-nsname must appear immediately after its -host");
                }
                let name = next_value(&mut args, "-nsname")?;
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                    bail!("-nsname may contain only ASCII letters, numbers, '_' and '-'");
                }
                parsed.hosts.last_mut().unwrap().namespace = Some(name);
                namespace_is_next = false;
            }
            "-headers" => match scope {
                Some(Scope::Host) => parsed.hosts.last_mut().unwrap().add_headers_json(&next_value(&mut args, "-headers")?)?,
                Some(Scope::Model) => parsed.model.as_mut().unwrap().add_headers_json(&next_value(&mut args, "-headers")?)?,
                None => bail!("-headers must appear after -host or -model"),
            },
            "-body" => match scope {
                Some(Scope::Host) => parsed.hosts.last_mut().unwrap().add_body_json(&next_value(&mut args, "-body")?)?,
                Some(Scope::Model) => parsed.model.as_mut().unwrap().add_body_json(&next_value(&mut args, "-body")?)?,
                None => bail!("-body must appear after -host or -model"),
            },
            "-timeout" => {
                let raw = next_value(&mut args, "-timeout")?;
                let seconds: i64 = raw.parse().map_err(|_| anyhow!("-timeout must be -1 or a non-negative number of seconds"))?;
                if seconds < -1 { bail!("-timeout must be -1 or a non-negative number of seconds"); }
                match scope {
                    Some(Scope::Host) => parsed.hosts.last_mut().unwrap().timeout_seconds = Some(seconds),
                    Some(Scope::Model) => parsed.model.as_mut().unwrap().timeout_seconds = Some(seconds),
                    None => bail!("-timeout must appear after -host or -model"),
                }
            }
            "-messages" => {
                if !matches!(scope, Some(Scope::Model)) { bail!("-messages must appear after -model"); }
                let value = parse_json(&next_value(&mut args, "-messages")?, "-messages")?;
                let messages: Vec<Message> = serde_json::from_value(value).context("-messages requires a JSON array of messages")?;
                parsed.messages.extend(messages);
            }
            "-tools" => parsed.tools = Some(next_value(&mut args, "-tools")?.split(',').filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string()).collect()),
            "-message" => parsed.message = Some(next_value(&mut args, "-message")?),
            "-meta" => parsed.meta_path = Some(next_value(&mut args, "-meta")?),
            "-prompt" => parsed.prompt = Some(next_value(&mut args, "-prompt")?),
            "-prompt-type" => parsed.prompt_type = next_value(&mut args, "-prompt-type")?,
            "-protocol-version" => parsed.protocol_version = Some(next_value(&mut args, "-protocol-version")?),
            "-h" | "--help" | "-help" => { println!("{}", usage()); std::process::exit(0); }
            other => bail!("unrecognized argument '{other}'\n{}", usage()),
        }
        if !matches!(arg.as_str(), "-host" | "-nsname") { namespace_is_next = false; }
    }
    if !matches!(parsed.prompt_type.as_str(), "system" | "user" | "assistant") {
        bail!("-prompt-type must be system, user, or assistant");
    }
    if parsed.hosts.is_empty() { bail!("-host is required"); }
    Ok(parsed)
}

fn is_url(value: &str) -> bool { value.starts_with("http://") || value.starts_with("https://") }

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next().ok_or_else(|| anyhow!("{flag} requires a value"))
}

fn expand_tilde(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/").and_then(|path| dirs::home_dir().map(|home| home.join(path))) {
        Some(path) => path,
        None => PathBuf::from(raw),
    }
}
