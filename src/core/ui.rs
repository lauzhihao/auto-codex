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

    pub fn table_headers(&self) -> [&'static str; 7] {
        if self.is_zh() {
            ["当前", "邮箱", "套餐", "5h", "Weekly", "重置时间", "状态"]
        } else {
            [
                "Active", "Email", "Plan", "5h", "Weekly", "ResetOn", "Status",
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

    pub fn add_opening_signup(&self) -> &'static str {
        if self.is_zh() {
            "正在打开 OpenAI 账号注册页。"
        } else {
            "Opening the OpenAI account signup page."
        }
    }

    pub fn add_opened_signup(&self, url: &str) -> String {
        if self.is_zh() {
            format!("已尝试打开：{url}")
        } else {
            format!("Opened: {url}")
        }
    }

    pub fn add_no_gui_open_manually(&self, url: &str) -> String {
        if self.is_zh() {
            format!("未检测到可用图形界面。请在另一台可用浏览器的设备上打开：{url}")
        } else {
            format!(
                "No GUI environment detected. Open this URL on another browser-enabled device: {url}"
            )
        }
    }

    pub fn add_browser_open_failed(&self, url: &str) -> String {
        if self.is_zh() {
            format!("未能自动打开浏览器。请手动访问：{url}")
        } else {
            format!("Could not open a browser automatically. Please visit: {url}")
        }
    }

    pub fn add_finish_signup_then_continue(&self) -> &'static str {
        if self.is_zh() {
            "完成注册或登录后，按回车继续。"
        } else {
            "After you finish signup or login, press Enter to continue."
        }
    }

    pub fn add_waiting_enter(&self) -> &'static str {
        if self.is_zh() {
            "按回车继续："
        } else {
            "Press Enter to continue: "
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
