use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use uuid::Uuid;

use self::auth::decode_identity;
use self::paths::{codex_home, codex_install_command, find_codex_bin, find_in_path};
use crate::adapters::{AdapterCapabilities, CliAdapter};
use crate::core::policy::{choose_best_account, choose_current_account};
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
    pub fn add_account_via_browser(
        &self,
        state_dir: &Path,
        state: &mut State,
    ) -> Result<AccountRecord> {
        const SIGNUP_URL: &str = "https://auth.openai.com/create-account";
        let ui = core_ui::messages();

        println!("{}", ui.add_opening_signup());
        match try_open_signup_page(SIGNUP_URL) {
            Ok(BrowserOpenOutcome::Opened) => println!("{}", ui.add_opened_signup(SIGNUP_URL)),
            Ok(BrowserOpenOutcome::NoGui) => {
                println!("{}", ui.add_no_gui_open_manually(SIGNUP_URL))
            }
            Ok(BrowserOpenOutcome::Failed) | Err(_) => {
                println!("{}", ui.add_browser_open_failed(SIGNUP_URL))
            }
        }
        self.wait_for_enter_after_signup()?;
        self.run_device_auth_login(state_dir, state)
    }

    pub fn read_live_identity(&self) -> Option<LiveIdentity> {
        let auth_path = codex_home().join("auth.json");
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

    pub fn run_device_auth_login(
        &self,
        state_dir: &Path,
        state: &mut State,
    ) -> Result<AccountRecord> {
        let ui = core_ui::messages();
        let codex_bin = self.resolve_codex_bin()?;
        let temp_root = state_dir.join(".tmp");
        fs::create_dir_all(&temp_root)
            .with_context(|| format!("failed to create {}", temp_root.display()))?;
        let tmp_home = temp_root.join(format!("scodex-login-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp_home)
            .with_context(|| format!("failed to create {}", tmp_home.display()))?;

        println!("{}", ui.login_start());
        println!("{}", ui.login_open_url());
        println!("{}", ui.login_headless_ip(&detect_local_ip()));
        println!();

        let status = Command::new(&codex_bin)
            .arg("login")
            .arg("--device-auth")
            .env("CODEX_HOME", &tmp_home)
            .status()
            .with_context(|| format!("failed to execute {}", codex_bin.display()))?;
        if !status.success() {
            let _ = fs::remove_dir_all(&tmp_home);
            bail!("{}", ui.codex_login_failed(status.code().unwrap_or(1)));
        }

        let auth_path = tmp_home.join("auth.json");
        if !auth_path.exists() {
            let _ = fs::remove_dir_all(&tmp_home);
            bail!("{}", ui.login_missing_auth());
        }

        let record = self.import_auth_path(state_dir, state, &tmp_home)?;
        let _ = fs::remove_dir_all(&tmp_home);
        Ok(record)
    }

    pub fn run_device_auth_login_autofill(
        &self,
        state_dir: &Path,
        state: &mut State,
        request: AutofillRequest,
    ) -> Result<AccountRecord> {
        let ui = core_ui::messages();
        let codex_bin = self.resolve_codex_bin()?;
        let temp_root = state_dir.join(".tmp");
        fs::create_dir_all(&temp_root)
            .with_context(|| format!("failed to create {}", temp_root.display()))?;
        let tmp_home = temp_root.join(format!("scodex-login-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp_home)
            .with_context(|| format!("failed to create {}", tmp_home.display()))?;

        println!("{}", ui.login_autofill_start());

        let run = device_autofill::run_device_autofill_login(&codex_bin, &tmp_home, &request);
        if let Err(error) = run {
            let _ = fs::remove_dir_all(&tmp_home);
            return Err(error);
        }

        let auth_path = tmp_home.join("auth.json");
        if !auth_path.exists() {
            let _ = fs::remove_dir_all(&tmp_home);
            bail!("{}", ui.login_missing_auth());
        }

        let record = self.import_auth_path(state_dir, state, &tmp_home)?;
        let _ = fs::remove_dir_all(&tmp_home);
        Ok(record)
    }

    fn wait_for_enter_after_signup(&self) -> Result<()> {
        let ui = core_ui::messages();
        println!("{}", ui.add_finish_signup_then_continue());
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Ok(());
        }
        print!("{}", ui.add_waiting_enter());
        io::stdout().flush().context("failed to flush stdout")?;
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("failed to read continuation input")?;
        Ok(())
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

        let Some(installer_bin) = find_in_path(&install.program) else {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserOpenOutcome {
    Opened,
    NoGui,
    Failed,
}

fn try_open_signup_page(url: &str) -> Result<BrowserOpenOutcome> {
    if requires_gui_hint() && !has_gui_environment() {
        return Ok(BrowserOpenOutcome::NoGui);
    }

    let Some((program, args)) = browser_open_command(url) else {
        return Ok(BrowserOpenOutcome::NoGui);
    };

    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to open browser for {url}"))?;
    if status.success() {
        Ok(BrowserOpenOutcome::Opened)
    } else {
        Ok(BrowserOpenOutcome::Failed)
    }
}

fn requires_gui_hint() -> bool {
    !(cfg!(target_os = "windows") || cfg!(target_os = "macos"))
}

fn has_gui_environment() -> bool {
    if cfg!(target_os = "windows") || cfg!(target_os = "macos") {
        return true;
    }

    env::var_os("DISPLAY").is_some()
        || env::var_os("WAYLAND_DISPLAY").is_some()
        || env::var_os("MIR_SOCKET").is_some()
}

fn browser_open_command(url: &str) -> Option<(&'static str, Vec<String>)> {
    if cfg!(target_os = "macos") {
        return Some(("open", vec![url.to_string()]));
    }
    if cfg!(target_os = "windows") {
        return Some((
            "cmd",
            vec!["/C".into(), "start".into(), "".into(), url.to_string()],
        ));
    }

    if find_in_path("xdg-open").is_some() {
        Some(("xdg-open", vec![url.to_string()]))
    } else if find_in_path("gio").is_some() {
        Some(("gio", vec!["open".into(), url.to_string()]))
    } else {
        None
    }
}

fn parse_yes_no(value: &str) -> Option<bool> {
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

    use super::{build_codex_launch_command, has_resumable_session_under, parse_yes_no};

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
}
