//! `novelty ingest-history`: import a Spotify Extended Streaming History export.

use std::path::Path;

use anyhow::Result;

use super::App;
use crate::integrations::spotify;

pub fn history(app: &App, path: &Path) -> Result<()> {
    let listens = spotify::history::read_export(path)?;
    let store = app.open_store()?;
    let summary = store.import_history(&listens)?;
    println!(
        "read {} listens: {} recorded, {} already on file; {} polled plays replaced",
        listens.len(),
        summary.recorded,
        listens.len() - summary.recorded,
        summary.replaced_polled,
    );
    Ok(())
}
