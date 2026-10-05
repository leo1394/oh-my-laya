use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PROTOCOL: u32 = 1;
pub const MAX_MESSAGE: usize = 1024 * 1024;

/// Missing support is a legacy peer, never proof of an available feature.
/// Returns whether an enabled request had to fall back to the V1 path.
pub fn negotiate_orchestration(params: &mut Value, peer: &Value) -> Result<bool> {
    let Some(context)=params.get("orchestration") else {return Ok(false);};
    if context.is_null() {
        if let Some(object)=params.as_object_mut() {object.remove("orchestration");}
        return Ok(false);
    }
    validate_orchestration(context)?;
    let enabled=context.get("enabled")==Some(&Value::Bool(true));
    let supported=peer["supported_contracts"].as_array().is_some_and(|items|items.iter().any(|item|item=="orchestration_plan_v1"));
    if !supported || context.get("enabled")==Some(&Value::Bool(false)) {
        if let Some(object)=params.as_object_mut() {object.remove("orchestration");}
        return Ok(enabled);
    }
    Ok(false)
}

pub fn validate_orchestration(context: &Value) -> Result<()> {
    let fields=["schema_version","enabled","run_id","stage_id","snapshot_revision","independent_work","dependencies_known","required_roles","constraint_refs"];
    let object=context.as_object().ok_or_else(||anyhow::anyhow!("invalid: orchestration requires an object"))?;
    if object.len()!=fields.len() || fields.iter().any(|key|!object.contains_key(*key)) {bail!("invalid: orchestration requires only the versioned context fields");}
    if context["schema_version"].as_u64()!=Some(1) {bail!("invalid: orchestration schema_version");}
    for key in ["enabled","independent_work","dependencies_known"] {
        if !context[key].is_boolean() {bail!("invalid: orchestration boolean field");}
    }
    for key in ["run_id","stage_id","snapshot_revision"] {
        if !context[key].as_str().is_some_and(|s|!s.trim().is_empty() && s.chars().count()<=128) {bail!("invalid: orchestration identifier");}
    }
    for (key,limit) in [("required_roles",5),("constraint_refs",32)] {
        let items=context[key].as_array().ok_or_else(||anyhow::anyhow!("invalid: orchestration list"))?;
        if items.len()>limit {bail!("invalid: orchestration list limit");}
        let mut seen=std::collections::HashSet::new();
        for item in items {
            let text=item.as_str().ok_or_else(||anyhow::anyhow!("invalid: orchestration list item"))?;
            if text.trim().is_empty() || text.chars().count()>256 || !seen.insert(text) {bail!("invalid: orchestration list item");}
            if key=="required_roles" && !["explorer","worker","tester","researcher","reviewer"].contains(&text) {bail!("invalid: orchestration role");}
        }
    }
    Ok(())
}

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
    fn orchestration_negotiates_legacy_service_and_worker_without_silent_success() {
        let context=json!({"schema_version":1,"enabled":true,"run_id":"run","stage_id":"stage","snapshot_revision":"v1","independent_work":false,"dependencies_known":true,"required_roles":[],"constraint_refs":[]});
        let mut params=json!({"state":"task","orchestration":context});
        assert!(negotiate_orchestration(&mut params,&json!({})).unwrap());
        assert!(params.get("orchestration").is_none());
        let mut disabled=json!({"state":"task","orchestration":context});
        disabled["orchestration"]["enabled"]=json!(false);
        assert!(!negotiate_orchestration(&mut disabled,&json!({})).unwrap());
        assert!(disabled.get("orchestration").is_none());
        let mut supported=json!({"orchestration":context});
        assert!(!negotiate_orchestration(&mut supported,&json!({"supported_contracts":["orchestration_plan_v1"]})).unwrap());
        assert_eq!(supported["orchestration"]["enabled"],true);
        let mut null=json!({"orchestration":null});
        assert!(!negotiate_orchestration(&mut null,&json!({})).unwrap());
        assert!(null.get("orchestration").is_none());
        let mut malformed=json!({"orchestration":{"enabled":false}});
        assert!(negotiate_orchestration(&mut malformed,&json!({})).is_err());
    }

    #[test]
    fn redacts_nested_secrets_not_usage() {
        let (value, changed) = redact(&json!({"api_key":"private", "total_tokens":10, "state":"Bearer abcdefg", "nested":[{"password":"x"}]}));
        assert!(changed);
        assert_eq!(value["total_tokens"], 10);
        assert_eq!(value["nested"][0]["password"], "[REDACTED]");
        assert!(!value.to_string().contains("abcdefg"));
    }
}
