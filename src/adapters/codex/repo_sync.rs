use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng, rand_core::RngCore};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::CodexAdapter;
use super::paths::find_program;
use crate::core::state::{AccountRecord, STATE_VERSION, State};
use crate::core::storage;
use crate::core::ui as core_ui;

const DEFAULT_BUNDLE_DIR: &str = ".scodex-account-pool";
const BUNDLE_FILENAME: &str = "bundle.enc.json";
const BUNDLE_KEY_ENV: &str = "SCODEX_POOL_KEY";
const BUNDLE_DIR_ENV: &str = "SCODEX_POOL_PATH";
const LEGACY_BUNDLE_DIR_ENVS: [&str; 2] = ["AUTO_CODEX_POOL_PATH", "CODEX_AUTOSWITCH_POOL_PATH"];
const BUNDLE_ALGORITHM: &str = "xchacha20poly1305-sha256";

impl CodexAdapter {
    pub fn push_account_pool(
        &self,
        state: &State,
        repo: &str,
        bundle_dir: Option<&str>,
        identity_file: Option<&Path>,
    ) -> Result<PushOutcome> {
        let ui = core_ui::messages();
        if state.accounts.is_empty() {
            bail!("{}", ui.repo_push_no_accounts());
        }

        let git_bin = resolve_git_bin()?;
        let repo = repo.trim();
        if repo.is_empty() {
            bail!("{}", ui.repo_sync_invalid_repo());
        }
        validate_identity_file(identity_file)?;
        let bundle_dir = resolve_bundle_dir(bundle_dir)?;
        let bundle_key = resolve_bundle_key()?;
        let checkout = clone_repo(&git_bin, repo, identity_file)?;
        let bundle_root = checkout.checkout_dir.join(&bundle_dir);
        let bundle_path = bundle_root.join(BUNDLE_FILENAME);
        let bundle = build_repo_bundle(state)?;
        let bundle_bytes = serde_json::to_vec(&bundle)?;

        println!("{}", ui.repo_push_start(repo));
        if bundle_path.exists() {
            let existing = decrypt_bundle_file(&bundle_path, &bundle_key)?;
            if existing == bundle_bytes {
                return Ok(PushOutcome {
                    changed: false,
                    exported_accounts: state.accounts.len(),
                });
            }
        }

        prepare_bundle_dir(&bundle_root)?;
        write_bundle_file(&bundle_path, &bundle_bytes, &bundle_key)?;

        git_add(&git_bin, &checkout.checkout_dir, &bundle_dir)?;
        if !git_has_changes(&git_bin, &checkout.checkout_dir, &bundle_dir)? {
            return Ok(PushOutcome {
                changed: false,
                exported_accounts: state.accounts.len(),
            });
        }

        git_commit(&git_bin, &checkout.checkout_dir)?;
        git_push(&git_bin, &checkout.checkout_dir, repo, identity_file)?;

        Ok(PushOutcome {
            changed: true,
            exported_accounts: state.accounts.len(),
        })
    }

    pub fn pull_account_pool(
        &self,
        state_dir: &Path,
        state: &mut State,
        repo: &str,
        bundle_dir: Option<&str>,
        identity_file: Option<&Path>,
    ) -> Result<PullOutcome> {
        let ui = core_ui::messages();
        let git_bin = resolve_git_bin()?;
        let repo = repo.trim();
        if repo.is_empty() {
            bail!("{}", ui.repo_sync_invalid_repo());
        }
        validate_identity_file(identity_file)?;
        let bundle_dir = resolve_bundle_dir(bundle_dir)?;
        let bundle_key = resolve_bundle_key()?;
        let checkout = clone_repo(&git_bin, repo, identity_file)?;
        let bundle_root = checkout.checkout_dir.join(&bundle_dir);
        let bundle_path = bundle_root.join(BUNDLE_FILENAME);

        println!("{}", ui.repo_pull_start(repo));
        if !bundle_path.exists() {
            bail!(
                "{}",
                ui.repo_pull_missing_bundle(&bundle_dir.display().to_string())
            );
        }

        let bundle: RepoBundle =
            serde_json::from_slice(&decrypt_bundle_file(&bundle_path, &bundle_key)?)
                .context("failed to parse decrypted account-pool bundle")?;
        if bundle.accounts.is_empty() {
            bail!(
                "{}",
                ui.repo_pull_no_accounts(&bundle_dir.display().to_string())
            );
        }
        *state = overwrite_local_account_pool(state_dir, &bundle)?;

        Ok(PullOutcome {
            imported_accounts: state.accounts.len(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PushOutcome {
    pub changed: bool,
    pub exported_accounts: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct PullOutcome {
    pub imported_accounts: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct RepoBundle {
    version: u32,
    exported_at: i64,
    accounts: Vec<RepoBundleAccount>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RepoBundleAccount {
    id: String,
    email: String,
    account_id: Option<String>,
    plan: Option<String>,
    added_at: i64,
    updated_at: i64,
    auth_json: String,
    config_toml: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EncryptedBundleFile {
    version: u32,
    algorithm: String,
    nonce_b64: String,
    ciphertext_b64: String,
}

fn build_repo_bundle(state: &State) -> Result<RepoBundle> {
    let mut accounts = state.accounts.iter().collect::<Vec<_>>();
    accounts.sort_by(|left, right| left.id.cmp(&right.id).then(left.email.cmp(&right.email)));

    let mut bundle_accounts = Vec::with_capacity(accounts.len());
    for account in accounts {
        bundle_accounts.push(export_account_bundle(account)?);
    }

    Ok(RepoBundle {
        version: 1,
        exported_at: super::now_ts(),
        accounts: bundle_accounts,
    })
}

fn export_account_bundle(account: &AccountRecord) -> Result<RepoBundleAccount> {
    let auth_path = Path::new(&account.auth_path);
    storage::ensure_exists(auth_path, "stored auth.json")?;
    let auth_json = fs::read_to_string(auth_path)
        .with_context(|| format!("failed to read {}", auth_path.display()))?;

    let config_toml = if let Some(config_path) = account.config_path.as_ref() {
        let config_path = Path::new(config_path);
        if config_path.exists() {
            Some(
                fs::read_to_string(config_path)
                    .with_context(|| format!("failed to read {}", config_path.display()))?,
            )
        } else {
            None
        }
    } else {
        None
    };

    Ok(RepoBundleAccount {
        id: account.id.clone(),
        email: account.email.clone(),
        account_id: account.account_id.clone(),
        plan: account.plan.clone(),
        added_at: account.added_at,
        updated_at: account.updated_at,
        auth_json,
        config_toml,
    })
}

fn prepare_bundle_dir(bundle_root: &Path) -> Result<()> {
    if bundle_root.exists() {
        fs::remove_dir_all(bundle_root)
            .with_context(|| format!("failed to remove {}", bundle_root.display()))?;
    }
    fs::create_dir_all(bundle_root)
        .with_context(|| format!("failed to create {}", bundle_root.display()))?;
    Ok(())
}

fn write_bundle_file(path: &Path, plaintext: &[u8], bundle_key: &[u8; 32]) -> Result<()> {
    let encrypted = encrypt_bundle_bytes(plaintext, bundle_key)?;
    let mut bytes = serde_json::to_vec_pretty(&encrypted)?;
    bytes.push(b'\n');
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn decrypt_bundle_file(path: &Path, bundle_key: &[u8; 32]) -> Result<Vec<u8>> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let encrypted: EncryptedBundleFile = serde_json::from_str(&contents)
        .with_context(|| format!("invalid encrypted bundle file: {}", path.display()))?;
    decrypt_bundle_bytes(&encrypted, bundle_key)
}

fn encrypt_bundle_bytes(plaintext: &[u8], bundle_key: &[u8; 32]) -> Result<EncryptedBundleFile> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(bundle_key));
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .map_err(|_| anyhow!("failed to encrypt account-pool bundle"))?;

    Ok(EncryptedBundleFile {
        version: 1,
        algorithm: BUNDLE_ALGORITHM.into(),
        nonce_b64: BASE64_STANDARD.encode(nonce),
        ciphertext_b64: BASE64_STANDARD.encode(ciphertext),
    })
}

fn decrypt_bundle_bytes(encrypted: &EncryptedBundleFile, bundle_key: &[u8; 32]) -> Result<Vec<u8>> {
    if encrypted.version != 1 || encrypted.algorithm != BUNDLE_ALGORITHM {
        bail!(
            "{}",
            core_ui::messages().repo_sync_decrypt_failed(BUNDLE_KEY_ENV)
        );
    }

    let nonce = BASE64_STANDARD
        .decode(&encrypted.nonce_b64)
        .map_err(|_| anyhow!(core_ui::messages().repo_sync_decrypt_failed(BUNDLE_KEY_ENV)))?;
    let ciphertext = BASE64_STANDARD
        .decode(&encrypted.ciphertext_b64)
        .map_err(|_| anyhow!(core_ui::messages().repo_sync_decrypt_failed(BUNDLE_KEY_ENV)))?;
    if nonce.len() != 24 {
        bail!(
            "{}",
            core_ui::messages().repo_sync_decrypt_failed(BUNDLE_KEY_ENV)
        );
    }

    let cipher = XChaCha20Poly1305::new(Key::from_slice(bundle_key));
    cipher
        .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| anyhow!(core_ui::messages().repo_sync_decrypt_failed(BUNDLE_KEY_ENV)))
}

fn resolve_bundle_key() -> Result<[u8; 32]> {
    resolve_bundle_key_from_value(env::var(BUNDLE_KEY_ENV).ok())
}

fn resolve_bundle_key_from_value(value: Option<String>) -> Result<[u8; 32]> {
    let value = value
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .ok_or_else(|| anyhow!(core_ui::messages().repo_sync_missing_key(BUNDLE_KEY_ENV)))?;
    Ok(derive_bundle_key(&value))
}

fn derive_bundle_key(secret: &str) -> [u8; 32] {
    let digest = Sha256::digest(secret.as_bytes());
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

fn overwrite_local_account_pool(state_dir: &Path, bundle: &RepoBundle) -> Result<State> {
    let staging_root = state_dir.join(format!(".scodex-pull-{}", Uuid::new_v4()));
    let staging_accounts = staging_root.join("accounts");
    fs::create_dir_all(&staging_accounts)
        .with_context(|| format!("failed to create {}", staging_accounts.display()))?;

    let mut accounts = bundle.accounts.iter().collect::<Vec<_>>();
    accounts.sort_by(|left, right| left.id.cmp(&right.id).then(left.email.cmp(&right.email)));

    let mut records = Vec::with_capacity(accounts.len());
    for account in accounts {
        let staged_home = staging_accounts.join(&account.id);
        fs::create_dir_all(&staged_home)
            .with_context(|| format!("failed to create {}", staged_home.display()))?;

        let staged_auth = staged_home.join("auth.json");
        fs::write(&staged_auth, account.auth_json.as_bytes())
            .with_context(|| format!("failed to write {}", staged_auth.display()))?;

        let final_home = state_dir.join("accounts").join(&account.id);
        let final_auth = final_home.join("auth.json");
        let final_config = if let Some(config) = account.config_toml.as_ref() {
            let staged_config = staged_home.join("config.toml");
            fs::write(&staged_config, config.as_bytes())
                .with_context(|| format!("failed to write {}", staged_config.display()))?;
            Some(final_home.join("config.toml"))
        } else {
            None
        };

        records.push(AccountRecord {
            id: account.id.clone(),
            email: account.email.clone(),
            account_id: account.account_id.clone(),
            plan: account.plan.clone(),
            auth_path: final_auth.to_string_lossy().into_owned(),
            config_path: final_config.map(|item| item.to_string_lossy().into_owned()),
            added_at: account.added_at,
            updated_at: account.updated_at,
        });
    }

    let final_accounts = state_dir.join("accounts");
    if final_accounts.exists() {
        fs::remove_dir_all(&final_accounts)
            .with_context(|| format!("failed to remove {}", final_accounts.display()))?;
    }
    fs::rename(&staging_accounts, &final_accounts)
        .with_context(|| format!("failed to move {} into place", final_accounts.display()))?;
    let _ = fs::remove_dir_all(&staging_root);

    Ok(State {
        version: STATE_VERSION,
        accounts: records,
        usage_cache: std::collections::BTreeMap::new(),
    })
}

fn resolve_git_bin() -> Result<PathBuf> {
    let Some(git_bin) = find_program(git_binary_names()) else {
        bail!(
            "{}",
            core_ui::messages().repo_sync_missing_git(git_install_hint_command())
        );
    };
    Ok(git_bin)
}

fn clone_repo(git_bin: &Path, repo: &str, identity_file: Option<&Path>) -> Result<RepoCheckout> {
    let checkout = RepoCheckout::new("scodex-git")?;
    let output = run_git(
        git_bin,
        ["clone", "--depth", "1", repo],
        Some(&checkout.checkout_dir),
        None,
        identity_file,
    )?;
    if !output.status.success() {
        let stderr = git_stderr(&output);
        if git_output_indicates_auth_failure(&stderr) {
            bail!("{}", core_ui::messages().repo_sync_clone_auth_failed(repo));
        }
        bail!(
            "{}",
            core_ui::messages().repo_sync_clone_failed(repo, output.status.code().unwrap_or(1))
        );
    }
    Ok(checkout)
}

fn git_add(git_bin: &Path, checkout_dir: &Path, bundle_dir: &Path) -> Result<()> {
    let output = run_git(
        git_bin,
        ["-C", ""],
        None,
        Some((
            checkout_dir,
            vec![
                "add".into(),
                "--all".into(),
                "--".into(),
                bundle_dir.display().to_string(),
            ],
        )),
        None,
    )?;
    if !output.status.success() {
        bail!(
            "{}",
            core_ui::messages().repo_sync_stage_failed(output.status.code().unwrap_or(1))
        );
    }
    Ok(())
}

fn git_has_changes(git_bin: &Path, checkout_dir: &Path, bundle_dir: &Path) -> Result<bool> {
    let output = run_git(
        git_bin,
        ["-C", ""],
        None,
        Some((
            checkout_dir,
            vec![
                "status".into(),
                "--porcelain".into(),
                "--".into(),
                bundle_dir.display().to_string(),
            ],
        )),
        None,
    )?;
    if !output.status.success() {
        bail!(
            "{}",
            core_ui::messages().repo_sync_status_failed(output.status.code().unwrap_or(1))
        );
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

fn git_commit(git_bin: &Path, checkout_dir: &Path) -> Result<()> {
    let message = format!("scodex encrypted account pool sync {}", super::now_ts());
    let output = run_git(
        git_bin,
        ["-C", ""],
        None,
        Some((
            checkout_dir,
            vec![
                "-c".into(),
                "user.name=scodex".into(),
                "-c".into(),
                "user.email=scodex@local".into(),
                "commit".into(),
                "-m".into(),
                message,
            ],
        )),
        None,
    )?;
    if !output.status.success() {
        bail!(
            "{}",
            core_ui::messages().repo_sync_commit_failed(output.status.code().unwrap_or(1))
        );
    }
    Ok(())
}

fn git_push(
    git_bin: &Path,
    checkout_dir: &Path,
    repo: &str,
    identity_file: Option<&Path>,
) -> Result<()> {
    let output = run_git(
        git_bin,
        ["-C", ""],
        None,
        Some((
            checkout_dir,
            vec!["push".into(), "origin".into(), "HEAD".into()],
        )),
        identity_file,
    )?;
    if !output.status.success() {
        let stderr = git_stderr(&output);
        if git_output_indicates_auth_failure(&stderr) {
            bail!("{}", core_ui::messages().repo_sync_push_auth_failed(repo));
        }
        bail!(
            "{}",
            core_ui::messages().repo_sync_push_failed(repo, output.status.code().unwrap_or(1))
        );
    }
    Ok(())
}

fn run_git<const N: usize>(
    git_bin: &Path,
    fixed_args: [&str; N],
    clone_target: Option<&Path>,
    dynamic: Option<(&Path, Vec<String>)>,
    identity_file: Option<&Path>,
) -> Result<Output> {
    let mut command = Command::new(git_bin);
    if let Some((checkout_dir, args)) = dynamic {
        command.arg("-C").arg(checkout_dir).args(args);
    } else {
        command.args(fixed_args);
        if let Some(clone_target) = clone_target {
            command.arg(clone_target);
        }
    }
    if let Some(identity_file) = identity_file {
        command.env("GIT_SSH_COMMAND", build_git_ssh_command(identity_file));
    }
    command
        .output()
        .with_context(|| format!("failed to execute {}", git_bin.display()))
}

fn validate_identity_file(identity_file: Option<&Path>) -> Result<()> {
    if let Some(path) = identity_file {
        let ui = core_ui::messages();
        storage::ensure_exists(path, "SSH identity file")
            .map_err(|_| anyhow!(ui.deploy_identity_not_found(path)))?;
    }
    Ok(())
}

// 用单引号包裹并把内部单引号转义为 '\''，避免 shell 拆分路径中的空格或特殊字符
fn build_git_ssh_command(identity_file: &Path) -> String {
    let raw = identity_file.to_string_lossy();
    let escaped = raw.replace('\'', "'\\''");
    format!("ssh -i '{escaped}' -o IdentitiesOnly=yes")
}

fn git_stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn git_output_indicates_auth_failure(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    [
        "authentication failed",
        "permission denied",
        "repository not found",
        "could not read username",
        "could not read password",
        "could not read from remote repository",
        "access denied",
        "403",
        "denied to",
    ]
    .iter()
    .any(|pattern| stderr.contains(pattern))
}

fn resolve_bundle_dir(bundle_dir: Option<&str>) -> Result<PathBuf> {
    let configured =
        resolve_bundle_dir_source(bundle_dir, configured_bundle_dir_from_env().as_deref())
            .to_string();
    let raw = configured.trim();
    if raw.is_empty() {
        return Ok(PathBuf::from(DEFAULT_BUNDLE_DIR));
    }

    let path = Path::new(raw);
    if path.is_absolute() {
        bail!("{}", core_ui::messages().repo_sync_invalid_path(raw));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("{}", core_ui::messages().repo_sync_invalid_path(raw));
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Ok(PathBuf::from(DEFAULT_BUNDLE_DIR));
    }
    Ok(normalized)
}

fn resolve_bundle_dir_source<'a>(bundle_dir: Option<&'a str>, env_dir: Option<&'a str>) -> &'a str {
    bundle_dir
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| env_dir.map(str::trim).filter(|value| !value.is_empty()))
        .unwrap_or(DEFAULT_BUNDLE_DIR)
}

fn configured_bundle_dir_from_env() -> Option<String> {
    std::iter::once(BUNDLE_DIR_ENV)
        .chain(LEGACY_BUNDLE_DIR_ENVS)
        .find_map(|env_name| {
            env::var(env_name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn git_binary_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["git.exe", "git"]
    } else {
        &["git"]
    }
}

fn git_install_hint_command() -> &'static str {
    if cfg!(target_os = "macos") {
        "brew install git"
    } else if cfg!(windows) {
        "winget install --id Git.Git -e --source winget"
    } else {
        "sudo apt-get update && sudo apt-get install -y git"
    }
}

struct RepoCheckout {
    temp_root: PathBuf,
    checkout_dir: PathBuf,
}

impl RepoCheckout {
    fn new(prefix: &str) -> Result<Self> {
        let temp_root = env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
        let checkout_dir = temp_root.join("checkout");
        fs::create_dir_all(&temp_root)
            .with_context(|| format!("failed to create {}", temp_root.display()))?;
        Ok(Self {
            temp_root,
            checkout_dir,
        })
    }
}

impl Drop for RepoCheckout {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_root);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use anyhow::Result;

    use super::{
        RepoBundle, RepoBundleAccount, build_git_ssh_command, decrypt_bundle_bytes,
        derive_bundle_key, encrypt_bundle_bytes, overwrite_local_account_pool, resolve_bundle_dir,
        resolve_bundle_dir_source, resolve_bundle_key_from_value,
    };

    #[test]
    fn bundle_dir_defaults_when_missing() -> Result<()> {
        assert_eq!(
            resolve_bundle_dir(None)?,
            PathBuf::from(".scodex-account-pool")
        );
        Ok(())
    }

    #[test]
    fn bundle_dir_prefers_cli_argument_over_environment() {
        assert_eq!(
            resolve_bundle_dir_source(Some("custom/pool"), Some("env/pool")),
            "custom/pool"
        );
    }

    #[test]
    fn bundle_dir_uses_environment_when_cli_argument_is_missing() {
        assert_eq!(
            resolve_bundle_dir_source(None, Some("env/pool")),
            "env/pool"
        );
    }

    #[test]
    fn bundle_dir_rejects_parent_escape() {
        assert!(resolve_bundle_dir(Some("../secrets")).is_err());
        assert!(resolve_bundle_dir(Some("/tmp/pool")).is_err());
    }

    #[test]
    fn bundle_dir_keeps_normal_relative_path() -> Result<()> {
        assert_eq!(
            resolve_bundle_dir(Some(".sync/accounts"))?,
            PathBuf::from(".sync/accounts")
        );
        Ok(())
    }

    #[test]
    fn bundle_round_trip_requires_matching_key() -> Result<()> {
        let bundle = RepoBundle {
            version: 1,
            exported_at: 1,
            accounts: vec![RepoBundleAccount {
                id: "acct-1".into(),
                email: "a@example.com".into(),
                account_id: Some("acct-remote-1".into()),
                plan: Some("Plus".into()),
                added_at: 1,
                updated_at: 2,
                auth_json: "{\"tokens\":{}}".into(),
                config_toml: Some("model = \"gpt-5\"".into()),
            }],
        };
        let plaintext = serde_json::to_vec(&bundle)?;
        let key = derive_bundle_key("test-secret");
        let wrong_key = derive_bundle_key("wrong-secret");

        let encrypted = encrypt_bundle_bytes(&plaintext, &key)?;
        assert_eq!(decrypt_bundle_bytes(&encrypted, &key)?, plaintext);
        assert!(decrypt_bundle_bytes(&encrypted, &wrong_key).is_err());
        Ok(())
    }

    #[test]
    fn resolve_bundle_key_requires_env_var() {
        assert!(resolve_bundle_key_from_value(None).is_err());
    }

    #[test]
    fn build_git_ssh_command_quotes_plain_path() {
        let cmd = build_git_ssh_command(std::path::Path::new("/home/alice/.ssh/id_ed25519"));
        assert_eq!(
            cmd,
            "ssh -i '/home/alice/.ssh/id_ed25519' -o IdentitiesOnly=yes"
        );
    }

    #[test]
    fn build_git_ssh_command_handles_spaces() {
        let cmd = build_git_ssh_command(std::path::Path::new("/tmp/with space/id_rsa"));
        assert_eq!(cmd, "ssh -i '/tmp/with space/id_rsa' -o IdentitiesOnly=yes");
    }

    #[test]
    fn build_git_ssh_command_escapes_single_quote() {
        let cmd = build_git_ssh_command(std::path::Path::new("/tmp/alice's keys/id_rsa"));
        assert_eq!(
            cmd,
            "ssh -i '/tmp/alice'\\''s keys/id_rsa' -o IdentitiesOnly=yes"
        );
    }

    #[test]
    fn overwrite_local_account_pool_replaces_existing_accounts() -> Result<()> {
        let state_dir =
            std::env::temp_dir().join(format!("scodex-overwrite-{}", uuid::Uuid::new_v4()));
        let old_home = state_dir.join("accounts").join("old-acct");
        fs::create_dir_all(&old_home)?;
        fs::write(old_home.join("auth.json"), "{\"tokens\":{}}")?;

        let bundle = RepoBundle {
            version: 1,
            exported_at: 1,
            accounts: vec![RepoBundleAccount {
                id: "acct-1".into(),
                email: "pool@example.com".into(),
                account_id: Some("acct-remote-1".into()),
                plan: Some("Plus".into()),
                added_at: 10,
                updated_at: 20,
                auth_json: "{\"tokens\":{\"access_token\":\"x\"}}".into(),
                config_toml: Some("model = \"gpt-5\"".into()),
            }],
        };

        let state = overwrite_local_account_pool(&state_dir, &bundle)?;

        assert_eq!(state.accounts.len(), 1);
        assert_eq!(state.accounts[0].id, "acct-1");
        assert!(state.usage_cache.is_empty());
        assert!(!state_dir.join("accounts").join("old-acct").exists());
        assert!(
            state_dir
                .join("accounts")
                .join("acct-1")
                .join("auth.json")
                .exists()
        );
        assert!(
            state_dir
                .join("accounts")
                .join("acct-1")
                .join("config.toml")
                .exists()
        );

        fs::remove_dir_all(&state_dir)?;
        Ok(())
    }
}
