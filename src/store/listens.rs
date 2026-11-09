//! Recording listens, and reading them back as plays.

use anyhow::Result;
use jiff::Timestamp;
use rusqlite::params;

use super::{Store, from_ms, parse_stored, to_ms};
use crate::model::{ArtistId, Ending, Listen, Play, Provenance, StartedBy, TrackId};

/// What an import changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// Listens added, or completed where `ms_played` had been unknown.
    pub recorded: usize,
    /// Polled plays replaced because the import covers their span.
    pub replaced_polled: usize,
}

impl Store {
    /// Records listens, resolving each one's track.
    ///
    /// A listen already on file is left alone, except that one whose
    /// `ms_played` was unknown gets completed by a later observation. Returns
    /// how many rows were added or completed.
    pub fn record_listens(&self, listens: &[Listen]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let recorded = self.insert_listens(listens)?;
        tx.commit()?;
        Ok(recorded)
    }

    /// Imports a full listening-history export.
    ///
    /// An export is a better record than polling for the span it covers (exact
    /// `ms_played`, real endings), so polled plays inside that span go first.
    /// Re-importing the same export changes nothing.
    pub fn import_history(&self, listens: &[Listen]) -> Result<ImportSummary> {
        let Some((first, last)) = span(listens) else {
            return Ok(ImportSummary::default());
        };
        let tx = self.conn.unchecked_transaction()?;
        let replaced_polled = self.conn.execute(
            "DELETE FROM plays WHERE provenance IN (?1, ?2) AND started_at BETWEEN ?3 AND ?4",
            params![
                Provenance::Poll.as_str(),
                Provenance::PollEstimated.as_str(),
                to_ms(first),
                to_ms(last)
            ],
        )?;
        let recorded = self.insert_listens(listens)?;
        tx.commit()?;
        Ok(ImportSummary {
            recorded,
            replaced_polled,
        })
    }

    /// When the newest polled listen started: where the next poll picks up.
    pub fn latest_polled_start(&self) -> Result<Option<Timestamp>> {
        let ms: Option<i64> = self.conn.query_row(
            "SELECT MAX(started_at) FROM plays WHERE provenance IN (?1, ?2)",
            params![
                Provenance::Poll.as_str(),
                Provenance::PollEstimated.as_str()
            ],
            |row| row.get(0),
        )?;
        ms.map(from_ms).transpose().map_err(Into::into)
    }

    /// Every play on file, oldest first.
    pub fn plays(&self) -> Result<Vec<Play>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.track_id, t.artist_id, p.started_at, p.ms_played, t.duration_ms,
                    p.ending, p.started_by
             FROM plays p JOIN tracks t USING (track_id)
             ORDER BY p.started_at",
        )?;
        let plays = stmt
            .query_map([], |row| {
                Ok(Play {
                    track: TrackId(row.get(0)?),
                    artist: ArtistId(row.get(1)?),
                    started_at: from_ms(row.get(2)?)?,
                    ms_played: row.get(3)?,
                    duration_ms: row.get(4)?,
                    ending: parse_stored(row.get(5)?, Ending::parse)?,
                    started_by: parse_stored(row.get(6)?, StartedBy::parse)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(plays)
    }

    /// Callers wrap this in a transaction; thousands of single-row commits
    /// would take minutes.
    fn insert_listens(&self, listens: &[Listen]) -> Result<usize> {
        let mut recorded = 0;
        for listen in listens {
            let track = self.resolve_track(&listen.track)?;
            recorded += self
                .conn
                .prepare_cached(
                    "INSERT INTO plays (track_id, started_at, ms_played, ending, started_by, provenance)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT (track_id, started_at) DO UPDATE SET
                         ms_played = excluded.ms_played,
                         ending = excluded.ending,
                         provenance = excluded.provenance
                     WHERE plays.ms_played IS NULL AND excluded.ms_played IS NOT NULL",
                )?
                .execute(params![
                    track.0,
                    to_ms(listen.started_at),
                    listen.ms_played,
                    listen.ending.as_str(),
                    listen.started_by.as_str(),
                    listen.provenance.as_str(),
                ])?;
        }
        Ok(recorded)
    }
}

/// The earliest and latest start among `listens`.
fn span(listens: &[Listen]) -> Option<(Timestamp, Timestamp)> {
    let first = listens.iter().map(|l| l.started_at).min()?;
    let last = listens.iter().map(|l| l.started_at).max()?;
    Some((first, last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArtistRef, TrackRef};

    fn listen(title: &str, at_s: i64, ms_played: Option<u32>, provenance: Provenance) -> Listen {
        Listen {
            track: TrackRef::new(title, ArtistRef::named("Artist")),
            started_at: Timestamp::from_second(at_s).unwrap(),
            ms_played,
            ending: Ending::Unknown,
            started_by: StartedBy::Unknown,
            provenance,
        }
    }

    #[test]
    fn recording_the_same_listen_twice_is_a_no_op() {
        let s = Store::open_in_memory().unwrap();
        let batch = [listen("A", 100, Some(200_000), Provenance::Poll)];
        assert_eq!(s.record_listens(&batch).unwrap(), 1);
        assert_eq!(s.record_listens(&batch).unwrap(), 0);
        assert_eq!(s.plays().unwrap().len(), 1);
    }

    #[test]
    fn a_pending_listen_is_completed_but_never_overwritten() {
        let s = Store::open_in_memory().unwrap();
        s.record_listens(&[listen("A", 100, None, Provenance::Poll)])
            .unwrap();
        s.record_listens(&[listen("A", 100, Some(90_000), Provenance::Poll)])
            .unwrap();
        s.record_listens(&[listen("A", 100, Some(1), Provenance::Poll)])
            .unwrap();
        assert_eq!(s.plays().unwrap()[0].ms_played, Some(90_000));
    }

    #[test]
    fn history_replaces_polled_plays_only_inside_its_span() {
        let s = Store::open_in_memory().unwrap();
        s.record_listens(&[
            listen("inside", 150, Some(1_000), Provenance::Poll),
            listen("after", 999, Some(1_000), Provenance::Poll),
        ])
        .unwrap();
        let history = [
            listen("A", 100, Some(200_000), Provenance::History),
            listen("B", 200, Some(200_000), Provenance::History),
        ];

        let summary = s.import_history(&history).unwrap();
        assert_eq!(
            summary,
            ImportSummary {
                recorded: 2,
                replaced_polled: 1
            }
        );
        assert_eq!(s.plays().unwrap().len(), 3);

        let again = s.import_history(&history).unwrap();
        assert_eq!(again, ImportSummary::default());
    }
}
