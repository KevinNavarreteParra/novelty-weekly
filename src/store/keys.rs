//! Rebuilding identity keys after a rule in `model::names` changes (its
//! `KEY_RULES` goes up). Every artist and track is re-keyed, and any two that
//! now share a key are merged: they were one artist, or one song, spelled two
//! ways.

use anyhow::Result;
use log::info;
use rusqlite::{OptionalExtension, params};

use super::Store;
use crate::model::names::{KEY_RULES, name_key, title_key};
use crate::model::{ArtistId, TrackId};

/// The setting that records which `KEY_RULES` the keys were made under.
const KEYED_UNDER: &str = "key_rules";

impl Store {
    /// Rebuilds every key if they were made under older rules. Each merge is
    /// its own transaction, and the version is recorded last, so an
    /// interrupted rebuild simply runs again.
    pub(super) fn ensure_current_keys(&self) -> Result<()> {
        let current = KEY_RULES.to_string();
        if self.setting(KEYED_UNDER)?.as_deref() == Some(current.as_str()) {
            return Ok(());
        }
        let (artists, tracks) = self.rebuild_keys()?;
        if artists + tracks > 0 {
            info!("new name rules: merged {artists} artists and {tracks} tracks spelled two ways");
        }
        self.set_setting(KEYED_UNDER, &current)
    }

    /// Returns how many artists and tracks were merged into another.
    fn rebuild_keys(&self) -> Result<(usize, usize)> {
        let mut merged_artists = 0;
        let artists: Vec<(i64, String, String)> = self
            .conn
            .prepare("SELECT artist_id, name, name_key FROM artists")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, name, old_key) in artists {
            let key = name_key(&name);
            if key == old_key {
                continue;
            }
            let existing: Option<i64> = self
                .conn
                .query_row(
                    "SELECT artist_id FROM artists WHERE name_key = ?1",
                    [&key],
                    |row| row.get(0),
                )
                .optional()?;
            match existing {
                Some(keep) => {
                    self.merge_artist_into(ArtistId(keep), ArtistId(id))?;
                    merged_artists += 1;
                }
                None => {
                    self.conn.execute(
                        "UPDATE artists SET name_key = ?1 WHERE artist_id = ?2",
                        params![key, id],
                    )?;
                }
            }
        }

        let mut merged_tracks = 0;
        let tracks: Vec<(i64, i64, String, String)> = self
            .conn
            .prepare("SELECT track_id, artist_id, title, title_key FROM tracks")?
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (id, artist, title, old_key) in tracks {
            let key = title_key(&title);
            if key == old_key {
                continue;
            }
            match self.track_with_key(artist, &key)? {
                Some(keep) => {
                    self.merge_track_into(TrackId(keep), TrackId(id))?;
                    merged_tracks += 1;
                }
                None => {
                    self.conn.execute(
                        "UPDATE tracks SET title_key = ?1 WHERE track_id = ?2",
                        params![key, id],
                    )?;
                }
            }
        }
        Ok((merged_artists, merged_tracks))
    }

    /// Folds `duplicate` into `keep`: its songs (merged with the kept artist's
    /// copy of the same song), IDs, knowledge, rankings and recommendations.
    pub fn merge_artist_into(&self, keep: ArtistId, duplicate: ArtistId) -> Result<()> {
        if keep == duplicate {
            return Ok(());
        }
        let tracks: Vec<(i64, String)> = self
            .conn
            .prepare("SELECT track_id, title_key FROM tracks WHERE artist_id = ?1")?
            .query_map([duplicate.0], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (track, key) in tracks {
            match self.track_with_key(keep.0, &key)? {
                Some(same) => self.merge_track_into(TrackId(same), TrackId(track))?,
                None => {
                    self.conn.execute(
                        "UPDATE tracks SET artist_id = ?1 WHERE track_id = ?2",
                        params![keep.0, track],
                    )?;
                }
            }
        }

        let tx = self.conn.unchecked_transaction()?;
        for (table, column) in [
            ("artist_ids", "artist_id"),
            ("similar_artists", "artist_id"),
            ("similar_artists", "similar_id"),
            ("artist_tags", "artist_id"),
            ("popular_tracks", "artist_id"),
            ("top_artists", "artist_id"),
            ("ledger", "artist_id"),
        ] {
            self.conn.execute(
                &format!("UPDATE OR IGNORE {table} SET {column} = ?1 WHERE {column} = ?2"),
                params![keep.0, duplicate.0],
            )?;
            self.conn.execute(
                &format!("DELETE FROM {table} WHERE {column} = ?1"),
                [duplicate.0],
            )?;
        }
        // Similar to itself, now that both sides are one artist.
        self.conn.execute(
            "DELETE FROM similar_artists WHERE artist_id = similar_id",
            [],
        )?;
        self.conn.execute(
            "UPDATE artists SET listeners = COALESCE(listeners,
                 (SELECT listeners FROM artists WHERE artist_id = ?2))
             WHERE artist_id = ?1",
            params![keep.0, duplicate.0],
        )?;
        self.conn
            .execute("DELETE FROM artists WHERE artist_id = ?1", [duplicate.0])?;
        tx.commit()?;
        Ok(())
    }

    fn track_with_key(&self, artist: i64, key: &str) -> Result<Option<i64>> {
        let found = self
            .conn
            .query_row(
                "SELECT track_id FROM tracks WHERE artist_id = ?1 AND title_key = ?2",
                params![artist, key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::model::{ArtistRef, Ending, Listen, Provenance, StartedBy, TrackRef};

    fn listen(artist: &str, title: &str, at: &str) -> Listen {
        Listen {
            track: TrackRef::new(title, ArtistRef::named(artist)),
            started_at: at.parse().unwrap(),
            ms_played: Some(200_000),
            ending: Ending::Finished,
            started_by: StartedBy::Listener,
            provenance: Provenance::History,
        }
    }

    /// A store holding "Travi$ Scott" keyed under the old rules, as
    /// "travi scott", beside "Travis Scott".
    fn store_keyed_under_old_rules() -> Store {
        let store = Store::open_in_memory().unwrap();
        let saved_at: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        store
            .record_saved(&[(
                TrackRef::new("Antidote", ArtistRef::named("Travis Scott")),
                saved_at,
            )])
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO artists (name, name_key) VALUES ('Travi$ Scott', 'travi scott')",
                [],
            )
            .unwrap();
        let old = store.conn.last_insert_rowid();
        store
            .conn
            .execute(
                "INSERT INTO tracks (artist_id, title, title_key) VALUES (?1, 'Antidote', 'antidote'),
                                                                        (?1, '90210', '90210')",
                [old],
            )
            .unwrap();
        store.set_setting(KEYED_UNDER, "1").unwrap();
        store
    }

    #[test]
    fn new_rules_merge_an_artist_spelled_two_ways() {
        let store = store_keyed_under_old_rules();
        store.ensure_current_keys().unwrap();

        let artists: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM artists", [], |r| r.get(0))
            .unwrap();
        assert_eq!(artists, 1);
        let travis = store
            .resolve_artist(&ArtistRef::named("Travi$ Scott"))
            .unwrap();
        assert_eq!(
            store.artist_names([travis]).unwrap()[&travis],
            "Travis Scott"
        );
        // "Antidote" was the same song twice; "90210" moved over.
        let tracks: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM tracks WHERE artist_id = ?1",
                [travis.0],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tracks, 2);
        assert_eq!(
            store.setting(KEYED_UNDER).unwrap(),
            Some(KEY_RULES.to_string())
        );
    }

    #[test]
    fn merging_artists_keeps_both_copies_of_a_song_as_one() {
        let store = Store::open_in_memory().unwrap();
        store
            .import_history(&[listen("Low", "Words", "2026-01-01T00:00:00Z")])
            .unwrap();
        let low = store.resolve_artist(&ArtistRef::named("Low")).unwrap();
        // The same band and song under a key the current rules wouldn't make.
        store
            .conn
            .execute_batch(
                "INSERT INTO artists (artist_id, name, name_key) VALUES (900, 'Low', 'low old');
                 INSERT INTO tracks (track_id, artist_id, title, title_key)
                     VALUES (900, 900, 'Words', 'words');
                 INSERT INTO plays (track_id, started_at, ms_played, ending, started_by, provenance)
                     VALUES (900, 1767312000000, 200000, 'finished', 'listener', 'history');",
            )
            .unwrap();

        store.merge_artist_into(low, ArtistId(900)).unwrap();
        let plays = store.plays().unwrap();
        assert_eq!(plays.len(), 2);
        assert!(plays.iter().all(|p| p.artist == low));
        assert_eq!(plays[0].track, plays[1].track, "one song, two plays");
    }
}
