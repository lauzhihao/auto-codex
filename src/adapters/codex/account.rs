use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use super::CodexAdapter;
use super::auth::decode_identity;
use super::now_ts;
use super::paths::codex_home;
use crate::core::state::{AccountRecord, State};
use crate::core::storage;

impl CodexAdapter {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use anyhow::Result;
    use base64::Engine;
    use uuid::Uuid;

    use crate::adapters::codex::CodexAdapter;
    use crate::core::state::State;

    fn fake_jwt(payload: &str) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        format!("{header}.{payload}.sig")
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
}
