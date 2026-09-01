use anyhow::{anyhow, Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default)]
pub struct EndpointConfig {
    pub value: String,
    pub namespace: Option<String>,
    pub timeout_seconds: Option<i64>,
    pub headers: Map<String, Value>,
    pub body: Value,
}

impl EndpointConfig {
    pub fn new(value: String) -> Self {
        Self { value, namespace: None, timeout_seconds: None, headers: Map::new(), body: Value::Object(Map::new()) }
    }

    pub fn add_headers_json(&mut self, raw: &str) -> Result<()> {
        let headers = parse_object(raw, "-headers")?;
        for (name, value) in headers {
            self.headers.insert(name, value);
        }
        Ok(())
    }

    pub fn add_body_json(&mut self, raw: &str) -> Result<()> {
        self.body = merge_json(self.body.clone(), parse_json(raw, "-body")?);
        Ok(())
    }

    pub fn header_map(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        for (name, value) in &self.headers {
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("header '{name}' must have a string value"))?;
            let name = HeaderName::from_bytes(name.as_bytes()).with_context(|| format!("invalid header name '{name}'"))?;
            let value = HeaderValue::from_str(value).with_context(|| format!("invalid value for header '{name}'"))?;
            headers.insert(name, value);
        }
        Ok(headers)
    }
}

pub fn parse_json(raw: &str, flag: &str) -> Result<Value> {
    serde_json::from_str(raw).with_context(|| format!("{flag} requires valid JSON"))
}

pub fn parse_object(raw: &str, flag: &str) -> Result<Map<String, Value>> {
    match parse_json(raw, flag)? {
        Value::Object(value) => Ok(value),
        _ => Err(anyhow!("{flag} requires a JSON object")),
    }
}

pub fn merge_json(mut base: Value, overlay: Value) -> Value {
    match (&mut base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                let value = match base.remove(&key) {
                    Some(existing) => merge_json(existing, value),
                    None => value,
                };
                base.insert(key, value);
            }
            Value::Object(base.clone())
        }
        (_, overlay) => overlay,
    }
}
