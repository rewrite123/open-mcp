//! Minimal client for Ollama's `/api/chat` endpoint, including tool calling.

use anyhow::{Context, Result};
use crate::endpoint::{merge_json, EndpointConfig};
use crate::settings;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub struct OllamaClient {
    http: reqwest::Client,
    endpoint: EndpointConfig,
    model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into(), tool_calls: None, tool_name: None }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into(), tool_calls: None, tool_name: None }
    }

    /// Builds a message with an arbitrary role (e.g. "system", "user", "assistant").
    pub fn with_role(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self { role: role.into(), content: content.into(), tool_calls: None, tool_name: None }
    }

    pub fn tool_result(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self { role: "tool".into(), content: content.into(), tool_calls: None, tool_name: Some(name.into()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub message: Message,
    #[serde(default)]
    pub done: bool,
}

impl OllamaClient {
    pub fn new(endpoint: EndpointConfig, model: impl Into<String>) -> Result<Self> {
        let timeout = match endpoint.timeout_seconds {
            Some(seconds) => settings::seconds_to_duration(seconds)?,
            None => settings::timeout()?,
        };
        let mut builder = reqwest::Client::builder();
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        Ok(Self {
            http: builder.build().context("failed to build HTTP client")?,
            endpoint,
            model: model.into(),
        })
    }

    /// Sends the conversation (plus any MCP tool schemas) to Ollama and
    /// returns the assistant's reply, which may include tool calls.
    pub async fn chat(&self, messages: &[Message], tools: &[Value]) -> Result<Message> {
        let url = model_url(&self.endpoint.value);
        let mut request = serde_json::json!({
            "messages": messages,
            "stream": false,
        });
        if !self.model.is_empty() {
            request["model"] = Value::from(self.model.clone());
        }
        let mut body = merge_json(self.endpoint.body.clone(), request);
        if !self.model.is_empty() {
            body["model"] = Value::from(self.model.clone());
        }
        body["messages"] = serde_json::to_value(messages)?;
        body["stream"] = Value::Bool(false);
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools.to_vec());
        }

        let response = self
            .http
            .post(&url)
            .headers(self.endpoint.header_map()?)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("request to Ollama at {url} timed out or failed"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Ollama returned HTTP {status}: {text}");
        }

        let parsed: ChatResponse = response
            .json()
            .await
            .context("failed to parse Ollama chat response")?;
        Ok(parsed.message)
    }
}

fn model_url(endpoint: &str) -> String {
    // A bare model name selects this Ollama base; URL-valued -model arguments
    // are explicit provider endpoints and must be used unchanged.
    if endpoint.trim_end_matches('/') == "http://localhost:11434" {
        format!("{}/api/chat", endpoint.trim_end_matches('/'))
    } else {
        endpoint.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ollama_base_uses_chat_path() {
        assert_eq!(model_url("http://localhost:11434"), "http://localhost:11434/api/chat");
        assert_eq!(model_url("http://localhost:11434/"), "http://localhost:11434/api/chat");
    }

    #[test]
    fn explicit_model_url_is_unchanged() {
        assert_eq!(model_url("https://example.test/v1/chat"), "https://example.test/v1/chat");
    }
}

/// Converts an MCP tool definition into the JSON schema Ollama's `/api/chat`
/// `tools` field expects (OpenAI-style function calling).
pub fn mcp_tool_to_ollama(tool: &crate::mcp::Tool) -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description.clone().unwrap_or_default(),
            "parameters": tool.input_schema,
        }
    })
}
