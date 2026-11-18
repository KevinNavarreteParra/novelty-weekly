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

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::Store;
use crate::model::{ArtistId, ArtistRef, ExternalIds, Namespace, TrackId, TrackRef};

/// Two artists whose names overlap are one when their popular tracks share
/// at least this many songs (see [`Store::merge_artists_sharing_songs`]).
pub const ALIAS_MIN_SHARED: i64 = 5;

/// Whether one name key's words appear, in order and together, in the other's:
/// "marina" in "marina and the diamonds", "the four seasons" in "frankie
/// valli and the four seasons".
fn one_name_holds_the_other(a: &str, b: &str) -> bool {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.split(' ').collect(), b.split(' ').collect());
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    !short.is_empty() && long.windows(short.len()).any(|w| w == short.as_slice())
}

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

    /// The artist with this name key, or that goes by it, created if new.
    fn artist_by_key(&self, artist: &ArtistRef) -> Result<i64> {
        let key = artist.key();
        let alias: Option<i64> = self
            .conn
            .prepare_cached("SELECT artist_id FROM artist_aliases WHERE name_key = ?1")?
            .query_row([&key], |row| row.get(0))
            .optional()?;
        if let Some(id) = alias {
            return Ok(id);
        }
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

    /// Records that `found`, a service's copy of a song, is `track`: its IDs
    /// and details are added to `track`.
    pub fn record_match(&self, track: TrackId, found: &TrackRef) -> Result<()> {
        self.fill_track_details(track.0, found)?;
        self.attach_ids(&TRACK_IDS, track.0, &found.ids)
    }

    /// Folds `duplicate` into `keep` when they turn out to be the same song
    /// under two titles ("Pt. 1" and "Part 1"): every row pointing at
    /// `duplicate` moves to `keep`, and `duplicate` is deleted. Where `keep`
    /// already has an equivalent row (a play at the same moment), `keep`'s wins.
    pub fn merge_track_into(&self, keep: TrackId, duplicate: TrackId) -> Result<()> {
        if keep == duplicate {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        for table in [
            "track_ids",
            "plays",
            "saved_tracks",
            "top_tracks",
            "popular_tracks",
            "ledger",
            "playlist_tracks",
        ] {
            self.conn.execute(
                &format!("UPDATE OR IGNORE {table} SET track_id = ?1 WHERE track_id = ?2"),
                params![keep.0, duplicate.0],
            )?;
            self.conn.execute(
                &format!("DELETE FROM {table} WHERE track_id = ?1"),
                [duplicate.0],
            )?;
        }
        self.conn
            .execute("DELETE FROM tracks WHERE track_id = ?1", [duplicate.0])?;
        tx.commit()?;
        Ok(())
    }

    /// Merges artists that are one act under two names. Last.fm keeps some
    /// acts on two pages ("Marina" and "Marina & the Diamonds", "S Club 7" and
    /// "S Club"), and a duo is its first-credited artist. Two artists are one
    /// when one's name contains the other's, word for word, and their popular
    /// tracks share at least [`ALIAS_MIN_SHARED`] songs, at least half of the
    /// shorter list. The one played more, else the older, keeps its name.
    ///
    /// Run it only while no store IDs are held elsewhere: it deletes the
    /// merged artist and its duplicate songs. Returns `(kept, merged)` names.
    pub fn merge_artists_sharing_songs(&self) -> Result<Vec<(String, String)>> {
        let mut merged = Vec::new();
        loop {
            let pairs: Vec<(i64, String, i64, String)> = self
                .conn
                .prepare_cached(
                    "WITH songs AS (
                         SELECT DISTINCT p.artist_id AS artist, t.title_key AS song
                         FROM popular_tracks p JOIN tracks t USING (track_id)),
                     sizes AS (SELECT artist, COUNT(*) AS n FROM songs GROUP BY artist),
                     pairs AS (
                         SELECT x.artist AS a, y.artist AS b, COUNT(*) AS shared
                         FROM songs x JOIN songs y ON x.song = y.song AND x.artist < y.artist
                         GROUP BY x.artist, y.artist HAVING COUNT(*) >= ?1)
                     SELECT pairs.a, aa.name_key, pairs.b, bb.name_key
                     FROM pairs
                     JOIN sizes sa ON sa.artist = pairs.a
                     JOIN sizes sb ON sb.artist = pairs.b
                     JOIN artists aa ON aa.artist_id = pairs.a
                     JOIN artists bb ON bb.artist_id = pairs.b
                     WHERE 2 * pairs.shared >= MIN(sa.n, sb.n)
                     ORDER BY pairs.shared DESC, pairs.a, pairs.b",
                )?
                .query_map([ALIAS_MIN_SHARED], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let mut touched = HashSet::new();
            let before = merged.len();
            for (a, a_key, b, b_key) in pairs {
                if touched.contains(&a)
                    || touched.contains(&b)
                    || !one_name_holds_the_other(&a_key, &b_key)
                {
                    continue;
                }
                let (keep, gone) = if self.play_count(b)? > self.play_count(a)? {
                    (b, a)
                } else {
                    (a, b)
                };
                let names = (
                    self.artist_ref(ArtistId(keep))?.name,
                    self.artist_ref(ArtistId(gone))?.name,
                );
                self.merge_artist_into(ArtistId(keep), ArtistId(gone))?;
                // Sources still use the other name; it should find this artist.
                self.conn.execute(
                    "INSERT OR REPLACE INTO artist_aliases (name_key, name, artist_id)
                     VALUES (?1, ?2, ?3)",
                    params![if keep == a { &b_key } else { &a_key }, names.1, keep],
                )?;
                touched.extend([a, b]);
                merged.push(names);
            }
            // A merge can make a new pair (an artist with two aliases).
            if merged.len() == before {
                return Ok(merged);
            }
        }
    }

    fn play_count(&self, artist: i64) -> Result<i64> {
        let n = self
            .conn
            .prepare_cached(
                "SELECT COUNT(*) FROM plays p JOIN tracks t USING (track_id) WHERE t.artist_id = ?1",
            )?
            .query_row([artist], |row| row.get(0))?;
        Ok(n)
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
    fn merging_moves_ids_to_the_kept_track() {
        let s = store();
        let keep = s.resolve_track(&track("Song Part 1", "Artist")).unwrap();
        let mut other = track("Song Pt 1", "Artist");
        other.ids.insert(Namespace::Spotify, "sp1");
        let duplicate = s.resolve_track(&other).unwrap();
        assert_ne!(keep, duplicate);
        s.record_top_tracks(crate::model::Span::Month, std::slice::from_ref(&other))
            .unwrap();

        s.merge_track_into(keep, duplicate).unwrap();
        assert_eq!(s.resolve_track(&other).unwrap(), keep);
        assert!(s.track_ref(duplicate).is_err());
        let ranked = &s.cold_start().unwrap().top_tracks[&crate::model::Span::Month];
        assert_eq!(ranked[0].0, keep, "a ranking follows the merge");
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

    fn record_hits(s: &Store, artist: &str, titles: &[&str]) -> ArtistId {
        let id = s.resolve_artist(&ArtistRef::named(artist)).unwrap();
        let popular: Vec<crate::model::PopularTrack> = titles
            .iter()
            .map(|t| crate::model::PopularTrack {
                track: track(t, artist),
                listeners: None,
            })
            .collect();
        s.record_popular(id, "lastfm", &popular).unwrap();
        id
    }

    const HITS: &[&str] = &[
        "I Am Not a Robot",
        "Primadonna",
        "Bubblegum Bitch",
        "Oh No!",
        "Hollywood",
        "Froot",
    ];

    #[test]
    fn one_act_under_two_names_becomes_one_artist() {
        let s = store();
        let marina = record_hits(&s, "Marina", HITS);
        let legacy = record_hits(&s, "Marina & the Diamonds", &HITS[..5]);
        let merged = s.merge_artists_sharing_songs().unwrap();
        assert_eq!(
            merged,
            vec![("Marina".to_owned(), "Marina & the Diamonds".to_owned())]
        );
        assert_ne!(marina, legacy);
        // The other name still finds the artist, so it isn't created again.
        assert_eq!(
            s.resolve_artist(&ArtistRef::named("Marina and the Diamonds"))
                .unwrap(),
            marina
        );
        let robot = s
            .resolve_track(&track("I Am Not a Robot", "Marina"))
            .unwrap();
        let refs = s.track_ref(robot).unwrap();
        assert_eq!(refs.artist.name, "Marina");
        assert_eq!(s.popular(&[marina]).unwrap()[&marina].len(), HITS.len());
        assert!(s.merge_artists_sharing_songs().unwrap().is_empty());
    }

    #[test]
    fn the_name_i_have_played_is_the_one_kept() {
        let s = store();
        record_hits(&s, "Marina", HITS);
        record_hits(&s, "Marina & the Diamonds", HITS);
        s.record_listens(&[crate::model::Listen {
            track: track("Primadonna", "Marina & the Diamonds"),
            started_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            ms_played: Some(200_000),
            ending: crate::model::Ending::Finished,
            started_by: crate::model::StartedBy::Listener,
            provenance: crate::model::Provenance::Poll,
        }])
        .unwrap();
        let merged = s.merge_artists_sharing_songs().unwrap();
        assert_eq!(merged[0].0, "Marina & the Diamonds");
    }

    #[test]
    fn shared_hits_alone_or_a_shared_name_alone_is_not_enough() {
        let s = store();
        record_hits(&s, "Marina", HITS);
        // A covers act sings the same songs under an unrelated name.
        record_hits(&s, "Karaoke All Stars", HITS);
        // A different Marina shares a word and two titles.
        record_hits(
            &s,
            "Marina Kaye",
            &["Froot", "Hollywood", "Homeless", "Freeze You Out"],
        );
        assert!(s.merge_artists_sharing_songs().unwrap().is_empty());
    }

    #[test]
    fn names_hold_each_other_word_for_word() {
        assert!(one_name_holds_the_other(
            "marina",
            "marina and the diamonds"
        ));
        assert!(one_name_holds_the_other(
            "frankie valli and the four seasons",
            "the four seasons"
        ));
        assert!(!one_name_holds_the_other("marina", "supersubmarina"));
        assert!(!one_name_holds_the_other("s club", "club s"));
    }
}
