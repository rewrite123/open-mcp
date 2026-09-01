//! Parsing for `~/.mcp/config`, an ssh_config-style file describing MCP servers.
//!
//! Example entry:
//! ```text
//! name exampleMcp
//!     mcp https://docmason.co/mcp
//!     model granite4.2:latest
//!     model_host http://localhost:11434
//!     meta .mcp/exampleMcp.json
//! ```

use anyhow::{bail, Context, Result};
use crate::endpoint::EndpointConfig;
use crate::ollama::Message;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_MODEL_HOST: &str = "http://localhost:11434";

/// One `name` block from the config file.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub name: String,
    pub mcp: String,
    pub model: String,
    pub model_host: String,
    /// Resolved absolute path to the meta json file, if configured.
    pub meta: Option<PathBuf>,
    pub mcp_endpoint: EndpointConfig,
    pub mcp_endpoints: Vec<EndpointConfig>,
    pub model_endpoint: EndpointConfig,
    pub messages: Vec<Message>,
}

/// Returns `~/.mcp`.
pub fn mcp_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".mcp"))
}

/// Returns `~/.mcp/config`.
pub fn config_path() -> Result<PathBuf> {
    Ok(mcp_dir()?.join("config"))
}

/// Loads and parses all entries from `~/.mcp/config`.
pub fn load_all() -> Result<Vec<ServerConfig>> {
    let path = config_path()?;
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file at {}", path.display()))?;
    parse(&contents)
}

/// Loads a single entry by name.
pub fn load(name: &str) -> Result<ServerConfig> {
    let entries = load_all()?;
    entries
        .into_iter()
        .find(|e| e.name == name)
        .with_context(|| format!("no entry named '{name}' found in {}", config_path().unwrap_or_default().display()))
}

fn parse(contents: &str) -> Result<Vec<ServerConfig>> {
    let home = dirs::home_dir();
    let mut entries = Vec::new();
    let mut current: Option<HashMap<String, String>> = None;

    for (lineno, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts
            .next()
            .with_context(|| format!("malformed line {}", lineno + 1))?
            .trim();
        let value = parts.next().unwrap_or("").trim();
        if value.is_empty() {
            bail!("line {}: '{}' is missing a value", lineno + 1, key);
        }

        if key.eq_ignore_ascii_case("name") {
            if let Some(fields) = current.take() {
                entries.push(build_entry(fields, home.as_deref())?);
            }
            let mut fields = HashMap::new();
            fields.insert("name".to_string(), value.to_string());
            current = Some(fields);
        } else {
            let fields = current
                .as_mut()
                .with_context(|| format!("line {}: '{}' appears before any 'name' directive", lineno + 1, key))?;
            fields.insert(key.to_ascii_lowercase(), value.to_string());
        }
    }
    if let Some(fields) = current {
        entries.push(build_entry(fields, home.as_deref())?);
    }
    Ok(entries)
}

fn build_entry(fields: HashMap<String, String>, home: Option<&Path>) -> Result<ServerConfig> {
    let name = fields.get("name").context("entry missing 'name'")?.clone();
    let mcp = fields
        .get("mcp")
        .with_context(|| format!("entry '{name}' is missing required 'mcp' directive"))?
        .clone();
    let model = fields
        .get("model")
        .with_context(|| format!("entry '{name}' is missing required 'model' directive"))?
        .clone();
    let model_host = fields
        .get("model_host")
        .cloned()
        .unwrap_or_else(|| DEFAULT_MODEL_HOST.to_string());
    let meta = fields.get("meta").map(|p| resolve_path(p, home));

    let mut mcp_endpoint = EndpointConfig::new(mcp.clone());
    if let Some(headers) = fields.get("mcp_headers") { mcp_endpoint.add_headers_json(headers)?; }
    if let Some(body) = fields.get("mcp_body") { mcp_endpoint.add_body_json(body)?; }
    if let Some(timeout) = fields.get("mcp_timeout") { mcp_endpoint.timeout_seconds = Some(parse_timeout(timeout)?); }
    let model_value = fields.get("model_endpoint").cloned().unwrap_or_else(|| model_host.clone());
    let mut model_endpoint = EndpointConfig::new(model_value);
    if let Some(headers) = fields.get("model_headers") { model_endpoint.add_headers_json(headers)?; }
    if let Some(body) = fields.get("model_body") { model_endpoint.add_body_json(body)?; }
    if let Some(timeout) = fields.get("model_timeout") { model_endpoint.timeout_seconds = Some(parse_timeout(timeout)?); }
    let messages = match fields.get("messages") {
        Some(raw) => serde_json::from_str::<Vec<Message>>(raw).context("invalid messages JSON in config")?,
        None => Vec::new(),
    };

    let mcp_endpoints = match fields.get("mcp_hosts") {
        Some(raw) => parse_mcp_hosts(raw)?,
        None => vec![mcp_endpoint.clone()],
    };

    Ok(ServerConfig {
        name,
        mcp,
        model,
        model_host,
        meta,
        mcp_endpoint,
        mcp_endpoints,
        model_endpoint,
        messages,
    })
}

fn parse_timeout(raw: &str) -> Result<i64> {
    let value: i64 = raw.parse().context("timeout must be an integer")?;
    if value < -1 { bail!("timeout must be -1 or a non-negative number of seconds"); }
    Ok(value)
}

fn parse_mcp_hosts(raw: &str) -> Result<Vec<EndpointConfig>> {
    let values: Vec<serde_json::Value> = serde_json::from_str(raw).context("invalid mcp_hosts JSON in config")?;
    values.into_iter().map(|value| match value {
        serde_json::Value::String(host) => Ok(EndpointConfig::new(host)),
        serde_json::Value::Object(mut object) => {
            let host = object.remove("host").and_then(|value| value.as_str().map(str::to_string))
                .context("each mcp_hosts object needs a string host")?;
            let mut endpoint = EndpointConfig::new(host);
            if let Some(headers) = object.remove("headers") {
                endpoint.add_headers_json(&headers.to_string())?;
            }
            if let Some(body) = object.remove("body") {
                endpoint.add_body_json(&body.to_string())?;
            }
            Ok(endpoint)
        }
        _ => bail!("each mcp_hosts entry must be a host string or object"),
    }).collect()
}

/// Resolves a meta path. Absolute paths and `~/...` are used as-is; anything
/// else (e.g. `.mcp/exampleMcp.json`) is resolved relative to the home directory,
/// matching where `~/.mcp/config` itself lives.
fn resolve_path(raw: &str, home: Option<&Path>) -> PathBuf {
    if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = home {
            return home.join(stripped);
        }
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return path;
    }
    match home {
        Some(home) => home.join(path),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_entries_with_defaults() {
        let input = "\
name a
    mcp https://a.example/mcp
    model modelA
name b
    mcp https://b.example/mcp
    model modelB
    model_host http://localhost:9999
    meta .mcp/b.json
";
        let entries = parse(input).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a");
        assert_eq!(entries[0].model_host, DEFAULT_MODEL_HOST);
        assert!(entries[0].meta.is_none());
        assert_eq!(entries[1].model_host, "http://localhost:9999");
        assert!(entries[1].meta.as_ref().unwrap().ends_with("b.json"));
    }

    #[test]
    fn missing_mcp_directive_errors() {
        let input = "name a\n    model modelA\n";
        assert!(parse(input).is_err());
    }

    #[test]
    fn missing_model_directive_errors() {
        let input = "name a\n    mcp https://a.example/mcp\n";
        assert!(parse(input).is_err());
    }

    #[test]
    fn directive_before_name_errors() {
        let input = "mcp https://a.example/mcp\n";
        assert!(parse(input).is_err());
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let input = "# a comment\n\nname a\n    mcp https://a.example/mcp\n    model m\n";
        let entries = parse(input).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn resolve_path_uses_home_for_relative_paths() {
        let home = Path::new("/home/tester");
        assert_eq!(resolve_path(".mcp/x.json", Some(home)), home.join(".mcp/x.json"));
        assert_eq!(resolve_path("~/foo.json", Some(home)), home.join("foo.json"));
        assert_eq!(resolve_path("/abs/foo.json", Some(home)), PathBuf::from("/abs/foo.json"));
    }

    #[test]
    fn parses_endpoint_timeout_directives() {
        let entries = parse("name test\n    mcp http://mcp.test\n    mcp_timeout -1\n    model model\n    model_timeout 90\n").unwrap();
        assert_eq!(entries[0].mcp_endpoint.timeout_seconds, Some(-1));
        assert_eq!(entries[0].model_endpoint.timeout_seconds, Some(90));
    }
}

