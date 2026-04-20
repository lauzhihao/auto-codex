use std::env;
use std::path::Path;

use anyhow::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiLanguage {
    En,
    ZhHans,
}

#[derive(Debug, Clone, Copy)]
pub struct Messages {
    language: UiLanguage,
}

pub fn messages() -> Messages {
    Messages {
        language: detect_ui_language(),
    }
}

pub fn detect_ui_language() -> UiLanguage {
    let locale = env::var("LC_ALL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env::var("LC_MESSAGES")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| {
            env::var("LANG")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });

    locale
        .as_deref()
        .and_then(parse_ui_language_from_locale)
        .unwrap_or(UiLanguage::En)
}

pub fn parse_ui_language_from_locale(locale: &str) -> Option<UiLanguage> {
    let normalized = locale.trim().to_ascii_lowercase();
    if !normalized.starts_with("zh") {
        return None;
    }
    if normalized.contains("utf-8") || normalized.contains("utf8") {
        Some(UiLanguage::ZhHans)
    } else {
        None
    }
}

pub fn format_top_level_error(error: &Error) -> String {
    let ui = messages();
    let prefix = if ui.is_zh() { "错误" } else { "Error" };
    let chain = error.chain().map(ToString::to_string).collect::<Vec<_>>();
    if chain.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {}", chain.join(": "))
    }
}

impl Messages {
    pub fn is_zh(&self) -> bool {
        matches!(self.language, UiLanguage::ZhHans)
    }

    pub fn cli_about(&self) -> &'static str {
        if self.is_zh() {
            "面向代理 CLI 的跨平台账号感知启动器。"
        } else {
            "Cross-platform account-aware launcher for agent CLIs."
        }
    }

    pub fn no_usable_account(&self) -> &'static str {
        if self.is_zh() {
            "没有找到可用账号。"
        } else {
            "No usable account found."
        }
    }

    pub fn no_usable_account_hint(&self) -> &'static str {
        if self.is_zh() {
            "没有可用账号，请先执行 `scodex add` 添加一个账号。"
        } else {
            "No usable accounts found. Run `scodex add` to add one first."
        }
    }

    pub fn no_importable_accounts(&self) -> &'static str {
        if self.is_zh() {
            "没有找到可导入的账号。"
        } else {
            "No importable accounts found."
        }
    }

    pub fn added_account(&self, email: &str) -> String {
        if self.is_zh() {
            format!("已添加 {email}")
        } else {
            format!("Added {email}")
        }
    }

    pub fn unknown_account(&self, email: &str) -> String {
        if self.is_zh() {
            format!("未知账号：{email}")
        } else {
            format!("Unknown account: {email}")
        }
    }

    pub fn confirm_rm(&self, email: &str) -> String {
        if self.is_zh() {
            format!("确认删除账号 {email}？此操作不可恢复 (Y/N)：")
        } else {
            format!("Remove account {email}? This cannot be undone (Y/N): ")
        }
    }

    pub fn rm_cancelled(&self) -> &'static str {
        if self.is_zh() {
            "已取消。"
        } else {
            "Cancelled."
        }
    }

    pub fn removed_account(&self, email: &str) -> String {
        if self.is_zh() {
            format!("已移除 {email}")
        } else {
            format!("Removed {email}")
        }
    }

    pub fn rm_requires_tty(&self) -> &'static str {
        if self.is_zh() {
            "当前输入不是终端；请加 -y 跳过确认。"
        } else {
            "Input is not a terminal; pass -y to skip confirmation."
        }
    }

    pub fn refreshed_accounts(&self, count: usize) -> String {
        if self.is_zh() {
            format!("已刷新 {count} 个账号。")
        } else {
            format!("Refreshed {count} account(s).")
        }
    }

    pub fn usable_account_summary(&self, count: usize) -> String {
        if self.is_zh() {
            format!("共有 {count} 个可用账号")
        } else {
            format!("{count} usable account(s)")
        }
    }

    pub fn update_already_current(&self, version: &str, path: &Path) -> String {
        if self.is_zh() {
            format!(
                "当前已是最新已安装版本（{version}），位置：{}",
                path.display()
            )
        } else {
            format!(
                "Already on the latest installed version ({version}) at {}",
                path.display()
            )
        }
    }

    pub fn update_completed(&self, previous: &str, installed: &str, path: &Path) -> String {
        if self.is_zh() {
            format!(
                "已将 scodex 从 {previous} 更新到 {installed}，位置：{}",
                path.display()
            )
        } else {
            format!(
                "Updated scodex from {previous} to {installed} at {}",
                path.display()
            )
        }
    }

    pub fn restart_terminal_hint(&self) -> &'static str {
        if self.is_zh() {
            "如果当前终端仍然解析到旧二进制，请重启终端。"
        } else {
            "Restart the current terminal if it still resolves the old binary."
        }
    }

    pub fn imported_account(&self, email: &str, id: &str) -> String {
        if self.is_zh() {
            format!("已导入 {email} -> {id}")
        } else {
            format!("Imported {email} -> {id}")
        }
    }

    pub fn selection_switched(&self) -> &'static str {
        if self.is_zh() {
            "已切换到"
        } else {
            "Switched to"
        }
    }

    pub fn selection_would_select(&self) -> &'static str {
        if self.is_zh() {
            "将会选择"
        } else {
            "Would select"
        }
    }

    pub fn na(&self) -> &'static str {
        if self.is_zh() { "无" } else { "N/A" }
    }

    pub fn unknown(&self) -> &'static str {
        if self.is_zh() { "未知" } else { "Unknown" }
    }

    pub fn table_headers(&self) -> [&'static str; 8] {
        if self.is_zh() {
            [
                "当前",
                "邮箱",
                "类型",
                "套餐",
                "5h",
                "Weekly",
                "重置时间",
                "状态",
            ]
        } else {
            [
                "Active", "Email", "Type", "Plan", "5h", "Weekly", "ResetOn", "Status",
            ]
        }
    }

    pub fn status_ok(&self) -> &'static str {
        if self.is_zh() { "正常" } else { "OK" }
    }

    pub fn status_error(&self) -> &'static str {
        if self.is_zh() { "错误" } else { "ERROR" }
    }

    pub fn status_relogin(&self) -> &'static str {
        if self.is_zh() { "需重登" } else { "RELOGIN" }
    }

    pub fn login_start(&self) -> &'static str {
        if self.is_zh() {
            "正在启动 `codex login --device-auth`。"
        } else {
            "Starting `codex login --device-auth`."
        }
    }

    pub fn login_open_url(&self) -> &'static str {
        if self.is_zh() {
            "请在任意可用浏览器的设备上打开上面输出的 URL 并完成登录。"
        } else {
            "Open the printed URL on any browser-enabled machine and finish the login there."
        }
    }

    pub fn login_headless_ip(&self, ip: &str) -> String {
        if self.is_zh() {
            format!("当前无头主机局域网 IP：{ip}")
        } else {
            format!("Headless host LAN IP: {ip}")
        }
    }

    pub fn resume_session(&self) -> &'static str {
        if self.is_zh() {
            "正在恢复当前目录的最新 Codex 会话。"
        } else {
            "Resuming latest Codex session for this directory."
        }
    }

    pub fn resume_fallback(&self) -> &'static str {
        if self.is_zh() {
            "恢复会话未能正常完成，正在回退到新会话。"
        } else {
            "Resume did not complete cleanly; falling back to a fresh Codex session."
        }
    }

    pub fn fresh_session(&self) -> &'static str {
        if self.is_zh() {
            "正在启动新的 Codex 会话。"
        } else {
            "Starting a fresh Codex session."
        }
    }

    pub fn missing_codex(&self) -> &'static str {
        if self.is_zh() {
            "未找到 codex。这会导致 scodex 无法正常工作。"
        } else {
            "codex not found. This will cause scodex to behave incorrectly."
        }
    }

    pub fn install_hint(&self) -> &'static str {
        if self.is_zh() {
            "你可以先运行下面的命令安装 Codex CLI："
        } else {
            "You can install Codex CLI by running:"
        }
    }

    pub fn manual_install(&self) -> &'static str {
        if self.is_zh() {
            "请先手动安装 Codex CLI，然后重新运行 scodex。"
        } else {
            "Please install Codex CLI manually and run scodex again."
        }
    }

    pub fn confirm_install(&self) -> &'static str {
        if self.is_zh() {
            "如果你希望我现在帮你安装，请确认（Y/N）："
        } else {
            "I can try to install it for you now. Continue? (Y/N): "
        }
    }

    pub fn invalid_yes_no(&self) -> &'static str {
        if self.is_zh() {
            "请输入 Y/YES/N/NO。"
        } else {
            "Please answer Y/YES/N/NO."
        }
    }

    pub fn codex_install_still_missing(&self) -> &'static str {
        if self.is_zh() {
            "Codex 安装似乎已完成，但当前仍然找不到 `codex`。请重启 shell，或显式设置 CODEX_BIN。"
        } else {
            "Codex installation completed, but `codex` is still not available. Restart the shell or set CODEX_BIN explicitly."
        }
    }

    pub fn codex_install_failed(&self, status: i32) -> String {
        if self.is_zh() {
            format!("Codex 安装失败，退出码：{status}")
        } else {
            format!("Codex installation failed with status {status}")
        }
    }

    pub fn codex_install_tool_missing(&self, tool: &str) -> String {
        if self.is_zh() {
            format!("未找到 {tool}。要自动安装 Codex CLI，当前机器需要先安装 Node.js/npm。")
        } else {
            format!(
                "{tool} not found. Install Node.js/npm first before trying to install Codex CLI automatically."
            )
        }
    }

    pub fn codex_login_failed(&self, status: i32) -> String {
        if self.is_zh() {
            format!("codex 登录失败，退出码：{status}")
        } else {
            format!("codex login failed with status {status}")
        }
    }

    pub fn login_missing_auth(&self) -> &'static str {
        if self.is_zh() {
            "登录流程已结束，但没有生成 auth.json。"
        } else {
            "Login finished but no auth.json was produced."
        }
    }

    pub fn login_autofill_start(&self) -> &'static str {
        if self.is_zh() {
            "正在启动 `codex login` 并打开受控 Chrome 完成 OAuth 自动填充。"
        } else {
            "Starting `codex login` and opening a controlled Chrome window for OAuth auto-fill."
        }
    }

    pub fn login_autofill_prompt(&self, url: &str, code: Option<&str>) -> String {
        match (self.is_zh(), code) {
            (true, Some(code)) => format!("设备授权链接：{url}\n一次性 code：{code}"),
            (true, None) => format!("设备授权链接：{url}"),
            (false, Some(code)) => format!("Device URL: {url}\nOne-time code: {code}"),
            (false, None) => format!("Device URL: {url}"),
        }
    }

    pub fn login_autofill_waiting_consent(&self) -> &'static str {
        if self.is_zh() {
            "OAuth 自动填充完成。请在刚打开的 Chrome 窗口里点一次 `Authorize` 完成登录。"
        } else {
            "OAuth auto-fill complete. Click `Authorize` once in the opened Chrome window to finish."
        }
    }

    pub fn login_autofill_no_chrome(&self) -> &'static str {
        if self.is_zh() {
            "未检测到 Chrome 或 Chromium，无法执行 OAuth 自动填充。请安装 Chrome，或改用 `scodex login`（不带 --oauth）。"
        } else {
            "Chrome or Chromium not detected; cannot run OAuth auto-fill. Install Chrome or run `scodex login` without --oauth."
        }
    }

    pub fn login_autofill_missing_credentials(&self) -> &'static str {
        if self.is_zh() {
            "使用 --oauth 时必须同时传入 --username 和 --password。"
        } else {
            "--oauth requires both --username and --password."
        }
    }

    pub fn login_api_missing_credentials(&self) -> &'static str {
        if self.is_zh() {
            "使用 --api 时必须同时传入 --API_TOKEN、--BASE_URL 和 --provider，且 token 去掉 sk- 前缀后至少 8 个字符。"
        } else {
            "--api requires --API_TOKEN, --BASE_URL, and --provider, and the token must be at least 8 characters after removing the sk- prefix."
        }
    }

    pub fn login_mode_conflict(&self) -> &'static str {
        if self.is_zh() {
            "--api 和 --oauth 不能同时使用。"
        } else {
            "--api and --oauth cannot be used together."
        }
    }

    pub fn deploy_start(&self, target: &str) -> String {
        if self.is_zh() {
            format!("正在把当前 Codex 凭证上传到 {target}")
        } else {
            format!("Deploying the current Codex credential to {target}")
        }
    }

    pub fn deploy_completed(&self, target: &str) -> String {
        if self.is_zh() {
            format!("已把当前 Codex 凭证上传到 {target}")
        } else {
            format!("Deployed the current Codex credential to {target}")
        }
    }

    pub fn deploy_missing_auth(&self, path: &Path) -> String {
        if self.is_zh() {
            format!("当前可用的 auth.json 不存在：{}", path.display())
        } else {
            format!("Current auth.json not found: {}", path.display())
        }
    }

    pub fn deploy_invalid_target(&self, target: &str) -> String {
        if self.is_zh() {
            format!("无效的远端目标：{target}。请使用 user@host:/target_path")
        } else {
            format!("Invalid remote target: {target}. Use user@host:/target_path")
        }
    }

    pub fn deploy_missing_ssh(&self) -> &'static str {
        if self.is_zh() {
            "未找到 ssh。执行 `scodex deploy` 需要它。"
        } else {
            "ssh not found; `scodex deploy` requires it."
        }
    }

    pub fn deploy_missing_scp(&self) -> &'static str {
        if self.is_zh() {
            "未找到 scp。执行 `scodex deploy` 需要它。"
        } else {
            "scp not found; `scodex deploy` requires it."
        }
    }

    pub fn deploy_identity_not_found(&self, path: &Path) -> String {
        if self.is_zh() {
            format!("SSH 身份文件不存在：{}", path.display())
        } else {
            format!("SSH identity file not found: {}", path.display())
        }
    }

    pub fn deploy_prepare_remote_dir_failed(&self, status: i32) -> String {
        if self.is_zh() {
            format!("远端目录准备失败，退出码：{status}")
        } else {
            format!("Preparing the remote directory failed with status {status}")
        }
    }

    pub fn deploy_copy_failed(&self, status: i32) -> String {
        if self.is_zh() {
            format!("凭证复制失败，退出码：{status}")
        } else {
            format!("Credential copy failed with status {status}")
        }
    }

    pub fn repo_sync_missing_git(&self, install_command: &str) -> String {
        if self.is_zh() {
            format!(
                "未找到 git。执行 `scodex push` 或 `scodex pull` 需要它。请先安装 git，例如：{install_command}"
            )
        } else {
            format!(
                "git not found; `scodex push` and `scodex pull` require it. Install git first, for example: {install_command}"
            )
        }
    }

    pub fn repo_sync_invalid_repo(&self) -> &'static str {
        if self.is_zh() {
            "仓库参数不能为空。"
        } else {
            "Repository argument must not be empty."
        }
    }

    pub fn repo_sync_invalid_path(&self, path: &str) -> String {
        if self.is_zh() {
            format!("无效的仓库子目录：{path}。只允许相对路径，且不能包含 `..`。")
        } else {
            format!("Invalid repository subdirectory: {path}. Use a relative path without `..`.")
        }
    }

    pub fn repo_sync_missing_key(&self, env_name: &str) -> String {
        if self.is_zh() {
            format!("未设置账号池加密密钥环境变量：{env_name}")
        } else {
            format!("Missing account-pool encryption key environment variable: {env_name}")
        }
    }

    pub fn repo_sync_decrypt_failed(&self, env_name: &str) -> String {
        if self.is_zh() {
            format!(
                "账号池解密失败。请检查 {env_name} 是否正确，或确认远端仓库里的加密 bundle 没有损坏。"
            )
        } else {
            format!(
                "Failed to decrypt the account pool. Check whether {env_name} is correct and whether the encrypted bundle in the repository is intact."
            )
        }
    }

    pub fn repo_sync_clone_failed(&self, repo: &str, status: i32) -> String {
        if self.is_zh() {
            format!("克隆仓库失败：{repo}，退出码：{status}")
        } else {
            format!("Repository clone failed: {repo}, status {status}")
        }
    }

    pub fn repo_sync_clone_auth_failed(&self, repo: &str) -> String {
        if self.is_zh() {
            format!(
                "无法访问仓库：{repo}。请检查仓库 URL，以及当前 Git 凭据、SSH key 或 PAT 是否有这个私有仓库的读取权限。"
            )
        } else {
            format!(
                "Cannot access repository: {repo}. Check the repository URL and whether your current Git credentials, SSH key, or PAT has read access to this private repository."
            )
        }
    }

    pub fn repo_sync_stage_failed(&self, status: i32) -> String {
        if self.is_zh() {
            format!("暂存账号池变更失败，退出码：{status}")
        } else {
            format!("Staging account-pool changes failed with status {status}")
        }
    }

    pub fn repo_sync_status_failed(&self, status: i32) -> String {
        if self.is_zh() {
            format!("检查仓库状态失败，退出码：{status}")
        } else {
            format!("Checking repository status failed with status {status}")
        }
    }

    pub fn repo_sync_commit_failed(&self, status: i32) -> String {
        if self.is_zh() {
            format!("提交账号池变更失败，退出码：{status}")
        } else {
            format!("Committing account-pool changes failed with status {status}")
        }
    }

    pub fn repo_sync_push_failed(&self, repo: &str, status: i32) -> String {
        if self.is_zh() {
            format!("推送账号池变更失败：{repo}，退出码：{status}")
        } else {
            format!("Pushing account-pool changes failed: {repo}, status {status}")
        }
    }

    pub fn repo_sync_push_auth_failed(&self, repo: &str) -> String {
        if self.is_zh() {
            format!(
                "无法写入仓库：{repo}。请检查当前 Git 凭据、SSH key 或 PAT 是否有这个私有仓库的写入权限。"
            )
        } else {
            format!(
                "Cannot write to repository: {repo}. Check whether your current Git credentials, SSH key, or PAT has write access to this private repository."
            )
        }
    }

    pub fn repo_push_no_accounts(&self) -> &'static str {
        if self.is_zh() {
            "当前状态目录里没有账号可推送。"
        } else {
            "No accounts found in the current state directory."
        }
    }

    pub fn repo_push_start(&self, repo: &str) -> String {
        if self.is_zh() {
            format!("正在把本地账号池全量推送到 {repo}")
        } else {
            format!("Pushing the full local account pool to {repo}")
        }
    }

    pub fn repo_push_completed(&self, repo: &str, count: usize) -> String {
        if self.is_zh() {
            format!("已用本地账号池覆盖 {repo}，共 {count} 个账号")
        } else {
            format!("Overwrote {repo} with the local account pool ({count} account(s))")
        }
    }

    pub fn repo_push_no_changes(&self, repo: &str) -> String {
        if self.is_zh() {
            format!("{repo} 里的账号池没有差异，无需推送")
        } else {
            format!("No account-pool changes to push to {repo}")
        }
    }

    pub fn repo_pull_start(&self, repo: &str) -> String {
        if self.is_zh() {
            format!("正在从 {repo} 拉取账号池，并准备覆盖本地")
        } else {
            format!("Pulling the account pool from {repo} and preparing to overwrite local state")
        }
    }

    pub fn repo_pull_missing_bundle(&self, path: &str) -> String {
        if self.is_zh() {
            format!("仓库里没有找到账号池目录：{path}")
        } else {
            format!("Account-pool directory not found in repository: {path}")
        }
    }

    pub fn repo_pull_no_accounts(&self, path: &str) -> String {
        if self.is_zh() {
            format!("账号池目录里没有可导入的账号：{path}")
        } else {
            format!("No importable accounts found in account-pool directory: {path}")
        }
    }

    pub fn repo_pull_completed(&self, repo: &str, count: usize) -> String {
        if self.is_zh() {
            format!("已用 {repo} 的账号池覆盖本地，共 {count} 个账号")
        } else {
            format!("Overwrote the local account pool with {count} account(s) from {repo}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{UiLanguage, parse_ui_language_from_locale};

    #[test]
    fn chinese_utf8_locale_selects_chinese_messages() {
        assert_eq!(
            parse_ui_language_from_locale("zh_CN.UTF-8"),
            Some(UiLanguage::ZhHans)
        );
        assert_eq!(
            parse_ui_language_from_locale("zh_CN.utf8"),
            Some(UiLanguage::ZhHans)
        );
    }

    #[test]
    fn locale_without_utf8_or_without_zh_falls_back_to_english() {
        assert_eq!(parse_ui_language_from_locale("zh_CN.GBK"), None);
        assert_eq!(parse_ui_language_from_locale("en_US.UTF-8"), None);
        assert_eq!(parse_ui_language_from_locale("C"), None);
    }
}
