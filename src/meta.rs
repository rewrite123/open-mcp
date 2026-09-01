//! Parsing for the per-server meta JSON file referenced by a config entry's
//! `meta` directive (e.g. `~/.mcp/exampleMcp.json`).
//!
//! ```json
//! {
//!   "protocol_version": "2025-06-18",
//!   "auth": { "type": "bearer", "token": "env:DOCMASON_API_KEY" },
//!   "system_prompt": "You are a helpful assistant with access to tools.",
//!   "headers": { "X-Custom": "value" },
//!   "allowed_tools": ["template.list", "template.get"],
//!   "model_params": { "num_ctx": 32768, "temperature": 0.2 }
//! }
//! ```

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use serde::Deserialize;
use serde_json::{Map, Value as JsonValue};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Meta {
    /// MCP protocol version the client should request during initialize.
    pub protocol_version: Option<String>,
    pub auth: Option<AuthConfig>,
    /// Extra static headers to send with every MCP request.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    pub system_prompt: Option<String>,
    /// If set, only these tool names are exposed to the model; other tools
    /// remain discoverable via `tools/list` but won't be offered for calling.
    pub allowed_tools: Option<Vec<String>>,
    /// Arbitrary, provider-specific model parameters (e.g. Ollama's `num_ctx`,
    /// `temperature`) forwarded to the model backend as-is. omcp does not
    /// interpret these itself, so this works regardless of which model
    /// provider is configured.
    pub model_params: Option<Map<String, JsonValue>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AuthConfig {
    /// `Authorization: Bearer <token>`
    Bearer { token: String },
    /// `Authorization: Basic base64(username:password)`
    Basic { username: String, password: String },
    /// An arbitrary header, e.g. `X-Api-Key: <value>`.
    Header { name: String, value: String },
    None,
}

impl Meta {
    pub fn load(path: &Path) -> Result<Meta> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read meta file at {}", path.display()))?;
        let meta: Meta = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse meta file at {}", path.display()))?;
        Ok(meta)
    }

    /// Builds the header map to attach to outgoing MCP requests, resolving any
    /// `env:VAR_NAME` indirections so secrets never need to live in plaintext.
    pub fn build_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();

        match &self.auth {
            Some(AuthConfig::Bearer { token }) => {
                let token = resolve_secret(token)?;
                let value = HeaderValue::from_str(&format!("Bearer {token}"))
                    .context("invalid characters in bearer token")?;
                headers.insert(AUTHORIZATION, value);
            }
            Some(AuthConfig::Basic { username, password }) => {
                let password = resolve_secret(password)?;
                let encoded = base64_encode(&format!("{username}:{password}"));
                let value = HeaderValue::from_str(&format!("Basic {encoded}"))
                    .context("invalid characters in basic auth credentials")?;
                headers.insert(AUTHORIZATION, value);
            }
            Some(AuthConfig::Header { name, value }) => {
                let value = resolve_secret(value)?;
                let header_name = HeaderName::from_bytes(name.as_bytes())
                    .with_context(|| format!("invalid header name '{name}'"))?;
                let header_value =
                    HeaderValue::from_str(&value).context("invalid characters in header value")?;
                headers.insert(header_name, header_value);
            }
            Some(AuthConfig::None) | None => {}
        }

        for (name, value) in &self.headers {
            let value = resolve_secret(value)?;
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("invalid header name '{name}'"))?;
            let header_value =
                HeaderValue::from_str(&value).context("invalid characters in header value")?;
            headers.insert(header_name, header_value);
        }

        Ok(headers)
    }
}

/// Resolves `env:VAR_NAME` to the value of the environment variable `VAR_NAME`,
/// otherwise returns the input unchanged.
fn resolve_secret(value: &str) -> Result<String> {
    match value.strip_prefix("env:") {
        Some(var) => std::env::var(var)
            .with_context(|| format!("environment variable '{var}' referenced by meta file is not set")),
        None => Ok(value.to_string()),
    }
}

fn base64_encode(input: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18 & 0x3F) as usize] as char);
        out.push(ALPHABET[(triple >> 12 & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6 & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bearer_auth_and_allowed_tools() {
        let raw = r#"{
            "auth": {"type": "bearer", "token": "abc"},
            "allowed_tools": ["a", "b"],
            "system_prompt": "hi",
            "model_params": {"num_ctx": 32768, "temperature": 0.2}
        }"#;
        let meta: Meta = serde_json::from_str(raw).unwrap();
        assert_eq!(meta.allowed_tools, Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(meta.system_prompt.as_deref(), Some("hi"));
        let params = meta.model_params.as_ref().unwrap();
        assert_eq!(params["num_ctx"], 32768);
        assert_eq!(params["temperature"], 0.2);
        let headers = meta.build_headers().unwrap();
        assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Bearer abc");
    }

    #[test]
    fn model_params_defaults_to_none_when_absent() {
        let meta: Meta = serde_json::from_str("{}").unwrap();
        assert_eq!(meta.model_params, None);
    }

    #[test]
    fn env_indirection_resolves_from_environment() {
        std::env::set_var("OMCP_META_TEST_TOKEN", "resolved-value");
        let raw = r#"{"auth": {"type": "bearer", "token": "env:OMCP_META_TEST_TOKEN"}}"#;
        let meta: Meta = serde_json::from_str(raw).unwrap();
        let headers = meta.build_headers().unwrap();
        assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Bearer resolved-value");
        std::env::remove_var("OMCP_META_TEST_TOKEN");
    }

    #[test]
    fn missing_env_var_errors() {
        let raw = r#"{"auth": {"type": "bearer", "token": "env:OMCP_META_TEST_DOES_NOT_EXIST"}}"#;
        let meta: Meta = serde_json::from_str(raw).unwrap();
        assert!(meta.build_headers().is_err());
    }

    #[test]
    fn basic_auth_encodes_base64() {
        let raw = r#"{"auth": {"type": "basic", "username": "user", "password": "pass"}}"#;
        let meta: Meta = serde_json::from_str(raw).unwrap();
        let headers = meta.build_headers().unwrap();
        assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Basic dXNlcjpwYXNz");
    }

    #[test]
    fn custom_header_auth() {
        let raw = r#"{"auth": {"type": "header", "name": "X-Api-Key", "value": "k123"}}"#;
        let meta: Meta = serde_json::from_str(raw).unwrap();
        let headers = meta.build_headers().unwrap();
        assert_eq!(headers.get("X-Api-Key").unwrap(), "k123");
    }

    #[test]
    fn no_auth_produces_no_authorization_header() {
        let meta = Meta::default();
        let headers = meta.build_headers().unwrap();
        assert!(headers.get(AUTHORIZATION).is_none());
    }
}

