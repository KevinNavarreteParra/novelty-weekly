//! What knowledge sources said about artists: similarity, tags, popular
//! tracks and listener counts. Each `record_*` replaces what a source said
//! before about that artist, so the store always holds its latest answer.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::params;

use super::Store;
use crate::model::{
    ArtistId, ArtistTag, ArtistTags, PopularTrack, PopularTracks, SimilarArtist, SimilarityGraph,
    TrackId,
};

impl Store {
    pub fn record_similar(
        &self,
        artist: ArtistId,
        source: &str,
        similar: &[SimilarArtist],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute(
            "DELETE FROM similar_artists WHERE artist_id = ?1 AND source = ?2",
            params![artist.0, source],
        )?;
        for s in similar {
            let other = self.resolve_artist(&s.artist)?;
            if other == artist {
                continue;
            }
            self.conn
                .prepare_cached(
                    "INSERT OR REPLACE INTO similar_artists (artist_id, similar_id, similarity, source)
                     VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute(params![artist.0, other.0, s.similarity, source])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The similarity links out of each of `artists` (from any source; the
    /// strongest where sources overlap).
    pub fn similar(&self, artists: &[ArtistId]) -> Result<SimilarityGraph> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT similar_id, MAX(similarity) AS sim FROM similar_artists
             WHERE artist_id = ?1 GROUP BY similar_id ORDER BY sim DESC",
        )?;
        let mut graph = SimilarityGraph::new();
        for &artist in artists {
            let links = stmt
                .query_map([artist.0], |row| Ok((ArtistId(row.get(0)?), row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !links.is_empty() {
                graph.insert(artist, links);
            }
        }
        Ok(graph)
    }

    pub fn record_tags(&self, artist: ArtistId, source: &str, tags: &[ArtistTag]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute(
            "DELETE FROM artist_tags WHERE artist_id = ?1 AND source = ?2",
            params![artist.0, source],
        )?;
        for t in tags {
            self.conn
                .prepare_cached(
                    "INSERT OR REPLACE INTO artist_tags (artist_id, tag, weight, source)
                     VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute(params![artist.0, t.tag, t.weight, source])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Tags for each of `artists`, or for every tagged artist when `None`.
    pub fn tags(&self, artists: Option<&[ArtistId]>) -> Result<ArtistTags> {
        let mut out = ArtistTags::new();
        let mut push = |artist: i64, tag: String, weight: f64| {
            out.entry(ArtistId(artist))
                .or_default()
                .push(ArtistTag { tag, weight });
        };
        match artists {
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT artist_id, tag, MAX(weight) FROM artist_tags GROUP BY artist_id, tag",
                )?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    push(row.get(0)?, row.get(1)?, row.get(2)?);
                }
            }
            Some(artists) => {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT tag, MAX(weight) FROM artist_tags WHERE artist_id = ?1 GROUP BY tag",
                )?;
                for artist in artists {
                    let mut rows = stmt.query([artist.0])?;
                    while let Some(row) = rows.next()? {
                        push(artist.0, row.get(0)?, row.get(1)?);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Records an artist's popular tracks and returns them as stored tracks.
    pub fn record_popular(
        &self,
        artist: ArtistId,
        source: &str,
        popular: &[PopularTrack],
    ) -> Result<Vec<(TrackId, Option<u64>)>> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute(
            "DELETE FROM popular_tracks WHERE artist_id = ?1 AND source = ?2",
            params![artist.0, source],
        )?;
        let mut stored = Vec::with_capacity(popular.len());
        for (rank, p) in popular.iter().enumerate() {
            let track = self.resolve_track(&p.track)?;
            self.conn
                .prepare_cached(
                    "INSERT OR REPLACE INTO popular_tracks (artist_id, rank, track_id, listeners, source)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?
                .execute(params![artist.0, rank as i64, track.0, p.listeners.map(|l| l as i64), source])?;
            stored.push((track, p.listeners));
        }
        tx.commit()?;
        Ok(stored)
    }

    pub fn popular(&self, artists: &[ArtistId]) -> Result<PopularTracks> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT track_id, listeners FROM popular_tracks WHERE artist_id = ?1 ORDER BY rank",
        )?;
        let mut out = PopularTracks::new();
        for &artist in artists {
            let tracks = stmt
                .query_map([artist.0], |row| {
                    let listeners: Option<i64> = row.get(1)?;
                    Ok((TrackId(row.get(0)?), listeners.map(|l| l as u64)))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !tracks.is_empty() {
                out.insert(artist, tracks);
            }
        }
        Ok(out)
    }

    pub fn set_listeners(&self, artist: ArtistId, listeners: Option<u64>) -> Result<()> {
        self.conn
            .prepare_cached("UPDATE artists SET listeners = ?2 WHERE artist_id = ?1")?
            .execute(params![artist.0, listeners.map(|l| l as i64)])?;
        Ok(())
    }

    pub fn listeners(&self, artists: &[ArtistId]) -> Result<HashMap<ArtistId, u64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT listeners FROM artists WHERE artist_id = ?1 AND listeners IS NOT NULL",
        )?;
        let mut out = HashMap::new();
        for &artist in artists {
            let mut rows = stmt.query([artist.0])?;
            if let Some(row) = rows.next()? {
                out.insert(artist, row.get::<_, i64>(0)? as u64);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArtistRef, TrackRef};

    #[test]
    fn rerecording_replaces_a_sources_previous_answer() {
        let s = Store::open_in_memory().unwrap();
        let a = s.resolve_artist(&ArtistRef::named("A")).unwrap();
        let first = [
            SimilarArtist::new(ArtistRef::named("B"), 0.9),
            SimilarArtist::new(ArtistRef::named("C"), 0.5),
        ];
        s.record_similar(a, "lastfm", &first).unwrap();
        s.record_similar(
            a,
            "lastfm",
            &[SimilarArtist::new(ArtistRef::named("C"), 0.7)],
        )
        .unwrap();

        let graph = s.similar(&[a]).unwrap();
        let c = s.resolve_artist(&ArtistRef::named("C")).unwrap();
        assert_eq!(graph[&a], vec![(c, 0.7)]);
    }

    #[test]
    fn an_artist_is_never_similar_to_itself() {
        let s = Store::open_in_memory().unwrap();
        let a = s.resolve_artist(&ArtistRef::named("A")).unwrap();
        s.record_similar(
            a,
            "lastfm",
            &[SimilarArtist::new(ArtistRef::named("a!"), 1.0)],
        )
        .unwrap();
        assert!(s.similar(&[a]).unwrap().is_empty());
    }

    #[test]
    fn tags_and_popular_tracks_round_trip() {
        let s = Store::open_in_memory().unwrap();
        let a = s.resolve_artist(&ArtistRef::named("A")).unwrap();
        s.record_tags(a, "lastfm", &[ArtistTag::new("Dream Pop", 1.0)])
            .unwrap();
        assert_eq!(s.tags(Some(&[a])).unwrap()[&a][0].tag, "dream pop");
        assert_eq!(s.tags(None).unwrap().len(), 1);

        let popular = [
            PopularTrack {
                track: TrackRef::new("Hit", ArtistRef::named("A")),
                listeners: Some(900),
            },
            PopularTrack {
                track: TrackRef::new("Deep", ArtistRef::named("A")),
                listeners: None,
            },
        ];
        let stored = s.record_popular(a, "lastfm", &popular).unwrap();
        assert_eq!(s.popular(&[a]).unwrap()[&a], stored);
        assert_eq!(stored[0].1, Some(900));
    }

    #[test]
    fn listeners_are_kept_on_the_artist() {
        let s = Store::open_in_memory().unwrap();
        let a = s.resolve_artist(&ArtistRef::named("A")).unwrap();
        let b = s.resolve_artist(&ArtistRef::named("B")).unwrap();
        s.set_listeners(a, Some(1234)).unwrap();
        let known = s.listeners(&[a, b]).unwrap();
        assert_eq!(known, HashMap::from([(a, 1234)]));
    }
}
