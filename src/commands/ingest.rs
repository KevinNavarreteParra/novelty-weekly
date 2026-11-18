//! `novelty ingest-history`: import a Spotify data export, either package
//! (Extended Streaming History or Account data) or both.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use super::App;
use super::build::is_own_playlist;
use crate::integrations::spotify::history::{self, Export};
use crate::model::{Familiarity, Provenance};
use crate::store::Store;

/// What the export's familiarity ratings are recorded under.
const SOURCE: &str = "spotify";

pub fn history(app: &App, path: &Path) -> Result<()> {
    let export = history::read_export(path)?;
    let store = app.open_store()?;
    let run = store.start_run("ingest")?;
    let result = ingest(&store, export, &app.config.spotify.playlist_name);
    let notes = match &result {
        Ok(lines) => lines.join("; "),
        Err(e) => format!("{e:#}"),
    };
    store.finish_run(run, result.is_ok(), &notes)?;
    for line in result? {
        println!("{line}");
    }
    if !store.has_full_history()? && store.has_history()? {
        println!(
            "(only the last year's history so far: builds skip deep cuts until an \
             Extended Streaming History export is imported)"
        );
    }
    Ok(())
}

/// Records every part of the export; returns a line about each.
pub fn ingest(store: &Store, export: Export, weekly_playlist: &str) -> Result<Vec<String>> {
    let mut lines = Vec::new();

    if !export.listens.is_empty() {
        let read = export.listens.len();
        let mut sources: BTreeMap<&str, usize> = BTreeMap::new();
        for l in &export.listens {
            *sources.entry(source_name(l.provenance)).or_default() += 1;
        }
        let first = export.listens.first().map(|l| l.started_at.to_string());
        let last = export.listens.last().map(|l| l.started_at.to_string());
        let summary = store.import_history(&export.listens)?;
        lines.push(format!(
            "listens: read {read} ({}) from {} to {}: {} recorded, {} covered by a fuller \
             history, {} already on file; {} rougher plays replaced",
            sources
                .iter()
                .map(|(s, n)| format!("{n} {s}"))
                .collect::<Vec<_>>()
                .join(", "),
            first.unwrap_or_default(),
            last.unwrap_or_default(),
            summary.recorded,
            summary.covered,
            read - summary.recorded - summary.covered,
            summary.replaced,
        ));
    }

    if let Some(playlists) = export.playlists {
        let (own, mine): (Vec<_>, Vec<_>) = playlists
            .into_iter()
            .partition(|p| is_own_playlist(&p.name, weekly_playlist));
        let tracks = store.record_playlists(&mine)?;
        let mut line = format!("playlists: {} of mine, {tracks} tracks", mine.len());
        if !own.is_empty() {
            let names: Vec<&str> = own.iter().map(|p| p.name.as_str()).collect();
            line.push_str(&format!(" (left out the app's own: {})", names.join(", ")));
        }
        lines.push(line);
    }

    if let Some(albums) = export.saved_albums {
        let recorded = store.record_saved_albums(&albums)?;
        lines.push(format!("saved albums: {recorded}"));
    }

    if let Some(rated) = export.familiarity {
        let recorded = store.record_familiarity(SOURCE, &rated)?;
        let mut levels: BTreeMap<Familiarity, usize> = BTreeMap::new();
        for (_, level) in &rated {
            *levels.entry(*level).or_default() += 1;
        }
        lines.push(format!(
            "artists Spotify rates my listening of: {recorded} ({})",
            levels
                .iter()
                .map(|(l, n)| format!("{n} {l}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(lines)
}

fn source_name(provenance: Provenance) -> &'static str {
    match provenance {
        Provenance::History => "extended history",
        Provenance::BasicHistory => "account data",
        Provenance::Poll | Provenance::PollEstimated => "polled",
    }
}
