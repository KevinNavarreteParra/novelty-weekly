//! The rest of the state: the library, a few settings, and the run log.

use std::collections::HashMap;

use anyhow::{Context, Result};
use jiff::Timestamp;
use rusqlite::{OptionalExtension, params};

use super::{Store, from_ms, to_ms};
use crate::model::{TrackId, TrackRef};

/// The playlist the app writes every week, once it exists.
pub const PLAYLIST_ID: &str = "playlist_id";

impl Store {
    /// Records saved tracks; returns how many were new.
    pub fn record_saved(&self, saved: &[(TrackRef, Timestamp)]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut added = 0;
        for (track, at) in saved {
            let id = self.resolve_track(track)?;
            added += self
                .conn
                .prepare_cached(
                    "INSERT OR IGNORE INTO saved_tracks (track_id, saved_at) VALUES (?1, ?2)",
                )?
                .execute(params![id.0, to_ms(*at)])?;
        }
        tx.commit()?;
        Ok(added)
    }

    pub fn saved_tracks(&self) -> Result<HashMap<TrackId, Timestamp>> {
        let mut stmt = self
            .conn
            .prepare("SELECT track_id, saved_at FROM saved_tracks")?;
        let saved = stmt
            .query_map([], |row| Ok((TrackId(row.get(0)?), from_ms(row.get(1)?)?)))?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?;
        Ok(saved)
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(value)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Writes a consistent copy of the database to `path`, which must not exist.
    pub fn back_up_to(&self, path: &std::path::Path) -> Result<()> {
        let path = path.to_str().context("backup path isn't UTF-8")?;
        self.conn.execute("VACUUM INTO ?1", [path])?;
        Ok(())
    }

    /// Starts a run log entry; finish it with [`Store::finish_run`].
    pub fn start_run(&self, kind: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs (kind, started_at) VALUES (?1, ?2)",
            params![kind, to_ms(Timestamp::now())],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run(&self, run: i64, ok: bool, notes: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET finished_at = ?2, ok = ?3, notes = ?4 WHERE run_id = ?1",
            params![run, to_ms(Timestamp::now()), ok, notes],
        )?;
        Ok(())
    }

    /// How many of the latest runs of `kind` failed in a row.
    pub fn consecutive_failures(&self, kind: &str) -> Result<u32> {
        let mut stmt = self.conn.prepare(
            "SELECT ok FROM runs WHERE kind = ?1 AND finished_at IS NOT NULL ORDER BY run_id DESC",
        )?;
        let mut rows = stmt.query([kind])?;
        let mut failures = 0;
        while let Some(row) = rows.next()? {
            if row.get::<_, bool>(0)? {
                break;
            }
            failures += 1;
        }
        Ok(failures)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ArtistRef;

    #[test]
    fn a_backup_holds_the_same_data() {
        let store = Store::open_in_memory().unwrap();
        store.set_setting(PLAYLIST_ID, "p1").unwrap();
        let dir = std::env::temp_dir().join(format!("novelty-backup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("copy.db");
        store.back_up_to(&path).unwrap();
        let copy = Store::open(&path).unwrap();
        let playlist = copy.setting(PLAYLIST_ID).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(playlist.as_deref(), Some("p1"));
    }

    #[test]
    fn settings_overwrite() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.setting(PLAYLIST_ID).unwrap(), None);
        s.set_setting(PLAYLIST_ID, "a").unwrap();
        s.set_setting(PLAYLIST_ID, "b").unwrap();
        assert_eq!(s.setting(PLAYLIST_ID).unwrap().as_deref(), Some("b"));
    }

    #[test]
    fn saving_twice_counts_once() {
        let s = Store::open_in_memory().unwrap();
        let save = [(
            TrackRef::new("T", ArtistRef::named("A")),
            Timestamp::UNIX_EPOCH,
        )];
        assert_eq!(s.record_saved(&save).unwrap(), 1);
        assert_eq!(s.record_saved(&save).unwrap(), 0);
        assert_eq!(s.saved_tracks().unwrap().len(), 1);
    }

    #[test]
    fn failures_are_counted_back_to_the_last_success() {
        let s = Store::open_in_memory().unwrap();
        for ok in [false, true, false, false] {
            let run = s.start_run("poll").unwrap();
            s.finish_run(run, ok, "").unwrap();
        }
        assert_eq!(s.consecutive_failures("poll").unwrap(), 2);
        assert_eq!(s.consecutive_failures("build").unwrap(), 0);
    }
}
