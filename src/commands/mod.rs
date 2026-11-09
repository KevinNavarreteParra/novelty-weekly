//! One function per CLI command. Each one wires the store, the integrations and
//! the engine together; none of them holds recommendation logic itself.

mod ingest;

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::cli::{Cli, Command};
use crate::config::{Config, Paths};
use crate::store::Store;

pub fn run(cli: Cli) -> Result<()> {
    let app = App::load(cli.home)?;
    match cli.command {
        Command::IngestHistory { path } => ingest::history(&app, &path),
        Command::Auth => not_yet("auth"),
        Command::Poll => not_yet("poll"),
        Command::Build(_) => not_yet("build"),
        Command::Report(_) => not_yet("report"),
        Command::Ledger { .. } => not_yet("ledger"),
        Command::Block { .. } => not_yet("block"),
    }
}

/// What every command starts from: where things live and how to behave.
pub struct App {
    pub paths: Paths,
    pub config: Config,
}

impl App {
    fn load(home: Option<PathBuf>) -> Result<App> {
        let paths = Paths::resolve(home)?;
        let config = Config::load(&paths.config())?;
        Ok(App { paths, config })
    }

    fn open_store(&self) -> Result<Store> {
        self.paths.ensure_home()?;
        Store::open(&self.paths.database())
    }
}

fn not_yet(name: &str) -> Result<()> {
    bail!("`novelty {name}` is not implemented yet")
}
