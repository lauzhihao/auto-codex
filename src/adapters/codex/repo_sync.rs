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
use crate::core::state::{AccountRecord, AccountType, STATE_VERSION, State};
use crate::core::storage;
use crate::core::ui as core_ui;

const DEFAULT_BUNDLE_DIR: &str = ".scodex-account-pool";
const BUNDLE_FILENAME: &str = "bundle.enc.json";
const BUNDLE_KEY_ENV: &str = "SCODEX_POOL_KEY";
const BUNDLE_DIR_ENV: &str = "SCODEX_POOL_PATH";
const LEGACY_BUNDLE_DIR_ENVS: [&str; 2] = ["AUTO_CODEX_POOL_PATH", "CODEX_AUTOSWITCH_POOL_PATH"];
const BUNDLE_ALGORITHM: &str = "xchacha20poly1305-sha256";

// PBKDF2 参数：≥100_000 次迭代，16 字节随机 salt
const KDF_VERSION: u32 = 2;
const KDF_ITERATIONS: u32 = 100_000;
const KDF_SALT_LEN: usize = 16;

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
        let bundle_key_raw = resolve_bundle_key_raw()?;
        let checkout = clone_repo(&git_bin, repo, identity_file)?;
        let bundle_root = checkout.checkout_dir.join(&bundle_dir);
        let bundle_path = bundle_root.join(BUNDLE_FILENAME);
        let bundle = build_repo_bundle(state)?;
        let bundle_bytes = serde_json::to_vec(&bundle)?;

        println!("{}", ui.repo_push_start(repo));
        if bundle_path.exists() {
            let existing = decrypt_bundle_file(&bundle_path, &bundle_key_raw)?;
            if existing == bundle_bytes {
                return Ok(PushOutcome {
                    changed: false,
                    exported_accounts: state.accounts.len(),
                });
            }
        }

        prepare_bundle_dir(&bundle_root)?;
        write_bundle_file(&bundle_path, &bundle_bytes, &bundle_key_raw)?;

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
        let bundle_key_raw = resolve_bundle_key_raw()?;
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
            serde_json::from_slice(&decrypt_bundle_file(&bundle_path, &bundle_key_raw)?)
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
    #[serde(default)]
    account_type: AccountType,
    email: String,
    account_id: Option<String>,
    plan: Option<String>,
    #[serde(default)]
    api_provider: Option<String>,
    #[serde(default)]
    api_base_url: Option<String>,
    #[serde(default)]
    api_token_label: Option<String>,
    added_at: i64,
    updated_at: i64,
    auth_json: String,
    config_toml: Option<String>,
}

/// bundle.enc.json 顶层 kdf 字段，v2 使用 PBKDF2-HMAC-SHA256
#[derive(Debug, Serialize, Deserialize, Clone)]
struct KdfParams {
    version: u32,
    salt_b64: String,
    iterations: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct EncryptedBundleFile {
    version: u32,
    algorithm: String,
    nonce_b64: String,
    ciphertext_b64: String,
    /// 缺失表示旧 v1 bundle（SHA-256 直接派生）
    #[serde(skip_serializing_if = "Option::is_none")]
    kdf: Option<KdfParams>,
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
        account_type: account.account_type,
        email: account.email.clone(),
        account_id: account.account_id.clone(),
        plan: account.plan.clone(),
        api_provider: account.api_provider.clone(),
        api_base_url: account.api_base_url.clone(),
        api_token_label: account.api_token_label.clone(),
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

fn write_bundle_file(path: &Path, plaintext: &[u8], bundle_key_raw: &str) -> Result<()> {
    let encrypted = encrypt_bundle_bytes(plaintext, bundle_key_raw)?;
    let mut bytes = serde_json::to_vec_pretty(&encrypted)?;
    bytes.push(b'\n');
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    // 限制 bundle 文件权限，防止其他用户读取
    set_file_mode_600(path)?;
    Ok(())
}

fn decrypt_bundle_file(path: &Path, bundle_key_raw: &str) -> Result<Vec<u8>> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let encrypted: EncryptedBundleFile = serde_json::from_str(&contents)
        .with_context(|| format!("invalid encrypted bundle file: {}", path.display()))?;
    decrypt_bundle_bytes(&encrypted, bundle_key_raw)
}

/// 加密时：生成随机 salt → PBKDF2 派生密钥 → 写 kdf 元信息到 bundle
fn encrypt_bundle_bytes(plaintext: &[u8], bundle_key_raw: &str) -> Result<EncryptedBundleFile> {
    let mut salt = [0u8; KDF_SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let key = derive_bundle_key_v2(bundle_key_raw, &salt, KDF_ITERATIONS);

    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
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
        kdf: Some(KdfParams {
            version: KDF_VERSION,
            salt_b64: BASE64_STANDARD.encode(salt),
            iterations: KDF_ITERATIONS,
        }),
    })
}

/// 解密时：读 kdf.version 决定派生路径
/// - kdf 缺失 或 version=1 → 旧 SHA-256 路径（兼容旧 bundle）
/// - version=2 → PBKDF2-HMAC-SHA256
fn decrypt_bundle_bytes(encrypted: &EncryptedBundleFile, bundle_key_raw: &str) -> Result<Vec<u8>> {
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

    // 根据 kdf 字段决定密钥派生方式
    let key: [u8; 32] = match &encrypted.kdf {
        Some(kdf) if kdf.version == KDF_VERSION => {
            let salt = BASE64_STANDARD.decode(&kdf.salt_b64).map_err(|_| {
                anyhow!(core_ui::messages().repo_sync_decrypt_failed(BUNDLE_KEY_ENV))
            })?;
            derive_bundle_key_v2(bundle_key_raw, &salt, kdf.iterations)
        }
        // 缺失或 version != 2：兼容旧 v1 SHA-256 路径
        _ => derive_bundle_key_v1(bundle_key_raw),
    };

    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| anyhow!(core_ui::messages().repo_sync_decrypt_failed(BUNDLE_KEY_ENV)))
}

/// v1 兼容路径：SHA-256 直接派生（无 salt，弱）
fn derive_bundle_key_v1(secret: &str) -> [u8; 32] {
    let digest = Sha256::digest(secret.as_bytes());
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

/// v2：PBKDF2-HMAC-SHA256，带 salt 和迭代次数
///
/// 使用 sha2 0.10 手动实现 HMAC-SHA256（RFC 2104）和 PBKDF2（RFC 2898）。
/// pbkdf2 crate 0.13 需要 digest 0.11，与项目依赖的 sha2 0.10（digest 0.10）版本不兼容，
/// 故直接用 Sha256::digest 手动推算，避免引入第二个 digest 版本。
fn derive_bundle_key_v2(secret: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    pbkdf2_hmac_sha256(secret.as_bytes(), salt, iterations)
}

/// HMAC-SHA256（RFC 2104）：用 sha2 0.10 / digest 0.10 实现
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    // 若 key 超过块大小则先哈希
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let h = Sha256::digest(key);
        k[..32].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = {
        let mut h = Sha256::new();
        h.update(&ipad);
        h.update(data);
        h.finalize()
    };
    let mut h = Sha256::new();
    h.update(&opad);
    h.update(&inner);
    let out = h.finalize();
    let mut result = [0u8; 32];
    result.copy_from_slice(&out);
    result
}

/// PBKDF2-HMAC-SHA256（RFC 2898），输出 32 字节
fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    // 块编号 = 1（只需一个 32 字节块）
    let mut salt_with_block = salt.to_vec();
    salt_with_block.extend_from_slice(&1u32.to_be_bytes());

    let mut u = hmac_sha256(password, &salt_with_block);
    let mut result = u;
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for (r, &ui) in result.iter_mut().zip(u.iter()) {
            *r ^= ui;
        }
    }
    result
}

/// Unix-only：写出后立即 chmod 0600，防止其他用户读取敏感文件
fn set_file_mode_600(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod 0600 {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path; // Windows 不强制
    }
    Ok(())
}

fn resolve_bundle_key_raw() -> Result<String> {
    resolve_bundle_key_raw_from_value(env::var(BUNDLE_KEY_ENV).ok())
}

fn resolve_bundle_key_raw_from_value(value: Option<String>) -> Result<String> {
    value
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .ok_or_else(|| anyhow!(core_ui::messages().repo_sync_missing_key(BUNDLE_KEY_ENV)))
}

// 保留旧名供测试兼容（测试模块内部用 v1 路径验证往返）
#[cfg(test)]
fn derive_bundle_key(secret: &str) -> [u8; 32] {
    derive_bundle_key_v1(secret)
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
        // 写入后立即限制 auth.json 权限
        set_file_mode_600(&staged_auth)?;

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
            account_type: account.account_type,
            email: account.email.clone(),
            account_id: account.account_id.clone(),
            plan: account.plan.clone(),
            auth_path: final_auth.to_string_lossy().into_owned(),
            config_path: final_config.map(|item| item.to_string_lossy().into_owned()),
            api_provider: account.api_provider.clone(),
            api_base_url: account.api_base_url.clone(),
            api_token_label: account.api_token_label.clone(),
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
        repo_sync: Default::default(),
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
        &[
            "clone",
            "--depth",
            "1",
            repo,
            &checkout.checkout_dir.to_string_lossy(),
        ],
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

/// 通用 git 子命令执行器：统一 fixed + dynamic 路径，消除歧义
///
/// - `args`：完整参数列表，由调用方组装
/// - `checkout_dir`：若提供则在参数首位插入 `-C <dir>`
fn run_git(
    git_bin: &Path,
    args: &[&str],
    checkout_dir: Option<&Path>,
    identity_file: Option<&Path>,
) -> Result<Output> {
    let mut command = Command::new(git_bin);
    if let Some(dir) = checkout_dir {
        command.arg("-C").arg(dir);
    }
    command.args(args);
    if let Some(identity_file) = identity_file {
        command.env("GIT_SSH_COMMAND", build_git_ssh_command(identity_file));
    }
    command
        .output()
        .with_context(|| format!("failed to execute {}", git_bin.display()))
}

/// git 子命令执行器：期望成功，失败时返回带 label 的错误
///
/// git_has_changes 因需解析 stdout 而单独保留；其余 git_* 薄包装此函数。
fn run_git_expect_success(
    label: &str,
    git_bin: &Path,
    args: &[&str],
    checkout_dir: Option<&Path>,
    identity_file: Option<&Path>,
) -> Result<()> {
    let output = run_git(git_bin, args, checkout_dir, identity_file)?;
    if !output.status.success() {
        bail!(
            "git {} failed (exit {}): {}",
            label,
            output.status.code().unwrap_or(1),
            git_stderr(&output)
        );
    }
    Ok(())
}

fn git_add(git_bin: &Path, checkout_dir: &Path, bundle_dir: &Path) -> Result<()> {
    run_git_expect_success(
        "add",
        git_bin,
        &["add", "--all", "--", &bundle_dir.display().to_string()],
        Some(checkout_dir),
        None,
    )
    .map_err(|e| {
        anyhow!(
            "{}",
            core_ui::messages().repo_sync_stage_failed(e.to_string().parse::<i32>().unwrap_or(1))
        )
    })
    .or_else(|_| {
        // 保留原始错误消息
        run_git_expect_success(
            "add",
            git_bin,
            &["add", "--all", "--", &bundle_dir.display().to_string()],
            Some(checkout_dir),
            None,
        )
    })
}

fn git_has_changes(git_bin: &Path, checkout_dir: &Path, bundle_dir: &Path) -> Result<bool> {
    let output = run_git(
        git_bin,
        &[
            "status",
            "--porcelain",
            "--",
            &bundle_dir.display().to_string(),
        ],
        Some(checkout_dir),
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
    run_git_expect_success(
        "commit",
        git_bin,
        &[
            "-c",
            "user.name=scodex",
            "-c",
            "user.email=scodex@local",
            "commit",
            "-m",
            &message,
        ],
        Some(checkout_dir),
        None,
    )
    .map_err(|_| anyhow!("{}", core_ui::messages().repo_sync_commit_failed(1)))
}

fn git_push(
    git_bin: &Path,
    checkout_dir: &Path,
    repo: &str,
    identity_file: Option<&Path>,
) -> Result<()> {
    let output = run_git(
        git_bin,
        &["push", "origin", "HEAD"],
        Some(checkout_dir),
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
        BASE64_STANDARD, BUNDLE_ALGORITHM, EncryptedBundleFile, RepoBundle, RepoBundleAccount,
        build_git_ssh_command, decrypt_bundle_bytes, derive_bundle_key, derive_bundle_key_v1,
        encrypt_bundle_bytes, overwrite_local_account_pool, resolve_bundle_dir,
        resolve_bundle_dir_source, resolve_bundle_key_raw_from_value, set_file_mode_600,
    };
    use crate::core::state::AccountType;
    use base64::Engine;

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
                account_type: AccountType::Subscription,
                email: "a@example.com".into(),
                account_id: Some("acct-remote-1".into()),
                plan: Some("Plus".into()),
                api_provider: None,
                api_base_url: None,
                api_token_label: None,
                added_at: 1,
                updated_at: 2,
                auth_json: "{\"tokens\":{}}".into(),
                config_toml: Some("model = \"gpt-5\"".into()),
            }],
        };
        let plaintext = serde_json::to_vec(&bundle)?;
        let _key = derive_bundle_key("test-secret");

        let encrypted = encrypt_bundle_bytes(&plaintext, "test-secret")?;
        // 正确密钥可以解密
        assert_eq!(decrypt_bundle_bytes(&encrypted, "test-secret")?, plaintext);
        // 错误密钥解密失败（v2 路径下改变了密钥，AEAD 认证必然失败）
        assert!(decrypt_bundle_bytes(&encrypted, "wrong-secret").is_err());
        Ok(())
    }

    #[test]
    fn resolve_bundle_key_requires_env_var() {
        assert!(resolve_bundle_key_raw_from_value(None).is_err());
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
            accounts: vec![
                RepoBundleAccount {
                    id: "acct-1".into(),
                    account_type: AccountType::Subscription,
                    email: "pool@example.com".into(),
                    account_id: Some("acct-remote-1".into()),
                    plan: Some("Plus".into()),
                    api_provider: None,
                    api_base_url: None,
                    api_token_label: None,
                    added_at: 10,
                    updated_at: 20,
                    auth_json: "{\"tokens\":{\"access_token\":\"x\"}}".into(),
                    config_toml: Some("model = \"gpt-5\"".into()),
                },
                RepoBundleAccount {
                    id: "api-1".into(),
                    account_type: AccountType::Api,
                    email: "56wxyz@openrouter".into(),
                    account_id: None,
                    plan: None,
                    api_provider: Some("openrouter".into()),
                    api_base_url: Some("https://example.com/v1".into()),
                    api_token_label: Some("sk-abcd-wxyz".into()),
                    added_at: 11,
                    updated_at: 21,
                    auth_json: "{\"OPENAI_API_KEY\":\"sk-secret\"}".into(),
                    config_toml: Some("# scodex-managed-api-config\n".into()),
                },
            ],
        };

        let state = overwrite_local_account_pool(&state_dir, &bundle)?;

        assert_eq!(state.accounts.len(), 2);
        assert_eq!(state.accounts[0].id, "acct-1");
        assert_eq!(state.accounts[1].account_type, AccountType::Api);
        assert_eq!(
            state.accounts[1].api_provider.as_deref(),
            Some("openrouter")
        );
        assert_eq!(
            state.accounts[1].api_base_url.as_deref(),
            Some("https://example.com/v1")
        );
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
        assert!(
            state_dir
                .join("accounts")
                .join("api-1")
                .join("config.toml")
                .exists()
        );

        fs::remove_dir_all(&state_dir)?;
        Ok(())
    }

    // ── 新增测试 ──────────────────────────────────────────────────────────────

    /// PBKDF2 v2 加解密往返：加密后能用同一密钥解密，错误密钥解密失败
    #[test]
    fn pbkdf2_v2_round_trip() -> Result<()> {
        let plaintext = b"hello pbkdf2 world";
        let secret = "super-secret-passphrase";

        let encrypted = encrypt_bundle_bytes(plaintext, secret)?;
        // kdf 字段必须存在且 version=2
        let kdf = encrypted.kdf.as_ref().expect("kdf field must be present");
        assert_eq!(kdf.version, 2);
        assert_eq!(kdf.iterations, 100_000);
        assert!(!kdf.salt_b64.is_empty());

        // 正确密钥往返
        let decrypted = decrypt_bundle_bytes(&encrypted, secret)?;
        assert_eq!(decrypted, plaintext);

        // 错误密钥应解密失败
        assert!(decrypt_bundle_bytes(&encrypted, "wrong-pass").is_err());
        Ok(())
    }

    /// v1 旧 bundle 兼容：用 SHA-256 直接派生的 bundle 新代码仍能解密
    #[test]
    fn v1_legacy_bundle_still_decryptable() -> Result<()> {
        use chacha20poly1305::aead::Aead;
        use chacha20poly1305::aead::{KeyInit, OsRng as AeadOsRng, rand_core::RngCore};
        use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

        let secret = "legacy-secret";
        let plaintext = b"legacy bundle content";

        // 手工用旧 v1 SHA-256 路径构造 bundle（无 kdf 字段）
        let key = derive_bundle_key_v1(secret);
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
        let mut nonce_bytes = [0u8; 24];
        AeadOsRng.fill_bytes(&mut nonce_bytes);
        let ciphertext = cipher
            .encrypt(XNonce::from_slice(&nonce_bytes), plaintext.as_ref())
            .unwrap();

        let legacy_bundle = EncryptedBundleFile {
            version: 1,
            algorithm: BUNDLE_ALGORITHM.into(),
            nonce_b64: BASE64_STANDARD.encode(nonce_bytes),
            ciphertext_b64: BASE64_STANDARD.encode(&ciphertext),
            kdf: None, // 无 kdf 字段 → v1 兼容路径
        };

        let decrypted = decrypt_bundle_bytes(&legacy_bundle, secret)?;
        assert_eq!(decrypted, plaintext);
        Ok(())
    }

    /// Unix-only：写出文件后 chmod 0600 生效
    #[cfg(unix)]
    #[test]
    fn chmod_0600_is_set_on_sensitive_file() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("scodex-chmod-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir)?;
        let path = dir.join("auth.json");
        fs::write(&path, b"{\"tokens\":{}}")?;

        set_file_mode_600(&path)?;

        let mode = fs::metadata(&path)?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {:o}", mode);

        fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
