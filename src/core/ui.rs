use std::env;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::sync::OnceLock;

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

// 缓存 locale 探测结果，避免每次调用都读环境变量
static CACHED_LANGUAGE: OnceLock<UiLanguage> = OnceLock::new();
static STYLE_ENABLED: OnceLock<bool> = OnceLock::new();

pub fn messages() -> Messages {
    Messages {
        language: *CACHED_LANGUAGE.get_or_init(detect_ui_language),
    }
}

pub fn style_enabled() -> bool {
    *STYLE_ENABLED.get_or_init(|| {
        io::stdout().is_terminal()
            && env::var_os("NO_COLOR").is_none()
            && !matches!(env::var("TERM").ok().as_deref(), Some("dumb"))
    })
}

pub fn detect_ui_language() -> UiLanguage {
    // GNU gettext 优先级：LANGUAGE > LC_ALL > LC_MESSAGES > LANG
    let locale = env::var("LANGUAGE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| env::var("LC_ALL").ok().filter(|v| !v.trim().is_empty()))
        .or_else(|| {
            env::var("LC_MESSAGES")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .or_else(|| env::var("LANG").ok().filter(|v| !v.trim().is_empty()));

    locale
        .as_deref()
        .and_then(parse_ui_language_from_locale)
        .unwrap_or(UiLanguage::En)
}

/// 放宽判定：locale 以 zh 开头即识别为中文，不再强求含 utf-8/utf8
pub fn parse_ui_language_from_locale(locale: &str) -> Option<UiLanguage> {
    let normalized = locale.trim().to_ascii_lowercase();
    // LANGUAGE 变量可能形如 "zh_CN:en_US"，取第一段
    let first = normalized.split(':').next().unwrap_or(&normalized);
    if first.starts_with("zh") {
        Some(UiLanguage::ZhHans)
    } else {
        None
    }
}

pub fn format_top_level_error(error: &Error) -> String {
    let ui = messages();
    let prefix = ui.error_prefix();
    let chain = error.chain().map(ToString::to_string).collect::<Vec<_>>();
    if chain.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {}", chain.join(": "))
    }
}

/// 剥离 ANSI/VT100 CSI 转义序列。
/// 终止符范围遵循 ECMA-48：0x40-0x7E（'@'..='~'），比仅检测字母更准确。
pub fn strip_ansi(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            chars.next(); // 消费 '['
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        result.push(ch);
    }
    result
}

// ────────────────────────────────────────────────────────────────────────────
// i18n 表结构：所有静态双语文案集中于此，is_zh 仅在 pick() 处分支一次
// ────────────────────────────────────────────────────────────────────────────

struct Msg {
    en: &'static str,
    zh: &'static str,
}

impl Messages {
    pub fn is_zh(&self) -> bool {
        matches!(self.language, UiLanguage::ZhHans)
    }

    #[inline]
    fn pick(&self, msg: &'static Msg) -> &'static str {
        if self.is_zh() { msg.zh } else { msg.en }
    }

    fn format_msg(&self, msg: &'static Msg, vars: &[(&str, String)]) -> String {
        let mut output = self.pick(msg).to_string();
        for (key, value) in vars {
            output = output.replace(key, value);
        }
        output
    }
}

// 静态消息表
static MSG_NO_USABLE_ACCOUNT: Msg = Msg {
    en: "No usable account found.",
    zh: "没有找到可用账号。",
};
static MSG_ERROR_PREFIX: Msg = Msg {
    en: "Error",
    zh: "错误",
};
static MSG_NO_USABLE_ACCOUNT_HINT: Msg = Msg {
    en: "No usable accounts found. Run `scodex add` to add one first.",
    zh: "没有可用账号，请先执行 `scodex add` 添加一个账号。",
};
static MSG_NO_IMPORTABLE_ACCOUNTS: Msg = Msg {
    en: "No importable accounts found.",
    zh: "没有找到可导入的账号。",
};
static MSG_RM_CANCELLED: Msg = Msg {
    en: "Cancelled.",
    zh: "已取消。",
};
static MSG_RM_REQUIRES_TTY: Msg = Msg {
    en: "Input is not a terminal; pass -y to skip confirmation.",
    zh: "当前输入不是终端；请加 -y 跳过确认。",
};
static MSG_RESTART_TERMINAL_HINT: Msg = Msg {
    en: "Restart the current terminal if it still resolves the old binary.",
    zh: "如果当前终端仍然解析到旧二进制，请重启终端。",
};
static MSG_SELECTION_SWITCHED: Msg = Msg {
    en: "Switched to",
    zh: "已切换到",
};
static MSG_SELECTION_WOULD_SELECT: Msg = Msg {
    en: "Would select",
    zh: "将会选择",
};
static MSG_NA: Msg = Msg {
    en: "N/A",
    zh: "无",
};
static MSG_UNKNOWN: Msg = Msg {
    en: "Unknown",
    zh: "未知",
};
static MSG_STATUS_OK: Msg = Msg {
    en: "OK",
    zh: "正常",
};
static MSG_STATUS_ERROR: Msg = Msg {
    en: "ERROR",
    zh: "错误",
};
static MSG_STATUS_RELOGIN: Msg = Msg {
    en: "RELOGIN",
    zh: "需重登",
};
static MSG_LOGIN_START: Msg = Msg {
    en: "Starting `codex login --device-auth`.",
    zh: "正在启动 `codex login --device-auth`。",
};
static MSG_LOGIN_OPEN_URL: Msg = Msg {
    en: "Open the printed URL on any browser-enabled machine and finish the login there.",
    zh: "请在任意可用浏览器的设备上打开上面输出的 URL 并完成登录。",
};
static MSG_RESUME_SESSION: Msg = Msg {
    en: "Resuming latest Codex session for this directory.",
    zh: "正在恢复当前目录的最新 Codex 会话。",
};
static MSG_RESUME_FALLBACK: Msg = Msg {
    en: "Resume did not complete cleanly; falling back to a fresh Codex session.",
    zh: "恢复会话未能正常完成，正在回退到新会话。",
};
static MSG_FRESH_SESSION: Msg = Msg {
    en: "Starting a fresh Codex session.",
    zh: "正在启动新的 Codex 会话。",
};
static MSG_MISSING_CODEX: Msg = Msg {
    en: "codex not found. This will cause scodex to behave incorrectly.",
    zh: "未找到 codex。这会导致 scodex 无法正常工作。",
};
static MSG_INSTALL_HINT: Msg = Msg {
    en: "You can install Codex CLI by running:",
    zh: "你可以先运行下面的命令安装 Codex CLI：",
};
static MSG_MANUAL_INSTALL: Msg = Msg {
    en: "Please install Codex CLI manually and run scodex again.",
    zh: "请先手动安装 Codex CLI，然后重新运行 scodex。",
};
static MSG_CONFIRM_INSTALL: Msg = Msg {
    en: "I can try to install it for you now. Continue? (Y/N): ",
    zh: "如果你希望我现在帮你安装，请确认（Y/N）：",
};
static MSG_INVALID_YES_NO: Msg = Msg {
    en: "Please answer Y/YES/N/NO.",
    zh: "请输入 Y/YES/N/NO。",
};
static MSG_CODEX_INSTALL_STILL_MISSING: Msg = Msg {
    en: "Codex installation completed, but `codex` is still not available. Restart the shell or set CODEX_BIN explicitly.",
    zh: "Codex 安装似乎已完成，但当前仍然找不到 `codex`。请重启 shell，或显式设置 CODEX_BIN。",
};
static MSG_LOGIN_MISSING_AUTH: Msg = Msg {
    en: "Login finished but no auth.json was produced.",
    zh: "登录流程已结束，但没有生成 auth.json。",
};
static MSG_LOGIN_AUTOFILL_START: Msg = Msg {
    en: "Starting `codex login` and opening a controlled Chrome window for OAuth auto-fill.",
    zh: "正在启动 `codex login` 并打开受控 Chrome 完成 OAuth 自动填充。",
};
static MSG_LOGIN_AUTOFILL_WAITING_CONSENT: Msg = Msg {
    en: "OAuth auto-fill complete. Click `Authorize` once in the opened Chrome window to finish.",
    zh: "OAuth 自动填充完成。请在刚打开的 Chrome 窗口里点一次 `Authorize` 完成登录。",
};
static MSG_LOGIN_AUTOFILL_NO_CHROME: Msg = Msg {
    en: "Chrome or Chromium not detected; cannot run OAuth auto-fill. Install Chrome or run `scodex login` without --oauth.",
    zh: "未检测到 Chrome 或 Chromium，无法执行 OAuth 自动填充。请安装 Chrome，或改用 `scodex login`（不带 --oauth）。",
};
static MSG_LOGIN_AUTOFILL_MISSING_CREDENTIALS: Msg = Msg {
    en: "--oauth requires both --username and --password.",
    zh: "使用 --oauth 时必须同时传入 --username 和 --password。",
};
static MSG_LOGIN_API_MISSING_CREDENTIALS: Msg = Msg {
    en: "--api requires --API_TOKEN, --BASE_URL, and --provider, and the token must be at least 8 characters after removing the sk- prefix.",
    zh: "使用 --api 时必须同时传入 --API_TOKEN、--BASE_URL 和 --provider，且 token 去掉 sk- 前缀后至少 8 个字符。",
};
static MSG_LOGIN_MODE_CONFLICT: Msg = Msg {
    en: "--api and --oauth cannot be used together.",
    zh: "--api 和 --oauth 不能同时使用。",
};
static MSG_DEPLOY_MISSING_SSH: Msg = Msg {
    en: "ssh not found; `scodex deploy` requires it.",
    zh: "未找到 ssh。执行 `scodex deploy` 需要它。",
};
static MSG_DEPLOY_MISSING_SCP: Msg = Msg {
    en: "scp not found; `scodex deploy` requires it.",
    zh: "未找到 scp。执行 `scodex deploy` 需要它。",
};
static MSG_REPO_SYNC_INVALID_REPO: Msg = Msg {
    en: "Repository argument must not be empty.",
    zh: "仓库参数不能为空。",
};
static MSG_REPO_PUSH_NO_ACCOUNTS: Msg = Msg {
    en: "No accounts found in the current state directory.",
    zh: "当前状态目录里没有账号可推送。",
};
static MSG_ADDED_ACCOUNT: Msg = Msg {
    en: "Added {email}",
    zh: "已添加 {email}",
};
static MSG_UNKNOWN_ACCOUNT: Msg = Msg {
    en: "Unknown account: {email}",
    zh: "未知账号：{email}",
};
static MSG_CONFIRM_RM: Msg = Msg {
    en: "Remove account {email}? This cannot be undone (Y/N): ",
    zh: "确认删除账号 {email}？此操作不可恢复 (Y/N)：",
};
static MSG_REMOVED_ACCOUNT: Msg = Msg {
    en: "Removed {email}",
    zh: "已移除 {email}",
};
static MSG_REFRESHED_ACCOUNTS: Msg = Msg {
    en: "Refreshed {count} account(s).",
    zh: "已刷新 {count} 个账号。",
};
static MSG_USABLE_ACCOUNT_SUMMARY: Msg = Msg {
    en: "{count} usable account(s)",
    zh: "共有 {count} 个可用账号",
};
static MSG_UPDATE_ALREADY_CURRENT: Msg = Msg {
    en: "Already on the latest installed version ({version}) at {path}",
    zh: "当前已是最新已安装版本（{version}），位置：{path}",
};
static MSG_UPDATE_COMPLETED: Msg = Msg {
    en: "Updated scodex from {previous} to {installed} at {path}",
    zh: "已将 scodex 从 {previous} 更新到 {installed}，位置：{path}",
};
static MSG_IMPORTED_ACCOUNT: Msg = Msg {
    en: "Imported {email} -> {id}",
    zh: "已导入 {email} -> {id}",
};
static TABLE_HEADER_MSGS: [Msg; 8] = [
    Msg {
        en: "Active",
        zh: "当前",
    },
    Msg {
        en: "Email",
        zh: "邮箱",
    },
    Msg {
        en: "Type",
        zh: "类型",
    },
    Msg {
        en: "Plan",
        zh: "套餐",
    },
    Msg { en: "5h", zh: "5h" },
    Msg {
        en: "Weekly",
        zh: "每周",
    },
    Msg {
        en: "ResetOn",
        zh: "重置时间",
    },
    Msg {
        en: "Status",
        zh: "状态",
    },
];
static MSG_ACCOUNT_TYPE_SUBSCRIPTION: Msg = Msg {
    en: "SUBSCRIPTION",
    zh: "官方订阅",
};
static MSG_ACCOUNT_TYPE_API: Msg = Msg {
    en: "API",
    zh: "API",
};
static MSG_LOGIN_HEADLESS_IP: Msg = Msg {
    en: "Headless host LAN IP: {ip}",
    zh: "当前无头主机局域网 IP：{ip}",
};
static MSG_CODEX_INSTALL_FAILED: Msg = Msg {
    en: "Codex installation failed with status {status}",
    zh: "Codex 安装失败，退出码：{status}",
};
static MSG_CODEX_INSTALL_TOOL_MISSING: Msg = Msg {
    en: "{tool} not found. Install Node.js/npm first before trying to install Codex CLI automatically.",
    zh: "未找到 {tool}。要自动安装 Codex CLI，当前机器需要先安装 Node.js/npm。",
};
static MSG_CODEX_LOGIN_FAILED: Msg = Msg {
    en: "codex login failed with status {status}",
    zh: "codex 登录失败，退出码：{status}",
};
static MSG_LOGIN_AUTOFILL_PROMPT_WITH_CODE: Msg = Msg {
    en: "Device URL: {url}\nOne-time code: {code}",
    zh: "设备授权链接：{url}\n一次性 code：{code}",
};
static MSG_LOGIN_AUTOFILL_PROMPT_WITHOUT_CODE: Msg = Msg {
    en: "Device URL: {url}",
    zh: "设备授权链接：{url}",
};
static MSG_DEPLOY_START: Msg = Msg {
    en: "Deploying the current Codex credential to {target}",
    zh: "正在把当前 Codex 凭证上传到 {target}",
};
static MSG_DEPLOY_COMPLETED: Msg = Msg {
    en: "Deployed the current Codex credential to {target}",
    zh: "已把当前 Codex 凭证上传到 {target}",
};
static MSG_DEPLOY_MISSING_AUTH: Msg = Msg {
    en: "Current auth.json not found: {path}",
    zh: "当前可用的 auth.json 不存在：{path}",
};
static MSG_DEPLOY_INVALID_TARGET: Msg = Msg {
    en: "Invalid remote target: {target}. Use user@host:/target_path",
    zh: "无效的远端目标：{target}。请使用 user@host:/target_path",
};
static MSG_DEPLOY_IDENTITY_NOT_FOUND: Msg = Msg {
    en: "SSH identity file not found: {path}",
    zh: "SSH 身份文件不存在：{path}",
};
static MSG_DEPLOY_PREPARE_REMOTE_DIR_FAILED: Msg = Msg {
    en: "Preparing the remote directory failed with status {status}",
    zh: "远端目录准备失败，退出码：{status}",
};
static MSG_DEPLOY_COPY_FAILED: Msg = Msg {
    en: "Credential copy failed with status {status}",
    zh: "凭证复制失败，退出码：{status}",
};
static MSG_REPO_SYNC_MISSING_GIT: Msg = Msg {
    en: "git not found; `scodex push` and `scodex pull` require it. Install git first, for example: {install_command}",
    zh: "未找到 git。执行 `scodex push` 或 `scodex pull` 需要它。请先安装 git，例如：{install_command}",
};
static MSG_REPO_SYNC_MISSING_REPO: Msg = Msg {
    en: "No account-pool repository configured. Pass `<REPO>`, set `{env_name}`, or run `scodex push/pull` once with `<REPO>` to save it locally.",
    zh: "未找到账号池仓库配置。请传入 `<REPO>`，或设置环境变量 `{env_name}`，或先执行一次带 `<REPO>` 的 `scodex push/pull` 以保存本地配置。",
};
static MSG_REPO_SYNC_INVALID_PATH: Msg = Msg {
    en: "Invalid repository subdirectory: {path}. Use a relative path without `..`.",
    zh: "无效的仓库子目录：{path}。只允许相对路径，且不能包含 `..`。",
};
static MSG_REPO_SYNC_MISSING_KEY: Msg = Msg {
    en: "Missing account-pool encryption key environment variable: {env_name}",
    zh: "未设置账号池加密密钥环境变量：{env_name}",
};
static MSG_REPO_SYNC_DECRYPT_FAILED: Msg = Msg {
    en: "Failed to decrypt the account pool. Check whether {env_name} is correct and whether the encrypted bundle in the repository is intact.",
    zh: "账号池解密失败。请检查 {env_name} 是否正确，或确认远端仓库里的加密 bundle 没有损坏。",
};
static MSG_REPO_SYNC_CLONE_FAILED: Msg = Msg {
    en: "Repository clone failed: {repo}, status {status}",
    zh: "克隆仓库失败：{repo}，退出码：{status}",
};
static MSG_REPO_SYNC_CLONE_AUTH_FAILED: Msg = Msg {
    en: "Cannot access repository: {repo}. Check the repository URL and whether your current Git credentials, SSH key, or PAT has read access to this private repository.",
    zh: "无法访问仓库：{repo}。请检查仓库 URL，以及当前 Git 凭据、SSH key 或 PAT 是否有这个私有仓库的读取权限。",
};
static MSG_REPO_SYNC_STAGE_FAILED: Msg = Msg {
    en: "Staging account-pool changes failed with status {status}",
    zh: "暂存账号池变更失败，退出码：{status}",
};
static MSG_REPO_SYNC_STATUS_FAILED: Msg = Msg {
    en: "Checking repository status failed with status {status}",
    zh: "检查仓库状态失败，退出码：{status}",
};
static MSG_REPO_SYNC_COMMIT_FAILED: Msg = Msg {
    en: "Committing account-pool changes failed with status {status}",
    zh: "提交账号池变更失败，退出码：{status}",
};
static MSG_REPO_SYNC_PUSH_FAILED: Msg = Msg {
    en: "Pushing account-pool changes failed: {repo}, status {status}",
    zh: "推送账号池变更失败：{repo}，退出码：{status}",
};
static MSG_REPO_SYNC_PUSH_AUTH_FAILED: Msg = Msg {
    en: "Cannot write to repository: {repo}. Check whether your current Git credentials, SSH key, or PAT has write access to this private repository.",
    zh: "无法写入仓库：{repo}。请检查当前 Git 凭据、SSH key 或 PAT 是否有这个私有仓库的写入权限。",
};
static MSG_REPO_PUSH_START: Msg = Msg {
    en: "Pushing the full local account pool to {repo}",
    zh: "正在把本地账号池全量推送到 {repo}",
};
static MSG_REPO_PUSH_COMPLETED: Msg = Msg {
    en: "Overwrote {repo} with the local account pool ({count} account(s))",
    zh: "已用本地账号池覆盖 {repo}，共 {count} 个账号",
};
static MSG_REPO_PUSH_NO_CHANGES: Msg = Msg {
    en: "No account-pool changes to push to {repo}",
    zh: "{repo} 里的账号池没有差异，无需推送",
};
static MSG_REPO_PULL_START: Msg = Msg {
    en: "Pulling the account pool from {repo} and preparing to overwrite local state",
    zh: "正在从 {repo} 拉取账号池，并准备覆盖本地",
};
static MSG_REPO_PULL_MISSING_BUNDLE: Msg = Msg {
    en: "Account-pool directory not found in repository: {path}",
    zh: "仓库里没有找到账号池目录：{path}",
};
static MSG_REPO_PULL_NO_ACCOUNTS: Msg = Msg {
    en: "No importable accounts found in account-pool directory: {path}",
    zh: "账号池目录里没有可导入的账号：{path}",
};
static MSG_REPO_PULL_COMPLETED: Msg = Msg {
    en: "Overwrote the local account pool with {count} account(s) from {repo}",
    zh: "已用 {repo} 的账号池覆盖本地，共 {count} 个账号",
};

// ────────────────────────────────────────────────────────────────────────────
// Messages 方法实现：保持原有签名，内部走 pick() 或 format! + 表常量
// ────────────────────────────────────────────────────────────────────────────

impl Messages {
    pub fn error_prefix(&self) -> &'static str {
        self.pick(&MSG_ERROR_PREFIX)
    }

    pub fn no_usable_account(&self) -> &'static str {
        self.pick(&MSG_NO_USABLE_ACCOUNT)
    }

    pub fn no_usable_account_hint(&self) -> &'static str {
        self.pick(&MSG_NO_USABLE_ACCOUNT_HINT)
    }

    pub fn no_importable_accounts(&self) -> &'static str {
        self.pick(&MSG_NO_IMPORTABLE_ACCOUNTS)
    }

    pub fn added_account(&self, email: &str) -> String {
        self.format_msg(&MSG_ADDED_ACCOUNT, &[("{email}", email.to_string())])
    }

    pub fn unknown_account(&self, email: &str) -> String {
        self.format_msg(&MSG_UNKNOWN_ACCOUNT, &[("{email}", email.to_string())])
    }

    pub fn confirm_rm(&self, email: &str) -> String {
        self.format_msg(&MSG_CONFIRM_RM, &[("{email}", email.to_string())])
    }

    pub fn rm_cancelled(&self) -> &'static str {
        self.pick(&MSG_RM_CANCELLED)
    }

    pub fn removed_account(&self, email: &str) -> String {
        self.format_msg(&MSG_REMOVED_ACCOUNT, &[("{email}", email.to_string())])
    }

    pub fn rm_requires_tty(&self) -> &'static str {
        self.pick(&MSG_RM_REQUIRES_TTY)
    }

    pub fn refreshed_accounts(&self, count: usize) -> String {
        self.format_msg(&MSG_REFRESHED_ACCOUNTS, &[("{count}", count.to_string())])
    }

    pub fn usable_account_summary(&self, count: usize) -> String {
        self.format_msg(
            &MSG_USABLE_ACCOUNT_SUMMARY,
            &[("{count}", count.to_string())],
        )
    }

    pub fn update_already_current(&self, version: &str, path: &Path) -> String {
        self.format_msg(
            &MSG_UPDATE_ALREADY_CURRENT,
            &[
                ("{version}", version.to_string()),
                ("{path}", path.display().to_string()),
            ],
        )
    }

    pub fn update_completed(&self, previous: &str, installed: &str, path: &Path) -> String {
        self.format_msg(
            &MSG_UPDATE_COMPLETED,
            &[
                ("{previous}", previous.to_string()),
                ("{installed}", installed.to_string()),
                ("{path}", path.display().to_string()),
            ],
        )
    }

    pub fn restart_terminal_hint(&self) -> &'static str {
        self.pick(&MSG_RESTART_TERMINAL_HINT)
    }

    pub fn imported_account(&self, email: &str, id: &str) -> String {
        self.format_msg(
            &MSG_IMPORTED_ACCOUNT,
            &[("{email}", email.to_string()), ("{id}", id.to_string())],
        )
    }

    pub fn selection_switched(&self) -> &'static str {
        self.pick(&MSG_SELECTION_SWITCHED)
    }

    pub fn selection_would_select(&self) -> &'static str {
        self.pick(&MSG_SELECTION_WOULD_SELECT)
    }

    pub fn na(&self) -> &'static str {
        self.pick(&MSG_NA)
    }

    pub fn unknown(&self) -> &'static str {
        self.pick(&MSG_UNKNOWN)
    }

    pub fn table_headers(&self) -> [&'static str; 8] {
        std::array::from_fn(|index| self.pick(&TABLE_HEADER_MSGS[index]))
    }

    pub fn account_type_subscription(&self) -> &'static str {
        self.pick(&MSG_ACCOUNT_TYPE_SUBSCRIPTION)
    }

    pub fn account_type_api(&self) -> &'static str {
        self.pick(&MSG_ACCOUNT_TYPE_API)
    }

    pub fn status_ok(&self) -> &'static str {
        self.pick(&MSG_STATUS_OK)
    }

    pub fn status_error(&self) -> &'static str {
        self.pick(&MSG_STATUS_ERROR)
    }

    pub fn status_relogin(&self) -> &'static str {
        self.pick(&MSG_STATUS_RELOGIN)
    }

    pub fn login_start(&self) -> &'static str {
        self.pick(&MSG_LOGIN_START)
    }

    pub fn login_open_url(&self) -> &'static str {
        self.pick(&MSG_LOGIN_OPEN_URL)
    }

    pub fn login_headless_ip(&self, ip: &str) -> String {
        self.format_msg(&MSG_LOGIN_HEADLESS_IP, &[("{ip}", ip.to_string())])
    }

    pub fn resume_session(&self) -> &'static str {
        self.pick(&MSG_RESUME_SESSION)
    }

    pub fn resume_fallback(&self) -> &'static str {
        self.pick(&MSG_RESUME_FALLBACK)
    }

    pub fn fresh_session(&self) -> &'static str {
        self.pick(&MSG_FRESH_SESSION)
    }

    pub fn missing_codex(&self) -> &'static str {
        self.pick(&MSG_MISSING_CODEX)
    }

    pub fn install_hint(&self) -> &'static str {
        self.pick(&MSG_INSTALL_HINT)
    }

    pub fn manual_install(&self) -> &'static str {
        self.pick(&MSG_MANUAL_INSTALL)
    }

    pub fn confirm_install(&self) -> &'static str {
        self.pick(&MSG_CONFIRM_INSTALL)
    }

    pub fn invalid_yes_no(&self) -> &'static str {
        self.pick(&MSG_INVALID_YES_NO)
    }

    pub fn codex_install_still_missing(&self) -> &'static str {
        self.pick(&MSG_CODEX_INSTALL_STILL_MISSING)
    }

    pub fn codex_install_failed(&self, status: i32) -> String {
        self.format_msg(
            &MSG_CODEX_INSTALL_FAILED,
            &[("{status}", status.to_string())],
        )
    }

    pub fn codex_install_tool_missing(&self, tool: &str) -> String {
        self.format_msg(
            &MSG_CODEX_INSTALL_TOOL_MISSING,
            &[("{tool}", tool.to_string())],
        )
    }

    pub fn codex_login_failed(&self, status: i32) -> String {
        self.format_msg(&MSG_CODEX_LOGIN_FAILED, &[("{status}", status.to_string())])
    }

    pub fn login_missing_auth(&self) -> &'static str {
        self.pick(&MSG_LOGIN_MISSING_AUTH)
    }

    pub fn login_autofill_start(&self) -> &'static str {
        self.pick(&MSG_LOGIN_AUTOFILL_START)
    }

    pub fn login_autofill_prompt(&self, url: &str, code: Option<&str>) -> String {
        if let Some(code) = code {
            self.format_msg(
                &MSG_LOGIN_AUTOFILL_PROMPT_WITH_CODE,
                &[("{url}", url.to_string()), ("{code}", code.to_string())],
            )
        } else {
            self.format_msg(
                &MSG_LOGIN_AUTOFILL_PROMPT_WITHOUT_CODE,
                &[("{url}", url.to_string())],
            )
        }
    }

    pub fn login_autofill_waiting_consent(&self) -> &'static str {
        self.pick(&MSG_LOGIN_AUTOFILL_WAITING_CONSENT)
    }

    pub fn login_autofill_no_chrome(&self) -> &'static str {
        self.pick(&MSG_LOGIN_AUTOFILL_NO_CHROME)
    }

    pub fn login_autofill_missing_credentials(&self) -> &'static str {
        self.pick(&MSG_LOGIN_AUTOFILL_MISSING_CREDENTIALS)
    }

    pub fn login_api_missing_credentials(&self) -> &'static str {
        self.pick(&MSG_LOGIN_API_MISSING_CREDENTIALS)
    }

    pub fn login_mode_conflict(&self) -> &'static str {
        self.pick(&MSG_LOGIN_MODE_CONFLICT)
    }

    pub fn deploy_start(&self, target: &str) -> String {
        self.format_msg(&MSG_DEPLOY_START, &[("{target}", target.to_string())])
    }

    pub fn deploy_completed(&self, target: &str) -> String {
        self.format_msg(&MSG_DEPLOY_COMPLETED, &[("{target}", target.to_string())])
    }

    pub fn deploy_missing_auth(&self, path: &Path) -> String {
        self.format_msg(
            &MSG_DEPLOY_MISSING_AUTH,
            &[("{path}", path.display().to_string())],
        )
    }

    pub fn deploy_invalid_target(&self, target: &str) -> String {
        self.format_msg(
            &MSG_DEPLOY_INVALID_TARGET,
            &[("{target}", target.to_string())],
        )
    }

    pub fn deploy_missing_ssh(&self) -> &'static str {
        self.pick(&MSG_DEPLOY_MISSING_SSH)
    }

    pub fn deploy_missing_scp(&self) -> &'static str {
        self.pick(&MSG_DEPLOY_MISSING_SCP)
    }

    pub fn deploy_identity_not_found(&self, path: &Path) -> String {
        self.format_msg(
            &MSG_DEPLOY_IDENTITY_NOT_FOUND,
            &[("{path}", path.display().to_string())],
        )
    }

    pub fn deploy_prepare_remote_dir_failed(&self, status: i32) -> String {
        self.format_msg(
            &MSG_DEPLOY_PREPARE_REMOTE_DIR_FAILED,
            &[("{status}", status.to_string())],
        )
    }

    pub fn deploy_copy_failed(&self, status: i32) -> String {
        self.format_msg(&MSG_DEPLOY_COPY_FAILED, &[("{status}", status.to_string())])
    }

    pub fn repo_sync_missing_git(&self, install_command: &str) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_MISSING_GIT,
            &[("{install_command}", install_command.to_string())],
        )
    }

    pub fn repo_sync_invalid_repo(&self) -> &'static str {
        self.pick(&MSG_REPO_SYNC_INVALID_REPO)
    }

    pub fn repo_sync_missing_repo(&self, env_name: &str) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_MISSING_REPO,
            &[("{env_name}", env_name.to_string())],
        )
    }

    pub fn repo_sync_invalid_path(&self, path: &str) -> String {
        self.format_msg(&MSG_REPO_SYNC_INVALID_PATH, &[("{path}", path.to_string())])
    }

    pub fn repo_sync_missing_key(&self, env_name: &str) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_MISSING_KEY,
            &[("{env_name}", env_name.to_string())],
        )
    }

    pub fn repo_sync_decrypt_failed(&self, env_name: &str) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_DECRYPT_FAILED,
            &[("{env_name}", env_name.to_string())],
        )
    }

    pub fn repo_sync_clone_failed(&self, repo: &str, status: i32) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_CLONE_FAILED,
            &[
                ("{repo}", repo.to_string()),
                ("{status}", status.to_string()),
            ],
        )
    }

    pub fn repo_sync_clone_auth_failed(&self, repo: &str) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_CLONE_AUTH_FAILED,
            &[("{repo}", repo.to_string())],
        )
    }

    pub fn repo_sync_stage_failed(&self, status: i32) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_STAGE_FAILED,
            &[("{status}", status.to_string())],
        )
    }

    pub fn repo_sync_status_failed(&self, status: i32) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_STATUS_FAILED,
            &[("{status}", status.to_string())],
        )
    }

    pub fn repo_sync_commit_failed(&self, status: i32) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_COMMIT_FAILED,
            &[("{status}", status.to_string())],
        )
    }

    pub fn repo_sync_push_failed(&self, repo: &str, status: i32) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_PUSH_FAILED,
            &[
                ("{repo}", repo.to_string()),
                ("{status}", status.to_string()),
            ],
        )
    }

    pub fn repo_sync_push_auth_failed(&self, repo: &str) -> String {
        self.format_msg(
            &MSG_REPO_SYNC_PUSH_AUTH_FAILED,
            &[("{repo}", repo.to_string())],
        )
    }

    pub fn repo_push_no_accounts(&self) -> &'static str {
        self.pick(&MSG_REPO_PUSH_NO_ACCOUNTS)
    }

    pub fn repo_push_start(&self, repo: &str) -> String {
        self.format_msg(&MSG_REPO_PUSH_START, &[("{repo}", repo.to_string())])
    }

    pub fn repo_push_completed(&self, repo: &str, count: usize) -> String {
        self.format_msg(
            &MSG_REPO_PUSH_COMPLETED,
            &[("{repo}", repo.to_string()), ("{count}", count.to_string())],
        )
    }

    pub fn repo_push_no_changes(&self, repo: &str) -> String {
        self.format_msg(&MSG_REPO_PUSH_NO_CHANGES, &[("{repo}", repo.to_string())])
    }

    pub fn repo_pull_start(&self, repo: &str) -> String {
        self.format_msg(&MSG_REPO_PULL_START, &[("{repo}", repo.to_string())])
    }

    pub fn repo_pull_missing_bundle(&self, path: &str) -> String {
        self.format_msg(
            &MSG_REPO_PULL_MISSING_BUNDLE,
            &[("{path}", path.to_string())],
        )
    }

    pub fn repo_pull_no_accounts(&self, path: &str) -> String {
        self.format_msg(&MSG_REPO_PULL_NO_ACCOUNTS, &[("{path}", path.to_string())])
    }

    pub fn repo_pull_completed(&self, repo: &str, count: usize) -> String {
        self.format_msg(
            &MSG_REPO_PULL_COMPLETED,
            &[("{repo}", repo.to_string()), ("{count}", count.to_string())],
        )
    }
}

// ────────────────────────────────────────────────────────────────────────────
// 单元测试
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{Messages, UiLanguage, parse_ui_language_from_locale, strip_ansi};
    use std::sync::Mutex;

    // 串行化所有修改环境变量的测试，避免并发干扰
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    // ── locale 解析 ─────────────────────────────────────────────────────────

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
    fn locale_without_utf8_still_selects_chinese() {
        // 放宽判定：GBK locale 也应识别为中文
        assert_eq!(
            parse_ui_language_from_locale("zh_CN.GBK"),
            Some(UiLanguage::ZhHans)
        );
        assert_eq!(
            parse_ui_language_from_locale("zh_TW"),
            Some(UiLanguage::ZhHans)
        );
        assert_eq!(
            parse_ui_language_from_locale("zh"),
            Some(UiLanguage::ZhHans)
        );
    }

    #[test]
    fn non_chinese_locale_falls_back_to_english() {
        assert_eq!(parse_ui_language_from_locale("en_US.UTF-8"), None);
        assert_eq!(parse_ui_language_from_locale("C"), None);
        assert_eq!(parse_ui_language_from_locale("ja_JP.UTF-8"), None);
    }

    #[test]
    fn language_var_colon_separated_zh_first() {
        // LANGUAGE 格式 "zh_CN:en_US" 应取第一段，识别为中文
        assert_eq!(
            parse_ui_language_from_locale("zh_CN:en_US"),
            Some(UiLanguage::ZhHans)
        );
    }

    #[test]
    fn language_var_colon_separated_en_first() {
        // "en_US:zh_CN" 第一段是英文，应回退英文
        assert_eq!(parse_ui_language_from_locale("en_US:zh_CN"), None);
    }

    // ── LANGUAGE 环境变量优先级 ──────────────────────────────────────────────

    #[test]
    fn language_env_takes_priority_over_lang() {
        let _guard = ENV_MUTEX.lock().unwrap();
        // 保存原值
        let orig_language = std::env::var("LANGUAGE").ok();
        let orig_lang = std::env::var("LANG").ok();
        let orig_lc_all = std::env::var("LC_ALL").ok();
        let orig_lc_messages = std::env::var("LC_MESSAGES").ok();

        unsafe {
            std::env::remove_var("LC_ALL");
            std::env::remove_var("LC_MESSAGES");
            std::env::set_var("LANG", "en_US.UTF-8");
            std::env::set_var("LANGUAGE", "zh_CN.UTF-8");
        }

        let lang = super::detect_ui_language();

        // 恢复
        unsafe {
            match orig_language {
                Some(v) => std::env::set_var("LANGUAGE", v),
                None => std::env::remove_var("LANGUAGE"),
            }
            match orig_lang {
                Some(v) => std::env::set_var("LANG", v),
                None => std::env::remove_var("LANG"),
            }
            match orig_lc_all {
                Some(v) => std::env::set_var("LC_ALL", v),
                None => std::env::remove_var("LC_ALL"),
            }
            match orig_lc_messages {
                Some(v) => std::env::set_var("LC_MESSAGES", v),
                None => std::env::remove_var("LC_MESSAGES"),
            }
        }

        assert_eq!(
            lang,
            UiLanguage::ZhHans,
            "LANGUAGE should take priority over LANG"
        );
    }

    // ── strip_ansi 公共版本 ──────────────────────────────────────────────────

    #[test]
    fn strip_ansi_removes_color_codes() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn strip_ansi_removes_bold_and_reset() {
        assert_eq!(strip_ansi("\x1b[1mbold\x1b[0m text"), "bold text");
    }

    #[test]
    fn strip_ansi_plain_text_unchanged() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn strip_ansi_empty_string() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn strip_ansi_multiple_sequences() {
        let input = "\x1b[32mgreen\x1b[0m and \x1b[34mblue\x1b[0m";
        assert_eq!(strip_ansi(input), "green and blue");
    }

    #[test]
    fn strip_ansi_256_color_sequence() {
        // ESC[38;5;196m 是 256 色前景色，含分号和数字参数
        assert_eq!(strip_ansi("\x1b[38;5;196mtext\x1b[0m"), "text");
    }

    // ── table_headers 中文修正 ───────────────────────────────────────────────

    #[test]
    fn table_headers_weekly_has_chinese_translation() {
        let msg = Messages {
            language: UiLanguage::ZhHans,
        };
        let headers = msg.table_headers();
        // 第 6 列（index 5）不应再是英文 "Weekly"
        assert_ne!(
            headers[5], "Weekly",
            "zh Weekly column should be translated"
        );
        assert_eq!(headers[5], "每周");
    }
}
