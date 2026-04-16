use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::adapters::codex::CodexAdapter;
use crate::core::state::{AccountRecord, UsageSnapshot};
use crate::core::storage;
use crate::core::update;

#[derive(Debug, Parser)]
#[command(name = "scodex")]
#[command(about = "Cross-platform account-aware launcher for agent CLIs.")]
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
    Login(LoginArgs),
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
    pub switch: bool,
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
        Self::parse()
    }
}

pub fn run(cli: Cli) -> Result<i32> {
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
                        print_selection("Would select", &account, &usage);
                        storage::save_state(&state_dir, &state)?;
                        0
                    } else {
                        print_selection("Switched to", &account, &usage);
                        storage::save_state(&state_dir, &state)?;
                        if args.no_launch {
                            0
                        } else {
                            adapter.launch_codex(&args.extra_args, !args.no_resume)?
                        }
                    }
                }
                None => {
                    println!("No usable account found.");
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
                        print_selection("Would select", &account, &usage);
                    } else {
                        print_selection("Switched to", &account, &usage);
                    }
                    storage::save_state(&state_dir, &state)?;
                    0
                }
                None => {
                    println!("No usable account found.");
                    storage::save_state(&state_dir, &state)?;
                    1
                }
            }
        }
        Command::Login(args) => {
            let record = adapter.run_device_auth_login(&state_dir, &mut state)?;
            let usage = adapter.refresh_account_usage(&mut state, &record);
            println!("Added {}", record.email);
            if args.switch {
                adapter.switch_account(&record)?;
                print_selection("Switched to", &record, &usage);
            }
            storage::save_state(&state_dir, &state)?;
            0
        }
        Command::Use(args) => {
            adapter.import_known_sources(&state_dir, &mut state);
            let Some(record) = adapter.find_account_by_email(&state, &args.email) else {
                println!("Unknown account: {}", args.email);
                storage::save_state(&state_dir, &state)?;
                return Ok(1);
            };
            adapter.switch_account(record)?;
            let usage = state
                .usage_cache
                .get(&record.id)
                .cloned()
                .unwrap_or_default();
            print_selection("Switched to", record, &usage);
            storage::save_state(&state_dir, &state)?;
            0
        }
        Command::List => {
            if state.accounts.is_empty() {
                println!("No accounts.");
                return Ok(1);
            }
            adapter.refresh_all_accounts(&mut state);
            storage::save_state(&state_dir, &state)?;
            let active = adapter.read_live_identity();
            println!("{}", adapter.render_account_table(&state, active.as_ref()));
            println!("{} row(s) in set.", state.accounts.len());
            0
        }
        Command::Refresh => {
            if state.accounts.is_empty() {
                println!("No accounts.");
                return Ok(1);
            }
            adapter.refresh_all_accounts(&mut state);
            storage::save_state(&state_dir, &state)?;
            let active = adapter.read_live_identity();
            println!("Refreshed {} account(s).", state.accounts.len());
            println!("{}", adapter.render_account_table(&state, active.as_ref()));
            println!("{} row(s) in set.", state.accounts.len());
            0
        }
        Command::Update(args) => {
            let outcome = update::self_update(args.force)?;
            match outcome.status {
                update::UpdateStatus::AlreadyCurrent => {
                    println!(
                        "Already on the latest installed version ({}) at {}",
                        outcome.installed_version,
                        outcome.executable_path.display()
                    );
                }
                update::UpdateStatus::Updated => {
                    println!(
                        "Updated scodex from {} to {} at {}",
                        outcome.previous_version,
                        outcome.installed_version,
                        outcome.executable_path.display()
                    );
                    if cfg!(windows) {
                        println!("Restart the current terminal if it still resolves the old binary.");
                    }
                }
            }
            0
        }
        Command::ImportAuth(args) => {
            let record = adapter.import_auth_path(&state_dir, &mut state, &args.path)?;
            storage::save_state(&state_dir, &state)?;
            println!("Imported {} -> {}", record.email, record.id);
            0
        }
        Command::ImportKnown => {
            let imported = adapter.import_known_sources(&state_dir, &mut state);
            if imported.is_empty() {
                println!("No importable accounts found.");
                storage::save_state(&state_dir, &state)?;
                return Ok(1);
            }
            storage::save_state(&state_dir, &state)?;
            for account in imported {
                println!("Imported {} -> {}", account.email, account.id);
            }
            0
        }
        Command::Passthrough(args) => {
            match adapter.ensure_best_account(&state_dir, &mut state, false, false, true)? {
                Some((account, usage)) => {
                    print_selection("Switched to", &account, &usage);
                    storage::save_state(&state_dir, &state)?;
                    adapter.run_passthrough(&args)?
                }
                None => {
                    println!("No usable account found.");
                    storage::save_state(&state_dir, &state)?;
                    1
                }
            }
        }
    };

    Ok(exit_code)
}

fn format_percent(value: Option<i64>) -> String {
    value
        .map(|value| format!("{value}%"))
        .unwrap_or_else(|| "N/A".into())
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
