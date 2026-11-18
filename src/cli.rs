//! The command-line surface (spec §11.1). Parsing only; `commands` does the work.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "novelty",
    version,
    about = "A weekly playlist where novelty is the objective."
)]
pub struct Cli {
    /// Directory holding config.toml, the database and the Spotify token
    /// [default: $XDG_DATA_HOME/novelty]
    #[arg(long, env = "NOVELTY_HOME", global = true)]
    pub home: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Log in to Spotify once and store the refresh token.
    Auth {
        /// Paste the address the browser lands on, instead of listening for
        /// the redirect (for a browser on another machine).
        #[arg(long)]
        paste: bool,
    },
    /// Import a Spotify data export (safe to re-run).
    ///
    /// Reads either package: Extended Streaming History (every year), or
    /// Account data (the last year, plus playlists, saved albums and
    /// Spotify's rating of how much I listen to each artist). A fuller
    /// history replaces a rougher one wherever both cover the same days.
    IngestHistory {
        /// The export's .zip, or a directory of its JSON files.
        path: PathBuf,
    },
    /// Seed my taste from Spotify's top items and library, until the export arrives.
    ///
    /// Fetches Spotify's rankings of my top artists and tracks over the last
    /// month, six months and year, and every saved track. Builds use them
    /// until a history export is imported. Safe to re-run.
    ColdStart,
    /// Record recently played tracks and score recommendation outcomes.
    Poll,
    /// Build this week's playlist.
    Build(BuildArgs),
    /// Print listening and recommendation metrics.
    Report(ReportArgs),
    /// Copy the database to $NOVELTY_HOME/backups, keeping 30 days of copies.
    Backup,
    /// Inspect past recommendations.
    Ledger {
        #[command(subcommand)]
        action: LedgerAction,
    },
    /// Never recommend an artist, or anything tagged with a tag.
    Block { kind: BlockKind, name: String },
}

#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Print the playlist with each pick's rationale; write nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Also save a dated snapshot playlist.
    #[arg(long)]
    pub archive: bool,
    /// Playlist size [default: from config]
    #[arg(long)]
    pub k: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// Trailing weeks to include.
    #[arg(long, default_value_t = 8)]
    pub weeks: u32,
    #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
    pub format: ReportFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReportFormat {
    Text,
    Svg,
}

#[derive(Debug, Subcommand)]
pub enum LedgerAction {
    /// List recommendations, newest week first.
    Show {
        /// ISO week, e.g. 2026-W46.
        #[arg(long)]
        week: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BlockKind {
    Artist,
    Tag,
}
