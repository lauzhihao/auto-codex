mod adapters;
mod cli;
mod core;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse_args();
    let code = cli::run(cli)?;
    std::process::exit(code);
}
