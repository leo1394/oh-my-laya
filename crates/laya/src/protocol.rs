use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PROTOCOL: u32 = 1;
pub const MAX_MESSAGE: usize = 1024 * 1024;

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

pub fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).expect("JSON serialization")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub request_id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(method: &str, params: Value) -> Self {
        Self { protocol_version: PROTOCOL, request_id: uuid::Uuid::new_v4().to_string(), method: method.into(), params }
    }

    pub fn validate(&self) -> Result<()> {
        if self.protocol_version != PROTOCOL { bail!("unsupported protocol version"); }
        if self.request_id.is_empty() || self.request_id.len() > 128 { bail!("invalid request_id"); }
        if serde_json::to_vec(self)?.len() > MAX_MESSAGE { bail!("message too large"); }
        Ok(())
    }
}

/// Redact before any disk write. Conservative patterns are not a secrecy guarantee.
pub fn redact(value: &Value) -> (Value, bool) {
    fn walk(value: &Value, changed: &mut bool) -> Value {
        match value {
            Value::Object(map) => Value::Object(map.iter().map(|(key, value)| {
                let lower = key.to_lowercase();
                let sensitive = ["api_key", "apikey", "password", "secret", "authorization", "access_token", "refresh_token", "private_key"].iter().any(|part| lower.contains(part));
                if sensitive { *changed = true; (key.clone(), Value::String("[REDACTED]".into())) }
                else { (key.clone(), walk(value, changed)) }
            }).collect()),
            Value::Array(items) => Value::Array(items.iter().map(|item| walk(item, changed)).collect()),
            Value::String(text) => {
                static PATTERNS:std::sync::OnceLock<Vec<regex::Regex>>=std::sync::OnceLock::new();
                let patterns=PATTERNS.get_or_init(||[r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]+", r"\b(?:sk-|ghp_|github_pat_)[A-Za-z0-9_-]{12,}", r"(?is)-----BEGIN[^-]*PRIVATE KEY-----.*?-----END[^-]*PRIVATE KEY-----", r"(?i)\b(?:password|api_key|access_token)\s*[:=]\s*[^\s,;]+"].iter().map(|pattern|regex::Regex::new(pattern).expect("static regex")).collect());
                let mut clean = text.clone();
                for pattern in patterns {
                    clean = pattern.replace_all(&clean, "[REDACTED]").into_owned();
                }
                *changed |= clean != *text;
                Value::String(clean)
            }
            other => other.clone(),
        }
    }
    let mut changed = false;
    let result = walk(value, &mut changed);
    (result, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_nested_secrets_not_usage() {
        let (value, changed) = redact(&json!({"api_key":"private", "total_tokens":10, "state":"Bearer abcdefg", "nested":[{"password":"x"}]}));
        assert!(changed);
        assert_eq!(value["total_tokens"], 10);
        assert_eq!(value["nested"][0]["password"], "[REDACTED]");
        assert!(!value.to_string().contains("abcdefg"));
    }
}
