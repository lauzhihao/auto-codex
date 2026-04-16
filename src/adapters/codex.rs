use std::env;
use std::fs;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Local, Utc};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::Value;
use uuid::Uuid;

use crate::adapters::{AdapterCapabilities, CliAdapter};
use crate::core::policy::{choose_best_account, choose_current_account};
use crate::core::state::{AccountRecord, LiveIdentity, State, UsageSnapshot};
use crate::core::storage;

#[derive(Debug, Default)]
pub struct CodexAdapter;

impl CliAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            import_known: true,
            read_current_identity: true,
            switch_account: true,
            login: true,
            launch: true,
            resume: true,
            live_usage: true,
        }
    }
}

impl CodexAdapter {
    pub fn import_auth_path(
        &self,
        state_dir: &Path,
        state: &mut State,
        raw_path: &Path,
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
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let account_home = state_dir.join("accounts").join(&account_id);
        fs::create_dir_all(&account_home)
            .with_context(|| format!("failed to create {}", account_home.display()))?;

        let stored_auth_path = account_home.join("auth.json");
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
            email: identity.email,
            account_id: identity.account_id,
            plan: identity.plan,
            auth_path: stored_auth_path.to_string_lossy().into_owned(),
            config_path: stored_config_path.map(|item| item.to_string_lossy().into_owned()),
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
        let dst = codex_home().join("auth.json");
        atomic_copy(src, &dst)
    }

    pub fn read_live_identity(&self) -> Option<LiveIdentity> {
        let auth_path = codex_home().join("auth.json");
        let auth = self.read_auth_json(&auth_path).ok()?;
        decode_identity(&auth).ok().map(Into::into)
    }

    pub fn refresh_all_accounts(&self, state: &mut State) {
        for account in &state.accounts {
            let previous = state.usage_cache.get(&account.id).cloned();
            let usage = self.fetch_usage_for_account(account, previous.as_ref());
            state.usage_cache.insert(account.id.clone(), usage);
        }
    }

    pub fn refresh_account_usage(
        &self,
        state: &mut State,
        account: &AccountRecord,
    ) -> UsageSnapshot {
        let usage = self.fetch_usage_for_account(account, state.usage_cache.get(&account.id));
        state.usage_cache.insert(account.id.clone(), usage.clone());
        usage
    }

    pub fn ensure_best_account(
        &self,
        state_dir: &Path,
        state: &mut State,
        no_import_known: bool,
        no_login: bool,
        perform_switch: bool,
    ) -> Result<Option<(AccountRecord, UsageSnapshot)>> {
        if !no_import_known {
            self.import_known_sources(state_dir, state);
        }

        if state.accounts.is_empty() {
            if no_login {
                return Ok(None);
            }
            let record = self.run_device_auth_login(state_dir, state)?;
            let usage = self.refresh_account_usage(state, &record);
            if perform_switch {
                self.switch_account(&record)?;
            }
            return Ok(Some((record, usage)));
        }

        self.refresh_all_accounts(state);
        if let Some(current) =
            choose_current_account(state, self.read_live_identity().as_ref()).cloned()
        {
            let usage = state
                .usage_cache
                .get(&current.id)
                .cloned()
                .unwrap_or_default();
            if perform_switch {
                self.switch_account(&current)?;
            }
            return Ok(Some((current, usage)));
        }

        if let Some(best) = choose_best_account(state).cloned() {
            let usage = state.usage_cache.get(&best.id).cloned().unwrap_or_default();
            if perform_switch {
                self.switch_account(&best)?;
            }
            return Ok(Some((best, usage)));
        }

        if no_login {
            return Ok(None);
        }
        let record = self.run_device_auth_login(state_dir, state)?;
        let usage = self.refresh_account_usage(state, &record);
        if perform_switch {
            self.switch_account(&record)?;
        }
        Ok(Some((record, usage)))
    }

    pub fn render_account_table(&self, state: &State, active: Option<&LiveIdentity>) -> String {
        let mut accounts = state.accounts.iter().collect::<Vec<_>>();
        accounts.sort_by(|left, right| left.email.cmp(&right.email));

        let rows = accounts
            .into_iter()
            .map(|account| {
                let usage = state
                    .usage_cache
                    .get(&account.id)
                    .cloned()
                    .unwrap_or_default();
                let plan = account
                    .plan
                    .clone()
                    .or(usage.plan.clone())
                    .unwrap_or_else(|| "Unknown".into());
                vec![
                    if active.is_some_and(|live| {
                        account.email.eq_ignore_ascii_case(&live.email)
                            || account.account_id.is_some() && account.account_id == live.account_id
                    }) {
                        active_account_marker()
                    } else {
                        String::new()
                    },
                    account.email.clone(),
                    plan,
                    format_percent(usage.five_hour_remaining_percent),
                    format_percent(usage.weekly_remaining_percent),
                    format_reset_on(usage.weekly_refresh_at.as_deref()),
                    format_account_status(&usage),
                ]
            })
            .collect::<Vec<_>>();

        render_ascii_table(
            &[
                "Active", "Email", "Plan", "5h", "Weekly", "ResetOn", "Status",
            ],
            &rows,
            &[
                "center", "left", "center", "center", "center", "center", "center",
            ],
        )
    }

    pub fn run_device_auth_login(
        &self,
        state_dir: &Path,
        state: &mut State,
    ) -> Result<AccountRecord> {
        let codex_bin = self.resolve_codex_bin()?;
        let temp_root = state_dir.join(".tmp");
        fs::create_dir_all(&temp_root)
            .with_context(|| format!("failed to create {}", temp_root.display()))?;
        let tmp_home = temp_root.join(format!("codex-autoswitch-login-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp_home)
            .with_context(|| format!("failed to create {}", tmp_home.display()))?;

        println!("Starting `codex login --device-auth`.");
        println!("Open the printed URL on any browser-enabled machine and finish the login there.");
        println!("Headless host LAN IP: {}", detect_local_ip());
        println!();

        let status = Command::new(&codex_bin)
            .arg("login")
            .arg("--device-auth")
            .env("CODEX_HOME", &tmp_home)
            .status()
            .with_context(|| format!("failed to execute {}", codex_bin.display()))?;
        if !status.success() {
            let _ = fs::remove_dir_all(&tmp_home);
            bail!(
                "codex login failed with status {}",
                status.code().unwrap_or(1)
            );
        }

        let auth_path = tmp_home.join("auth.json");
        if !auth_path.exists() {
            let _ = fs::remove_dir_all(&tmp_home);
            bail!("Login finished but no auth.json was produced.");
        }

        let record = self.import_auth_path(state_dir, state, &tmp_home)?;
        let _ = fs::remove_dir_all(&tmp_home);
        Ok(record)
    }

    pub fn launch_codex(&self, extra_args: &[std::ffi::OsString], resume: bool) -> Result<i32> {
        let codex_bin = self.resolve_codex_bin()?;
        let fresh_cmd = build_codex_launch_command(&codex_bin, extra_args, false);
        if resume
            && self.has_resumable_session(
                &env::current_dir().context("failed to read current directory")?,
            )
        {
            let resume_cmd = build_codex_launch_command(&codex_bin, extra_args, true);
            println!("Resuming latest Codex session for this directory.");
            let status = Command::new(&resume_cmd[0])
                .args(&resume_cmd[1..])
                .status()
                .context("failed to execute codex resume")?;
            if status.success() {
                return Ok(status.code().unwrap_or(0));
            }
            eprintln!("Resume did not complete cleanly; falling back to a fresh Codex session.");
        } else {
            println!("Starting a fresh Codex session.");
        }

        let status = Command::new(&fresh_cmd[0])
            .args(&fresh_cmd[1..])
            .status()
            .context("failed to execute codex")?;
        Ok(status.code().unwrap_or(1))
    }

    pub fn run_passthrough(&self, extra_args: &[std::ffi::OsString]) -> Result<i32> {
        let codex_bin = self.resolve_codex_bin()?;
        let status = Command::new(&codex_bin)
            .args(extra_args)
            .status()
            .with_context(|| format!("failed to execute {}", codex_bin.display()))?;
        Ok(status.code().unwrap_or(1))
    }

    pub fn resolve_codex_bin(&self) -> Result<PathBuf> {
        if let Some(env) = env::var_os("CODEX_BIN") {
            let path = PathBuf::from(env);
            if path.exists() {
                return Ok(path);
            }
        }

        if let Some(path) = find_in_path("codex") {
            return Ok(path);
        }

        if let Some(home) = env::var_os("HOME") {
            let path = PathBuf::from(home).join(".local").join("bin").join("codex");
            if path.exists() {
                return Ok(path);
            }
        }

        bail!("Unable to find `codex`. Set CODEX_BIN or install Codex CLI first.")
    }

    fn has_resumable_session(&self, cwd: &Path) -> bool {
        let sessions_root = codex_home().join("sessions");
        if !sessions_root.exists() {
            return false;
        }
        let target = match cwd.canonicalize() {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(_) => return false,
        };
        has_resumable_session_under(&sessions_root, &target)
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
                    UsageSnapshot {
                        plan: account.plan.clone(),
                        last_synced_at: Some(timestamp),
                        last_sync_error: Some(error.to_string()),
                        ..UsageSnapshot::default()
                    },
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
                    UsageSnapshot {
                        plan: account.plan.clone(),
                        last_synced_at: Some(timestamp),
                        last_sync_error: Some("auth.json is missing tokens.access_token".into()),
                        ..UsageSnapshot::default()
                    },
                );
            }
        };

        let url = resolve_usage_url(config_path.as_deref());
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static("codex-cli"));
        let auth_value = format!("Bearer {access_token}");
        let auth_header = HeaderValue::from_str(&auth_value);
        if let Ok(value) = auth_header {
            headers.insert(AUTHORIZATION, value);
        }
        if let Some(account_id) = account_id
            .as_ref()
            .and_then(|value| HeaderValue::from_str(value).ok())
        {
            headers.insert("ChatGPT-Account-Id", account_id);
        }

        let client = Client::new();
        let response = client.get(&url).headers(headers).send();
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                return merge_usage_with_previous(
                    previous,
                    UsageSnapshot {
                        plan: account.plan.clone(),
                        last_synced_at: Some(timestamp),
                        last_sync_error: Some(error.to_string()),
                        ..UsageSnapshot::default()
                    },
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
                UsageSnapshot {
                    plan: account.plan.clone(),
                    last_synced_at: Some(timestamp),
                    last_sync_error: Some(format!("GET {url} failed: {}", response.status())),
                    ..UsageSnapshot::default()
                },
            );
        }

        let payload = match response.json::<Value>() {
            Ok(value) => value,
            Err(error) => {
                return merge_usage_with_previous(
                    previous,
                    UsageSnapshot {
                        plan: account.plan.clone(),
                        last_synced_at: Some(timestamp),
                        last_sync_error: Some(error.to_string()),
                        ..UsageSnapshot::default()
                    },
                );
            }
        };

        let mut normalized = normalize_usage_response(&payload);
        normalized.last_synced_at = Some(timestamp);
        normalized.last_sync_error = None;
        normalized.needs_relogin = false;
        normalized
    }

    fn read_auth_json(&self, path: &Path) -> Result<Value> {
        storage::ensure_exists(path, "auth.json")?;
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let auth: Value = serde_json::from_str(&contents)
            .with_context(|| format!("invalid JSON in {}", path.display()))?;
        Ok(auth)
    }
}

fn codex_home() -> PathBuf {
    if let Some(home) = env::var_os("CODEX_HOME") {
        PathBuf::from(home)
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".codex")
    } else {
        PathBuf::from(".codex")
    }
}

fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn build_codex_launch_command(
    codex_bin: &Path,
    extra_args: &[std::ffi::OsString],
    resume: bool,
) -> Vec<std::ffi::OsString> {
    let mut command = vec![codex_bin.as_os_str().to_os_string()];
    if resume {
        command.push("resume".into());
        command.push("--last".into());
    }
    if !extra_args.iter().any(|arg| arg == "--yolo") {
        command.push("--yolo".into());
    }
    command.extend(extra_args.iter().cloned());
    command
}

fn has_resumable_session_under(root: &Path, target: &str) -> bool {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return false,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if has_resumable_session_under(&path, target) {
                return true;
            }
            continue;
        }
        if path.extension().and_then(|item| item.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(first_line) = contents.lines().next() else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<Value>(first_line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let payload = record.get("payload").unwrap_or(&Value::Null);
        if payload.get("originator").and_then(Value::as_str) != Some("codex-tui") {
            continue;
        }
        if payload.get("cwd").and_then(Value::as_str) == Some(target) {
            return true;
        }
    }
    false
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn decode_identity(auth: &Value) -> Result<LiveIdentityWithPlan> {
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

fn normalize_plan(raw: &str) -> String {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return String::new();
    }
    match value.as_str() {
        "plus" | "free" | "pro" => {
            let mut chars = value.chars();
            let head = chars.next().unwrap().to_ascii_uppercase();
            format!("{head}{}", chars.as_str())
        }
        _ => {
            let mut chars = value.chars();
            let head = chars.next().unwrap().to_ascii_uppercase();
            format!("{head}{}", chars.as_str())
        }
    }
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

fn merge_usage_with_previous(
    previous: Option<&UsageSnapshot>,
    update: UsageSnapshot,
) -> UsageSnapshot {
    if let Some(previous) = previous {
        let mut merged = previous.clone();
        if update.plan.is_some() {
            merged.plan = update.plan;
        }
        if update.weekly_remaining_percent.is_some() {
            merged.weekly_remaining_percent = update.weekly_remaining_percent;
        }
        if update.weekly_refresh_at.is_some() {
            merged.weekly_refresh_at = update.weekly_refresh_at;
        }
        if update.five_hour_remaining_percent.is_some() {
            merged.five_hour_remaining_percent = update.five_hour_remaining_percent;
        }
        if update.five_hour_refresh_at.is_some() {
            merged.five_hour_refresh_at = update.five_hour_refresh_at;
        }
        if update.credits_balance.is_some() {
            merged.credits_balance = update.credits_balance;
        }
        if update.last_synced_at.is_some() {
            merged.last_synced_at = update.last_synced_at;
        }
        if update.last_sync_error.is_some() || update.last_sync_error.is_none() {
            merged.last_sync_error = update.last_sync_error;
        }
        merged.needs_relogin = update.needs_relogin;
        return merged;
    }
    update
}

fn resolve_usage_url(config_path: Option<&Path>) -> String {
    let mut base = env::var("CODEX_USAGE_BASE_URL")
        .unwrap_or_else(|_| "https://chatgpt.com/backend-api".into());
    if base.trim().is_empty() {
        base = "https://chatgpt.com/backend-api".into();
    } else if env::var("CODEX_USAGE_BASE_URL").is_err()
        && let Some(config_path) = config_path
        && let Ok(contents) = fs::read_to_string(config_path)
        && let Some(parsed) = parse_chatgpt_base_url(&contents)
    {
        base = parsed;
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

fn format_percent(value: Option<i64>) -> String {
    value
        .map(|value| format!("{value}%"))
        .unwrap_or_else(|| "N/A".into())
}

fn format_reset_on(value: Option<&str>) -> String {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return "N/A".into();
    };
    if value.eq_ignore_ascii_case("none")
        || value.eq_ignore_ascii_case("null")
        || value.eq_ignore_ascii_case("n/a")
    {
        return "N/A".into();
    }
    if let Ok(timestamp) = value.parse::<i64>() {
        if let Some(parsed) = DateTime::<Utc>::from_timestamp(timestamp, 0) {
            return parsed
                .with_timezone(&Local)
                .format("%m-%d %H:%M")
                .to_string();
        }
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return parsed
            .with_timezone(&Local)
            .format("%m-%d %H:%M")
            .to_string();
    }
    "N/A".into()
}

fn format_account_status(usage: &UsageSnapshot) -> String {
    if usage.needs_relogin {
        "RELOGIN".into()
    } else if usage.last_sync_error.is_some() {
        "ERROR".into()
    } else {
        "OK".into()
    }
}

fn active_account_marker() -> String {
    "✓".into()
}

fn detect_local_ip() -> String {
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(sock) => sock,
        Err(_) => return "127.0.0.1".into(),
    };
    if sock.connect("8.8.8.8:80").is_ok()
        && let Ok(address) = sock.local_addr()
    {
        return address.ip().to_string();
    }
    "127.0.0.1".into()
}

fn render_ascii_table(headers: &[&str], rows: &[Vec<String>], aligns: &[&str]) -> String {
    let widths = headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            rows.iter()
                .map(|row| row.get(index).map_or(0, String::len))
                .fold(header.len(), usize::max)
        })
        .collect::<Vec<_>>();
    let border = format!(
        "+{}+",
        widths
            .iter()
            .map(|width| "-".repeat(width + 2))
            .collect::<Vec<_>>()
            .join("+")
    );

    let render_row = |values: Vec<String>| {
        let cells = values
            .into_iter()
            .enumerate()
            .map(|(index, value)| align_cell(value, widths[index], aligns[index]))
            .collect::<Vec<_>>();
        format!("| {} |", cells.join(" | "))
    };

    let mut lines = vec![
        border.clone(),
        render_row(headers.iter().map(|item| (*item).to_string()).collect()),
        border.clone(),
    ];
    for row in rows {
        lines.push(render_row(row.clone()));
        lines.push(border.clone());
    }
    lines.join("\n")
}

fn align_cell(value: String, width: usize, align: &str) -> String {
    match align {
        "left" => format!("{value:<width$}"),
        "right" => format!("{value:>width$}"),
        "center" => {
            let total_padding = width.saturating_sub(value.len());
            let left = total_padding / 2;
            let right = total_padding - left;
            format!("{}{}{}", " ".repeat(left), value, " ".repeat(right))
        }
        _ => value,
    }
}

#[derive(Debug)]
struct LiveIdentityWithPlan {
    email: String,
    account_id: Option<String>,
    plan: Option<String>,
}

impl From<LiveIdentityWithPlan> for LiveIdentity {
    fn from(value: LiveIdentityWithPlan) -> Self {
        Self {
            email: value.email,
            account_id: value.account_id,
        }
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
    use std::fs;
    use std::path::Path;

    use anyhow::Result;
    use base64::Engine;
    use uuid::Uuid;

    use std::ffi::OsString;

    use super::{
        CodexAdapter, build_codex_launch_command, decode_identity, has_resumable_session_under,
        normalize_usage_response, parse_chatgpt_base_url,
    };
    use crate::core::state::State;

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
    fn build_launch_command_adds_resume_and_yolo_when_needed() {
        let command = build_codex_launch_command(
            Path::new("/usr/bin/codex"),
            &[OsString::from("exec"), OsString::from("fix it")],
            true,
        );

        assert_eq!(command[1], OsString::from("resume"));
        assert_eq!(command[2], OsString::from("--last"));
        assert!(command.iter().any(|arg| arg == "--yolo"));
    }

    #[test]
    fn detects_resumable_session_from_session_meta() -> Result<()> {
        let tmp = std::env::temp_dir().join(format!("scodex-sessions-{}", Uuid::new_v4()));
        fs::create_dir_all(tmp.join("2026"))?;
        let cwd = tmp.join("project");
        fs::create_dir_all(&cwd)?;
        let session_file = tmp.join("2026").join("session.jsonl");
        fs::write(
            &session_file,
            format!(
                "{}\n",
                serde_json::json!({
                    "type": "session_meta",
                    "payload": {
                        "originator": "codex-tui",
                        "cwd": cwd.canonicalize()?.to_string_lossy(),
                    }
                })
            ),
        )?;

        assert!(has_resumable_session_under(
            &tmp,
            &cwd.canonicalize()?.to_string_lossy(),
        ));
        fs::remove_dir_all(&tmp)?;
        Ok(())
    }
}
