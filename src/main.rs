use clap::Parser;

use novelty::cli::Cli;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    novelty::commands::run(cli)
}
