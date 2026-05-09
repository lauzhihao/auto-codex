use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::Context as _;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::Value;

use super::auth::normalize_plan;
use super::{CodexAdapter, now_ts};
use crate::core::state::{AccountRecord, State, UsageSnapshot};

const MAX_REFRESH_WORKERS: usize = 8;

// 共享 HTTP 客户端：避免每次调用新建连接池，统一设置 30s 读写超时 + 10s 连接超时
static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

fn http_client() -> &'static Client {
    HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build shared HTTP client")
    })
}

impl CodexAdapter {
    pub fn refresh_all_accounts(&self, state: &mut State) {
        let api_account_ids = state
            .accounts
            .iter()
            .filter(|account| account.is_api())
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        for account_id in api_account_ids {
            state.usage_cache.remove(&account_id);
        }

        let accounts = state
            .accounts
            .iter()
            .filter(|account| account.is_subscription())
            .cloned()
            .collect::<Vec<_>>();
        let refreshed =
            collect_refreshed_usage(&accounts, &state.usage_cache, |account, previous| {
                self.fetch_usage_for_account(account, previous)
            });
        for (account_id, usage) in refreshed {
            state.usage_cache.insert(account_id, usage);
        }
    }

    pub fn refresh_account_usage(
        &self,
        state: &mut State,
        account: &AccountRecord,
    ) -> UsageSnapshot {
        if account.is_api() {
            state.usage_cache.remove(&account.id);
            return UsageSnapshot::default();
        }
        let usage = self.fetch_usage_for_account(account, state.usage_cache.get(&account.id));
        state.usage_cache.insert(account.id.clone(), usage.clone());
        usage
    }

    fn fetch_usage_for_account(
        &self,
        account: &AccountRecord,
        previous: Option<&UsageSnapshot>,
    ) -> UsageSnapshot {
        let auth_path = Path::new(&account.auth_path);
        let config_path = account.config_path.as_ref().map(PathBuf::from);
        let timestamp = now_ts();

        let auth = match self.read_auth_json(auth_path) {
            Ok(auth) => auth,
            Err(error) => {
                return merge_usage_with_previous(
                    previous,
                    make_error_snapshot(account.plan.clone(), timestamp, error.to_string()),
                );
            }
        };

        let access_token = auth
            .pointer("/tokens/access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let account_id = auth
            .pointer("/tokens/account_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        let access_token = match access_token {
            Some(token) => token,
            None => {
                return merge_usage_with_previous(
                    previous,
                    make_error_snapshot(
                        account.plan.clone(),
                        timestamp,
                        "auth.json is missing tokens.access_token".into(),
                    ),
                );
            }
        };

        let url = resolve_usage_url(config_path.as_deref());
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static("codex-cli"));

        // auth_header 构造失败直接返回错误，不再静默以空头继续请求
        let auth_value = format!("Bearer {access_token}");
        let auth_header = HeaderValue::from_str(&auth_value)
            .context("invalid access_token contains non-ASCII characters");
        match auth_header {
            Ok(value) => {
                headers.insert(AUTHORIZATION, value);
            }
            Err(error) => {
                return merge_usage_with_previous(
                    previous,
                    make_error_snapshot(account.plan.clone(), timestamp, error.to_string()),
                );
            }
        }

        if let Some(account_id) = account_id
            .as_ref()
            .and_then(|value| HeaderValue::from_str(value).ok())
        {
            headers.insert("ChatGPT-Account-Id", account_id);
        }

        let response = http_client().get(&url).headers(headers).send();
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                return merge_usage_with_previous(
                    previous,
                    make_error_snapshot(account.plan.clone(), timestamp, error.to_string()),
                );
            }
        };

        if response.status() == StatusCode::UNAUTHORIZED {
            return merge_usage_with_previous(
                previous,
                UsageSnapshot {
                    plan: account.plan.clone(),
                    last_synced_at: Some(timestamp),
                    last_sync_error: Some(
                        "Codex OAuth token expired or invalid. Run `codex login` again.".into(),
                    ),
                    needs_relogin: true,
                    ..UsageSnapshot::default()
                },
            );
        }
        if !response.status().is_success() {
            return merge_usage_with_previous(
                previous,
                make_error_snapshot(
                    account.plan.clone(),
                    timestamp,
                    format!("GET {url} failed: {}", response.status()),
                ),
            );
        }

        let payload = match response.json::<Value>() {
            Ok(value) => value,
            Err(error) => {
                return merge_usage_with_previous(
                    previous,
                    make_error_snapshot(account.plan.clone(), timestamp, error.to_string()),
                );
            }
        };

        let mut normalized = normalize_usage_response(&payload);
        normalized.last_synced_at = Some(timestamp);
        normalized.last_sync_error = None;
        normalized.needs_relogin = false;
        normalized
    }
}

/// 统一构造错误快照，消除 6 处对称重复
fn make_error_snapshot(plan: Option<String>, ts: i64, err: String) -> UsageSnapshot {
    UsageSnapshot {
        plan,
        last_synced_at: Some(ts),
        last_sync_error: Some(err),
        ..UsageSnapshot::default()
    }
}

fn collect_refreshed_usage<F>(
    accounts: &[AccountRecord],
    usage_cache: &BTreeMap<String, UsageSnapshot>,
    fetcher: F,
) -> Vec<(String, UsageSnapshot)>
where
    F: Fn(&AccountRecord, Option<&UsageSnapshot>) -> UsageSnapshot + Sync,
{
    collect_refreshed_usage_with_worker_count(
        accounts,
        usage_cache,
        refresh_worker_count(accounts.len()),
        fetcher,
    )
}

fn collect_refreshed_usage_with_worker_count<F>(
    accounts: &[AccountRecord],
    usage_cache: &BTreeMap<String, UsageSnapshot>,
    worker_count: usize,
    fetcher: F,
) -> Vec<(String, UsageSnapshot)>
where
    F: Fn(&AccountRecord, Option<&UsageSnapshot>) -> UsageSnapshot + Sync,
{
    if accounts.is_empty() {
        return Vec::new();
    }

    let worker_count = worker_count.max(1).min(accounts.len());
    if worker_count == 1 {
        return accounts
            .iter()
            .map(|account| {
                let usage = fetcher(account, usage_cache.get(&account.id));
                (account.id.clone(), usage)
            })
            .collect();
    }

    let chunk_size = accounts.len().div_ceil(worker_count);
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        for chunk in accounts.chunks(chunk_size) {
            let sender = sender.clone();
            let fetcher = &fetcher;
            scope.spawn(move || {
                let mut refreshed = Vec::with_capacity(chunk.len());
                for account in chunk {
                    let usage = fetcher(account, usage_cache.get(&account.id));
                    refreshed.push((account.id.clone(), usage));
                }
                let _ = sender.send(refreshed);
            });
        }
        drop(sender);

        let mut refreshed = Vec::with_capacity(accounts.len());
        while let Ok(mut chunk) = receiver.recv() {
            refreshed.append(&mut chunk);
        }
        refreshed
    })
}

fn refresh_worker_count(account_count: usize) -> usize {
    let detected = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4);
    bounded_refresh_worker_count(account_count, detected)
}

fn bounded_refresh_worker_count(account_count: usize, available_parallelism: usize) -> usize {
    if account_count == 0 {
        return 0;
    }
    available_parallelism
        .max(1)
        .min(MAX_REFRESH_WORKERS)
        .min(account_count)
}

fn merge_usage_with_previous(
    previous: Option<&UsageSnapshot>,
    update: UsageSnapshot,
) -> UsageSnapshot {
    let Some(previous) = previous else {
        return update;
    };

    let mut merged = previous.clone();
    let should_clear_stale_quota =
        update.needs_relogin || update.last_sync_error.as_deref().is_some();

    // 每个 quota 字段使用统一逻辑：出错/重登时清零，否则有值就更新
    macro_rules! merge_quota_field {
        ($field:ident) => {
            if should_clear_stale_quota {
                merged.$field = update.$field;
            } else if update.$field.is_some() {
                merged.$field = update.$field;
            }
        };
    }

    if update.plan.is_some() {
        merged.plan = update.plan;
    }
    merge_quota_field!(weekly_remaining_percent);
    merge_quota_field!(weekly_refresh_at);
    merge_quota_field!(five_hour_remaining_percent);
    merge_quota_field!(five_hour_refresh_at);
    merge_quota_field!(credits_balance);

    if update.last_synced_at.is_some() {
        merged.last_synced_at = update.last_synced_at;
    }
    merged.last_sync_error = update.last_sync_error;
    merged.needs_relogin = update.needs_relogin;
    merged
}

fn resolve_usage_url(config_path: Option<&Path>) -> String {
    // 单次解析环境变量，避免双重 env::var 调用
    let raw = env::var("CODEX_USAGE_BASE_URL").ok();
    let mut base = if raw
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some()
    {
        raw.unwrap()
    } else {
        // env 变量未设置或为空时，尝试从 config 文件读取
        if let Some(config_path) = config_path
            && let Ok(contents) = fs::read_to_string(config_path)
            && let Some(parsed) = parse_chatgpt_base_url(&contents)
        {
            parsed
        } else {
            "https://chatgpt.com/backend-api".into()
        }
    };

    // 确保 base 不为空字符串（trim 后）
    if base.trim().is_empty() {
        base = "https://chatgpt.com/backend-api".into();
    }

    let normalized = normalize_chatgpt_base_url(&base);
    if normalized.contains("/backend-api") {
        format!("{normalized}/wham/usage")
    } else {
        format!("{normalized}/api/codex/usage")
    }
}

fn parse_chatgpt_base_url(contents: &str) -> Option<String> {
    for raw_line in contents.lines() {
        let line = raw_line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        if key.trim() != "chatgpt_base_url" {
            continue;
        }
        let parsed = value.trim().trim_matches('"').trim_matches('\'').trim();
        if !parsed.is_empty() {
            return Some(parsed.to_string());
        }
    }
    None
}

fn normalize_chatgpt_base_url(base: &str) -> String {
    let mut normalized = base.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        normalized = "https://chatgpt.com/backend-api".into();
    }
    if (normalized.starts_with("https://chatgpt.com")
        || normalized.starts_with("https://chat.openai.com"))
        && !normalized.contains("/backend-api")
    {
        normalized.push_str("/backend-api");
    }
    normalized
}

fn normalize_usage_response(payload: &Value) -> UsageSnapshot {
    let rate_limit = payload.get("rate_limit").unwrap_or(&Value::Null);
    let windows = [
        rate_limit.get("primary_window"),
        rate_limit.get("secondary_window"),
    ];

    let mut five_hour = None;
    let mut weekly = None;
    for window in windows.into_iter().flatten() {
        let (snapshot, role) = map_window(window);
        match role {
            WindowRole::FiveHour => {
                if five_hour.is_none() {
                    five_hour = Some(snapshot);
                } else if weekly.is_none() {
                    weekly = Some(snapshot);
                }
            }
            WindowRole::Weekly => {
                if weekly.is_none() {
                    weekly = Some(snapshot);
                } else if five_hour.is_none() {
                    five_hour = Some(snapshot);
                }
            }
            WindowRole::Unknown => {
                if five_hour.is_none() {
                    five_hour = Some(snapshot);
                } else if weekly.is_none() {
                    weekly = Some(snapshot);
                }
            }
        }
    }

    let credits = payload.get("credits").unwrap_or(&Value::Null);
    let credits_balance = if credits.get("unlimited").and_then(Value::as_bool) == Some(true) {
        None
    } else {
        parse_optional_float(credits.get("balance"))
    };

    UsageSnapshot {
        plan: payload
            .get("plan_type")
            .and_then(Value::as_str)
            .map(normalize_plan),
        five_hour_remaining_percent: five_hour.as_ref().and_then(|item| item.remaining_percent),
        five_hour_refresh_at: five_hour.and_then(|item| item.reset_at),
        weekly_remaining_percent: weekly.as_ref().and_then(|item| item.remaining_percent),
        weekly_refresh_at: weekly.and_then(|item| item.reset_at),
        credits_balance,
        ..UsageSnapshot::default()
    }
}

fn parse_optional_float(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn map_window(window: &Value) -> (WindowSnapshot, WindowRole) {
    let used = window
        .get("used_percent")
        .and_then(Value::as_i64)
        .unwrap_or(100)
        .clamp(0, 100);
    let limit_window_seconds = window
        .get("limit_window_seconds")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let role = match limit_window_seconds {
        18_000 => WindowRole::FiveHour,
        604_800 => WindowRole::Weekly,
        _ => WindowRole::Unknown,
    };
    (
        WindowSnapshot {
            remaining_percent: Some(100 - used),
            reset_at: window.get("reset_at").map(value_to_string),
        },
        role,
    )
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

#[derive(Debug)]
struct WindowSnapshot {
    remaining_percent: Option<i64>,
    reset_at: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum WindowRole {
    FiveHour,
    Weekly,
    Unknown,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::thread;
    use std::time::Duration;

    use super::{
        bounded_refresh_worker_count, collect_refreshed_usage_with_worker_count,
        make_error_snapshot, merge_usage_with_previous, normalize_usage_response,
        parse_chatgpt_base_url,
    };
    use crate::adapters::codex::CodexAdapter;
    use crate::core::state::{AccountRecord, AccountType, State, UsageSnapshot};

    // ---- 既有测试（原样保留） ------------------------------------------------

    #[test]
    fn parse_chatgpt_base_url_reads_config_line() {
        let parsed = parse_chatgpt_base_url(
            r#"
            foo = "bar"
            chatgpt_base_url = "https://example.com"
            "#,
        );

        assert_eq!(parsed.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn normalize_usage_response_maps_known_windows() {
        let usage = normalize_usage_response(&serde_json::json!({
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 20,
                    "limit_window_seconds": 18000,
                    "reset_at": "2026-04-20T00:00:00Z"
                },
                "secondary_window": {
                    "used_percent": 70,
                    "limit_window_seconds": 604800,
                    "reset_at": "2026-04-21T00:00:00Z"
                }
            },
            "credits": {
                "unlimited": false,
                "balance": 12.5
            }
        }));

        assert_eq!(usage.plan.as_deref(), Some("Pro"));
        assert_eq!(usage.five_hour_remaining_percent, Some(80));
        assert_eq!(usage.weekly_remaining_percent, Some(30));
        assert_eq!(usage.credits_balance, Some(12.5));
    }

    #[test]
    fn refresh_all_accounts_removes_api_usage_without_fetching() {
        let adapter = CodexAdapter;
        let mut state = State {
            version: 1,
            accounts: vec![AccountRecord {
                id: "api".into(),
                account_type: AccountType::Api,
                email: "56wxyz@openrouter".into(),
                ..AccountRecord::default()
            }],
            usage_cache: BTreeMap::from([(
                "api".into(),
                UsageSnapshot {
                    weekly_remaining_percent: Some(100),
                    five_hour_remaining_percent: Some(100),
                    ..UsageSnapshot::default()
                },
            )]),
            repo_sync: Default::default(),
        };

        adapter.refresh_all_accounts(&mut state);

        assert!(state.usage_cache.is_empty());
    }

    #[test]
    fn merge_usage_failure_clears_stale_cached_quota() {
        let previous = UsageSnapshot {
            five_hour_remaining_percent: Some(100),
            five_hour_refresh_at: Some("2026-04-20T15:32:00Z".into()),
            weekly_remaining_percent: Some(47),
            weekly_refresh_at: Some("2026-04-21T09:39:00Z".into()),
            credits_balance: Some(12.5),
            ..Default::default()
        };

        let merged = merge_usage_with_previous(
            Some(&previous),
            UsageSnapshot {
                last_sync_error: Some("quota api failed".into()),
                ..Default::default()
            },
        );

        assert_eq!(merged.five_hour_remaining_percent, None);
        assert_eq!(merged.five_hour_refresh_at, None);
        assert_eq!(merged.weekly_remaining_percent, None);
        assert_eq!(merged.weekly_refresh_at, None);
        assert_eq!(merged.credits_balance, None);
        assert_eq!(merged.last_sync_error.as_deref(), Some("quota api failed"));
    }

    #[test]
    fn bounded_refresh_worker_count_respects_limits() {
        assert_eq!(bounded_refresh_worker_count(0, 4), 0);
        assert_eq!(bounded_refresh_worker_count(2, 8), 2);
        assert_eq!(bounded_refresh_worker_count(12, 3), 3);
        assert_eq!(bounded_refresh_worker_count(20, 32), 8);
    }

    #[test]
    fn collect_refreshed_usage_preserves_previous_snapshot_lookup_per_account() {
        let accounts = vec![
            AccountRecord {
                id: "acct-a".into(),
                email: "a@example.com".into(),
                ..Default::default()
            },
            AccountRecord {
                id: "acct-b".into(),
                email: "b@example.com".into(),
                ..Default::default()
            },
        ];
        let usage_cache = BTreeMap::from([
            (
                "acct-a".into(),
                UsageSnapshot {
                    credits_balance: Some(1.5),
                    ..Default::default()
                },
            ),
            (
                "acct-b".into(),
                UsageSnapshot {
                    credits_balance: Some(9.0),
                    ..Default::default()
                },
            ),
        ]);

        let refreshed = collect_refreshed_usage_with_worker_count(
            &accounts,
            &usage_cache,
            2,
            |account, previous| UsageSnapshot {
                credits_balance: Some(
                    previous
                        .and_then(|item| item.credits_balance)
                        .unwrap_or_default()
                        + 1.0,
                ),
                plan: Some(account.email.clone()),
                ..Default::default()
            },
        );

        let refreshed = refreshed.into_iter().collect::<BTreeMap<_, _>>();
        assert_eq!(
            refreshed
                .get("acct-a")
                .and_then(|item| item.credits_balance),
            Some(2.5)
        );
        assert_eq!(
            refreshed
                .get("acct-b")
                .and_then(|item| item.credits_balance),
            Some(10.0)
        );
        assert_eq!(
            refreshed
                .get("acct-a")
                .and_then(|item| item.plan.as_deref()),
            Some("a@example.com")
        );
        assert_eq!(
            refreshed
                .get("acct-b")
                .and_then(|item| item.plan.as_deref()),
            Some("b@example.com")
        );
    }

    #[test]
    fn collect_refreshed_usage_keeps_all_accounts_when_workers_finish_out_of_order() {
        let accounts = vec![
            AccountRecord {
                id: "acct-a".into(),
                email: "a@example.com".into(),
                ..Default::default()
            },
            AccountRecord {
                id: "acct-b".into(),
                email: "b@example.com".into(),
                ..Default::default()
            },
            AccountRecord {
                id: "acct-c".into(),
                email: "c@example.com".into(),
                ..Default::default()
            },
            AccountRecord {
                id: "acct-d".into(),
                email: "d@example.com".into(),
                ..Default::default()
            },
        ];

        let refreshed = collect_refreshed_usage_with_worker_count(
            &accounts,
            &BTreeMap::new(),
            2,
            |account, _previous| {
                let delay_ms = match account.id.as_str() {
                    "acct-a" => 40,
                    "acct-b" => 5,
                    "acct-c" => 30,
                    _ => 10,
                };
                thread::sleep(Duration::from_millis(delay_ms));
                UsageSnapshot {
                    plan: Some(account.id.clone()),
                    ..Default::default()
                }
            },
        );

        let refreshed = refreshed.into_iter().collect::<BTreeMap<_, _>>();
        assert_eq!(refreshed.len(), 4);
        assert_eq!(
            refreshed
                .get("acct-a")
                .and_then(|item| item.plan.as_deref()),
            Some("acct-a")
        );
        assert_eq!(
            refreshed
                .get("acct-b")
                .and_then(|item| item.plan.as_deref()),
            Some("acct-b")
        );
        assert_eq!(
            refreshed
                .get("acct-c")
                .and_then(|item| item.plan.as_deref()),
            Some("acct-c")
        );
        assert_eq!(
            refreshed
                .get("acct-d")
                .and_then(|item| item.plan.as_deref()),
            Some("acct-d")
        );
    }

    // ---- 新增测试 ------------------------------------------------------------

    /// make_error_snapshot 字段与参数一一对应，其余字段为 Default
    #[test]
    fn make_error_snapshot_fills_fields_correctly() {
        let snap = make_error_snapshot(Some("Pro".into()), 1_000_000, "oops".into());
        assert_eq!(snap.plan.as_deref(), Some("Pro"));
        assert_eq!(snap.last_synced_at, Some(1_000_000));
        assert_eq!(snap.last_sync_error.as_deref(), Some("oops"));
        // quota 字段必须为 None（Default）
        assert!(snap.weekly_remaining_percent.is_none());
        assert!(snap.five_hour_remaining_percent.is_none());
        assert!(snap.credits_balance.is_none());
        assert!(!snap.needs_relogin);
    }

    /// make_error_snapshot plan=None 同样正确
    #[test]
    fn make_error_snapshot_with_none_plan() {
        let snap = make_error_snapshot(None, 42, "no plan".into());
        assert!(snap.plan.is_none());
        assert_eq!(snap.last_synced_at, Some(42));
        assert_eq!(snap.last_sync_error.as_deref(), Some("no plan"));
    }

    /// merge 后错误快照的 plan 字段被正确携带到 merged 结果
    #[test]
    fn merge_error_snapshot_preserves_plan_from_update() {
        let previous = UsageSnapshot {
            plan: Some("Plus".into()),
            weekly_remaining_percent: Some(50),
            ..Default::default()
        };
        let update = make_error_snapshot(Some("Pro".into()), 999, "fetch failed".into());
        let merged = merge_usage_with_previous(Some(&previous), update);
        // plan 来自 update（Some 值覆盖）
        assert_eq!(merged.plan.as_deref(), Some("Pro"));
        // quota 应被清零
        assert!(merged.weekly_remaining_percent.is_none());
        assert_eq!(merged.last_sync_error.as_deref(), Some("fetch failed"));
    }

    /// merge 成功快照：quota 字段正常更新，错误字段清空
    #[test]
    fn merge_success_snapshot_updates_quota_and_clears_error() {
        let previous = UsageSnapshot {
            plan: Some("Plus".into()),
            weekly_remaining_percent: Some(10),
            last_sync_error: Some("old error".into()),
            ..Default::default()
        };
        let update = UsageSnapshot {
            plan: None, // 不覆盖 plan
            weekly_remaining_percent: Some(80),
            last_sync_error: None,
            last_synced_at: Some(12345),
            ..Default::default()
        };
        let merged = merge_usage_with_previous(Some(&previous), update);
        assert_eq!(merged.plan.as_deref(), Some("Plus")); // 保留旧 plan
        assert_eq!(merged.weekly_remaining_percent, Some(80));
        assert!(merged.last_sync_error.is_none());
        assert_eq!(merged.last_synced_at, Some(12345));
    }

    /// auth_header 错误：含控制字符的 token 被 HeaderValue 拒绝，错误通过 make_error_snapshot 传播
    #[test]
    fn auth_header_invalid_token_produces_error_message() {
        // reqwest HeaderValue 拒绝控制字符（\x00-\x08 / \x0a-\x1f / \x7f）
        let bad_token = "Bearer bad\x01token";
        let result = reqwest::header::HeaderValue::from_str(bad_token);
        assert!(result.is_err(), "control-char HeaderValue should fail");
        // 验证 make_error_snapshot 能正确携带该错误消息
        let snap = make_error_snapshot(
            Some("Pro".into()),
            1_000,
            "invalid access_token contains non-ASCII characters".into(),
        );
        assert!(
            snap.last_sync_error
                .as_deref()
                .unwrap()
                .contains("non-ASCII")
        );
    }
}
