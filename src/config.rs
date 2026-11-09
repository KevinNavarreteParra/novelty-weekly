//! Where state lives and how a build is tuned.
//!
//! Everything sits in one home directory ($NOVELTY_HOME, default
//! `$XDG_DATA_HOME/novelty`): `config.toml`, the database, a disposable
//! cache of API responses, and the Spotify token. Secrets never go in `config.toml`; they come from the environment
//! (see `.env.example`).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

/// The files the app reads and writes.
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    /// `home` from the CLI or $NOVELTY_HOME, else `$XDG_DATA_HOME/novelty`,
    /// else `~/.local/share/novelty`.
    pub fn resolve(home: Option<PathBuf>) -> Result<Paths> {
        let home = match home {
            Some(dir) => dir,
            None => default_home()?,
        };
        Ok(Paths { home })
    }

    pub fn config(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    pub fn database(&self) -> PathBuf {
        self.home.join("novelty.db")
    }

    /// Cached API responses; safe to delete.
    pub fn cache(&self) -> PathBuf {
        self.home.join("cache.db")
    }

    /// The Spotify refresh token, written with owner-only permissions.
    pub fn spotify_token(&self) -> PathBuf {
        self.home.join("spotify-token.json")
    }

    pub fn ensure_home(&self) -> Result<()> {
        fs::create_dir_all(&self.home).with_context(|| format!("creating {}", self.home.display()))
    }
}

fn default_home() -> Result<PathBuf> {
    if let Some(data) = env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(data).join("novelty"));
    }
    let home =
        env::var_os("HOME").ok_or_else(|| anyhow!("neither NOVELTY_HOME nor HOME is set"))?;
    Ok(PathBuf::from(home).join(".local/share/novelty"))
}

/// `config.toml`. Every field has a default, so the file may be absent or
/// partial; unknown keys are an error so typos don't silently do nothing.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub spotify: SpotifyConfig,
    pub musicbrainz: MusicBrainzConfig,
    pub build: BuildConfig,
    pub blocklist: Blocklist,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpotifyConfig {
    /// Must match a redirect URI registered on the Spotify app.
    pub redirect_uri: String,
    /// The persistent playlist's name, created on the first build.
    pub playlist_name: String,
}

impl Default for SpotifyConfig {
    fn default() -> Self {
        SpotifyConfig {
            redirect_uri: "http://127.0.0.1:8765/callback".into(),
            playlist_name: "Novelty Weekly".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MusicBrainzConfig {
    pub enabled: bool,
    /// MusicBrainz requires a descriptive User-Agent with a contact.
    pub user_agent: String,
}

impl Default for MusicBrainzConfig {
    fn default() -> Self {
        MusicBrainzConfig {
            enabled: false,
            user_agent: concat!(
                "novelty-weekly/",
                env!("CARGO_PKG_VERSION"),
                " ( https://github.com/KevinNavarreteParra/novelty-weekly )"
            )
            .into(),
        }
    }
}

/// Knobs for the weekly build. Names and defaults follow spec §11.2.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuildConfig {
    /// Playlist size.
    pub k: usize,
    /// Every pool gets at least this share of the playlist...
    pub pool_floor: f64,
    /// ...and at most this share.
    pub pool_cap: f64,
    /// MMR trade-off between relevance (1.0) and variety (0.0).
    pub mmr_lambda: f64,
    /// Weekly decay of the bandit's memory; 0.9 remembers about 10 weeks.
    pub decay_rho: f64,
    /// Listens this many days old count 1/e as much toward affinity.
    pub affinity_tau_days: f64,
    /// How much an autoplayed listen counts toward affinity (spec §14 floats 0.5).
    pub autoplay_weight: f64,
    /// How hard to discount popular artists in the near pool.
    pub popularity_gamma: f64,
    /// The far pool skips a tag's best-known artists, ranks 1 to this.
    pub far_skip_top_ranks: usize,
    /// IANA zone that decides which ISO week a build belongs to.
    pub timezone: String,
}

impl Default for BuildConfig {
    fn default() -> Self {
        BuildConfig {
            k: 40,
            pool_floor: 0.20,
            pool_cap: 0.50,
            mmr_lambda: 0.6,
            decay_rho: 0.9,
            affinity_tau_days: 365.0,
            autoplay_weight: 1.0,
            popularity_gamma: 0.5,
            far_skip_top_ranks: 20,
            timezone: "America/Los_Angeles".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Blocklist {
    pub artists: Vec<String>,
    pub tags: Vec<String>,
}

impl Default for Blocklist {
    fn default() -> Self {
        Blocklist {
            artists: Vec::new(),
            tags: vec!["christmas".into(), "comedy".into(), "spoken word".into()],
        }
    }
}

impl Config {
    /// Reads `path`, or returns the defaults if it doesn't exist.
    pub fn load(path: &Path) -> Result<Config> {
        match fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}

/// A secret from the environment, e.g. `LASTFM_API_KEY`.
pub fn secret(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            anyhow!("{name} is not set (see .env.example; run with `prun` to resolve it)")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_config_parses_and_matches_the_defaults() {
        let example: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        let defaults = Config::default();
        assert_eq!(example.build.k, defaults.build.k);
        assert_eq!(example.build.timezone, defaults.build.timezone);
        assert_eq!(example.blocklist.tags, defaults.blocklist.tags);
    }

    #[test]
    fn a_partial_config_keeps_other_defaults() {
        let config: Config = toml::from_str("[build]\nk = 20\n").unwrap();
        assert_eq!(config.build.k, 20);
        assert_eq!(config.build.pool_floor, 0.20);
    }

    #[test]
    fn a_typo_is_an_error() {
        assert!(toml::from_str::<Config>("[build]\nkk = 20\n").is_err());
    }
}
