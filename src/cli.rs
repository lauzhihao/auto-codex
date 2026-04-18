use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::adapters::codex::{AutofillRequest, CodexAdapter};
use crate::core::state::{AccountRecord, UsageSnapshot};
use crate::core::storage;
use crate::core::ui;
use crate::core::update;

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
    #[arg(long)]
    pub oauth: bool,
    #[arg(long)]
    pub username: Option<String>,
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(long)]
    pub switch: bool,
}

#[derive(Debug, Args)]
pub struct DeployArgs {
    #[arg(short = 'i', value_name = "IDENTITY_FILE")]
    pub identity_file: Option<PathBuf>,

    pub target: String,
}

#[derive(Debug, Args)]
pub struct RepoSyncArgs {
    #[arg(long, default_value = ".scodex-account-pool", value_name = "REPO_PATH")]
    pub path: String,

    pub repo: String,
}

#[derive(Debug, Args)]
pub struct UseArgs {
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
    let ui = ui::messages();
    let adapter = CodexAdapter::default();
    let state_dir = storage::resolve_state_dir(cli.state_dir.as_deref())?;
    let mut state = storage::load_state(&state_dir)?;
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
            let record = if args.oauth {
                let request = build_autofill_request(&args, &ui)?;
                adapter.run_device_auth_login_autofill(&state_dir, &mut state, request)?
            } else {
                adapter.run_device_auth_login(&state_dir, &mut state)?
            };
            let usage = adapter.refresh_account_usage(&mut state, &record);
            println!("{}", ui.added_account(&record.email));
            adapter.switch_account(&record)?;
            print_selection(ui.selection_switched(), &record, &usage);
            storage::save_state(&state_dir, &state)?;
            0
        }
        Command::Add(args) => {
            let record = adapter.add_account_via_browser(&state_dir, &mut state)?;
            let usage = adapter.refresh_account_usage(&mut state, &record);
            println!("{}", ui.added_account(&record.email));
            if args.switch {
                adapter.switch_account(&record)?;
                print_selection(ui.selection_switched(), &record, &usage);
            }
            storage::save_state(&state_dir, &state)?;
            0
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
        Command::Deploy(args) => {
            adapter.deploy_live_auth(&args.target, args.identity_file.as_deref())?;
            0
        }
        Command::Push(args) => {
            let outcome = adapter.push_account_pool(&state, &args.repo, Some(&args.path))?;
            if outcome.changed {
                println!(
                    "{}",
                    ui.repo_push_completed(&args.repo, outcome.exported_accounts)
                );
            } else {
                println!("{}", ui.repo_push_no_changes(&args.repo));
            }
            0
        }
        Command::Pull(args) => {
            let outcome =
                adapter.pull_account_pool(&state_dir, &mut state, &args.repo, Some(&args.path))?;
            storage::save_state(&state_dir, &state)?;
            println!(
                "{}",
                ui.repo_pull_completed(&args.repo, outcome.imported_accounts)
            );
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

fn build_autofill_request(args: &LoginArgs, ui: &ui::Messages) -> Result<AutofillRequest> {
    match (args.username.as_deref(), args.password.as_deref()) {
        (Some(email), Some(password))
            if !email.trim().is_empty() && !password.is_empty() =>
        {
            Ok(AutofillRequest {
                email: email.trim().to_string(),
                password: password.to_string(),
            })
        }
        _ => anyhow::bail!("{}", ui.login_autofill_missing_credentials()),
    }
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
        "list" => Some(HelpTopic::List),
        "refresh" => Some(HelpTopic::Refresh),
        "update" | "upgrade" => Some(HelpTopic::Update),
        "import-auth" => Some(HelpTopic::ImportAuth),
        "import-known" => Some(HelpTopic::ImportKnown),
        _ => None,
    }
}

fn render_help(topic: HelpTopic) -> String {
    let ui = ui::messages();
    if ui.is_zh() {
        render_help_zh(topic)
    } else {
        render_help_en(topic)
    }
}

fn render_help_en(topic: HelpTopic) -> String {
    let mut out = String::new();
    match topic {
        HelpTopic::Root => {
            writeln!(&mut out, "{}", ui::messages().cli_about()).unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex [OPTIONS] [COMMAND]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Commands:").unwrap();
            writeln!(
                &mut out,
                "  launch       Switch to the best account and launch or resume Codex"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  auto         Switch to the best account without launching Codex"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  add          Open the signup page, then add one account"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  login        Add one account through device auth"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  deploy       Copy the current auth.json to a remote machine [alias: sync]"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  push         Push the local account pool into a Git repository"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  pull         Pull an account pool from a Git repository"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  use          Switch directly to a known account by email"
            )
            .unwrap();
            writeln!(&mut out, "  list         Show the latest account quotas").unwrap();
            writeln!(
                &mut out,
                "  refresh      Refresh live usage for all known accounts"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  update       Self-update scodex [alias: upgrade]"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  import-auth  Import an auth.json file or home directory"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  import-known Import the default known auth sources"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  help         Print this message or the help of the given subcommand(s)"
            )
            .unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(
                &mut out,
                "      --state-dir <STATE_DIR>  Override the local state directory"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help                   Print help").unwrap();
        }
        HelpTopic::Launch => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex launch [OPTIONS] [<codex args...>]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(
                &mut out,
                "      --no-import-known  Skip auto-import of known auth sources"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --no-login         Do not start device auth when no usable account exists"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --dry-run          Show the selected account without switching or launching"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --no-resume        Always start a fresh Codex session"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --no-launch        Switch the account but do not start Codex"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help             Print help").unwrap();
        }
        HelpTopic::Auto => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex auto [OPTIONS]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(
                &mut out,
                "      --no-import-known  Skip auto-import of known auth sources"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --no-login         Do not start device auth when no usable account exists"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --dry-run          Show the selected account without switching"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help             Print help").unwrap();
        }
        HelpTopic::Add => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex add [OPTIONS]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(
                &mut out,
                "      --switch  Switch to the newly added account after signup/login"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help    Print help").unwrap();
        }
        HelpTopic::Login => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex login [OPTIONS]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(
                &mut out,
                "      --oauth              Use the browser OAuth flow with auto-fill; requires --username and --password"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --username <EMAIL>   Email used when --oauth is set"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --password <PASS>    Password used when --oauth is set (visible in ps; scope to trusted shells)"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help               Print help").unwrap();
        }
        HelpTopic::Deploy => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex deploy [OPTIONS] <TARGET>").unwrap();
            writeln!(&mut out, "  scodex sync [OPTIONS] <TARGET>").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Arguments:").unwrap();
            writeln!(
                &mut out,
                "  <TARGET>  Remote destination in the form user@host:/target_path"
            )
            .unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(
                &mut out,
                "  -i <IDENTITY_FILE>  Pass an SSH identity file to ssh/scp"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help          Print help").unwrap();
        }
        HelpTopic::Push => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex push [OPTIONS] <REPO>").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Arguments:").unwrap();
            writeln!(
                &mut out,
                "  <REPO>  Git remote URL or local repository path"
            )
            .unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(
                &mut out,
                "      --path <REPO_PATH>  Repository subdirectory used for the account pool"
            )
            .unwrap();
            writeln!(
                &mut out,
                "Environment:"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  SCODEX_POOL_KEY  Symmetric key source for encrypting the account pool"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help            Print help").unwrap();
        }
        HelpTopic::Pull => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex pull [OPTIONS] <REPO>").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Arguments:").unwrap();
            writeln!(
                &mut out,
                "  <REPO>  Git remote URL or local repository path"
            )
            .unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(
                &mut out,
                "      --path <REPO_PATH>  Repository subdirectory used for the account pool"
            )
            .unwrap();
            writeln!(
                &mut out,
                "Environment:"
            )
            .unwrap();
            writeln!(
                &mut out,
                "  SCODEX_POOL_KEY  Symmetric key source for decrypting the account pool"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help            Print help").unwrap();
        }
        HelpTopic::Use => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex use <EMAIL>").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Arguments:").unwrap();
            writeln!(&mut out, "  <EMAIL>  Account email to switch to").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(&mut out, "  -h, --help  Print help").unwrap();
        }
        HelpTopic::List => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex list").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(&mut out, "  -h, --help  Print help").unwrap();
        }
        HelpTopic::Refresh => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex refresh").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(&mut out, "  -h, --help  Print help").unwrap();
        }
        HelpTopic::Update => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex update [OPTIONS]").unwrap();
            writeln!(&mut out, "  scodex upgrade [OPTIONS]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(
                &mut out,
                "  -f, --force  Reinstall even when the current version is already latest"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help   Print help").unwrap();
        }
        HelpTopic::ImportAuth => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex import-auth <PATH>").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Arguments:").unwrap();
            writeln!(
                &mut out,
                "  <PATH>  Path to an auth.json file or a home directory containing it"
            )
            .unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(&mut out, "  -h, --help  Print help").unwrap();
        }
        HelpTopic::ImportKnown => {
            writeln!(&mut out, "Usage:").unwrap();
            writeln!(&mut out, "  scodex import-known").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "Options:").unwrap();
            writeln!(&mut out, "  -h, --help  Print help").unwrap();
        }
    }
    out
}

fn render_help_zh(topic: HelpTopic) -> String {
    let mut out = String::new();
    match topic {
        HelpTopic::Root => {
            writeln!(&mut out, "{}", ui::messages().cli_about()).unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex [选项] [命令]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "命令：").unwrap();
            writeln!(
                &mut out,
                "  launch       切换到最佳账号，并启动或恢复 Codex"
            )
            .unwrap();
            writeln!(&mut out, "  auto         切换到最佳账号，但不启动 Codex").unwrap();
            writeln!(&mut out, "  add          打开注册页，然后新增一个账号").unwrap();
            writeln!(&mut out, "  login        通过设备登录新增一个账号").unwrap();
            writeln!(
                &mut out,
                "  deploy       把当前 auth.json 复制到远端机器 [别名：sync]"
            )
            .unwrap();
            writeln!(&mut out, "  push         把本地账号池推送到 Git 仓库").unwrap();
            writeln!(&mut out, "  pull         从 Git 仓库拉取账号池").unwrap();
            writeln!(&mut out, "  use          按邮箱直接切换到一个已知账号").unwrap();
            writeln!(&mut out, "  list         显示最新账号额度").unwrap();
            writeln!(&mut out, "  refresh      刷新所有已知账号的实时额度").unwrap();
            writeln!(&mut out, "  update       自更新 scodex [别名：upgrade]").unwrap();
            writeln!(
                &mut out,
                "  import-auth  导入 auth.json 文件或其所在 home 目录"
            )
            .unwrap();
            writeln!(&mut out, "  import-known 导入默认已知认证来源").unwrap();
            writeln!(&mut out, "  help         显示帮助").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(&mut out, "      --state-dir <STATE_DIR>  覆盖本地状态目录").unwrap();
            writeln!(&mut out, "  -h, --help                   显示帮助").unwrap();
        }
        HelpTopic::Launch => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex launch [选项] [<codex 参数...>]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(
                &mut out,
                "      --no-import-known  跳过自动导入已知认证来源"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --no-login         当没有可用账号时，不自动发起设备登录"
            )
            .unwrap();
            writeln!(&mut out, "      --dry-run          只显示会选中的账号").unwrap();
            writeln!(&mut out, "      --no-resume        总是新开 Codex 会话").unwrap();
            writeln!(
                &mut out,
                "      --no-launch        只切换账号，不启动 Codex"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help             显示帮助").unwrap();
        }
        HelpTopic::Auto => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex auto [选项]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(
                &mut out,
                "      --no-import-known  跳过自动导入已知认证来源"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --no-login         当没有可用账号时，不自动发起设备登录"
            )
            .unwrap();
            writeln!(
                &mut out,
                "      --dry-run          只显示会选中的账号，不执行切换"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help             显示帮助").unwrap();
        }
        HelpTopic::Add => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex add [选项]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(&mut out, "      --switch  注册/登录完成后切换到新账号").unwrap();
            writeln!(&mut out, "  -h, --help    显示帮助").unwrap();
        }
        HelpTopic::Login => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex login [选项]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(
                &mut out,
                "      --oauth              使用浏览器 OAuth 流程并自动填充，需要同时传入 --username 和 --password"
            )
            .unwrap();
            writeln!(&mut out, "      --username <EMAIL>   --oauth 模式下使用的邮箱").unwrap();
            writeln!(
                &mut out,
                "      --password <PASS>    --oauth 模式下使用的密码（会出现在 ps 中，建议仅在可信 shell 使用）"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help               显示帮助").unwrap();
        }
        HelpTopic::Deploy => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex deploy [选项] <TARGET>").unwrap();
            writeln!(&mut out, "  scodex sync [选项] <TARGET>").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "参数：").unwrap();
            writeln!(
                &mut out,
                "  <TARGET>  远端目标，格式为 user@host:/target_path"
            )
            .unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(
                &mut out,
                "  -i <IDENTITY_FILE>  传给 ssh/scp 的 SSH 身份文件"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help          显示帮助").unwrap();
        }
        HelpTopic::Push => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex push [选项] <REPO>").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "参数：").unwrap();
            writeln!(&mut out, "  <REPO>  Git 远端 URL 或本地仓库路径").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(
                &mut out,
                "      --path <REPO_PATH>  仓库内用于保存账号池的子目录"
            )
            .unwrap();
            writeln!(&mut out, "环境变量：").unwrap();
            writeln!(
                &mut out,
                "  SCODEX_POOL_KEY  用于加密账号池的对称密钥来源"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help            显示帮助").unwrap();
        }
        HelpTopic::Pull => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex pull [选项] <REPO>").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "参数：").unwrap();
            writeln!(&mut out, "  <REPO>  Git 远端 URL 或本地仓库路径").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(
                &mut out,
                "      --path <REPO_PATH>  仓库内用于保存账号池的子目录"
            )
            .unwrap();
            writeln!(&mut out, "环境变量：").unwrap();
            writeln!(
                &mut out,
                "  SCODEX_POOL_KEY  用于解密账号池的对称密钥来源"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help            显示帮助").unwrap();
        }
        HelpTopic::Use => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex use <EMAIL>").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "参数：").unwrap();
            writeln!(&mut out, "  <EMAIL>  要切换到的账号邮箱").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(&mut out, "  -h, --help  显示帮助").unwrap();
        }
        HelpTopic::List => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex list").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(&mut out, "  -h, --help  显示帮助").unwrap();
        }
        HelpTopic::Refresh => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex refresh").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(&mut out, "  -h, --help  显示帮助").unwrap();
        }
        HelpTopic::Update => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex update [选项]").unwrap();
            writeln!(&mut out, "  scodex upgrade [选项]").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(
                &mut out,
                "  -f, --force  即使当前版本已经最新，也强制重新安装"
            )
            .unwrap();
            writeln!(&mut out, "  -h, --help   显示帮助").unwrap();
        }
        HelpTopic::ImportAuth => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex import-auth <PATH>").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "参数：").unwrap();
            writeln!(
                &mut out,
                "  <PATH>  auth.json 文件路径，或包含该文件的 home 目录"
            )
            .unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(&mut out, "  -h, --help  显示帮助").unwrap();
        }
        HelpTopic::ImportKnown => {
            writeln!(&mut out, "用法：").unwrap();
            writeln!(&mut out, "  scodex import-known").unwrap();
            writeln!(&mut out).unwrap();
            writeln!(&mut out, "选项：").unwrap();
            writeln!(&mut out, "  -h, --help  显示帮助").unwrap();
        }
    }
    out
}
