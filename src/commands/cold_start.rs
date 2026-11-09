//! `novelty cold-start`: until the history export arrives, fetch what the
//! listening service ranks as my most played, and my whole library, for
//! builds to lean on (spec §5.3). Safe to re-run: rankings are replaced.

use anyhow::Result;
use jiff::Timestamp;

use super::App;
use crate::integrations::ListeningService;
use crate::model::Span;
use crate::store::Store;

/// What one cold start fetched.
#[derive(Debug, Default)]
pub struct Fetched {
    /// Ranked artists and tracks per span, in `Span::ALL` order.
    pub top_artists: Vec<usize>,
    pub top_tracks: Vec<usize>,
    pub saved: usize,
}

pub fn run(app: &App) -> Result<()> {
    let store = app.open_store()?;
    let mut spotify = app.spotify()?;
    let fetched = cold_start(&store, &mut spotify)?;

    let spans: Vec<String> = Span::ALL.iter().map(Span::to_string).collect();
    println!(
        "top artists ({}): {:?}\ntop tracks ({}): {:?}\nsaved tracks: {}",
        spans.join(", "),
        fetched.top_artists,
        spans.join(", "),
        fetched.top_tracks,
        fetched.saved,
    );
    if store.has_history()? {
        println!("a history export is already imported, so builds won't use this");
    }
    Ok(())
}

pub fn cold_start(store: &Store, service: &mut impl ListeningService) -> Result<Fetched> {
    let mut fetched = Fetched::default();
    for span in Span::ALL {
        let artists = service.top_artists(*span)?;
        store.record_top_artists(*span, &artists)?;
        fetched.top_artists.push(artists.len());

        let tracks = service.top_tracks(*span)?;
        store.record_top_tracks(*span, &tracks)?;
        fetched.top_tracks.push(tracks.len());
    }
    let saved = service.saved_since(Timestamp::UNIX_EPOCH)?;
    store.record_saved(&saved)?;
    fetched.saved = saved.len();
    Ok(fetched)
}
