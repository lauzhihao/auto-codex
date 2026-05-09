use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use uuid::Uuid;

use self::auth::decode_identity;
use self::paths::{codex_home, codex_install_command, find_codex_bin, find_runtime_program};
use crate::core::policy::{
    choose_best_account, choose_current_account, choose_current_api_account,
};
use crate::core::state::{AccountRecord, LiveIdentity, State, UsageSnapshot};
use crate::core::ui as core_ui;

mod account;
mod auth;
mod deploy;
mod device_autofill;
mod paths;
mod repo_sync;
mod ui;
mod usage;

pub use device_autofill::AutofillRequest;

// RAII guard：无论成功失败都清理临时目录
struct TmpDirGuard(PathBuf);

impl Drop for TmpDirGuard {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.0) {
            if self.0.exists() {
                eprintln!(
                    "warning: failed to clean tmp login home {}: {e}",
                    self.0.display()
                );
            }
        }
    }
}

/// 在 state_dir/.tmp 下创建带 uuid 的临时目录，调用 f，结束后（成功或失败）自动清理。
fn with_tmp_login_home<R>(state_dir: &Path, f: impl FnOnce(&Path) -> Result<R>) -> Result<R> {
    let temp_root = state_dir.join(".tmp");
    fs::create_dir_all(&temp_root)
        .with_context(|| format!("failed to create {}", temp_root.display()))?;
    let tmp_home = temp_root.join(format!("scodex-login-{}", Uuid::new_v4()));
    fs::create_dir_all(&tmp_home)
        .with_context(|| format!("failed to create {}", tmp_home.display()))?;
    let _guard = TmpDirGuard(tmp_home.clone());
    f(&tmp_home)
}

#[derive(Debug, Clone)]
pub struct ApiLoginRequest {
    pub api_token: String,
    pub base_url: String,
    pub provider: String,
}

#[derive(Debug, Default)]
pub struct CodexAdapter;

impl CodexAdapter {
    pub fn read_live_identity(&self) -> Option<LiveIdentity> {
        let home = codex_home();
        if let Some(account_id) = account::read_managed_config_account_id(&home) {
            return Some(LiveIdentity {
                email: String::new(),
                account_id: None,
                scodex_account_id: Some(account_id),
            });
        }

        let auth_path = home.join("auth.json");
        let auth = self.read_auth_json(&auth_path).ok()?;
        decode_identity(&auth).ok().map(Into::into)
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

        let live_identity = self.read_live_identity();
        if let Some(current) = choose_current_api_account(state, live_identity.as_ref()).cloned() {
            let usage = UsageSnapshot::default();
            if perform_switch {
                self.switch_account(&current)?;
            }
            return Ok(Some((current, usage)));
        }

        self.refresh_all_accounts(state);
        if let Some(current) = choose_current_account(state, live_identity.as_ref()).cloned() {
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

    pub fn run_device_auth_login(
        &self,
        state_dir: &Path,
        state: &mut State,
    ) -> Result<AccountRecord> {
        let ui = core_ui::messages();
        let codex_bin = self.resolve_codex_bin()?;

        println!("{}", ui.login_start());
        println!("{}", ui.login_open_url());
        println!("{}", ui.login_headless_ip(&detect_local_ip()));
        println!();

        with_tmp_login_home(state_dir, |tmp_home| {
            let status = Command::new(&codex_bin)
                .arg("login")
                .arg("--device-auth")
                .env("CODEX_HOME", tmp_home)
                .status()
                .with_context(|| format!("failed to execute {}", codex_bin.display()))?;
            if !status.success() {
                bail!("{}", ui.codex_login_failed(status.code().unwrap_or(1)));
            }

            let auth_path = tmp_home.join("auth.json");
            if !auth_path.exists() {
                bail!("{}", ui.login_missing_auth());
            }

            self.import_auth_path(state_dir, state, tmp_home)
        })
    }

    pub fn run_device_auth_login_autofill(
        &self,
        state_dir: &Path,
        state: &mut State,
        request: AutofillRequest,
    ) -> Result<AccountRecord> {
        let ui = core_ui::messages();
        let codex_bin = self.resolve_codex_bin()?;

        println!("{}", ui.login_autofill_start());

        with_tmp_login_home(state_dir, |tmp_home| {
            device_autofill::run_device_autofill_login(&codex_bin, tmp_home, &request)?;

            let auth_path = tmp_home.join("auth.json");
            if !auth_path.exists() {
                bail!("{}", ui.login_missing_auth());
            }

            self.import_auth_path(state_dir, state, tmp_home)
        })
    }

    pub fn run_api_key_login(
        &self,
        state_dir: &Path,
        state: &mut State,
        request: ApiLoginRequest,
    ) -> Result<AccountRecord> {
        let temp_root = state_dir.join(".tmp");
        fs::create_dir_all(&temp_root)
            .with_context(|| format!("failed to create {}", temp_root.display()))?;
        let tmp_home = temp_root.join(format!("scodex-login-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp_home)
            .with_context(|| format!("failed to create {}", tmp_home.display()))?;
        let auth_path = tmp_home.join("auth.json");
        fs::write(
            &auth_path,
            serde_json::json!({
                "OPENAI_API_KEY": &request.api_token,
            })
            .to_string(),
        )
        .with_context(|| format!("failed to write {}", auth_path.display()))?;

        let record = self.import_api_auth_path(state_dir, state, &tmp_home, &request)?;
        let _ = fs::remove_dir_all(&tmp_home);
        Ok(record)
    }

    pub fn launch_codex(&self, extra_args: &[std::ffi::OsString], resume: bool) -> Result<i32> {
        let ui = core_ui::messages();
        let codex_bin = self.resolve_codex_bin()?;
        let fresh_cmd = build_codex_launch_command(&codex_bin, extra_args, false);
        if resume
            && self.has_resumable_session(
                &env::current_dir().context("failed to read current directory")?,
            )
        {
            let resume_cmd = build_codex_launch_command(&codex_bin, extra_args, true);
            println!("{}", ui.resume_session());
            let status = Command::new(&resume_cmd[0])
                .args(&resume_cmd[1..])
                .status()
                .context("failed to execute codex resume")?;
            if status.success() {
                return Ok(status.code().unwrap_or(0));
            }
            eprintln!("{}", ui.resume_fallback());
        } else {
            println!("{}", ui.fresh_session());
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
        if let Some(path) = find_codex_bin() {
            return Ok(path);
        }

        self.offer_to_install_codex()?;
        find_codex_bin()
            .ok_or_else(|| anyhow::anyhow!(core_ui::messages().codex_install_still_missing()))
    }

    fn offer_to_install_codex(&self) -> Result<()> {
        let install = codex_install_command();
        let install_line = install.display();
        let ui = core_ui::messages();

        eprintln!("{}", ui.missing_codex());
        eprintln!("{}", ui.install_hint());
        eprintln!();
        eprintln!("{install_line}");
        eprintln!();

        let Some(installer_bin) = find_runtime_program(&install.program) else {
            eprintln!("{}", ui.codex_install_tool_missing(&install.program));
            eprintln!();
            eprintln!("{}", ui.manual_install());
            eprintln!();
            eprintln!("{install_line}");
            std::process::exit(1);
        };

        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            eprintln!("{}", ui.manual_install());
            std::process::exit(1);
        }

        loop {
            print!("{}", ui.confirm_install());
            io::stdout().flush().context("failed to flush stdout")?;

            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .context("failed to read confirmation input")?;

            match parse_yes_no(&answer) {
                Some(true) => {
                    let status = Command::new(&installer_bin)
                        .args(&install.args)
                        .status()
                        .with_context(|| format!("failed to execute `{install_line}`"))?;
                    if !status.success() {
                        bail!("{}", ui.codex_install_failed(status.code().unwrap_or(1)));
                    }
                    return Ok(());
                }
                Some(false) => {
                    eprintln!("{}", ui.manual_install());
                    eprintln!();
                    eprintln!("{install_line}");
                    std::process::exit(1);
                }
                None => {
                    eprintln!("{}", ui.invalid_yes_no());
                }
            }
        }
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
}

pub(crate) fn parse_yes_no(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
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
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        if reader.read_line(&mut first_line).is_err() || first_line.is_empty() {
            continue;
        }
        let first_line = first_line.trim_end_matches('\n').trim_end_matches('\r');
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
        .expect("system clock before UNIX_EPOCH")
        .as_secs() as i64
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use anyhow::Result;
    use uuid::Uuid;

    use std::ffi::OsString;

    use super::{
        ApiLoginRequest, CodexAdapter, build_codex_launch_command, has_resumable_session_under,
        parse_yes_no, with_tmp_login_home,
    };
    use crate::core::state::{AccountType, State};

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

    #[test]
    fn parse_yes_no_accepts_expected_values_case_insensitively() {
        assert_eq!(parse_yes_no("Y"), Some(true));
        assert_eq!(parse_yes_no("yes"), Some(true));
        assert_eq!(parse_yes_no("N"), Some(false));
        assert_eq!(parse_yes_no("No"), Some(false));
        assert_eq!(parse_yes_no("maybe"), None);
    }

    #[test]
    fn api_login_writes_auth_json_from_cli_token() -> Result<()> {
        let tmp = std::env::temp_dir().join(format!("scodex-api-login-{}", Uuid::new_v4()));
        let state_dir = tmp.join("state");
        let mut state = State::default();

        let record = CodexAdapter.run_api_key_login(
            &state_dir,
            &mut state,
            ApiLoginRequest {
                api_token: "sk-abcdef123456wxyz".into(),
                base_url: "https://example.com/v1".into(),
                provider: "openrouter".into(),
            },
        )?;

        let auth_contents = fs::read_to_string(&record.auth_path)?;
        assert_eq!(
            auth_contents,
            "{\"OPENAI_API_KEY\":\"sk-abcdef123456wxyz\"}"
        );
        assert_eq!(record.account_type, AccountType::Api);
        assert_eq!(record.api_provider.as_deref(), Some("openrouter"));
        assert_eq!(state.accounts.len(), 1);
        fs::remove_dir_all(&tmp)?;
        Ok(())
    }

    // 验证大文件场景下 has_resumable_session_under 仅读首行即返回，不全量加载
    #[test]
    fn has_resumable_session_under_large_file_returns_correct_result() -> Result<()> {
        let tmp = std::env::temp_dir().join(format!("scodex-large-session-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp)?;

        let cwd = "/fake/project/path";
        let session_file = tmp.join("session.jsonl");

        // 首行：有效的 session_meta
        let first_line = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "originator": "codex-tui",
                "cwd": cwd,
            }
        })
        .to_string();

        // 构造 10000 行的大文件
        let mut content = first_line + "\n";
        let padding_line =
            serde_json::json!({"type": "message", "data": "x".repeat(200)}).to_string();
        for _ in 0..9999 {
            content.push_str(&padding_line);
            content.push('\n');
        }
        fs::write(&session_file, &content)?;

        // 函数必须正确识别 cwd，且不因大文件崩溃/超时
        assert!(has_resumable_session_under(&tmp, cwd));
        // 不匹配的 cwd 返回 false
        assert!(!has_resumable_session_under(&tmp, "/other/path"));

        fs::remove_dir_all(&tmp)?;
        Ok(())
    }

    // 验证 with_tmp_login_home：闭包成功时 tmp_home 已被清理
    #[test]
    fn with_tmp_login_home_cleans_up_on_success() -> Result<()> {
        let state_dir = std::env::temp_dir().join(format!("scodex-wth-ok-{}", Uuid::new_v4()));
        let mut captured_tmp = std::path::PathBuf::new();

        let result: Result<i32> = with_tmp_login_home(&state_dir, |tmp_home| {
            captured_tmp = tmp_home.to_path_buf();
            // 确认目录在闭包内存在
            assert!(tmp_home.exists(), "tmp_home should exist inside closure");
            Ok(42)
        });

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        // guard drop 后目录必须不存在
        assert!(
            !captured_tmp.exists(),
            "tmp_home should be cleaned up after success"
        );

        let _ = fs::remove_dir_all(&state_dir);
        Ok(())
    }

    // 验证 with_tmp_login_home：闭包返回 Err 时 tmp_home 也被清理
    #[test]
    fn with_tmp_login_home_cleans_up_on_error() -> Result<()> {
        let state_dir = std::env::temp_dir().join(format!("scodex-wth-err-{}", Uuid::new_v4()));
        let mut captured_tmp = std::path::PathBuf::new();

        let result: Result<i32> = with_tmp_login_home(&state_dir, |tmp_home| {
            captured_tmp = tmp_home.to_path_buf();
            assert!(tmp_home.exists(), "tmp_home should exist inside closure");
            anyhow::bail!("simulated login failure");
        });

        assert!(result.is_err());
        // 即使闭包出错，guard drop 后目录必须不存在
        assert!(
            !captured_tmp.exists(),
            "tmp_home should be cleaned up after error"
        );

        let _ = fs::remove_dir_all(&state_dir);
        Ok(())
    }
}
