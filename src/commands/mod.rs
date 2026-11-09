//! One function per CLI command. Each one wires the store, the integrations and
//! the engine together; none of them holds recommendation logic itself.

use anyhow::bail;

use crate::cli::{Cli, Command};

pub fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Auth => not_yet("auth"),
        Command::IngestHistory { .. } => not_yet("ingest-history"),
        Command::Poll => not_yet("poll"),
        Command::Build(_) => not_yet("build"),
        Command::Report(_) => not_yet("report"),
        Command::Ledger { .. } => not_yet("ledger"),
        Command::Block { .. } => not_yet("block"),
    }
}

fn not_yet(name: &str) -> anyhow::Result<()> {
    bail!("`novelty {name}` is not implemented yet")
}
