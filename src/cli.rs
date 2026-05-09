use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::adapters::codex::{ApiLoginRequest, AutofillRequest, CodexAdapter};
use crate::core::state::{AccountRecord, UsageSnapshot};
use crate::core::storage;
use crate::core::ui;
use crate::core::update;

const POOL_REPO_ENV: &str = "SCODEX_POOL_REPO";

#[derive(Debug, Parser)]
#[command(name = "scodex")]
pub struct Cli {
    #[arg(long)]
    pub state_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Launch(LaunchArgs),
    Auto(AutoArgs),
    Add(AddArgs),
    Login(LoginArgs),
    #[command(visible_alias = "sync")]
    Deploy(DeployArgs),
    Push(RepoSyncArgs),
    Pull(RepoSyncArgs),
    Use(UseArgs),
    Rm(RmArgs),
    List,
    Refresh,
    #[command(visible_alias = "upgrade")]
    Update(UpdateArgs),
    ImportAuth(ImportAuthArgs),
    ImportKnown,
    #[command(external_subcommand)]
    Passthrough(Vec<OsString>),
}

#[derive(Debug, Args)]
pub struct LaunchArgs {
    #[arg(long)]
    pub no_import_known: bool,
    #[arg(long)]
    pub no_login: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub no_resume: bool,
    #[arg(long)]
    pub no_launch: bool,
    #[arg(trailing_var_arg = true)]
    pub extra_args: Vec<OsString>,
}

#[derive(Debug, Args)]
pub struct AutoArgs {
    #[arg(long)]
    pub no_import_known: bool,
    #[arg(long)]
    pub no_login: bool,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    #[command(flatten)]
    pub api_args: ApiArgs,
    #[arg(long)]
    pub oauth: bool,
    #[arg(long)]
    pub username: Option<String>,
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[command(flatten)]
    pub api_args: ApiArgs,
    #[arg(long)]
    pub switch: bool,
}

#[derive(Debug, Args)]
pub struct ApiArgs {
    #[arg(long)]
    pub api: bool,
    #[arg(long = "API_TOKEN")]
    pub api_token: Option<String>,
    #[arg(long = "BASE_URL")]
    pub base_url: Option<String>,
    #[arg(long)]
    pub provider: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeployArgs {
    #[arg(short = 'i', value_name = "IDENTITY_FILE")]
    pub identity_file: Option<PathBuf>,

    pub target: String,
}

#[derive(Debug, Args)]
pub struct RepoSyncArgs {
    #[arg(long, value_name = "REPO_PATH")]
    pub path: Option<String>,

    #[arg(short = 'i', value_name = "IDENTITY_FILE")]
    pub identity_file: Option<PathBuf>,

    pub repo: Option<String>,
}

#[derive(Debug, Args)]
pub struct UseArgs {
    pub email: String,
}

#[derive(Debug, Args)]
pub struct RmArgs {
    #[arg(short = 'y', long = "yes")]
    pub assume_yes: bool,
    pub email: String,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    #[arg(short = 'f', long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ImportAuthArgs {
    pub path: PathBuf,
}

impl Cli {
    pub fn parse_args() -> Self {
        let args = env::args_os().collect::<Vec<_>>();
        if let Some(topic) = requested_help_topic(&args) {
            print!("{}", render_help(topic));
            std::process::exit(0);
        }
        Self::parse()
    }
}

pub fn run(cli: Cli) -> Result<i32> {
    // 迁移旧的二进制文件（从 ~/.local/bin 移到 $SCODEX_HOME/bin）
    let _ = storage::migrate_old_binaries();

    let ui = ui::messages();
    let adapter = CodexAdapter::default();
    let state_dir = storage::resolve_state_dir(cli.state_dir.as_deref())?;
    let mut state = storage::load_state(&state_dir)?;
    if adapter.normalize_account_records(&mut state) {
        storage::save_state(&state_dir, &state)?;
    }
    let command = cli.command.unwrap_or(Command::Launch(LaunchArgs {
        no_import_known: false,
        no_login: false,
        dry_run: false,
        no_resume: false,
        no_launch: false,
        extra_args: Vec::new(),
    }));

    let exit_code = match command {
        Command::Launch(args) => {
            match adapter.ensure_best_account(
                &state_dir,
                &mut state,
                args.no_import_known,
                args.no_login,
                !args.dry_run,
            )? {
                Some((account, usage)) => {
                    if args.dry_run {
                        print_selection(ui.selection_would_select(), &account, &usage);
                        storage::save_state(&state_dir, &state)?;
                        0
                    } else {
                        print_selection(ui.selection_switched(), &account, &usage);
                        storage::save_state(&state_dir, &state)?;
                        if args.no_launch {
                            0
                        } else {
                            adapter.launch_codex(&args.extra_args, !args.no_resume)?
                        }
                    }
                }
                None => {
                    println!("{}", ui.no_usable_account());
                    storage::save_state(&state_dir, &state)?;
                    1
                }
            }
        }
        Command::Auto(args) => {
            match adapter.ensure_best_account(
                &state_dir,
                &mut state,
                args.no_import_known,
                args.no_login,
                !args.dry_run,
            )? {
                Some((account, usage)) => {
                    if args.dry_run {
                        print_selection(ui.selection_would_select(), &account, &usage);
                    } else {
                        print_selection(ui.selection_switched(), &account, &usage);
                    }
                    storage::save_state(&state_dir, &state)?;
                    0
                }
                None => {
                    println!("{}", ui.no_usable_account());
                    storage::save_state(&state_dir, &state)?;
                    1
                }
            }
        }
        Command::Login(args) => {
            let record = if args.api_args.api {
                let request = build_api_login_request(&args.api_args, &ui)?;
                adapter.run_api_key_login(&state_dir, &mut state, request)?
            } else if args.oauth {
                let request = build_autofill_request(&args, &ui)?;
                adapter.run_device_auth_login_autofill(&state_dir, &mut state, request)?
            } else {
                adapter.run_device_auth_login(&state_dir, &mut state)?
            };
            finish_added_account(&adapter, &state_dir, &mut state, &record)?
        }
        Command::Add(args) => {
            let record = if args.api_args.api {
                let request = build_api_login_request(&args.api_args, &ui)?;
                adapter.run_api_key_login(&state_dir, &mut state, request)?
            } else {
                adapter.run_device_auth_login(&state_dir, &mut state)?
            };
            finish_added_account(&adapter, &state_dir, &mut state, &record)?
        }
        Command::Use(args) => {
            adapter.import_known_sources(&state_dir, &mut state);
            let Some(record) = adapter.find_account_by_email(&state, &args.email) else {
                println!("{}", ui.unknown_account(&args.email));
                storage::save_state(&state_dir, &state)?;
                return Ok(1);
            };
            adapter.switch_account(record)?;
            let usage = state
                .usage_cache
                .get(&record.id)
                .cloned()
                .unwrap_or_default();
            print_selection(ui.selection_switched(), record, &usage);
            storage::save_state(&state_dir, &state)?;
            0
        }
        Command::Rm(args) => {
            adapter.import_known_sources(&state_dir, &mut state);
            let Some((id, email)) = adapter
                .find_account_by_email(&state, &args.email)
                .map(|record| (record.id.clone(), record.email.clone()))
            else {
                println!("{}", ui.unknown_account(&args.email));
                storage::save_state(&state_dir, &state)?;
                return Ok(1);
            };
            if !args.assume_yes {
                use std::io::{self, IsTerminal, Write};
                if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                    println!("{}", ui.rm_requires_tty());
                    return Ok(1);
                }
                loop {
                    print!("{}", ui.confirm_rm(&email));
                    let _ = io::stdout().flush();
                    let mut line = String::new();
                    io::stdin().read_line(&mut line)?;
                    match crate::adapters::codex::parse_yes_no(&line) {
                        Some(true) => break,
                        Some(false) => {
                            println!("{}", ui.rm_cancelled());
                            return Ok(0);
                        }
                        None => println!("{}", ui.invalid_yes_no()),
                    }
                }
            }
            adapter.remove_account(&state_dir, &mut state, &id)?;
            storage::save_state(&state_dir, &state)?;
            println!("{}", ui.removed_account(&email));
            0
        }
        Command::Deploy(args) => {
            adapter.deploy_live_auth(&args.target, args.identity_file.as_deref())?;
            0
        }
        Command::Push(args) => {
            let (repo, repo_from_cli) = resolve_repo_for_sync(args.repo.as_deref(), &state, &ui)?;
            persist_repo_from_cli(&state_dir, &mut state, &repo, repo_from_cli)?;
            let outcome = adapter.push_account_pool(
                &state,
                &repo,
                args.path.as_deref(),
                args.identity_file.as_deref(),
            )?;
            if outcome.changed {
                println!(
                    "{}",
                    ui.repo_push_completed(&repo, outcome.exported_accounts)
                );
            } else {
                println!("{}", ui.repo_push_no_changes(&repo));
            }
            0
        }
        Command::Pull(args) => {
            let (repo, repo_from_cli) = resolve_repo_for_sync(args.repo.as_deref(), &state, &ui)?;
            persist_repo_from_cli(&state_dir, &mut state, &repo, repo_from_cli)?;
            let outcome = adapter.pull_account_pool(
                &state_dir,
                &mut state,
                &repo,
                args.path.as_deref(),
                args.identity_file.as_deref(),
            )?;
            storage::save_state(&state_dir, &state)?;
            println!(
                "{}",
                ui.repo_pull_completed(&repo, outcome.imported_accounts)
            );
            adapter.refresh_all_accounts(&mut state);
            storage::save_state(&state_dir, &state)?;
            let active = adapter.read_live_identity();
            println!("{}", adapter.render_account_table(&state, active.as_ref()));
            0
        }
        Command::List => {
            adapter.refresh_all_accounts(&mut state);
            storage::save_state(&state_dir, &state)?;
            let active = adapter.read_live_identity();
            println!("{}", adapter.render_account_table(&state, active.as_ref()));
            0
        }
        Command::Refresh => {
            adapter.refresh_all_accounts(&mut state);
            storage::save_state(&state_dir, &state)?;
            let active = adapter.read_live_identity();
            println!("{}", adapter.render_account_table(&state, active.as_ref()));
            println!("{}", ui.refreshed_accounts(state.accounts.len()));
            0
        }
        Command::Update(args) => {
            let outcome = update::self_update(args.force)?;
            match outcome.status {
                update::UpdateStatus::AlreadyCurrent => {
                    println!(
                        "{}",
                        ui.update_already_current(
                            &outcome.installed_version,
                            &outcome.executable_path
                        )
                    );
                }
                update::UpdateStatus::Updated => {
                    println!(
                        "{}",
                        ui.update_completed(
                            &outcome.previous_version,
                            &outcome.installed_version,
                            &outcome.executable_path
                        )
                    );
                    if cfg!(windows) {
                        println!("{}", ui.restart_terminal_hint());
                    }
                }
            }
            0
        }
        Command::ImportAuth(args) => {
            let record = adapter.import_auth_path(&state_dir, &mut state, &args.path)?;
            storage::save_state(&state_dir, &state)?;
            println!("{}", ui.imported_account(&record.email, &record.id));
            0
        }
        Command::ImportKnown => {
            let imported = adapter.import_known_sources(&state_dir, &mut state);
            if imported.is_empty() {
                println!("{}", ui.no_importable_accounts());
                storage::save_state(&state_dir, &state)?;
                return Ok(1);
            }
            storage::save_state(&state_dir, &state)?;
            for account in imported {
                println!("{}", ui.imported_account(&account.email, &account.id));
            }
            0
        }
        Command::Passthrough(args) => {
            match adapter.ensure_best_account(&state_dir, &mut state, false, false, true)? {
                Some((account, usage)) => {
                    print_selection(ui.selection_switched(), &account, &usage);
                    storage::save_state(&state_dir, &state)?;
                    adapter.run_passthrough(&args)?
                }
                None => {
                    println!("{}", ui.no_usable_account());
                    storage::save_state(&state_dir, &state)?;
                    1
                }
            }
        }
    };

    Ok(exit_code)
}

fn format_percent(value: Option<i64>) -> String {
    let ui = ui::messages();
    value
        .map(|value| format!("{value}%"))
        .unwrap_or_else(|| ui.na().into())
}

fn finish_added_account(
    adapter: &CodexAdapter,
    state_dir: &std::path::Path,
    state: &mut crate::core::state::State,
    record: &AccountRecord,
) -> Result<i32> {
    let ui = ui::messages();
    let usage = adapter.refresh_account_usage(state, record);
    println!("{}", ui.added_account(&record.email));
    adapter.switch_account(record)?;
    print_selection(ui.selection_switched(), record, &usage);
    storage::save_state(state_dir, state)?;
    Ok(0)
}

fn build_autofill_request(args: &LoginArgs, ui: &ui::Messages) -> Result<AutofillRequest> {
    if args.api_args.api {
        anyhow::bail!("{}", ui.login_mode_conflict());
    }
    match (args.username.as_deref(), args.password.as_deref()) {
        (Some(email), Some(password)) if !email.trim().is_empty() && !password.is_empty() => {
            Ok(AutofillRequest {
                email: email.trim().to_string(),
                password: password.to_string(),
            })
        }
        _ => anyhow::bail!("{}", ui.login_autofill_missing_credentials()),
    }
}

fn build_api_login_request(args: &ApiArgs, ui: &ui::Messages) -> Result<ApiLoginRequest> {
    let Some(api_token) = args.api_token.as_deref().map(str::trim) else {
        anyhow::bail!("{}", ui.login_api_missing_credentials());
    };
    let Some(base_url) = args.base_url.as_deref().map(str::trim) else {
        anyhow::bail!("{}", ui.login_api_missing_credentials());
    };
    let Some(provider) = args.provider.as_deref().map(str::trim) else {
        anyhow::bail!("{}", ui.login_api_missing_credentials());
    };

    let display_body = api_token.strip_prefix("sk-").unwrap_or(api_token);
    if display_body.chars().count() < 8 || base_url.is_empty() || provider.is_empty() {
        anyhow::bail!("{}", ui.login_api_missing_credentials());
    }

    Ok(ApiLoginRequest {
        api_token: api_token.to_string(),
        base_url: base_url.to_string(),
        provider: provider.to_ascii_lowercase(),
    })
}

fn resolve_repo_for_sync(
    cli_repo: Option<&str>,
    state: &crate::core::state::State,
    ui: &ui::Messages,
) -> Result<(String, bool)> {
    let cli_repo = cli_repo
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let env_repo = configured_repo_from_env();
    let stored_repo = state.repo_sync.pool_repo.as_deref();
    let resolved = resolve_repo_source(cli_repo.as_deref(), env_repo.as_deref(), stored_repo);
    let Some(repo) = resolved else {
        anyhow::bail!("{}", ui.repo_sync_missing_repo(POOL_REPO_ENV));
    };
    Ok((repo.to_string(), cli_repo.as_deref() == Some(repo)))
}

fn resolve_repo_source<'a>(
    cli_repo: Option<&'a str>,
    env_repo: Option<&'a str>,
    stored_repo: Option<&'a str>,
) -> Option<&'a str> {
    cli_repo
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| env_repo.map(str::trim).filter(|value| !value.is_empty()))
        .or_else(|| stored_repo.map(str::trim).filter(|value| !value.is_empty()))
}

fn configured_repo_from_env() -> Option<String> {
    env::var(POOL_REPO_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn persist_repo_from_cli(
    state_dir: &std::path::Path,
    state: &mut crate::core::state::State,
    repo: &str,
    repo_from_cli: bool,
) -> Result<()> {
    if !repo_from_cli {
        return Ok(());
    }

    if state.repo_sync.pool_repo.as_deref() == Some(repo) {
        return Ok(());
    }
    state.repo_sync.pool_repo = Some(repo.to_string());
    storage::save_state(state_dir, state)?;
    Ok(())
}

fn print_selection(prefix: &str, account: &AccountRecord, usage: &UsageSnapshot) {
    println!(
        "{} {} [weekly={}, 5h={}]",
        prefix,
        account.email,
        format_percent(usage.weekly_remaining_percent),
        format_percent(usage.five_hour_remaining_percent),
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTopic {
    Root,
    Launch,
    Auto,
    Add,
    Login,
    Deploy,
    Push,
    Pull,
    Use,
    Rm,
    List,
    Refresh,
    Update,
    ImportAuth,
    ImportKnown,
}

fn requested_help_topic(args: &[OsString]) -> Option<HelpTopic> {
    let tokens = args
        .iter()
        .skip(1)
        .map(|item| item.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let first = tokens.first()?.as_str();

    if matches!(first, "-h" | "--help") {
        return Some(HelpTopic::Root);
    }

    if first == "help" {
        return tokens
            .get(1)
            .and_then(|item| command_help_topic(item))
            .or(Some(HelpTopic::Root));
    }

    let topic = command_help_topic(first)?;
    if tokens
        .iter()
        .skip(1)
        .any(|item| item == "-h" || item == "--help")
    {
        Some(topic)
    } else {
        None
    }
}

fn command_help_topic(name: &str) -> Option<HelpTopic> {
    match name {
        "launch" => Some(HelpTopic::Launch),
        "auto" => Some(HelpTopic::Auto),
        "add" => Some(HelpTopic::Add),
        "login" => Some(HelpTopic::Login),
        "deploy" | "sync" => Some(HelpTopic::Deploy),
        "push" => Some(HelpTopic::Push),
        "pull" => Some(HelpTopic::Pull),
        "use" => Some(HelpTopic::Use),
        "rm" => Some(HelpTopic::Rm),
        "list" => Some(HelpTopic::List),
        "refresh" => Some(HelpTopic::Refresh),
        "update" | "upgrade" => Some(HelpTopic::Update),
        "import-auth" => Some(HelpTopic::ImportAuth),
        "import-known" => Some(HelpTopic::ImportKnown),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 帮助文本表驱动实现
// ---------------------------------------------------------------------------

/// 一条帮助条目（参数/选项/环境变量行）。
/// `name` 是标志/变量名，不需要翻译；`desc` 是 (en, zh) 说明。
#[derive(Copy, Clone)]
struct HelpItem {
    name: &'static str,
    desc: (&'static str, &'static str),
}

/// 每个 HelpTopic 对应一个 HelpEntry，持有 (en, zh) 文本对。
struct HelpEntry {
    /// 每条 usage 行，如有多条（别名）则多个元素。
    usage_lines: &'static [(&'static str, &'static str)],
    /// 可选的描述段落，在 usage 和 options 之间输出。
    description: Option<(&'static str, &'static str)>,
    /// Arguments 段条目。
    args: &'static [HelpItem],
    /// Options 段条目（每个 topic 的 -h/--help 已内置，不需要在表里重复）。
    options: &'static [HelpItem],
    /// Environment 段条目。
    env: &'static [HelpItem],
    /// Commands 段条目（仅 Root 使用）。
    commands: &'static [HelpItem],
}

// Root 命令列表
static ROOT_COMMANDS: &[HelpItem] = &[
    HelpItem {
        name: "launch",
        desc: (
            "Switch to the best account and launch or resume Codex",
            "切换到最佳账号，并启动或恢复 Codex",
        ),
    },
    HelpItem {
        name: "auto",
        desc: (
            "Switch to the best account without launching Codex",
            "切换到最佳账号，但不启动 Codex",
        ),
    },
    HelpItem {
        name: "add",
        desc: ("Add one account and switch to it", "新增一个账号并切换"),
    },
    HelpItem {
        name: "login",
        desc: (
            "Add one account through device auth",
            "通过设备登录新增一个账号",
        ),
    },
    HelpItem {
        name: "deploy",
        desc: (
            "Copy the current auth.json to a remote machine [alias: sync]",
            "把当前 auth.json 复制到远端机器 [别名：sync]",
        ),
    },
    HelpItem {
        name: "push",
        desc: (
            "Push the local account pool into a Git repository",
            "把本地账号池推送到 Git 仓库",
        ),
    },
    HelpItem {
        name: "pull",
        desc: (
            "Pull an account pool from a Git repository",
            "从 Git 仓库拉取账号池",
        ),
    },
    HelpItem {
        name: "use",
        desc: (
            "Switch directly to a known account by email",
            "按邮箱直接切换到一个已知账号",
        ),
    },
    HelpItem {
        name: "rm",
        desc: (
            "Remove a stored account by email",
            "按邮箱删除一个已保存的账号",
        ),
    },
    HelpItem {
        name: "list",
        desc: ("Show the latest account quotas", "显示最新账号额度"),
    },
    HelpItem {
        name: "refresh",
        desc: (
            "Refresh live usage for all known accounts",
            "刷新所有已知账号的实时额度",
        ),
    },
    HelpItem {
        name: "update",
        desc: (
            "Self-update scodex [alias: upgrade]",
            "自更新 scodex [别名：upgrade]",
        ),
    },
    HelpItem {
        name: "import-auth",
        desc: (
            "Import an auth.json file or home directory",
            "导入 auth.json 文件或其所在 home 目录",
        ),
    },
    HelpItem {
        name: "import-known",
        desc: (
            "Import the default known auth sources",
            "导入默认已知认证来源",
        ),
    },
    HelpItem {
        name: "help",
        desc: (
            "Print this message or the help of the given subcommand(s)",
            "显示帮助",
        ),
    },
];

// Root 全局选项
static ROOT_OPTIONS: &[HelpItem] = &[
    HelpItem {
        name: "      --state-dir <STATE_DIR>",
        desc: ("Override the local state directory", "覆盖本地状态目录"),
    },
    HelpItem {
        name: "  -h, --help                  ",
        desc: ("Print help", "显示帮助"),
    },
];

// add 命令选项
static ADD_OPTIONS: &[HelpItem] = &[
    HelpItem {
        name: "      --switch              ",
        desc: (
            "Deprecated compatibility option; add always switches now",
            "兼容旧用法的保留选项；当前 add 总是会切换",
        ),
    },
    HelpItem {
        name: "      --api                ",
        desc: (
            "Add an API-key account; requires --API_TOKEN, --BASE_URL, and --provider",
            "新增 API key 账号，需要同时传入 --API_TOKEN、--BASE_URL 和 --provider",
        ),
    },
    HelpItem {
        name: "      --API_TOKEN <TOKEN>  ",
        desc: (
            "API token used when --api is set",
            "--api 模式下使用的 API token",
        ),
    },
    HelpItem {
        name: "      --BASE_URL <URL>     ",
        desc: (
            "API base URL used when --api is set",
            "--api 模式下使用的 API base URL",
        ),
    },
    HelpItem {
        name: "      --provider <NAME>    ",
        desc: (
            "Provider id used when --api is set",
            "--api 模式下使用的 provider id",
        ),
    },
    HelpItem {
        name: "  -h, --help               ",
        desc: ("Print help", "显示帮助"),
    },
];

// login 命令选项
static LOGIN_OPTIONS: &[HelpItem] = &[
    HelpItem {
        name: "      --api                ",
        desc: (
            "Add an API-key account; requires --API_TOKEN, --BASE_URL, and --provider",
            "新增 API key 账号，需要同时传入 --API_TOKEN、--BASE_URL 和 --provider",
        ),
    },
    HelpItem {
        name: "      --API_TOKEN <TOKEN>  ",
        desc: (
            "API token used when --api is set",
            "--api 模式下使用的 API token",
        ),
    },
    HelpItem {
        name: "      --BASE_URL <URL>     ",
        desc: (
            "API base URL used when --api is set",
            "--api 模式下使用的 API base URL",
        ),
    },
    HelpItem {
        name: "      --provider <NAME>    ",
        desc: (
            "Provider id used when --api is set",
            "--api 模式下使用的 provider id",
        ),
    },
    HelpItem {
        name: "      --oauth              ",
        desc: (
            "Use the browser OAuth flow with auto-fill; requires --username and --password",
            "使用浏览器 OAuth 流程并自动填充，需要同时传入 --username 和 --password",
        ),
    },
    HelpItem {
        name: "      --username <EMAIL>   ",
        desc: ("Email used when --oauth is set", "--oauth 模式下使用的邮箱"),
    },
    HelpItem {
        name: "      --password <PASS>    ",
        desc: (
            "Password used when --oauth is set (visible in ps; scope to trusted shells)",
            "--oauth 模式下使用的密码（会出现在 ps 中，建议仅在可信 shell 使用）",
        ),
    },
    HelpItem {
        name: "  -h, --help               ",
        desc: ("Print help", "显示帮助"),
    },
];

// push/pull 共享选项
static REPO_SYNC_OPTIONS: &[HelpItem] = &[
    HelpItem {
        name: "      --path <REPO_PATH>",
        desc: (
            "Repository subdirectory used for the account pool",
            "仓库内用于保存账号池的子目录",
        ),
    },
    HelpItem {
        name: "  -i <IDENTITY_FILE>    ",
        desc: (
            "SSH private key passed to git via GIT_SSH_COMMAND",
            "通过 GIT_SSH_COMMAND 传给 git 的 SSH 私钥",
        ),
    },
    HelpItem {
        name: "  -h, --help            ",
        desc: ("Print help", "显示帮助"),
    },
];

static PUSH_ENV: &[HelpItem] = &[
    HelpItem {
        name: "SCODEX_POOL_KEY ",
        desc: (
            "Symmetric key source for encrypting the account pool",
            "用于加密账号池的对称密钥来源",
        ),
    },
    HelpItem {
        name: "SCODEX_POOL_PATH",
        desc: (
            "Repository subdirectory used for the account pool when --path is omitted",
            "未传 --path 时，仓库内账号池子目录来源",
        ),
    },
    HelpItem {
        name: "SCODEX_POOL_REPO",
        desc: (
            "Repository URL/path used when [REPO] is omitted",
            "未传 [REPO] 时，账号池仓库 URL/路径来源",
        ),
    },
];

static PULL_ENV: &[HelpItem] = &[
    HelpItem {
        name: "SCODEX_POOL_KEY ",
        desc: (
            "Symmetric key source for decrypting the account pool",
            "用于解密账号池的对称密钥来源",
        ),
    },
    HelpItem {
        name: "SCODEX_POOL_PATH",
        desc: (
            "Repository subdirectory used for the account pool when --path is omitted",
            "未传 --path 时，仓库内账号池子目录来源",
        ),
    },
    HelpItem {
        name: "SCODEX_POOL_REPO",
        desc: (
            "Repository URL/path used when [REPO] is omitted",
            "未传 [REPO] 时，账号池仓库 URL/路径来源",
        ),
    },
];

static REPO_SYNC_ARGS: &[HelpItem] = &[HelpItem {
    name: "  [REPO]",
    desc: (
        "Git remote URL or local repository path (CLI > SCODEX_POOL_REPO > local state)",
        "Git 远端 URL 或本地仓库路径（优先级：命令行 > SCODEX_POOL_REPO > 本地状态）",
    ),
}];

/// 根据 topic 获取对应的 HelpEntry。
fn help_entry(topic: HelpTopic) -> HelpEntry {
    match topic {
        HelpTopic::Root => HelpEntry {
            usage_lines: &[("  scodex [OPTIONS] [COMMAND]", "  scodex [选项] [命令]")],
            description: None,
            args: &[],
            options: ROOT_OPTIONS,
            env: &[],
            commands: ROOT_COMMANDS,
        },
        HelpTopic::Launch => HelpEntry {
            usage_lines: &[(
                "  scodex launch [OPTIONS] [<codex args...>]",
                "  scodex launch [选项] [<codex 参数...>]",
            )],
            description: None,
            args: &[],
            options: &[
                HelpItem {
                    name: "      --no-import-known",
                    desc: (
                        "Skip auto-import of known auth sources",
                        "跳过自动导入已知认证来源",
                    ),
                },
                HelpItem {
                    name: "      --no-login        ",
                    desc: (
                        "Do not start device auth when no usable account exists",
                        "当没有可用账号时，不自动发起设备登录",
                    ),
                },
                HelpItem {
                    name: "      --dry-run         ",
                    desc: (
                        "Show the selected account without switching or launching",
                        "只显示会选中的账号",
                    ),
                },
                HelpItem {
                    name: "      --no-resume       ",
                    desc: ("Always start a fresh Codex session", "总是新开 Codex 会话"),
                },
                HelpItem {
                    name: "      --no-launch       ",
                    desc: (
                        "Switch the account but do not start Codex",
                        "只切换账号，不启动 Codex",
                    ),
                },
                HelpItem {
                    name: "  -h, --help            ",
                    desc: ("Print help", "显示帮助"),
                },
            ],
            env: &[],
            commands: &[],
        },
        HelpTopic::Auto => HelpEntry {
            usage_lines: &[("  scodex auto [OPTIONS]", "  scodex auto [选项]")],
            description: None,
            args: &[],
            options: &[
                HelpItem {
                    name: "      --no-import-known",
                    desc: (
                        "Skip auto-import of known auth sources",
                        "跳过自动导入已知认证来源",
                    ),
                },
                HelpItem {
                    name: "      --no-login        ",
                    desc: (
                        "Do not start device auth when no usable account exists",
                        "当没有可用账号时，不自动发起设备登录",
                    ),
                },
                HelpItem {
                    name: "      --dry-run         ",
                    desc: (
                        "Show the selected account without switching",
                        "只显示会选中的账号，不执行切换",
                    ),
                },
                HelpItem {
                    name: "  -h, --help            ",
                    desc: ("Print help", "显示帮助"),
                },
            ],
            env: &[],
            commands: &[],
        },
        HelpTopic::Add => HelpEntry {
            usage_lines: &[("  scodex add [OPTIONS]", "  scodex add [选项]")],
            description: Some((
                "Adds one account and switches to it.",
                "新增一个账号，并立即切换到该账号。",
            )),
            args: &[],
            options: ADD_OPTIONS,
            env: &[],
            commands: &[],
        },
        HelpTopic::Login => HelpEntry {
            usage_lines: &[("  scodex login [OPTIONS]", "  scodex login [选项]")],
            description: None,
            args: &[],
            options: LOGIN_OPTIONS,
            env: &[],
            commands: &[],
        },
        HelpTopic::Deploy => HelpEntry {
            usage_lines: &[
                (
                    "  scodex deploy [OPTIONS] <TARGET>",
                    "  scodex deploy [选项] <TARGET>",
                ),
                (
                    "  scodex sync [OPTIONS] <TARGET>",
                    "  scodex sync [选项] <TARGET>",
                ),
            ],
            description: None,
            args: &[HelpItem {
                name: "  <TARGET>",
                desc: (
                    "Remote destination in the form user@host:/target_path",
                    "远端目标，格式为 user@host:/target_path",
                ),
            }],
            options: &[
                HelpItem {
                    name: "  -i <IDENTITY_FILE>",
                    desc: (
                        "Pass an SSH identity file to ssh/scp",
                        "传给 ssh/scp 的 SSH 身份文件",
                    ),
                },
                HelpItem {
                    name: "  -h, --help        ",
                    desc: ("Print help", "显示帮助"),
                },
            ],
            env: &[],
            commands: &[],
        },
        HelpTopic::Push => HelpEntry {
            usage_lines: &[(
                "  scodex push [OPTIONS] [REPO]",
                "  scodex push [选项] [REPO]",
            )],
            description: None,
            args: REPO_SYNC_ARGS,
            options: REPO_SYNC_OPTIONS,
            env: PUSH_ENV,
            commands: &[],
        },
        HelpTopic::Pull => HelpEntry {
            usage_lines: &[(
                "  scodex pull [OPTIONS] [REPO]",
                "  scodex pull [选项] [REPO]",
            )],
            description: None,
            args: REPO_SYNC_ARGS,
            options: REPO_SYNC_OPTIONS,
            env: PULL_ENV,
            commands: &[],
        },
        HelpTopic::Use => HelpEntry {
            usage_lines: &[("  scodex use <EMAIL>", "  scodex use <EMAIL>")],
            description: None,
            args: &[HelpItem {
                name: "  <EMAIL>",
                desc: ("Account email to switch to", "要切换到的账号邮箱"),
            }],
            options: &[HelpItem {
                name: "  -h, --help",
                desc: ("Print help", "显示帮助"),
            }],
            env: &[],
            commands: &[],
        },
        HelpTopic::Rm => HelpEntry {
            usage_lines: &[(
                "  scodex rm [OPTIONS] <EMAIL>",
                "  scodex rm [选项] <EMAIL>",
            )],
            description: None,
            args: &[HelpItem {
                name: "  <EMAIL>",
                desc: ("Account email to remove", "要删除的账号邮箱"),
            }],
            options: &[
                HelpItem {
                    name: "  -y, --yes  ",
                    desc: (
                        "Skip the interactive confirmation prompt",
                        "跳过交互式二次确认",
                    ),
                },
                HelpItem {
                    name: "  -h, --help ",
                    desc: ("Print help", "显示帮助"),
                },
            ],
            env: &[],
            commands: &[],
        },
        HelpTopic::List => HelpEntry {
            usage_lines: &[("  scodex list", "  scodex list")],
            description: None,
            args: &[],
            options: &[HelpItem {
                name: "  -h, --help",
                desc: ("Print help", "显示帮助"),
            }],
            env: &[],
            commands: &[],
        },
        HelpTopic::Refresh => HelpEntry {
            usage_lines: &[("  scodex refresh", "  scodex refresh")],
            description: None,
            args: &[],
            options: &[HelpItem {
                name: "  -h, --help",
                desc: ("Print help", "显示帮助"),
            }],
            env: &[],
            commands: &[],
        },
        HelpTopic::Update => HelpEntry {
            usage_lines: &[
                ("  scodex update [OPTIONS]", "  scodex update [选项]"),
                ("  scodex upgrade [OPTIONS]", "  scodex upgrade [选项]"),
            ],
            description: None,
            args: &[],
            options: &[
                HelpItem {
                    name: "  -f, --force",
                    desc: (
                        "Reinstall even when the current version is already latest",
                        "即使当前版本已经最新，也强制重新安装",
                    ),
                },
                HelpItem {
                    name: "  -h, --help ",
                    desc: ("Print help", "显示帮助"),
                },
            ],
            env: &[],
            commands: &[],
        },
        HelpTopic::ImportAuth => HelpEntry {
            usage_lines: &[("  scodex import-auth <PATH>", "  scodex import-auth <PATH>")],
            description: None,
            args: &[HelpItem {
                name: "  <PATH>",
                desc: (
                    "Path to an auth.json file or a home directory containing it",
                    "auth.json 文件路径，或包含该文件的 home 目录",
                ),
            }],
            options: &[HelpItem {
                name: "  -h, --help",
                desc: ("Print help", "显示帮助"),
            }],
            env: &[],
            commands: &[],
        },
        HelpTopic::ImportKnown => HelpEntry {
            usage_lines: &[("  scodex import-known", "  scodex import-known")],
            description: None,
            args: &[],
            options: &[HelpItem {
                name: "  -h, --help",
                desc: ("Print help", "显示帮助"),
            }],
            env: &[],
            commands: &[],
        },
    }
}

/// 统一渲染入口：is_zh 在此分支一次，下游渲染器用 pick() 取文本。
fn render_help(topic: HelpTopic) -> String {
    render_help_with_lang(topic, ui::messages().is_zh())
}

/// 实际渲染逻辑，接受显式语言参数（方便测试）。
fn render_help_with_lang(topic: HelpTopic, is_zh: bool) -> String {
    let entry = help_entry(topic);

    // pick 从 (en, zh) 对中按语言选文本
    let pick = |pair: (&'static str, &'static str)| if is_zh { pair.1 } else { pair.0 };

    let mut out = String::new();

    // Root topic 在 usage 前先输出 about 行
    if topic == HelpTopic::Root {
        // about 文本本身已按语言给出，复用 ui::messages 的翻译
        let about = if is_zh {
            "面向代理 CLI 的跨平台账号感知启动器。"
        } else {
            "Cross-platform account-aware launcher for agent CLIs."
        };
        out.push_str(about);
        out.push('\n');
        out.push('\n');
    }

    // Usage / 用法
    let usage_label = if is_zh { "用法：" } else { "Usage:" };
    out.push_str(usage_label);
    out.push('\n');
    for &line in entry.usage_lines {
        out.push_str(pick(line));
        out.push('\n');
    }

    // Description（可选段落）
    if let Some(desc) = entry.description {
        out.push('\n');
        out.push_str(pick(desc));
        out.push('\n');
    }

    // Commands（仅 Root）
    if !entry.commands.is_empty() {
        out.push('\n');
        let label = if is_zh { "命令：" } else { "Commands:" };
        out.push_str(label);
        out.push('\n');
        for item in entry.commands {
            out.push_str(&format!("  {:<12} {}\n", item.name, pick(item.desc)));
        }
    }

    // Arguments
    if !entry.args.is_empty() {
        out.push('\n');
        let label = if is_zh { "参数：" } else { "Arguments:" };
        out.push_str(label);
        out.push('\n');
        for item in entry.args {
            out.push_str(&format!("{}  {}\n", item.name, pick(item.desc)));
        }
    }

    // Options / 选项
    if !entry.options.is_empty() {
        out.push('\n');
        let label = if is_zh { "选项：" } else { "Options:" };
        out.push_str(label);
        out.push('\n');
        for item in entry.options {
            out.push_str(&format!("{}  {}\n", item.name, pick(item.desc)));
        }
    }

    // Environment / 环境变量
    if !entry.env.is_empty() {
        let label = if is_zh {
            "环境变量："
        } else {
            "Environment:"
        };
        out.push_str(label);
        out.push('\n');
        for item in entry.env {
            out.push_str(&format!("  {}  {}\n", item.name, pick(item.desc)));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Command, HelpTopic, render_help_with_lang, resolve_repo_source};

    #[test]
    fn add_supports_api_options() {
        let cli = Cli::try_parse_from([
            "scodex",
            "add",
            "--api",
            "--API_TOKEN",
            "sk-abcdef123456wxyz",
            "--BASE_URL",
            "https://example.com/v1",
            "--provider",
            "openrouter",
        ])
        .expect("add --api should parse");

        let Command::Add(args) = cli.command.expect("subcommand should exist") else {
            panic!("expected add command");
        };
        assert!(args.api_args.api);
        assert_eq!(
            args.api_args.api_token.as_deref(),
            Some("sk-abcdef123456wxyz")
        );
        assert_eq!(
            args.api_args.base_url.as_deref(),
            Some("https://example.com/v1")
        );
        assert_eq!(args.api_args.provider.as_deref(), Some("openrouter"));
    }

    #[test]
    fn push_allows_optional_repo_argument() {
        let cli = Cli::try_parse_from(["scodex", "push"]).expect("push without repo should parse");
        let Command::Push(args) = cli.command.expect("subcommand should exist") else {
            panic!("expected push command");
        };
        assert!(args.repo.is_none());
    }

    #[test]
    fn repo_source_prefers_cli_over_env_and_state() {
        assert_eq!(
            resolve_repo_source(
                Some("git@cli.example:pool.git"),
                Some("git@env.example:pool.git"),
                Some("git@state.example:pool.git")
            ),
            Some("git@cli.example:pool.git")
        );
    }

    #[test]
    fn repo_source_prefers_env_over_state_when_cli_missing() {
        assert_eq!(
            resolve_repo_source(
                None,
                Some("git@env.example:pool.git"),
                Some("git@state.example:pool.git")
            ),
            Some("git@env.example:pool.git")
        );
    }

    #[test]
    fn repo_source_uses_state_when_cli_and_env_missing() {
        assert_eq!(
            resolve_repo_source(None, None, Some("git@state.example:pool.git")),
            Some("git@state.example:pool.git")
        );
    }

    #[test]
    fn repo_source_ignores_blank_values() {
        assert_eq!(resolve_repo_source(Some("  "), Some(""), Some("   ")), None);
    }

    // help 渲染快照测试：English（直接传入 is_zh=false，不依赖环境变量）
    #[test]
    fn help_render_root_en_contains_key_sections() {
        let out = render_help_with_lang(HelpTopic::Root, false);
        assert!(out.contains("Usage:"), "should contain 'Usage:' header");
        assert!(
            out.contains("Commands:"),
            "should contain 'Commands:' header"
        );
        assert!(out.contains("Options:"), "should contain 'Options:' header");
        assert!(out.contains("launch"), "should list 'launch' command");
        assert!(
            out.contains("--state-dir"),
            "should list --state-dir option"
        );
        assert!(
            !out.contains("用法"),
            "EN output must not contain Chinese text"
        );
    }

    #[test]
    fn help_render_push_en_contains_environment_section() {
        let out = render_help_with_lang(HelpTopic::Push, false);
        assert!(out.contains("Usage:"));
        assert!(out.contains("Arguments:"));
        assert!(out.contains("Options:"));
        assert!(out.contains("Environment:"));
        assert!(out.contains("SCODEX_POOL_KEY"));
        assert!(out.contains("SCODEX_POOL_REPO"));
    }

    // help 渲染快照测试：Chinese（直接传入 is_zh=true，不依赖环境变量）
    #[test]
    fn help_render_root_zh_contains_key_sections() {
        let out = render_help_with_lang(HelpTopic::Root, true);
        assert!(out.contains("用法："), "should contain '用法：' header");
        assert!(out.contains("命令："), "should contain '命令：' header");
        assert!(out.contains("选项："), "should contain '选项：' header");
        assert!(out.contains("launch"), "command names are not translated");
        assert!(out.contains("覆盖本地状态目录"), "zh option desc present");
        assert!(
            !out.contains("Usage:"),
            "zh output must not contain EN header"
        );
    }

    #[test]
    fn help_render_push_zh_contains_env_section() {
        let out = render_help_with_lang(HelpTopic::Push, true);
        assert!(out.contains("用法："));
        assert!(out.contains("参数："));
        assert!(out.contains("选项："));
        assert!(out.contains("环境变量："));
        assert!(out.contains("SCODEX_POOL_KEY"));
        assert!(out.contains("用于加密账号池"));
    }
}
