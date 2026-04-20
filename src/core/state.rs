#![allow(dead_code)]

use serde::{Deserialize, Serialize};

pub const CURRENT_ACCOUNT_MIN_FIVE_HOUR_PERCENT: f64 = 20.0;
pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AccountRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub account_type: AccountType,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub auth_path: String,
    #[serde(default)]
    pub config_path: Option<String>,
    #[serde(default)]
    pub api_provider: Option<String>,
    #[serde(default)]
    pub api_base_url: Option<String>,
    #[serde(default)]
    pub api_token_label: Option<String>,
    #[serde(default)]
    pub added_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountType {
    #[default]
    Subscription,
    Api,
}

impl AccountRecord {
    pub fn is_api(&self) -> bool {
        self.account_type == AccountType::Api
    }

    pub fn is_subscription(&self) -> bool {
        self.account_type == AccountType::Subscription
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UsageSnapshot {
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub weekly_remaining_percent: Option<i64>,
    #[serde(default)]
    pub weekly_refresh_at: Option<String>,
    #[serde(default)]
    pub five_hour_remaining_percent: Option<i64>,
    #[serde(default)]
    pub five_hour_refresh_at: Option<String>,
    #[serde(default)]
    pub credits_balance: Option<f64>,
    #[serde(default)]
    pub last_synced_at: Option<i64>,
    #[serde(default)]
    pub last_sync_error: Option<String>,
    #[serde(default)]
    pub needs_relogin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveIdentity {
    pub email: String,
    pub account_id: Option<String>,
    #[serde(default)]
    pub scodex_account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    #[serde(default = "default_state_version")]
    pub version: u32,
    #[serde(default)]
    pub accounts: Vec<AccountRecord>,
    #[serde(default)]
    pub usage_cache: std::collections::BTreeMap<String, UsageSnapshot>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            accounts: Vec::new(),
            usage_cache: std::collections::BTreeMap::new(),
        }
    }
}

const fn default_state_version() -> u32 {
    STATE_VERSION
}
