use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

use super::CodexAdapter;
use crate::core::state::LiveIdentity;
use crate::core::storage;

impl CodexAdapter {
    pub(super) fn read_auth_json(&self, path: &Path) -> Result<Value> {
        storage::ensure_exists(path, "auth.json")?;
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let auth: Value = serde_json::from_str(&contents)
            .with_context(|| format!("invalid JSON in {}", path.display()))?;
        Ok(auth)
    }
}

pub(super) fn decode_identity(auth: &Value) -> Result<LiveIdentityWithPlan> {
    let id_token = auth
        .pointer("/tokens/id_token")
        .and_then(Value::as_str)
        .context("auth.json is missing tokens.id_token")?;
    let payload = id_token
        .split('.')
        .nth(1)
        .context("auth.json id_token is not a valid JWT")?;
    let claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(payload)
            .context("failed to decode JWT payload")?,
    )
    .context("failed to parse JWT claims")?;
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .context("auth.json is missing email in id_token")?;
    let plan = claims
        .get("https://api.openai.com/auth")
        .and_then(|value| value.get("chatgpt_plan_type"))
        .and_then(Value::as_str)
        .map(normalize_plan);
    let account_id = auth
        .pointer("/tokens/account_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    Ok(LiveIdentityWithPlan {
        email,
        account_id,
        plan,
    })
}

pub(super) fn normalize_plan(raw: &str) -> String {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return String::new();
    }
    let mut chars = value.chars();
    let head = chars.next().unwrap().to_ascii_uppercase();
    format!("{head}{}", chars.as_str())
}

#[derive(Debug)]
pub(super) struct LiveIdentityWithPlan {
    pub(super) email: String,
    pub(super) account_id: Option<String>,
    pub(super) plan: Option<String>,
}

impl From<LiveIdentityWithPlan> for LiveIdentity {
    fn from(value: LiveIdentityWithPlan) -> Self {
        Self {
            email: value.email,
            account_id: value.account_id,
            scodex_account_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use base64::Engine;

    use super::decode_identity;

    fn fake_jwt(payload: &str) -> String {
        let header = super::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = super::URL_SAFE_NO_PAD.encode(payload);
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn decode_identity_reads_email_plan_and_account_id() -> Result<()> {
        let auth = serde_json::json!({
            "tokens": {
                "id_token": fake_jwt(r#"{"email":"a@example.com","https://api.openai.com/auth":{"chatgpt_plan_type":"plus"}}"#),
                "account_id": "acct-1"
            }
        });

        let identity = decode_identity(&auth)?;

        assert_eq!(identity.email, "a@example.com");
        assert_eq!(identity.account_id.as_deref(), Some("acct-1"));
        assert_eq!(identity.plan.as_deref(), Some("Plus"));
        Ok(())
    }
}
