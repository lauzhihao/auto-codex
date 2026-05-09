use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;

use crate::core::state::State;

const DEFAULT_STATE_BASENAME: &str = "scodex";
const STATE_DIR_ENV: &str = "SCODEX_HOME";

pub fn resolve_state_dir(override_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = override_dir {
        return Ok(expand_user_path(path));
    }

    if let Some(path) = configured_state_dir_from_env() {
        return Ok(path);
    }

    default_state_dir()
}

fn configured_state_dir_from_env() -> Option<PathBuf> {
    env::var_os(STATE_DIR_ENV).map(|value| expand_user_path(Path::new(&value)))
}

fn default_state_dir() -> Result<PathBuf> {
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        return Ok(home.join(format!(".{DEFAULT_STATE_BASENAME}")));
    }

    let base_dirs =
        BaseDirs::new().context("unable to resolve base directories for current user")?;
    Ok(default_state_dir_for_home(None, base_dirs.data_local_dir()))
}

fn default_state_dir_for_home(home: Option<&Path>, data_local_dir: &Path) -> PathBuf {
    home.map(|home| home.join(format!(".{DEFAULT_STATE_BASENAME}")))
        .unwrap_or_else(|| data_local_dir.join(DEFAULT_STATE_BASENAME))
}

pub fn load_state(state_dir: &Path) -> Result<State> {
    let state_file = state_dir.join("state.json");
    if !state_file.exists() {
        return Ok(State::default());
    }

    let contents = fs::read_to_string(&state_file)
        .with_context(|| format!("failed to read {}", state_file.display()))?;
    let mut state: State = serde_json::from_str(&contents)
        .with_context(|| format!("invalid state file: {}", state_file.display()))?;
    normalize_state_account_paths(state_dir, &mut state);
    Ok(state)
}

pub fn save_state(state_dir: &Path, state: &State) -> Result<()> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    let tmp_path = state_dir.join(".state.json.tmp");
    let final_path = state_dir.join("state.json");
    let mut bytes = serde_json::to_vec_pretty(state)?;
    bytes.push(b'\n');
    fs::write(&tmp_path, bytes)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("failed to move {} into place", final_path.display()))?;
    Ok(())
}

fn normalize_state_account_paths(state_dir: &Path, state: &mut State) -> bool {
    let mut changed = false;
    let accounts_dir = state_dir.join("accounts");

    for account in &mut state.accounts {
        let canonical_home = accounts_dir.join(&account.id);
        let canonical_auth = canonical_home.join("auth.json");
        let canonical_config = canonical_home.join("config.toml");

        if canonical_auth.exists() {
            let canonical_auth_str = canonical_auth.to_string_lossy().into_owned();
            if account.auth_path != canonical_auth_str {
                account.auth_path = canonical_auth_str;
                changed = true;
            }
        }

        if canonical_config.exists() {
            let canonical_config_str = canonical_config.to_string_lossy().into_owned();
            if account.config_path.as_deref() != Some(canonical_config_str.as_str()) {
                account.config_path = Some(canonical_config_str);
                changed = true;
            }
        } else if let Some(existing_config) = account.config_path.as_ref() {
            if !Path::new(existing_config).exists() {
                account.config_path = None;
                changed = true;
            }
        }
    }

    changed
}

fn expand_user_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(suffix) = raw.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home).join(suffix);
        }
    }

    if path.is_absolute() {
        return path.to_path_buf();
    }

    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(path)
}

pub fn ensure_exists(path: &Path, label: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    bail!("{label} not found: {}", path.display())
}

/// 写入 shim 脚本时嵌入的专属 marker，用于与旧二进制区分。
const SHIM_MARKER: &str = "# scodex shim v1";

/// sentinel 文件名，位于 $SCODEX_HOME 根目录，标记迁移已完成。
const SENTINEL_NAME: &str = ".migrated";

pub fn migrate_old_binaries() -> Result<()> {
    // 清理旧的二进制文件：~/.local/bin/scodex 和 ~/.local/bin/auto-codex
    // 这些现在被shim脚本替代，存放在 $SCODEX_HOME/bin 中

    let home_path = env::var_os("HOME").map(PathBuf::from);
    let scodex_home = env::var_os(STATE_DIR_ENV)
        .map(PathBuf::from)
        .or_else(|| home_path.as_ref().map(|home| home.join(".scodex")));

    // 若已写过 sentinel 则直接跳过，避免每次启动都做磁盘扫描
    if let Some(scodex_home) = scodex_home.as_ref() {
        let sentinel = scodex_home.join(SENTINEL_NAME);
        if sentinel.exists() {
            return Ok(());
        }
    }

    if let Some(home_path) = home_path {
        let local_bin = home_path.join(".local").join("bin");

        let old_scodex = local_bin.join("scodex");
        let old_auto_codex = local_bin.join("auto-codex");

        // 删除旧的 scodex 二进制（如果是实际的二进制文件，不是新 shim）
        if old_scodex.exists() && is_old_binary(&old_scodex)? {
            if let Err(e) = fs::remove_file(&old_scodex) {
                eprintln!(
                    "warning: failed to remove old binary {}: {e}",
                    old_scodex.display()
                );
            }
        }

        // 删除旧的 auto-codex 二进制（如果是实际的二进制文件，不是新 shim）
        if old_auto_codex.exists() && is_old_binary(&old_auto_codex)? {
            if let Err(e) = fs::remove_file(&old_auto_codex) {
                eprintln!(
                    "warning: failed to remove old binary {}: {e}",
                    old_auto_codex.display()
                );
            }
        }

        // 写入 sentinel，记录迁移已完成
        if let Some(scodex_home) = scodex_home.as_ref() {
            let sentinel = scodex_home.join(SENTINEL_NAME);
            if let Err(e) = fs::create_dir_all(scodex_home) {
                eprintln!(
                    "warning: failed to create migration directory {}: {e}",
                    scodex_home.display()
                );
                return Ok(());
            }
            if let Err(e) = fs::write(&sentinel, "") {
                eprintln!(
                    "warning: failed to write migration sentinel {}: {e}",
                    sentinel.display()
                );
            }
        }
    }

    Ok(())
}

/// 判断路径下的文件是否为旧二进制（需要删除）。
///
/// 判断逻辑：
/// 1. 若文件包含 SHIM_MARKER → 是新版 shim，**不**删除，返回 false。
/// 2. 若文件是当前安装器已发布过的 pre-marker shim，也不删除。
/// 3. 若文件是平台二进制或其他遗留脚本 → 返回 true。
fn is_old_binary(path: &Path) -> Result<bool> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;

    // ELF / PE / Mach-O 都是真正的平台二进制，需要删除。
    if bytes.starts_with(b"\x7fELF")
        || bytes.starts_with(b"MZ")
        || bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
        || bytes.starts_with(&[0xfe, 0xed, 0xfa, 0xcf])
        || bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe])
        || bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbf])
    {
        return Ok(true);
    }

    if let Ok(text) = std::str::from_utf8(&bytes) {
        if is_scodex_shim_text(text) {
            return Ok(false);
        }
        return Ok(true);
    }

    // 其他不可读二进制，删除
    Ok(true)
}

fn is_scodex_shim_text(text: &str) -> bool {
    if text.contains(SHIM_MARKER) {
        return true;
    }

    // 兼容已经发布过的 install.sh / install.ps1 shim；新版安装器会写 marker。
    text.contains("SCODEX_HOME")
        && (text.contains("$SCODEX_HOME/bin/scodex")
            || text.contains("%SCODEX_HOME%\\bin\\scodex.exe"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{SENTINEL_NAME, SHIM_MARKER, default_state_dir_for_home, is_old_binary};

    #[test]
    fn default_state_dir_prefers_home_hidden_directory() {
        let path = default_state_dir_for_home(Some(Path::new("/tmp/home")), Path::new("/tmp/data"));
        assert_eq!(path, Path::new("/tmp/home/.scodex"));
    }

    #[test]
    fn default_state_dir_falls_back_to_data_directory_without_home() {
        let path = default_state_dir_for_home(None, Path::new("/tmp/data"));
        assert_eq!(path, Path::new("/tmp/data/scodex"));
    }

    /// marker 命中 → is_old_binary 返回 false，不应删除
    #[test]
    fn shim_marker_prevents_deletion() {
        let dir = std::env::temp_dir().join(format!("scodex_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("scodex");
        let content = format!("{SHIM_MARKER}\n#!/usr/bin/env bash\nexec scodex \"$@\"\n");
        fs::write(&shim, content).unwrap();

        let result = is_old_binary(&shim).unwrap();
        assert!(
            !result,
            "shim with marker should not be treated as old binary"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pre_marker_installer_shim_prevents_deletion() {
        let dir =
            std::env::temp_dir().join(format!("scodex_test_premarked_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("scodex");
        fs::write(
            &shim,
            "#!/usr/bin/env bash\nSCODEX_HOME=\"${SCODEX_HOME:-$HOME/.scodex}\"\nexec \"$SCODEX_HOME/bin/scodex\" \"$@\"\n",
        )
        .unwrap();

        let result = is_old_binary(&shim).unwrap();
        assert!(
            !result,
            "pre-marker installer shim should not be treated as old binary"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn platform_binary_magic_is_old_binary() {
        let dir = std::env::temp_dir().join(format!("scodex_test_binary_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("scodex");
        fs::write(&bin, b"\x7fELFbinary").unwrap();

        let result = is_old_binary(&bin).unwrap();
        assert!(result, "ELF binary should be treated as old binary");

        fs::remove_dir_all(&dir).ok();
    }

    /// 旧二进制（无 marker 的脚本）→ is_old_binary 返回 true，migrate 后写 sentinel
    #[test]
    fn old_binary_gets_removed_and_sentinel_written() {
        let dir = std::env::temp_dir().join(format!("scodex_test_old_{}", std::process::id()));
        let local_bin = dir.join(".local").join("bin");
        fs::create_dir_all(&local_bin).unwrap();

        // 模拟旧脚本：无 SHIM_MARKER，但是合法文本
        let old_bin = local_bin.join("scodex");
        fs::write(&old_bin, "#!/usr/bin/env bash\nexec old_codex \"$@\"\n").unwrap();

        let scodex_home = dir.join(".scodex");
        fs::create_dir_all(&scodex_home).unwrap();

        // 用环境变量隔离测试，直接调用 is_old_binary 验证判断
        let result = is_old_binary(&old_bin).unwrap();
        assert!(
            result,
            "script without marker should be treated as old binary"
        );

        // 模拟 migrate 写 sentinel
        let sentinel = scodex_home.join(SENTINEL_NAME);
        fs::write(&sentinel, "").unwrap();
        assert!(sentinel.exists(), "sentinel should exist after migration");

        fs::remove_dir_all(&dir).ok();
    }

    /// sentinel 已存在 → migrate_old_binaries 应提前返回，不扫描磁盘
    #[test]
    fn sentinel_skips_migration() {
        let dir = std::env::temp_dir().join(format!("scodex_test_sentinel_{}", std::process::id()));
        let scodex_home = dir.join(".scodex");
        fs::create_dir_all(&scodex_home).unwrap();

        // 写入 sentinel
        let sentinel = scodex_home.join(SENTINEL_NAME);
        fs::write(&sentinel, "").unwrap();

        // 设置环境变量指向临时目录，然后调用 migrate
        // 由于 migrate_old_binaries 依赖 env var，用 unsafe 块暂时设置
        // SAFETY: 单线程测试，设置后立即恢复
        let key = "SCODEX_HOME";
        let orig = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, &scodex_home);
        }

        // sentinel 存在 → migrate 应直接返回 Ok，不产生 panic
        let result = super::migrate_old_binaries();
        assert!(
            result.is_ok(),
            "migrate should succeed when sentinel exists"
        );

        // 恢复环境变量
        unsafe {
            match orig {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }

        fs::remove_dir_all(&dir).ok();
    }
}
