//! Fetching knowledge for a build: each call asks the integration (through its
//! cache), records the answer in the store as canonical facts, and hands back
//! the resolved map the engine needs.
//!
//! One artist failing (renamed, unknown, a flaky response) is logged and
//! skipped. Every artist failing means something systematic, like a bad API
//! key, and fails the build.

use std::collections::HashMap;
use std::ops::Range;

use anyhow::{Result, anyhow};
use log::warn;

use crate::engine::pools::deep::CatalogTrack;
use crate::integrations::{Knowledge, ListeningService};
use crate::model::{ArtistId, ArtistTags, PopularTracks, SimilarityGraph, TrackId};
use crate::store::Store;

pub struct Gather<'a, K> {
    pub store: &'a Store,
    pub knowledge: &'a K,
}

impl<K: Knowledge> Gather<'_, K> {
    pub fn similar(&self, artists: &[ArtistId]) -> Result<SimilarityGraph> {
        for_each(artists, "similar artists", |artist| {
            let found = self
                .knowledge
                .similar_artists(&self.store.artist_ref(artist)?)?;
            self.store
                .record_similar(artist, self.knowledge.source(), &found)
        })?;
        self.store.similar(artists)
    }

    /// One artist's links, for a random walk; a failure is a dead end.
    pub fn similar_to(&self, artist: ArtistId) -> Vec<(ArtistId, f64)> {
        match self.similar(&[artist]) {
            Ok(mut graph) => graph.remove(&artist).unwrap_or_default(),
            Err(e) => {
                warn!("similar artists for artist {}: {e:#}", artist.0);
                Vec::new()
            }
        }
    }

    pub fn tags(&self, artists: &[ArtistId]) -> Result<ArtistTags> {
        for_each(artists, "tags", |artist| {
            let found = self
                .knowledge
                .artist_tags(&self.store.artist_ref(artist)?)?;
            self.store
                .record_tags(artist, self.knowledge.source(), &found)
        })?;
        self.store.tags(Some(artists))
    }

    pub fn popular(&self, artists: &[ArtistId]) -> Result<PopularTracks> {
        for_each(artists, "popular tracks", |artist| {
            let found = self
                .knowledge
                .popular_tracks(&self.store.artist_ref(artist)?)?;
            self.store
                .record_popular(artist, self.knowledge.source(), &found)?;
            Ok(())
        })?;
        self.store.popular(artists)
    }

    pub fn listeners(&self, artists: &[ArtistId]) -> Result<HashMap<ArtistId, u64>> {
        for_each(artists, "listener counts", |artist| {
            let found = self.knowledge.listeners(&self.store.artist_ref(artist)?)?;
            self.store.set_listeners(artist, found)
        })?;
        self.store.listeners(artists)
    }

    /// A tag's chart, as store artists.
    pub fn tag_chart(&self, tag: &str, ranks: Range<usize>) -> Result<Vec<ArtistId>> {
        let chart = self.knowledge.tag_chart(tag, ranks)?;
        chart
            .iter()
            .map(|artist| self.store.resolve_artist(artist))
            .collect()
    }

    /// Each artist's catalog from the listening service, with listener counts
    /// from the knowledge source's popular tracks where they match.
    pub fn catalogs(
        &self,
        artists: &[ArtistId],
        service: &mut impl ListeningService,
    ) -> Result<HashMap<ArtistId, Vec<CatalogTrack>>> {
        let popular = self.popular(artists)?;
        let mut catalogs = HashMap::new();
        for_each(artists, "catalog", |artist| {
            let listeners: HashMap<TrackId, Option<u64>> = popular
                .get(&artist)
                .into_iter()
                .flatten()
                .copied()
                .collect();
            let mut catalog = Vec::new();
            for track in service.catalog(&self.store.artist_ref(artist)?)? {
                let id = self.store.resolve_track(&track)?;
                catalog.push(CatalogTrack {
                    track: id,
                    listeners: listeners.get(&id).copied().flatten(),
                });
            }
            catalogs.insert(artist, catalog);
            Ok(())
        })?;
        Ok(catalogs)
    }
}

/// Runs `fetch` for each artist, tolerating individual failures.
fn for_each(
    artists: &[ArtistId],
    what: &str,
    mut fetch: impl FnMut(ArtistId) -> Result<()>,
) -> Result<()> {
    let mut last_error = None;
    let mut failures = 0;
    for &artist in artists {
        if let Err(e) = fetch(artist) {
            warn!("{what} for artist {}: {e:#}", artist.0);
            failures += 1;
            last_error = Some(e);
        }
    }
    match last_error {
        Some(e) if failures == artists.len() => Err(anyhow!(
            "fetching {what} failed for every artist; last error: {e:#}"
        )),
        _ => Ok(()),
    }
}
