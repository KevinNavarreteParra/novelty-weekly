//! Identity resolution: matching an observation's names and external IDs to the
//! store's own IDs, creating rows the first time something is seen.
//!
//! For artists and tracks alike:
//! 1. an external ID already on file wins;
//! 2. otherwise the normalized key matches (tracks: artist plus title key);
//! 3. otherwise the thing is new.
//!
//! Whichever matched, the IDs the observation carries are added to the record,
//! so the next observation from any source matches on them.
//!
//! The other direction, from store IDs back to refs an integration can look
//! up, is here too.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::Store;
use crate::model::{ArtistId, ArtistRef, ExternalIds, Namespace, TrackId, TrackRef};

/// Where one kind of entity keeps its external IDs.
struct IdTable {
    table: &'static str,
    column: &'static str,
}

const ARTIST_IDS: IdTable = IdTable {
    table: "artist_ids",
    column: "artist_id",
};
const TRACK_IDS: IdTable = IdTable {
    table: "track_ids",
    column: "track_id",
};

impl Store {
    pub fn resolve_artist(&self, artist: &ArtistRef) -> Result<ArtistId> {
        let id = match self.find_by_external_id(&ARTIST_IDS, &artist.ids)? {
            Some(id) => id,
            None => self.artist_by_key(artist)?,
        };
        self.attach_ids(&ARTIST_IDS, id, &artist.ids)?;
        Ok(ArtistId(id))
    }

    pub fn resolve_track(&self, track: &TrackRef) -> Result<TrackId> {
        let artist = self.resolve_artist(&track.artist)?;
        let id = match self.find_by_external_id(&TRACK_IDS, &track.ids)? {
            Some(id) => id,
            None => self.track_by_key(artist, track)?,
        };
        self.fill_track_details(id, track)?;
        self.attach_ids(&TRACK_IDS, id, &track.ids)?;
        Ok(TrackId(id))
    }

    /// The artist with this name key, created if new.
    fn artist_by_key(&self, artist: &ArtistRef) -> Result<i64> {
        let key = artist.key();
        self.conn
            .prepare_cached(
                "INSERT INTO artists (name, name_key) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            )?
            .execute(params![artist.name, key])?;
        let id = self
            .conn
            .prepare_cached("SELECT artist_id FROM artists WHERE name_key = ?1")?
            .query_row([key], |row| row.get(0))?;
        Ok(id)
    }

    /// The track with this artist and title key, created if new.
    fn track_by_key(&self, artist: ArtistId, track: &TrackRef) -> Result<i64> {
        let key = track.title_key();
        self.conn
            .prepare_cached(
                "INSERT INTO tracks (artist_id, title, title_key) VALUES (?1, ?2, ?3)
                 ON CONFLICT DO NOTHING",
            )?
            .execute(params![artist.0, track.title, key])?;
        let id = self
            .conn
            .prepare_cached("SELECT track_id FROM tracks WHERE artist_id = ?1 AND title_key = ?2")?
            .query_row(params![artist.0, key], |row| row.get(0))?;
        Ok(id)
    }

    /// Fills in album and duration where the stored track has none yet.
    fn fill_track_details(&self, id: i64, track: &TrackRef) -> Result<()> {
        if track.album.is_none() && track.duration_ms.is_none() {
            return Ok(());
        }
        self.conn
            .prepare_cached(
                "UPDATE tracks SET album = COALESCE(album, ?2), duration_ms = COALESCE(duration_ms, ?3)
                 WHERE track_id = ?1",
            )?
            .execute(params![id, track.album, track.duration_ms])?;
        Ok(())
    }

    fn find_by_external_id(&self, ids_in: &IdTable, ids: &ExternalIds) -> Result<Option<i64>> {
        let sql = format!(
            "SELECT {} FROM {} WHERE namespace = ?1 AND value = ?2",
            ids_in.column, ids_in.table
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        for (namespace, value) in ids.iter() {
            let found = stmt
                .query_row(params![namespace.as_str(), value], |row| row.get(0))
                .optional()?;
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    /// Records the IDs against `id`. An ID already filed under something else
    /// stays where it is.
    fn attach_ids(&self, ids_in: &IdTable, id: i64, ids: &ExternalIds) -> Result<()> {
        let sql = format!(
            "INSERT INTO {} (namespace, value, {}) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
            ids_in.table, ids_in.column
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        for (namespace, value) in ids.iter() {
            stmt.execute(params![namespace.as_str(), value, id])?;
        }
        Ok(())
    }

    /// An artist as integrations know it: its name and every external ID.
    pub fn artist_ref(&self, artist: ArtistId) -> Result<ArtistRef> {
        let name: String = self
            .conn
            .prepare_cached("SELECT name FROM artists WHERE artist_id = ?1")?
            .query_row([artist.0], |row| row.get(0))?;
        let ids = self.external_ids(&ARTIST_IDS, artist.0)?;
        Ok(ArtistRef::named(name).with_ids(ids))
    }

    /// A track as integrations know it, artist included.
    pub fn track_ref(&self, track: TrackId) -> Result<TrackRef> {
        let (title, artist, album, duration_ms): (String, i64, Option<String>, Option<u32>) = self
            .conn
            .prepare_cached(
                "SELECT title, artist_id, album, duration_ms FROM tracks WHERE track_id = ?1",
            )?
            .query_row([track.0], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
        Ok(TrackRef {
            title,
            artist: self.artist_ref(ArtistId(artist))?,
            album,
            duration_ms,
            ids: self.external_ids(&TRACK_IDS, track.0)?,
        })
    }

    /// Display names, for rationales and reports.
    pub fn artist_names(
        &self,
        artists: impl IntoIterator<Item = ArtistId>,
    ) -> Result<HashMap<ArtistId, String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT name FROM artists WHERE artist_id = ?1")?;
        let mut names = HashMap::new();
        for artist in artists {
            names.insert(artist, stmt.query_row([artist.0], |row| row.get(0))?);
        }
        Ok(names)
    }

    /// Known lengths of `tracks`.
    pub fn durations(
        &self,
        tracks: impl IntoIterator<Item = TrackId>,
    ) -> Result<HashMap<TrackId, u32>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT duration_ms FROM tracks WHERE track_id = ?1 AND duration_ms IS NOT NULL",
        )?;
        let mut out = HashMap::new();
        for track in tracks {
            if let Some(ms) = stmt.query_row([track.0], |row| row.get(0)).optional()? {
                out.insert(track, ms);
            }
        }
        Ok(out)
    }

    /// Every external ID on file for one artist or track. Where a namespace
    /// has several (a song's single and album releases), the newest wins.
    fn external_ids(&self, ids_in: &IdTable, id: i64) -> Result<ExternalIds> {
        let sql = format!(
            "SELECT namespace, value FROM {} WHERE {} = ?1 ORDER BY rowid",
            ids_in.table, ids_in.column
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut rows = stmt.query([id])?;
        let mut ids = ExternalIds::new();
        while let Some(row) = rows.next()? {
            let namespace: String = row.get(0)?;
            if let Some(ns) = Namespace::parse(&namespace) {
                ids.insert(ns, row.get::<_, String>(1)?);
            }
        }
        Ok(ids)
    }

    /// One external ID for a track, e.g. the Spotify ID to put in a playlist.
    pub fn track_external_id(
        &self,
        track: TrackId,
        namespace: Namespace,
    ) -> Result<Option<String>> {
        let value = self
            .conn
            .prepare_cached(
                "SELECT value FROM track_ids WHERE track_id = ?1 AND namespace = ?2
                 ORDER BY rowid DESC LIMIT 1",
            )?
            .query_row(params![track.0, namespace.as_str()], |row| row.get(0))
            .optional()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn track(title: &str, artist: &str) -> TrackRef {
        TrackRef::new(title, ArtistRef::named(artist))
    }

    #[test]
    fn artists_match_on_normalized_name() {
        let s = store();
        let a = s.resolve_artist(&ArtistRef::named("The Beatles")).unwrap();
        let b = s.resolve_artist(&ArtistRef::named("the beatles!")).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn an_external_id_beats_the_name() {
        let s = store();
        let ids = ExternalIds::new().with(Namespace::Spotify, "abc");
        let prince = s
            .resolve_artist(&ArtistRef::named("Prince").with_ids(ids.clone()))
            .unwrap();
        let renamed = s
            .resolve_artist(&ArtistRef::named("The Artist").with_ids(ids))
            .unwrap();
        assert_eq!(prince, renamed);
    }

    #[test]
    fn versions_of_a_song_are_one_track() {
        let s = store();
        let original = s.resolve_track(&track("Heroes", "David Bowie")).unwrap();
        let remaster = s
            .resolve_track(&track("Heroes - 2017 Remaster", "David Bowie"))
            .unwrap();
        assert_eq!(original, remaster);
    }

    #[test]
    fn same_title_by_different_artists_are_different_tracks() {
        let s = store();
        let a = s.resolve_track(&track("Hurt", "Nine Inch Nails")).unwrap();
        let b = s.resolve_track(&track("Hurt", "Johnny Cash")).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn an_isrc_matches_across_differently_titled_releases() {
        let s = store();
        let mut first = track("Song", "Artist");
        first.ids.insert(Namespace::Isrc, "USAAA0000001");
        let mut second = track("Song (2009 Mix)", "Artist");
        second.ids.insert(Namespace::Isrc, "usaaa0000001");
        assert_eq!(
            s.resolve_track(&first).unwrap(),
            s.resolve_track(&second).unwrap()
        );
    }

    #[test]
    fn later_observations_fill_in_details_and_ids() {
        let s = store();
        let bare = s.resolve_track(&track("Song", "Artist")).unwrap();
        let mut rich = track("Song", "Artist");
        rich.duration_ms = Some(200_000);
        rich.ids.insert(Namespace::Spotify, "sp1");
        s.resolve_track(&rich).unwrap();

        let duration: Option<u32> = s
            .conn
            .query_row(
                "SELECT duration_ms FROM tracks WHERE track_id = ?1",
                [bare.0],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(duration, Some(200_000));
        assert_eq!(
            s.track_external_id(bare, Namespace::Spotify)
                .unwrap()
                .as_deref(),
            Some("sp1")
        );
    }

    #[test]
    fn a_track_ref_comes_back_with_its_artist_and_ids() {
        let s = store();
        let mut original = track("Song", "Artist");
        original.duration_ms = Some(1_000);
        original.ids.insert(Namespace::Spotify, "sp1");
        original.artist.ids.insert(Namespace::Spotify, "ar1");
        let id = s.resolve_track(&original).unwrap();
        assert_eq!(s.track_ref(id).unwrap(), original);
    }
}
