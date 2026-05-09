use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use uuid::Uuid;

use super::ApiLoginRequest;
use super::CodexAdapter;
use super::auth::decode_identity;
use super::now_ts;
use super::paths::codex_home;
use crate::core::state::{AccountRecord, AccountType, State};
use crate::core::storage;

const SCODEX_API_CONFIG_MARKER: &str = "# scodex-managed-api-config";
const SCODEX_ACCOUNT_ID_PREFIX: &str = "# scodex-account-id: ";

impl CodexAdapter {
    pub fn normalize_account_records(&self, state: &mut State) -> bool {
        let mut changed = false;
        for account in &mut state.accounts {
            changed |= normalize_account_record(account);
        }
        if changed {
            state.usage_cache.retain(|account_id, _| {
                state
                    .accounts
                    .iter()
                    .find(|account| account.id == *account_id)
                    .is_none_or(|account| account.is_subscription())
            });
        }
        changed
    }

    pub fn import_auth_path(
        &self,
        state_dir: &Path,
        state: &mut State,
        raw_path: &Path,
    ) -> Result<AccountRecord> {
        self.import_auth_path_with_id(state_dir, state, raw_path, None)
    }

    pub(super) fn import_auth_path_with_id(
        &self,
        state_dir: &Path,
        state: &mut State,
        raw_path: &Path,
        preferred_id: Option<&str>,
    ) -> Result<AccountRecord> {
        let input_path = if raw_path.is_dir() {
            raw_path.join("auth.json")
        } else {
            raw_path.to_path_buf()
        };
        storage::ensure_exists(&input_path, "auth.json")?;
        let auth = self.read_auth_json(&input_path)?;
        let identity = decode_identity(&auth)?;

        let config_path = input_path.parent().map(|item| item.join("config.toml"));
        let existing =
            find_matching_account(state, &identity.email, identity.account_id.as_deref());
        let account_id = existing
            .map(|item| item.id.clone())
            .or_else(|| {
                preferred_id
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let (account_home, stored_auth_path, _stored_config_base) =
            account_home_paths(state_dir, &account_id);
        fs::create_dir_all(&account_home)
            .with_context(|| format!("failed to create {}", account_home.display()))?;

        atomic_copy(&input_path, &stored_auth_path)?;
        let stored_config_path = if let Some(config_path) = config_path.filter(|path| path.exists())
        {
            let target = account_home.join("config.toml");
            atomic_copy(&config_path, &target)?;
            Some(target)
        } else {
            None
        };

        let timestamp = now_ts();
        let record = AccountRecord {
            id: account_id,
            account_type: AccountType::Subscription,
            email: identity.email,
            account_id: identity.account_id,
            plan: identity.plan,
            auth_path: stored_auth_path.to_string_lossy().into_owned(),
            config_path: stored_config_path.map(|item| item.to_string_lossy().into_owned()),
            api_provider: None,
            api_base_url: None,
            api_token_label: None,
            added_at: existing.map(|item| item.added_at).unwrap_or(timestamp),
            updated_at: timestamp,
        };

        replace_account(state, record.clone());
        Ok(record)
    }

    pub fn import_api_auth_path(
        &self,
        state_dir: &Path,
        state: &mut State,
        raw_home: &Path,
        request: &ApiLoginRequest,
    ) -> Result<AccountRecord> {
        let input_auth = raw_home.join("auth.json");
        storage::ensure_exists(&input_auth, "auth.json")?;

        let email = api_account_email(&request.api_token, &request.provider);
        let existing = state
            .accounts
            .iter()
            .find(|account| account.email.eq_ignore_ascii_case(&email));
        let account_id = existing
            .map(|item| item.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let (account_home, stored_auth_path, stored_config_path) =
            account_home_paths(state_dir, &account_id);
        fs::create_dir_all(&account_home)
            .with_context(|| format!("failed to create {}", account_home.display()))?;

        atomic_copy(&input_auth, &stored_auth_path)?;

        // 原子写 config.toml：先写 tmp，再 rename，避免写入中途崩溃污染主路径
        let tmp = stored_config_path.with_extension("toml.tmp");
        let content = build_api_config(&account_id, request);
        fs::write(&tmp, content.as_bytes())
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        fs::rename(&tmp, &stored_config_path).with_context(|| {
            format!("failed to move {} into place", stored_config_path.display())
        })?;

        let timestamp = now_ts();
        let record = AccountRecord {
            id: account_id,
            account_type: AccountType::Api,
            email,
            account_id: None,
            plan: None,
            auth_path: stored_auth_path.to_string_lossy().into_owned(),
            config_path: Some(stored_config_path.to_string_lossy().into_owned()),
            api_provider: Some(request.provider.clone()),
            api_base_url: Some(request.base_url.clone()),
            api_token_label: Some(api_token_label(&request.api_token)),
            added_at: existing.map(|item| item.added_at).unwrap_or(timestamp),
            updated_at: timestamp,
        };

        replace_account(state, record.clone());
        Ok(record)
    }

    pub fn import_known_sources(&self, state_dir: &Path, state: &mut State) -> Vec<AccountRecord> {
        let mut imported = Vec::new();
        let mut seen = std::collections::BTreeSet::new();

        let mut maybe_import = |path: PathBuf| {
            let key = path.to_string_lossy().into_owned();
            if seen.contains(&key) || !path.exists() {
                return;
            }
            seen.insert(key);
            if let Ok(record) = self.import_auth_path(state_dir, state, &path) {
                imported.push(record);
            }
        };

        maybe_import(codex_home().join("auth.json"));

        if !env_flag_enabled("AUTO_CODEX_IMPORT_ACCOUNTS_HUB") {
            return dedupe_imported(imported);
        }

        if let Some(home) = env::var_os("HOME") {
            let home = PathBuf::from(home);
            let candidate_roots = [
                home.join("Library")
                    .join("Application Support")
                    .join("com.murong.ai-accounts-hub")
                    .join("codex")
                    .join("managed-codex-homes"),
                home.join(".local")
                    .join("share")
                    .join("com.murong.ai-accounts-hub")
                    .join("codex")
                    .join("managed-codex-homes"),
            ];
            for root in candidate_roots {
                if !root.exists() {
                    continue;
                }
                let entries = match fs::read_dir(&root) {
                    Ok(entries) => entries,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    maybe_import(entry.path().join("auth.json"));
                }
            }
        }

        dedupe_imported(imported)
    }

    pub fn find_account_by_email<'a>(
        &self,
        state: &'a State,
        email: &str,
    ) -> Option<&'a AccountRecord> {
        let target = email.trim().to_ascii_lowercase();
        state
            .accounts
            .iter()
            .find(|account| account.email.eq_ignore_ascii_case(&target))
    }

    pub fn switch_account(&self, account: &AccountRecord) -> Result<()> {
        let src = Path::new(&account.auth_path);
        storage::ensure_exists(src, "stored auth.json")?;
        let home = codex_home();
        let dst = home.join("auth.json");
        atomic_copy(src, &dst)?;
        switch_config(&home, account)?;
        Ok(())
    }

    pub fn remove_account(&self, state_dir: &Path, state: &mut State, id: &str) -> Result<()> {
        state.accounts.retain(|account| account.id != id);
        state.usage_cache.remove(id);
        let account_home = state_dir.join("accounts").join(id);
        if account_home.exists() {
            fs::remove_dir_all(&account_home)
                .with_context(|| format!("failed to remove {}", account_home.display()))?;
        }
        Ok(())
    }
}

/// 构造账号目录下三条常用路径，减少 import_*_with_id 中的重复 path 拼接
fn account_home_paths(
    state_dir: &Path,
    id: &str,
) -> (
    PathBuf, /* home */
    PathBuf, /* auth.json */
    PathBuf, /* config.toml */
) {
    let home = state_dir.join("accounts").join(id);
    let auth = home.join("auth.json");
    let config = home.join("config.toml");
    (home, auth, config)
}

/// 取 token 去掉 "sk-" 前缀后末尾 n 个字符；token 不足 n 时返回全部 body
fn token_tail(token: &str, n: usize) -> String {
    let trimmed = token.trim();
    let body = trimmed.strip_prefix("sk-").unwrap_or(trimmed);
    let tail: String = body
        .chars()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if tail.is_empty() {
        body.to_string()
    } else {
        tail
    }
}

pub(super) fn api_account_email(api_token: &str, provider: &str) -> String {
    format!(
        "{}@{}",
        api_token_suffix(api_token),
        provider.trim().to_ascii_lowercase()
    )
}

pub(super) fn api_token_label(api_token: &str) -> String {
    let trimmed = api_token.trim();
    let body = trimmed.strip_prefix("sk-").unwrap_or(trimmed);
    let head: String = body.chars().take(4).collect();
    let tail = token_tail(api_token, 4);
    format!("sk-{head}-{tail}")
}

fn api_token_suffix(api_token: &str) -> String {
    token_tail(api_token, 6)
}

// ---------------------------------------------------------------------------
// build_api_config：用 toml crate 序列化，替代手拼字符串
// ---------------------------------------------------------------------------

/// openai 分支的顶层配置（model_provider = "openai"）
#[derive(Serialize)]
struct OpenaiApiConfig {
    model_provider: String,
    openai_base_url: String,
    forced_login_method: String,
}

/// openrouter / 其他 provider 分支的 provider entry
#[derive(Serialize)]
struct ProviderEntry {
    name: String,
    base_url: String,
    requires_openai_auth: bool,
    wire_api: String,
}

/// openrouter / 其他 provider 分支的顶层配置
#[derive(Serialize)]
struct GenericApiConfig {
    model_provider: String,
    forced_login_method: String,
    model_providers: HashMap<String, ProviderEntry>,
}

pub(super) fn build_api_config(account_id: &str, request: &ApiLoginRequest) -> String {
    let provider = request.provider.trim();
    let base_url = request.base_url.trim();

    // 注释行必须手写（toml crate 序列化不输出注释）
    let header = format!(
        "{}\n{}{}\n",
        SCODEX_API_CONFIG_MARKER, SCODEX_ACCOUNT_ID_PREFIX, account_id
    );

    let body = if provider.eq_ignore_ascii_case("openai") {
        let cfg = OpenaiApiConfig {
            model_provider: "openai".into(),
            openai_base_url: base_url.into(),
            forced_login_method: "api".into(),
        };
        toml::to_string_pretty(&cfg).expect("OpenaiApiConfig serialization failed")
    } else {
        let mut providers = HashMap::new();
        providers.insert(
            provider.to_ascii_lowercase(),
            ProviderEntry {
                name: provider.into(),
                base_url: base_url.into(),
                requires_openai_auth: true,
                wire_api: "responses".into(),
            },
        );
        let cfg = GenericApiConfig {
            model_provider: provider.into(),
            forced_login_method: "api".into(),
            model_providers: providers,
        };
        toml::to_string_pretty(&cfg).expect("GenericApiConfig serialization failed")
    };

    format!("{header}{body}")
}

pub(super) fn read_managed_config_account_id(codex_home: &Path) -> Option<String> {
    let config_path = codex_home.join("config.toml");
    let contents = fs::read_to_string(config_path).ok()?;
    if !contents.contains(SCODEX_API_CONFIG_MARKER) {
        return None;
    }
    contents.lines().find_map(|line| {
        line.strip_prefix(SCODEX_ACCOUNT_ID_PREFIX)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn switch_config(codex_home: &Path, account: &AccountRecord) -> Result<()> {
    if account.is_api() {
        let Some(config_path) = account.config_path.as_ref() else {
            return Ok(());
        };
        let src = Path::new(config_path);
        storage::ensure_exists(src, "stored config.toml")?;
        backup_user_config_if_needed(codex_home)?;
        return atomic_copy(src, &codex_home.join("config.toml"));
    }

    if let Some(config_path) = account.config_path.as_ref() {
        let src = Path::new(config_path);
        if src.exists() {
            backup_user_config_if_needed(codex_home)?;
            return atomic_copy(src, &codex_home.join("config.toml"));
        }
    }

    restore_user_config_if_managed(codex_home)
}

fn backup_user_config_if_needed(codex_home: &Path) -> Result<()> {
    let config_path = codex_home.join("config.toml");
    if !config_path.exists() || is_scodex_managed_config(&config_path) {
        return Ok(());
    }
    let backup_path = codex_home.join("config.toml.scodex-backup");
    if !backup_path.exists() {
        atomic_copy(&config_path, &backup_path)?;
    }
    Ok(())
}

fn restore_user_config_if_managed(codex_home: &Path) -> Result<()> {
    let config_path = codex_home.join("config.toml");
    if !config_path.exists() || !is_scodex_managed_config(&config_path) {
        return Ok(());
    }
    let backup_path = codex_home.join("config.toml.scodex-backup");
    if backup_path.exists() {
        atomic_copy(&backup_path, &config_path)
    } else {
        fs::remove_file(&config_path)
            .with_context(|| format!("failed to remove {}", config_path.display()))
    }
}

fn is_scodex_managed_config(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|contents| contents.contains(SCODEX_API_CONFIG_MARKER))
        .unwrap_or(false)
}

fn atomic_copy(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let tmp = dst.parent().unwrap_or_else(|| Path::new(".")).join(format!(
        ".{}.tmp",
        dst.file_name()
            .and_then(|item| item.to_str())
            .unwrap_or("copy")
    ));
    fs::copy(src, &tmp)
        .with_context(|| format!("failed to copy {} to {}", src.display(), tmp.display()))?;
    fs::rename(&tmp, dst)
        .with_context(|| format!("failed to move {} into place", dst.display()))?;
    Ok(())
}

fn find_matching_account<'a>(
    state: &'a State,
    email: &str,
    account_id: Option<&str>,
) -> Option<&'a AccountRecord> {
    state.accounts.iter().find(|account| {
        account.email.eq_ignore_ascii_case(email)
            || account_id.is_some_and(|candidate| account.account_id.as_deref() == Some(candidate))
    })
}

fn replace_account(state: &mut State, updated: AccountRecord) {
    if let Some(slot) = state
        .accounts
        .iter_mut()
        .find(|account| account.id == updated.id)
    {
        *slot = updated;
    } else {
        state.accounts.push(updated);
    }
}

fn dedupe_imported(accounts: Vec<AccountRecord>) -> Vec<AccountRecord> {
    let mut result = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for account in accounts {
        if seen.insert(account.id.clone()) {
            result.push(account);
        }
    }
    result
}

fn env_flag_enabled(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn normalize_account_record(account: &mut AccountRecord) -> bool {
    let Some(api_details) = infer_api_account_details(account) else {
        return false;
    };

    let mut changed = false;
    if account.account_type != AccountType::Api {
        account.account_type = AccountType::Api;
        changed = true;
    }
    if account.account_id.take().is_some() {
        changed = true;
    }
    if account.plan.take().is_some() {
        changed = true;
    }
    if account.email != api_details.email {
        account.email = api_details.email;
        changed = true;
    }
    if account.api_provider.as_deref() != Some(api_details.provider.as_str()) {
        account.api_provider = Some(api_details.provider);
        changed = true;
    }
    if account.api_base_url.as_deref() != Some(api_details.base_url.as_str()) {
        account.api_base_url = Some(api_details.base_url);
        changed = true;
    }
    if account.api_token_label.as_deref() != Some(api_details.token_label.as_str()) {
        account.api_token_label = Some(api_details.token_label);
        changed = true;
    }
    changed
}

fn infer_api_account_details(account: &AccountRecord) -> Option<InferredApiAccount> {
    let config_path = account.config_path.as_deref().map(Path::new)?;
    if !config_path.exists() || !is_scodex_managed_config(config_path) {
        return None;
    }

    let auth_path = Path::new(&account.auth_path);
    let auth = fs::read_to_string(auth_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())?;
    let api_token = auth
        .get("OPENAI_API_KEY")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let config = fs::read_to_string(config_path).ok()?;
    let provider = parse_config_string(&config, "model_provider")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "openai".into());
    let base_url = if provider.eq_ignore_ascii_case("openai") {
        parse_config_string(&config, "openai_base_url")
    } else {
        parse_config_string(&config, "base_url")
    }
    .filter(|value| !value.is_empty())?;
    let provider = provider.to_ascii_lowercase();
    let token_label = api_token_label(api_token);

    Some(InferredApiAccount {
        email: api_account_email(api_token, &provider),
        provider,
        base_url,
        token_label,
    })
}

fn parse_config_string(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let trimmed = line.trim();
        let prefix = format!("{key} = ");
        let raw = trimmed.strip_prefix(&prefix)?.trim();
        parse_toml_basic_string(raw)
    })
}

fn parse_toml_basic_string(raw: &str) -> Option<String> {
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    let mut output = String::new();
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        let escaped = chars.next()?;
        match escaped {
            '\\' => output.push('\\'),
            '"' => output.push('"'),
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            _ => return None,
        }
    }
    Some(output)
}

#[derive(Debug)]
struct InferredApiAccount {
    email: String,
    provider: String,
    base_url: String,
    token_label: String,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use anyhow::Result;
    use base64::Engine;
    use uuid::Uuid;

    use super::{account_home_paths, api_account_email, api_token_label, build_api_config};
    use crate::adapters::codex::ApiLoginRequest;
    use crate::adapters::codex::CodexAdapter;
    use crate::core::state::{AccountRecord, AccountType, State};

    fn fake_jwt(payload: &str) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        format!("{header}.{payload}.sig")
    }

    fn openrouter_request() -> ApiLoginRequest {
        ApiLoginRequest {
            api_token: "sk-abcdef123456wxyz".into(),
            base_url: "https://example.com/v1".into(),
            provider: "openrouter".into(),
        }
    }

    fn openai_request() -> ApiLoginRequest {
        ApiLoginRequest {
            api_token: "sk-openai1234abcd".into(),
            base_url: "https://api.openai.com/v1".into(),
            provider: "openai".into(),
        }
    }

    // ------------------------------------------------------------------
    // token_tail
    // ------------------------------------------------------------------

    #[test]
    fn token_tail_with_sk_prefix() {
        // "sk-abcdef" -> body = "abcdef", tail(4) = "cdef"
        assert_eq!(super::token_tail("sk-abcdef", 4), "cdef");
    }

    #[test]
    fn token_tail_without_prefix() {
        // body = "abcdef", tail(4) = "cdef"
        assert_eq!(super::token_tail("abcdef", 4), "cdef");
    }

    #[test]
    fn token_tail_shorter_than_n_returns_full_body() {
        // body = "ab" (2 chars), n = 6 -> tail would be "ab" (not empty)
        assert_eq!(super::token_tail("sk-ab", 6), "ab");
    }

    #[test]
    fn token_tail_empty_body_returns_empty() {
        // body = "" (empty after stripping prefix), tail is empty -> returns ""
        assert_eq!(super::token_tail("sk-", 4), "");
    }

    // ------------------------------------------------------------------
    // api_token_label
    // ------------------------------------------------------------------

    #[test]
    fn api_token_label_formats_head_and_tail() {
        // body = "abcdef123456wxyz", head(4) = "abcd", tail(4) = "wxyz"
        assert_eq!(api_token_label("sk-abcdef123456wxyz"), "sk-abcd-wxyz");
    }

    // ------------------------------------------------------------------
    // build_api_config — openai branch
    // ------------------------------------------------------------------

    #[test]
    fn build_api_config_openai_contains_expected_keys() {
        let req = openai_request();
        let config = build_api_config("acct-openai", &req);

        assert!(
            config.contains("# scodex-managed-api-config"),
            "missing marker"
        );
        assert!(
            config.contains("# scodex-account-id: acct-openai"),
            "missing account id"
        );
        assert!(
            config.contains("model_provider = \"openai\""),
            "missing model_provider"
        );
        assert!(
            config.contains("openai_base_url = \"https://api.openai.com/v1\""),
            "missing openai_base_url"
        );
        assert!(
            config.contains("forced_login_method = \"api\""),
            "missing forced_login_method"
        );
        // openai branch must NOT contain a [model_providers] section
        assert!(
            !config.contains("[model_providers"),
            "openai branch must not have model_providers table"
        );
    }

    // ------------------------------------------------------------------
    // build_api_config — openrouter branch
    // ------------------------------------------------------------------

    #[test]
    fn build_api_config_openrouter_contains_expected_keys() {
        let req = openrouter_request();
        let config = build_api_config("acct-api", &req);

        assert!(config.contains("# scodex-managed-api-config"));
        assert!(config.contains("# scodex-account-id: acct-api"));
        assert!(config.contains("model_provider = \"openrouter\""));
        // toml crate serializes bare-key table header (no quotes around openrouter)
        assert!(
            config.contains("[model_providers.openrouter]"),
            "missing model_providers section; got:\n{config}"
        );
        assert!(config.contains("base_url = \"https://example.com/v1\""));
        assert!(config.contains("requires_openai_auth = true"));
        assert!(config.contains("wire_api = \"responses\""));
    }

    // ------------------------------------------------------------------
    // build_api_config — provider case-insensitive (openai uppercase)
    // ------------------------------------------------------------------

    #[test]
    fn build_api_config_openai_case_insensitive() {
        let req = ApiLoginRequest {
            api_token: "sk-test".into(),
            base_url: "https://api.openai.com/v1".into(),
            provider: "OpenAI".into(),
        };
        let config = build_api_config("acct-x", &req);
        assert!(config.contains("model_provider = \"openai\""));
        assert!(!config.contains("[model_providers"));
    }

    // ------------------------------------------------------------------
    // atomic write: tmp file must not remain after success
    // ------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn atomic_write_no_tmp_pollution_on_success() -> Result<()> {
        let tmp_dir = std::env::temp_dir().join(format!("scodex-atomic-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir)?;

        let state_dir = tmp_dir.join("state");
        let mut state = State::default();

        // 写一个最小 auth.json
        let raw_home = tmp_dir.join("raw");
        fs::create_dir_all(&raw_home)?;
        fs::write(
            raw_home.join("auth.json"),
            serde_json::json!({ "OPENAI_API_KEY": "sk-abcdef123456wxyz" }).to_string(),
        )?;

        let req = openrouter_request();
        let record = CodexAdapter.import_api_auth_path(&state_dir, &mut state, &raw_home, &req)?;

        let config_path = Path::new(record.config_path.as_deref().unwrap());
        // 主路径必须存在
        assert!(config_path.exists(), "config.toml should exist");
        // tmp 文件不应残留
        let tmp_path = config_path.with_extension("toml.tmp");
        assert!(
            !tmp_path.exists(),
            "tmp file must not remain after atomic write"
        );

        fs::remove_dir_all(&tmp_dir)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // account_home_paths helper
    // ------------------------------------------------------------------

    #[test]
    fn account_home_paths_returns_correct_structure() {
        let state_dir = Path::new("/tmp/scodex-state");
        let id = "test-account-id";
        let (home, auth, config) = account_home_paths(state_dir, id);
        assert_eq!(home, state_dir.join("accounts").join(id));
        assert_eq!(auth, home.join("auth.json"));
        assert_eq!(config, home.join("config.toml"));
    }

    // ------------------------------------------------------------------
    // pre-existing tests (unchanged)
    // ------------------------------------------------------------------

    #[test]
    fn api_account_email_uses_short_secret_locator() {
        assert_eq!(
            api_account_email("sk-abcdef123456wxyz", "OpenRouter"),
            "56wxyz@openrouter"
        );
        assert_eq!(
            api_account_email("abcdef123456wxyz", "custom"),
            "56wxyz@custom"
        );
    }

    #[test]
    fn api_config_marks_scodex_managed_provider() {
        let config = build_api_config(
            "acct-api",
            &ApiLoginRequest {
                api_token: "sk-abcdef123456wxyz".into(),
                base_url: "https://example.com/v1".into(),
                provider: "openrouter".into(),
            },
        );

        assert!(config.contains("# scodex-managed-api-config"));
        assert!(config.contains("# scodex-account-id: acct-api"));
        assert!(config.contains("model_provider = \"openrouter\""));
        // toml crate uses bare key (no quotes) for simple alphanumeric keys
        assert!(
            config.contains("[model_providers.openrouter]"),
            "expected bare-key section header; got:\n{config}"
        );
        assert!(config.contains("base_url = \"https://example.com/v1\""));
    }

    #[test]
    fn import_auth_path_copies_auth_into_state_storage() -> Result<()> {
        let tmp = std::env::temp_dir().join(format!("scodex-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp)?;
        let raw_home = tmp.join("raw");
        fs::create_dir_all(&raw_home)?;
        fs::write(
            raw_home.join("auth.json"),
            serde_json::json!({
                "tokens": {
                    "id_token": fake_jwt(r#"{"email":"a@example.com"}"#),
                    "account_id": "acct-1"
                }
            })
            .to_string(),
        )?;

        let adapter = CodexAdapter;
        let state_dir = tmp.join("state");
        let mut state = State::default();
        let record = adapter.import_auth_path(&state_dir, &mut state, &raw_home)?;

        assert_eq!(record.email, "a@example.com");
        assert!(Path::new(&record.auth_path).exists());
        assert_eq!(state.accounts.len(), 1);
        fs::remove_dir_all(&tmp)?;
        Ok(())
    }

    #[test]
    fn normalize_account_records_repairs_legacy_api_account_shape() -> Result<()> {
        let tmp = std::env::temp_dir().join(format!("scodex-normalize-{}", Uuid::new_v4()));
        let state_dir = tmp.join("state");
        let account_home = state_dir.join("accounts").join("legacy-api");
        fs::create_dir_all(&account_home)?;
        fs::write(
            account_home.join("auth.json"),
            serde_json::json!({
                "OPENAI_API_KEY": "sk-abcdef123456wxyz"
            })
            .to_string(),
        )?;
        fs::write(
            account_home.join("config.toml"),
            build_api_config(
                "legacy-api",
                &ApiLoginRequest {
                    api_token: "sk-abcdef123456wxyz".into(),
                    base_url: "https://example.com/v1".into(),
                    provider: "openrouter".into(),
                },
            ),
        )?;

        let mut state = State::default();
        state.accounts.push(AccountRecord {
            id: "legacy-api".into(),
            account_type: AccountType::Subscription,
            email: "sk-abcdef123456wxyz@wrong".into(),
            account_id: Some("acct-should-clear".into()),
            plan: Some("Plus".into()),
            auth_path: account_home
                .join("auth.json")
                .to_string_lossy()
                .into_owned(),
            config_path: Some(
                account_home
                    .join("config.toml")
                    .to_string_lossy()
                    .into_owned(),
            ),
            added_at: 1,
            updated_at: 2,
            ..Default::default()
        });
        state.usage_cache.insert(
            "legacy-api".into(),
            crate::core::state::UsageSnapshot {
                last_sync_error: Some("auth.json is missing tokens.access_token".into()),
                ..Default::default()
            },
        );

        let changed = CodexAdapter.normalize_account_records(&mut state);

        assert!(changed);
        let account = &state.accounts[0];
        assert_eq!(account.account_type, AccountType::Api);
        assert_eq!(account.email, "56wxyz@openrouter");
        assert_eq!(account.account_id, None);
        assert_eq!(account.plan, None);
        assert_eq!(account.api_provider.as_deref(), Some("openrouter"));
        assert_eq!(
            account.api_base_url.as_deref(),
            Some("https://example.com/v1")
        );
        assert_eq!(account.api_token_label.as_deref(), Some("sk-abcd-wxyz"));
        assert!(!state.usage_cache.contains_key("legacy-api"));
        fs::remove_dir_all(&tmp)?;
        Ok(())
    }
}
