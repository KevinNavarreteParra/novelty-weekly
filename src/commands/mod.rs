//! One function per CLI command. Each one wires the store, the integrations and
//! the engine together; none of them holds recommendation logic itself.

mod auth;
mod backup;
pub mod build;
pub mod cold_start;
mod ingest;
mod ledger;
pub mod poll;

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::cli::{Cli, Command, LedgerAction};
use crate::config::{Config, Paths, secret};
use crate::integrations::cache::Cache;
use crate::integrations::http::USER_AGENT;
use crate::integrations::lastfm::LastFm;
use crate::integrations::spotify::Spotify;
use crate::store::Store;

pub fn run(cli: Cli) -> Result<()> {
    let app = App::load(cli.home)?;
    match cli.command {
        Command::IngestHistory { path } => ingest::history(&app, &path),
        Command::Auth { paste } => auth::run(&app, paste),
        Command::ColdStart => cold_start::run(&app),
        Command::Backup => backup::run(&app),
        Command::Poll => poll::run(&app),
        Command::Build(args) => build::run(&app, &args),
        Command::Report(_) => not_yet("report"),
        Command::Ledger {
            action: LedgerAction::Show { week },
        } => ledger::show(&app, week.as_deref()),
        Command::Block { .. } => {
            bail!(
                "`novelty block` is not implemented yet; add the name to [blocklist] in config.toml"
            )
        }
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

    fn open_cache(&self) -> Result<Cache> {
        self.paths.ensure_home()?;
        Cache::open(&self.paths.cache())
    }

    /// The knowledge source. Needs `LASTFM_API_KEY`.
    fn lastfm(&self) -> Result<LastFm> {
        Ok(LastFm::new(
            secret("LASTFM_API_KEY")?,
            self.open_cache()?,
            USER_AGENT,
        ))
    }

    /// The listening service. Needs `SPOTIFY_CLIENT_ID` and a prior `novelty auth`.
    fn spotify(&self) -> Result<Spotify> {
        Spotify::connect(
            secret("SPOTIFY_CLIENT_ID")?,
            self.paths.spotify_token(),
            self.open_cache()?,
            USER_AGENT,
        )
    }
}

fn not_yet(name: &str) -> Result<()> {
    bail!("`novelty {name}` is not implemented yet")
}
