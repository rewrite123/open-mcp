//! Named MCP host definitions from `~/.mcp/hosts`.
//!
//! ```text
//! name docmason
//!     host https://docmason.co/mcp
//!     headers {"Authorization":"Bearer env:DOCMASON_API_KEY"}
//!     body {"client":"omcp"}
//! ```

use crate::endpoint::EndpointConfig;
use anyhow::{Context, Result};
use std::fs;

pub fn load(name: &str) -> Result<Option<EndpointConfig>> {
    let home = match dirs::home_dir() { Some(home) => home, None => return Ok(None) };
    let path = home.join(".mcp/hosts");
    if !path.exists() { return Ok(None); }
    let contents = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut current_name: Option<String> = None;
    let mut endpoint: Option<EndpointConfig> = None;

    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("").to_ascii_lowercase();
        let value = parts.next().unwrap_or("").trim();
        match key.as_str() {
            "name" => {
                if current_name.as_deref() == Some(name) { return Ok(endpoint); }
                current_name = Some(value.to_string());
                endpoint = None;
            }
            "host" | "mcp" if current_name.as_deref() == Some(name) => endpoint = Some(EndpointConfig::new(value.to_string())),
            "headers" if current_name.as_deref() == Some(name) => {
                if let Some(endpoint) = endpoint.as_mut() { endpoint.add_headers_json(value)?; }
            }
            "body" if current_name.as_deref() == Some(name) => {
                if let Some(endpoint) = endpoint.as_mut() { endpoint.add_body_json(value)?; }
            }
            _ => {}
        }
    }
    Ok(if current_name.as_deref() == Some(name) { endpoint } else { None })
}
