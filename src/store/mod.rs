//! The SQLite database that holds the canonical state; `schema.sql` is the
//! whole data model in one place.
//!
//! [`Store`] methods are split by what they touch, one file each, so
//! `store/listens.rs` is everything the store does with listens.

mod identity;
mod listens;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use jiff::Timestamp;
use rusqlite::Connection;

/// Schema versions, applied in order. `PRAGMA user_version` records how many a
/// database has had. Until the first real deploy, version 1 is edited in place.
const MIGRATIONS: &[&str] = &[include_str!("schema.sql")];

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) the database at `path` and brings its schema
    /// up to date.
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // WAL lets a poll read while a build writes.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Store::init(conn)
    }

    /// A throwaway database, for tests.
    pub fn open_in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        migrate(&conn).context("migrating the database schema")?;
        Ok(Store { conn })
    }
}

fn migrate(conn: &Connection) -> Result<()> {
    let applied: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for (version, sql) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

fn to_ms(t: Timestamp) -> i64 {
    t.as_millisecond()
}

fn from_ms(ms: i64) -> rusqlite::Result<Timestamp> {
    Timestamp::from_millisecond(ms).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, e.into())
    })
}

/// Reads a column stored by a `stored_enum!` type back into that type.
fn parse_stored<T>(text: String, parse: fn(&str) -> Option<T>) -> rusqlite::Result<T> {
    parse(&text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unexpected stored value {text:?}").into(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrating_twice_is_a_no_op() {
        let store = Store::open_in_memory().unwrap();
        migrate(&store.conn).unwrap();
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
    }
}
