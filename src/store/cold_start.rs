//! The cold start's facts: the listening service's rankings of what I play
//! most, and whether a real history has arrived to replace them.

use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use super::{Store, from_ms, parse_stored};
use crate::model::{ArtistId, ArtistRef, ColdStart, Provenance, Span, TrackId, TrackRef};

impl Store {
    /// Replaces the ranking of top artists over `span`. An artist listed twice
    /// (two service IDs, one name) keeps its higher rank.
    pub fn record_top_artists(&self, span: Span, artists: &[ArtistRef]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn
            .execute("DELETE FROM top_artists WHERE span = ?1", [span.as_str()])?;
        let mut seen = HashSet::new();
        for artist in artists {
            let id = self.resolve_artist(artist)?;
            if seen.insert(id) {
                self.conn.execute(
                    "INSERT INTO top_artists (span, rank, artist_id) VALUES (?1, ?2, ?3)",
                    params![span.as_str(), seen.len() as i64 - 1, id.0],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Replaces the ranking of top tracks over `span`. A song listed twice
    /// (single and album releases) keeps its higher rank.
    pub fn record_top_tracks(&self, span: Span, tracks: &[TrackRef]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn
            .execute("DELETE FROM top_tracks WHERE span = ?1", [span.as_str()])?;
        let mut seen = HashSet::new();
        for track in tracks {
            let id = self.resolve_track(track)?;
            if seen.insert(id) {
                self.conn.execute(
                    "INSERT INTO top_tracks (span, rank, track_id) VALUES (?1, ?2, ?3)",
                    params![span.as_str(), seen.len() as i64 - 1, id.0],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Everything a cold start knows: both kinds of ranking and the library.
    pub fn cold_start(&self) -> Result<ColdStart> {
        let mut cold = ColdStart::default();

        let mut stmt = self
            .conn
            .prepare("SELECT span, artist_id FROM top_artists ORDER BY span, rank")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let span = parse_stored(row.get(0)?, Span::parse)?;
            let artist = ArtistId(row.get(1)?);
            cold.top_artists.entry(span).or_default().push(artist);
        }

        let mut stmt = self.conn.prepare(
            "SELECT r.span, r.track_id, t.artist_id
             FROM top_tracks r JOIN tracks t USING (track_id)
             ORDER BY r.span, r.rank",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let span = parse_stored(row.get(0)?, Span::parse)?;
            let entry = (TrackId(row.get(1)?), ArtistId(row.get(2)?));
            cold.top_tracks.entry(span).or_default().push(entry);
        }

        let mut stmt = self.conn.prepare(
            "SELECT s.track_id, t.artist_id, s.saved_at
             FROM saved_tracks s JOIN tracks t USING (track_id)
             ORDER BY s.saved_at",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            cold.saved.push((
                TrackId(row.get(0)?),
                ArtistId(row.get(1)?),
                from_ms(row.get(2)?)?,
            ));
        }
        Ok(cold)
    }

    /// Whether a listening-history export has been imported. Until one is,
    /// builds lean on the cold start.
    pub fn has_history(&self) -> Result<bool> {
        let found = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM plays WHERE provenance = ?1)",
            [Provenance::History.as_str()],
            |row| row.get(0),
        )?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;

    fn track(artist: &str, title: &str) -> TrackRef {
        TrackRef::new(title, ArtistRef::named(artist))
    }

    #[test]
    fn a_new_ranking_replaces_the_old_one_for_its_span_only() {
        let store = Store::open_in_memory().unwrap();
        let named = |names: &[&str]| -> Vec<ArtistRef> {
            names.iter().map(|n| ArtistRef::named(*n)).collect()
        };
        store
            .record_top_artists(Span::Year, &named(&["Low", "Slowdive"]))
            .unwrap();
        store
            .record_top_artists(Span::Month, &named(&["Duster"]))
            .unwrap();
        store
            .record_top_artists(Span::Year, &named(&["Slowdive", "Galaxie 500"]))
            .unwrap();

        let cold = store.cold_start().unwrap();
        let low = store.resolve_artist(&ArtistRef::named("Low")).unwrap();
        let slowdive = store.resolve_artist(&ArtistRef::named("Slowdive")).unwrap();
        let year = &cold.top_artists[&Span::Year];
        assert_eq!(year.len(), 2);
        assert_eq!(year[0], slowdive);
        assert!(!year.contains(&low));
        assert_eq!(cold.top_artists[&Span::Month].len(), 1);
    }

    #[test]
    fn two_releases_of_one_song_rank_once() {
        let store = Store::open_in_memory().unwrap();
        store
            .record_top_tracks(
                Span::Month,
                &[
                    track("Low", "Words"),
                    track("Low", "Words - 2021 Remaster"),
                    track("Low", "Lullaby"),
                ],
            )
            .unwrap();
        let cold = store.cold_start().unwrap();
        let month = &cold.top_tracks[&Span::Month];
        assert_eq!(month.len(), 2);
        let words = store.resolve_track(&track("Low", "Words")).unwrap();
        assert_eq!(month[0].0, words);
    }

    #[test]
    fn saved_tracks_come_with_their_artist() {
        let store = Store::open_in_memory().unwrap();
        let at: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        store
            .record_saved(&[(track("Duster", "Inside Out"), at)])
            .unwrap();
        let cold = store.cold_start().unwrap();
        let duster = store.resolve_artist(&ArtistRef::named("Duster")).unwrap();
        assert_eq!(cold.saved.len(), 1);
        assert_eq!(cold.saved[0].1, duster);
        assert_eq!(cold.saved[0].2, at);
        assert!(!cold.is_empty());
    }

    #[test]
    fn history_is_only_an_imported_export() {
        let store = Store::open_in_memory().unwrap();
        assert!(!store.has_history().unwrap());
        let listen = |provenance| crate::model::Listen {
            track: track("Low", "Words"),
            started_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            ms_played: Some(200_000),
            ending: crate::model::Ending::Finished,
            started_by: crate::model::StartedBy::Listener,
            provenance,
        };
        store.record_listens(&[listen(Provenance::Poll)]).unwrap();
        assert!(!store.has_history().unwrap());
        store
            .import_history(&[listen(Provenance::History)])
            .unwrap();
        assert!(store.has_history().unwrap());
    }
}
