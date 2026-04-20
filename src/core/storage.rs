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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::default_state_dir_for_home;

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
}
