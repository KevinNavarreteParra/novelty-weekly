//! Raw API responses kept for a while, so a re-run doesn't re-fetch.
//!
//! This lives in its own `cache.db`, apart from the canonical database: it is
//! a convenience for integrations, not part of the app's state, and deleting
//! it only costs time.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};

pub const DAY: Duration = Duration::from_secs(86_400);

pub struct Cache {
    conn: Connection,
}

impl Cache {
    pub fn open(path: &Path) -> Result<Cache> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Cache::init(conn)
    }

    pub fn open_in_memory() -> Result<Cache> {
        Cache::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Cache> {
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS responses (
                 key        TEXT PRIMARY KEY,
                 body       TEXT NOT NULL,
                 fetched_at INTEGER NOT NULL,   -- unix ms
                 ttl_s      INTEGER NOT NULL
             )",
        )?;
        Ok(Cache { conn })
    }

    /// The body cached under `key`, or else `fetch()`'s result, cached for
    /// `ttl`. Errors are never cached; the next call simply tries again.
    pub fn get_or_fetch(
        &self,
        key: &str,
        ttl: Duration,
        fetch: impl FnOnce() -> Result<String>,
    ) -> Result<String> {
        if let Some(body) = self.get(key)? {
            return Ok(body);
        }
        let body = fetch()?;
        self.put(key, &body, ttl)?;
        Ok(body)
    }

    /// The body cached under `key`, if it is younger than its TTL.
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        self.get_at(key, Timestamp::now())
    }

    pub fn put(&self, key: &str, body: &str, ttl: Duration) -> Result<()> {
        self.put_at(key, body, ttl, Timestamp::now())
    }

    fn get_at(&self, key: &str, now: Timestamp) -> Result<Option<String>> {
        let body = self
            .conn
            .prepare_cached(
                "SELECT body FROM responses WHERE key = ?1 AND fetched_at + ttl_s * 1000 > ?2",
            )?
            .query_row(params![key, now.as_millisecond()], |row| row.get(0))
            .optional()?;
        Ok(body)
    }

    fn put_at(&self, key: &str, body: &str, ttl: Duration, now: Timestamp) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT OR REPLACE INTO responses (key, body, fetched_at, ttl_s) VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![key, body, now.as_millisecond(), ttl.as_secs() as i64])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn a_fresh_entry_is_served_without_fetching() {
        let cache = Cache::open_in_memory().unwrap();
        let fetches = Cell::new(0);
        let fetch = || {
            fetches.set(fetches.get() + 1);
            Ok("body".to_owned())
        };
        assert_eq!(cache.get_or_fetch("k", DAY, fetch).unwrap(), "body");
        assert_eq!(cache.get_or_fetch("k", DAY, fetch).unwrap(), "body");
        assert_eq!(fetches.get(), 1);
    }

    #[test]
    fn an_expired_entry_is_not_served() {
        let cache = Cache::open_in_memory().unwrap();
        let then = Timestamp::from_second(1_000_000).unwrap();
        cache.put_at("k", "old", DAY, then).unwrap();
        let next_day = Timestamp::from_second(1_000_000 + 86_401).unwrap();
        assert_eq!(cache.get_at("k", then).unwrap().as_deref(), Some("old"));
        assert_eq!(cache.get_at("k", next_day).unwrap(), None);
    }

    #[test]
    fn errors_are_not_cached() {
        let cache = Cache::open_in_memory().unwrap();
        assert!(
            cache
                .get_or_fetch("k", DAY, || anyhow::bail!("down"))
                .is_err()
        );
        assert_eq!(
            cache.get_or_fetch("k", DAY, || Ok("up".into())).unwrap(),
            "up"
        );
    }
}
