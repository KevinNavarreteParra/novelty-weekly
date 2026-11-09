//! The ledger: every recommendation ever published.

use std::collections::HashSet;

use anyhow::{Context, Result};
use rusqlite::params;

use super::{Store, from_ms, parse_stored, to_ms};
use crate::model::{Arm, ArtistId, Pool, Recommendation, TrackId, Week};

impl Store {
    /// Every recommendation, oldest week first, in playlist order.
    pub fn ledger(&self) -> Result<Vec<Recommendation>> {
        let mut stmt = self.conn.prepare(
            "SELECT week, published_at, position, track_id, artist_id, pool, arm, score, rationale
             FROM ledger ORDER BY published_at, position",
        )?;
        let mut rows = stmt.query([])?;
        let mut recs = Vec::new();
        while let Some(row) = rows.next()? {
            let week: String = row.get(0)?;
            let arm: String = row.get(6)?;
            recs.push(Recommendation {
                week: week
                    .parse()
                    .with_context(|| format!("ledger week {week:?}"))?,
                published_at: from_ms(row.get(1)?)?,
                position: row.get(2)?,
                track: TrackId(row.get(3)?),
                artist: ArtistId(row.get(4)?),
                pool: parse_stored(row.get(5)?, Pool::parse)?,
                arm: arm.parse::<Arm>()?,
                score: row.get(7)?,
                rationale: row.get(8)?,
            });
        }
        Ok(recs)
    }

    pub fn recommended_tracks(&self) -> Result<HashSet<TrackId>> {
        let mut stmt = self.conn.prepare("SELECT track_id FROM ledger")?;
        let ids = stmt
            .query_map([], |row| Ok(TrackId(row.get(0)?)))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        Ok(ids)
    }

    /// Records `recs` and runs `publish`, as one transaction: if publishing
    /// fails, nothing is recorded (spec §7.6). The ledger's UNIQUE track
    /// constraint is checked before `publish` runs, so a repeat can't be
    /// published either.
    pub fn record_recommendations<T>(
        &self,
        recs: &[Recommendation],
        publish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let tx = self.conn.unchecked_transaction()?;
        for rec in recs {
            self.conn
                .prepare_cached(
                    "INSERT INTO ledger
                         (week, published_at, position, track_id, artist_id, pool, arm, score, rationale)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )?
                .execute(params![
                    rec.week.to_string(),
                    to_ms(rec.published_at),
                    rec.position,
                    rec.track.0,
                    rec.artist.0,
                    rec.pool.as_str(),
                    rec.arm.to_string(),
                    rec.score,
                    rec.rationale,
                ])
                .with_context(|| format!("recording track {} in the ledger", rec.track.0))?;
        }
        let published = publish()?;
        tx.commit()?;
        Ok(published)
    }

    /// Weeks with a published playlist, newest first.
    pub fn ledger_weeks(&self) -> Result<Vec<Week>> {
        let mut stmt = self
            .conn
            .prepare("SELECT week FROM ledger GROUP BY week ORDER BY MAX(published_at) DESC")?;
        let weeks = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        weeks.iter().map(|w| w.parse()).collect()
    }
}

#[cfg(test)]
mod tests {
    use anyhow::bail;
    use jiff::Timestamp;

    use super::*;
    use crate::model::{ArtistRef, TrackRef};

    fn rec(s: &Store, title: &str) -> Recommendation {
        let track = s
            .resolve_track(&TrackRef::new(title, ArtistRef::named("A")))
            .unwrap();
        Recommendation {
            week: "2026-W46".parse().unwrap(),
            published_at: Timestamp::from_second(1_000).unwrap(),
            position: 0,
            track,
            artist: ArtistId(1),
            pool: Pool::Near,
            arm: Arm::Pool(Pool::Near),
            score: 0.5,
            rationale: "because".into(),
        }
    }

    #[test]
    fn a_failed_publish_records_nothing() {
        let s = Store::open_in_memory().unwrap();
        let recs = [rec(&s, "one")];
        let result: Result<()> = s.record_recommendations(&recs, || bail!("spotify is down"));
        assert!(result.is_err());
        assert!(s.ledger().unwrap().is_empty());
    }

    #[test]
    fn a_published_week_round_trips() {
        let s = Store::open_in_memory().unwrap();
        let recs = [rec(&s, "one"), rec(&s, "two")];
        s.record_recommendations(&recs, || Ok(())).unwrap();
        assert_eq!(s.ledger().unwrap(), recs);
        assert_eq!(s.recommended_tracks().unwrap().len(), 2);
        assert_eq!(s.ledger_weeks().unwrap()[0].to_string(), "2026-W46");
    }

    #[test]
    fn a_track_can_never_be_recommended_twice() {
        let s = Store::open_in_memory().unwrap();
        s.record_recommendations(&[rec(&s, "one")], || Ok(()))
            .unwrap();
        let mut published = false;
        let again = s.record_recommendations(&[rec(&s, "one")], || {
            published = true;
            Ok(())
        });
        assert!(again.is_err());
        assert!(!published);
    }
}
