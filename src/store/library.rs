//! What I know apart from plays: my playlists, saved albums, and how much the
//! listening service says I listen to each artist, gathered into [`Heard`].

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::params;

use jiff::Timestamp;

use super::{Store, from_ms, parse_stored, to_ms};
use crate::model::names::{is_placeholder_artist, title_key};
use crate::model::{
    AlbumRef, ArtistId, ArtistRef, Familiarity, Heard, LapsedArtist, Playlist, SKIP_BEFORE_MS,
    TrackId,
};

impl Store {
    /// Replaces the playlists on file with `playlists`. Returns how many
    /// tracks they hold. The caller leaves the app's own playlists out.
    pub fn record_playlists(&self, playlists: &[Playlist]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute("DELETE FROM playlist_tracks", [])?;
        let mut recorded = 0;
        for playlist in playlists {
            for (track, added) in &playlist.tracks {
                let id = self.resolve_track(track)?;
                recorded += self
                    .conn
                    .prepare_cached(
                        "INSERT OR IGNORE INTO playlist_tracks (playlist, track_id, added_on)
                         VALUES (?1, ?2, ?3)",
                    )?
                    .execute(params![playlist.name, id.0, added.map(|d| d.to_string())])?;
            }
        }
        tx.commit()?;
        Ok(recorded)
    }

    /// Replaces the saved albums on file. Returns how many are recorded.
    pub fn record_saved_albums(&self, albums: &[AlbumRef]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute("DELETE FROM saved_albums", [])?;
        let mut recorded = 0;
        for album in albums {
            let artist = self.resolve_artist(&album.artist)?;
            recorded += self
                .conn
                .prepare_cached(
                    "INSERT OR IGNORE INTO saved_albums (artist_id, title, title_key)
                     VALUES (?1, ?2, ?3)",
                )?
                .execute(params![artist.0, album.title, album.title_key()])?;
        }
        tx.commit()?;
        Ok(recorded)
    }

    /// Replaces what `source` says about how much I listen to each artist.
    /// An artist listed twice keeps its first rating.
    pub fn record_familiarity(
        &self,
        source: &str,
        rated: &[(ArtistRef, Familiarity)],
    ) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn
            .execute("DELETE FROM artist_familiarity WHERE source = ?1", [source])?;
        let mut recorded = 0;
        for (artist, level) in rated {
            let id = self.resolve_artist(artist)?;
            recorded += self
                .conn
                .prepare_cached(
                    "INSERT OR IGNORE INTO artist_familiarity (artist_id, level, source)
                     VALUES (?1, ?2, ?3)",
                )?
                .execute(params![id.0, level.as_str(), source])?;
        }
        tx.commit()?;
        Ok(recorded)
    }

    /// Each rated artist's familiarity, from any source. Where sources
    /// disagree, one of their ratings is kept.
    pub fn familiarity(&self) -> Result<HashMap<ArtistId, Familiarity>> {
        let mut stmt = self
            .conn
            .prepare("SELECT artist_id, level FROM artist_familiarity")?;
        let rated = stmt
            .query_map([], |row| {
                Ok((
                    ArtistId(row.get(0)?),
                    parse_stored(row.get(1)?, Familiarity::parse)?,
                ))
            })?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?;
        Ok(rated)
    }

    /// Everything I know apart from plays (see [`Heard`]).
    pub fn heard(&self) -> Result<Heard> {
        const TRACKS: &str = "SELECT track_id FROM saved_tracks
                              UNION SELECT track_id FROM playlist_tracks
                              UNION SELECT track_id FROM top_tracks";
        let tracks = self
            .conn
            .prepare(TRACKS)?
            .query_map([], |row| Ok(TrackId(row.get(0)?)))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        let artists = self
            .conn
            .prepare(&format!(
                "SELECT artist_id FROM tracks WHERE track_id IN ({TRACKS})
                 UNION SELECT artist_id FROM top_artists
                 UNION SELECT artist_id FROM artist_familiarity
                 UNION SELECT artist_id FROM saved_albums"
            ))?
            .query_map([], |row| Ok(ArtistId(row.get(0)?)))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        Ok(Heard { tracks, artists })
    }

    /// Every artist with some evidence I liked them (a saved track or album,
    /// a playlist entry, a rating from the service, a stream) and no stream
    /// since `since`. Streams are listens of 30 s or more.
    pub fn lapsed_artists(&self, since: Timestamp) -> Result<Vec<LapsedArtist>> {
        let mut stmt = self.conn.prepare(
            "WITH streams AS (
                 SELECT t.artist_id, COUNT(*) AS n, MAX(p.started_at) AS last
                 FROM plays p JOIN tracks t USING (track_id)
                 WHERE p.ms_played >= ?2 GROUP BY t.artist_id),
             saved AS (
                 SELECT t.artist_id, COUNT(*) AS n
                 FROM saved_tracks s JOIN tracks t USING (track_id) GROUP BY t.artist_id),
             listed AS (
                 SELECT t.artist_id, COUNT(DISTINCT pt.track_id) AS n
                 FROM playlist_tracks pt JOIN tracks t USING (track_id) GROUP BY t.artist_id),
             albums AS (SELECT artist_id, COUNT(*) AS n FROM saved_albums GROUP BY artist_id),
             rated AS (SELECT artist_id, MIN(level) AS level FROM artist_familiarity GROUP BY artist_id),
             evidence AS (
                 SELECT artist_id FROM streams UNION SELECT artist_id FROM saved
                 UNION SELECT artist_id FROM listed UNION SELECT artist_id FROM albums
                 UNION SELECT artist_id FROM rated)
             SELECT e.artist_id, COALESCE(saved.n, 0), COALESCE(listed.n, 0),
                    COALESCE(albums.n, 0), rated.level, COALESCE(streams.n, 0), streams.last
             FROM evidence e
             LEFT JOIN streams USING (artist_id) LEFT JOIN saved USING (artist_id)
             LEFT JOIN listed USING (artist_id) LEFT JOIN albums USING (artist_id)
             LEFT JOIN rated USING (artist_id)
             WHERE streams.last IS NULL OR streams.last < ?1
             ORDER BY e.artist_id",
        )?;
        let placeholders: HashSet<i64> = self
            .conn
            .prepare("SELECT artist_id, name FROM artists")?
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| match r {
                Ok((id, name)) if is_placeholder_artist(&name) => Some(Ok(id)),
                Ok(_) => None,
                Err(e) => Some(Err(e)),
            })
            .collect::<rusqlite::Result<_>>()?;
        let lapsed = stmt
            .query_map(params![to_ms(since), SKIP_BEFORE_MS], |row| {
                let level: Option<String> = row.get(4)?;
                let last: Option<i64> = row.get(6)?;
                Ok(LapsedArtist {
                    artist: ArtistId(row.get(0)?),
                    saved: row.get(1)?,
                    listed: row.get(2)?,
                    albums: row.get(3)?,
                    familiarity: level
                        .map(|l| parse_stored(l, Familiarity::parse))
                        .transpose()?,
                    streams: row.get(5)?,
                    last_stream: last.map(from_ms).transpose()?,
                })
            })?
            .filter(|l| {
                l.as_ref()
                    .map_or(true, |l| !placeholders.contains(&l.artist.0))
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(lapsed)
    }

    /// Each artist's tracks I showed I loved, most loved first: saved, in a
    /// playlist I made, and streamed (each stream counts a fifth of a save).
    pub fn loved_tracks(&self, artists: &[ArtistId]) -> Result<HashMap<ArtistId, Vec<TrackId>>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT t.track_id,
                    EXISTS (SELECT 1 FROM saved_tracks s WHERE s.track_id = t.track_id)
                  + EXISTS (SELECT 1 FROM playlist_tracks pt WHERE pt.track_id = t.track_id)
                  + 0.2 * (SELECT COUNT(*) FROM plays p
                           WHERE p.track_id = t.track_id AND p.ms_played >= ?2) AS love
             FROM tracks t WHERE t.artist_id = ?1 AND love > 0
             ORDER BY love DESC, t.track_id",
        )?;
        let mut loved = HashMap::new();
        for artist in artists {
            let tracks = stmt
                .query_map(params![artist.0, SKIP_BEFORE_MS], |row| {
                    Ok(TrackId(row.get(0)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !tracks.is_empty() {
                loved.insert(*artist, tracks);
            }
        }
        Ok(loved)
    }

    /// Which of `tracks` are on an album in my library, going by the album
    /// each track was first seen on.
    pub fn on_saved_albums(
        &self,
        tracks: impl IntoIterator<Item = TrackId>,
    ) -> Result<HashSet<TrackId>> {
        let saved: HashSet<(i64, String)> = self
            .conn
            .prepare("SELECT artist_id, title_key FROM saved_albums")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut found = HashSet::new();
        if saved.is_empty() {
            return Ok(found);
        }
        let mut stmt = self
            .conn
            .prepare_cached("SELECT artist_id, album FROM tracks WHERE track_id = ?1")?;
        for track in tracks {
            let (artist, album): (i64, Option<String>) =
                stmt.query_row([track.0], |row| Ok((row.get(0)?, row.get(1)?)))?;
            if album.is_some_and(|a| saved.contains(&(artist, title_key(&a)))) {
                found.insert(track);
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::model::{Ending, Listen, Provenance, StartedBy, TrackRef};

    fn track(artist: &str, title: &str, album: Option<&str>) -> TrackRef {
        let mut t = TrackRef::new(title, ArtistRef::named(artist));
        t.album = album.map(str::to_owned);
        t
    }

    #[test]
    fn playlists_albums_and_ratings_are_heard_and_replaced_whole() {
        let s = Store::open_in_memory().unwrap();
        let mine = Playlist {
            name: "Chill".into(),
            tracks: vec![(track("CCR", "Run Through The Jungle", None), None)],
        };
        assert_eq!(s.record_playlists(&[mine]).unwrap(), 1);
        s.record_saved_albums(&[AlbumRef {
            title: "MONTERO".into(),
            artist: ArtistRef::named("Lil Nas X"),
        }])
        .unwrap();
        s.record_familiarity(
            "spotify",
            &[(ArtistRef::named("Andrew Garfield"), Familiarity::Lapsed)],
        )
        .unwrap();

        let heard = s.heard().unwrap();
        let jungle = s
            .resolve_track(&track("CCR", "Run Through The Jungle", None))
            .unwrap();
        assert!(heard.tracks.contains(&jungle));
        for name in ["CCR", "Lil Nas X", "Andrew Garfield"] {
            let artist = s.resolve_artist(&ArtistRef::named(name)).unwrap();
            assert!(heard.artists.contains(&artist), "{name}");
        }

        s.record_playlists(&[]).unwrap();
        assert!(!s.heard().unwrap().tracks.contains(&jungle));
    }

    #[test]
    fn saved_tracks_are_heard_even_after_a_history_import() {
        let s = Store::open_in_memory().unwrap();
        let at: Timestamp = "2020-01-01T00:00:00Z".parse().unwrap();
        s.record_saved(&[(track("A", "Old Favourite", None), at)])
            .unwrap();
        s.import_history(&[Listen {
            track: track("B", "Something Else", None),
            started_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            ms_played: Some(200_000),
            ending: Ending::Unknown,
            started_by: StartedBy::Unknown,
            provenance: Provenance::BasicHistory,
        }])
        .unwrap();
        let favourite = s.resolve_track(&track("A", "Old Favourite", None)).unwrap();
        assert!(s.heard().unwrap().tracks.contains(&favourite));
    }

    #[test]
    fn a_track_is_on_a_saved_album_by_its_album_title() {
        let s = Store::open_in_memory().unwrap();
        s.record_saved_albums(&[AlbumRef {
            title: "Cosmo's Factory".into(),
            artist: ArtistRef::named("CCR"),
        }])
        .unwrap();
        let on = s
            .resolve_track(&track(
                "CCR",
                "Lookin' Out My Back Door",
                Some("Cosmo's Factory (Deluxe)"),
            ))
            .unwrap();
        let off = s
            .resolve_track(&track(
                "CCR",
                "Fortunate Son",
                Some("Willy and the Poor Boys"),
            ))
            .unwrap();
        let no_album = s.resolve_track(&track("CCR", "Demo", None)).unwrap();
        let found = s.on_saved_albums([on, off, no_album]).unwrap();
        assert_eq!(found, HashSet::from([on]));
    }

    #[test]
    fn an_artist_is_lapsed_with_evidence_and_no_recent_stream() {
        let s = Store::open_in_memory().unwrap();
        let old: Timestamp = "2020-01-01T00:00:00Z".parse().unwrap();
        let recent: Timestamp = "2026-10-01T00:00:00Z".parse().unwrap();
        s.record_saved(&[
            (track("CCR", "Fortunate Son", None), old),
            (track("Kept Up", "Hit", None), old),
        ])
        .unwrap();
        s.record_playlists(&[Playlist {
            name: "Chill".into(),
            tracks: vec![
                (track("CCR", "Fortunate Son", None), None),
                (track("CCR", "Green River", None), None),
            ],
        }])
        .unwrap();
        s.record_familiarity(
            "spotify",
            &[(ArtistRef::named("Andrew Garfield"), Familiarity::Lapsed)],
        )
        .unwrap();
        let stream = |artist: &str, title: &str, at: Timestamp, ms: u32| Listen {
            track: track(artist, title, None),
            started_at: at,
            ms_played: Some(ms),
            ending: Ending::Unknown,
            started_by: StartedBy::Unknown,
            provenance: Provenance::BasicHistory,
        };
        s.import_history(&[
            stream(
                "CCR",
                "Green River",
                "2025-12-01T00:00:00Z".parse().unwrap(),
                180_000,
            ),
            stream(
                "CCR",
                "Proud Mary",
                "2026-09-01T00:00:00Z".parse().unwrap(),
                5_000,
            ),
            stream("Kept Up", "Hit", recent, 200_000),
        ])
        .unwrap();

        // A compilation names no artist, even if one slipped into the store.
        s.record_saved_albums(&[AlbumRef {
            title: "Hits 2020".into(),
            artist: ArtistRef::named("Various Artists"),
        }])
        .unwrap();
        let since: Timestamp = "2026-06-01T00:00:00Z".parse().unwrap();
        let lapsed = s.lapsed_artists(since).unwrap();
        let ccr = s.resolve_artist(&ArtistRef::named("CCR")).unwrap();
        let garfield = s
            .resolve_artist(&ArtistRef::named("Andrew Garfield"))
            .unwrap();
        let ids: Vec<ArtistId> = lapsed.iter().map(|l| l.artist).collect();
        assert_eq!(
            ids,
            vec![ccr, garfield],
            "Kept Up streamed recently; a 5 s skip isn't a stream"
        );
        let c = &lapsed[0];
        assert_eq!((c.saved, c.listed, c.albums, c.streams), (1, 2, 0, 1));
        assert_eq!(lapsed[1].familiarity, Some(Familiarity::Lapsed));

        let loved = s.loved_tracks(&[ccr]).unwrap();
        let son = s
            .resolve_track(&track("CCR", "Fortunate Son", None))
            .unwrap();
        let river = s.resolve_track(&track("CCR", "Green River", None)).unwrap();
        assert_eq!(
            loved[&ccr],
            vec![son, river],
            "saved and listed beats listed and streamed"
        );
    }
}
