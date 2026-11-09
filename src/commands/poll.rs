//! `novelty poll`: record what I've listened to since the last poll (spec §8).
//!
//! Outcomes of past recommendations need no update here: they are derived
//! from plays at build time.

use anyhow::Result;
use log::{error, info};

use super::App;
use crate::integrations::ListeningService;
use crate::store::Store;

/// After this many failed polls in a row, say so loudly (spec §12).
const ALERT_AFTER_FAILURES: u32 = 3;

pub fn run(app: &App) -> Result<()> {
    let store = app.open_store()?;
    let run = store.start_run("poll")?;
    let result = app
        .spotify()
        .and_then(|mut spotify| poll(&store, &mut spotify));
    let notes = match &result {
        Ok(recorded) => format!("{recorded} recorded"),
        Err(e) => format!("{e:#}"),
    };
    store.finish_run(run, result.is_ok(), &notes)?;
    let failures = store.consecutive_failures("poll")?;
    if failures >= ALERT_AFTER_FAILURES {
        error!("the last {failures} polls failed; listens may be going unrecorded");
    }
    let recorded = result?;
    info!("poll: {recorded} listens recorded or completed");
    Ok(())
}

/// One poll: read the feed from the cursor and record what's new.
pub fn poll(store: &Store, service: &mut impl ListeningService) -> Result<usize> {
    let after = store.poll_cursor()?;
    let listens = service.recent_listens(after)?;
    store.record_listens(&listens)
}
