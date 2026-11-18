//! Recording listens, and reading them back as plays.

use std::collections::BTreeMap;

use anyhow::Result;
use jiff::Timestamp;
use log::info;
use rusqlite::params;

use super::{Store, from_ms, parse_stored, to_ms};
use crate::model::{
    ArtistId, Ending, Listen, Play, Provenance, SKIP_BEFORE_MS, StartedBy, TrackId,
    played_since_previous,
};

/// Two streams of a track whose lengths differ by at most this much both
/// played it to the end (see [`Store::plays`]).
pub const AGREED_LENGTH_MS: i64 = 2_000;

/// The setting that says polled plays are measured from the previous
/// listen's end, because the feed's `played_at` is an end time. Plays polled
/// before it was set were stored as if `played_at` were the start.
const POLL_TIMING: &str = "poll_timing";
const FROM_PREVIOUS_END: &str = "previous_end";

/// What an import changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// Listens added, or completed where `ms_played` had been unknown.
    pub recorded: usize,
    /// Plays from less detailed sources (polls, a basic export) removed
    /// because the import covers their span.
    pub replaced: usize,
    /// Listens left out because a more detailed history already covers them.
    pub covered: usize,
}

impl Store {
    /// Records listens, resolving each one's track.
    ///
    /// A listen already on file is left alone, except that one whose
    /// `ms_played` was unknown gets completed by a later observation. Returns
    /// how many rows were added or completed.
    pub fn record_listens(&self, listens: &[Listen]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let recorded = self.insert_listens(listens.iter())?;
        tx.commit()?;
        Ok(recorded)
    }

    /// Imports a listening-history export.
    ///
    /// Where two sources cover the same span, the plays of the more detailed
    /// one stand ([`Provenance::detail`]). So an import removes less detailed
    /// plays (polled, or a basic export's) inside its span, and leaves out its
    /// own listens inside the span of a more detailed history already on file.
    /// Listens from several sources are imported most detailed first.
    /// Re-importing the same export changes nothing.
    pub fn import_history(&self, listens: &[Listen]) -> Result<ImportSummary> {
        let mut by_source: BTreeMap<std::cmp::Reverse<u8>, (Provenance, Vec<&Listen>)> =
            BTreeMap::new();
        for listen in listens {
            by_source
                .entry(std::cmp::Reverse(listen.provenance.detail()))
                .or_insert_with(|| (listen.provenance, Vec::new()))
                .1
                .push(listen);
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut summary = ImportSummary::default();
        for (provenance, listens) in by_source.into_values() {
            // A listen may really have started up to its source's uncertainty
            // later than recorded, so one just before a span may be inside it.
            let slack = provenance.end_uncertainty_ms();
            let covers = self.spans_more_detailed_than(provenance)?;
            let (covered, kept): (Vec<&Listen>, Vec<&Listen>) =
                listens.into_iter().partition(|l| {
                    let at = to_ms(l.started_at);
                    covers
                        .iter()
                        .any(|(from, to)| at + slack >= *from && at <= *to)
                });
            summary.covered += covered.len();
            let Some((from, to)) = span(kept.iter().copied()) else {
                continue;
            };
            let to = to + slack;
            for less in Provenance::ALL
                .iter()
                .filter(|p| p.detail() < provenance.detail())
            {
                summary.replaced += self.conn.execute(
                    "DELETE FROM plays WHERE provenance = ?1 AND started_at BETWEEN ?2 AND ?3",
                    params![less.as_str(), from - less.end_uncertainty_ms(), to],
                )?;
            }
            summary.recorded += self.insert_listens(kept.into_iter())?;
        }
        tx.commit()?;
        Ok(summary)
    }

    /// Each span, as `(first start, last start)` in unix ms, that a history
    /// more detailed than `provenance` already covers.
    fn spans_more_detailed_than(&self, provenance: Provenance) -> Result<Vec<(i64, i64)>> {
        let mut spans = Vec::new();
        for more in Provenance::ALL
            .iter()
            .filter(|p| p.detail() > provenance.detail())
        {
            let span: (Option<i64>, Option<i64>) = self.conn.query_row(
                "SELECT MIN(started_at), MAX(started_at) FROM plays WHERE provenance = ?1",
                [more.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if let (Some(from), Some(to)) = span {
                spans.push((from, to + more.end_uncertainty_ms()));
            }
        }
        Ok(spans)
    }

    /// When the last listen on file ended: where the next poll of a
    /// recently-played feed starts reading, and what measures the first
    /// listen it reads. A source that drops the seconds of its end times is
    /// taken to have ended at the end of that minute, so polling doesn't read
    /// its last listen again.
    pub fn poll_cursor(&self) -> Result<Option<Timestamp>> {
        let mut latest: Option<i64> = None;
        for provenance in Provenance::ALL {
            let end: Option<i64> = self.conn.query_row(
                "SELECT MAX(started_at + COALESCE(ms_played, 0)) FROM plays WHERE provenance = ?1",
                [provenance.as_str()],
                |row| row.get(0),
            )?;
            if let Some(end) = end {
                let end = end + provenance.end_uncertainty_ms();
                latest = Some(latest.map_or(end, |l| l.max(end)));
            }
        }
        latest.map(from_ms).transpose().map_err(Into::into)
    }

    /// Re-measures plays polled before the feed's `played_at` was known to be
    /// an end time. Each one's stored start is really when it ended, and its
    /// `ms_played` was the gap to the next listen, which measures that one
    /// instead. Both are derived again from the stored end times, the way the
    /// poller now does it. Runs once per database; a new database has nothing
    /// to re-measure.
    pub(super) fn ensure_polls_measured_from_end(&self) -> Result<()> {
        if self.setting(POLL_TIMING)?.as_deref() == Some(FROM_PREVIOUS_END) {
            return Ok(());
        }
        /// A stored play, by when it ended.
        struct Ended {
            end: i64,
            id: i64,
            duration: Option<u32>,
            polled: bool,
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut plays: Vec<Ended> = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT p.play_id, p.started_at, p.ms_played, t.duration_ms, p.provenance
             FROM plays p JOIN tracks t USING (track_id)",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let started_at: i64 = row.get(1)?;
            let ms_played: Option<i64> = row.get(2)?;
            let provenance = parse_stored(row.get(4)?, Provenance::parse)?;
            let polled = provenance.detail() == Provenance::Poll.detail();
            // A polled play's stored start is its end; any other's end is its
            // start plus what it played.
            let end = if polled {
                started_at
            } else {
                started_at + ms_played.unwrap_or(0) + provenance.end_uncertainty_ms()
            };
            plays.push(Ended {
                end,
                id: row.get(0)?,
                duration: row.get(3)?,
                polled,
            });
        }
        drop(rows);
        drop(stmt);
        plays.sort_unstable_by_key(|p| (p.end, p.id));
        let mut previous: Option<i64> = None;
        let mut remeasured = 0;
        for Ended {
            end,
            id,
            duration,
            polled,
        } in plays
        {
            if polled {
                let (ms, provenance) = played_since_previous(duration, previous.map(|p| end - p));
                let start = end - i64::from(ms.unwrap_or(0));
                let changed = self.conn.execute(
                    "UPDATE OR IGNORE plays SET started_at = ?2, ms_played = ?3, provenance = ?4
                     WHERE play_id = ?1",
                    params![id, start, ms, provenance.as_str()],
                )?;
                if changed == 0 {
                    // Another play of the track already starts then: the same listen.
                    self.conn
                        .execute("DELETE FROM plays WHERE play_id = ?1", [id])?;
                }
                remeasured += 1;
            }
            previous = Some(end);
        }
        self.set_setting(POLL_TIMING, FROM_PREVIOUS_END)?;
        tx.commit()?;
        if remeasured > 0 {
            info!("re-measured {remeasured} polled plays from the previous listen's end");
        }
        Ok(())
    }

    /// Every play on file, oldest first.
    ///
    /// A play's `duration_ms` is its track's length. Where none is on file
    /// (a basic export names tracks without one), the length two of the
    /// track's streams agree on, within [`AGREED_LENGTH_MS`], stands in:
    /// playing to the same moment twice is playing to the end. On tracks whose
    /// length is known, that agreed length is within 3 s of it for 96%. The
    /// stored length stays unknown.
    pub fn plays(&self) -> Result<Vec<Play>> {
        let mut stmt = self.conn.prepare(
            "WITH ranked AS (
                 SELECT p.track_id, p.ms_played,
                        ROW_NUMBER() OVER (PARTITION BY p.track_id ORDER BY p.ms_played DESC) AS r
                 FROM plays p JOIN tracks t USING (track_id)
                 WHERE t.duration_ms IS NULL AND p.ms_played >= ?1),
             agreed AS (
                 SELECT track_id,
                        MAX(CASE WHEN r = 1 THEN ms_played END) AS longest,
                        MAX(CASE WHEN r = 2 THEN ms_played END) AS next
                 FROM ranked WHERE r <= 2 GROUP BY track_id)
             SELECT p.track_id, t.artist_id, p.started_at, p.ms_played,
                    COALESCE(t.duration_ms,
                             CASE WHEN a.longest - a.next <= ?2 THEN a.longest END),
                    p.ending, p.started_by
             FROM plays p JOIN tracks t USING (track_id)
             LEFT JOIN agreed a USING (track_id)
             ORDER BY p.started_at",
        )?;
        let plays = stmt
            .query_map(params![SKIP_BEFORE_MS, AGREED_LENGTH_MS], |row| {
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
    fn insert_listens<'a>(&self, listens: impl Iterator<Item = &'a Listen>) -> Result<usize> {
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

/// The earliest and latest start among `listens`, in unix ms.
fn span<'a>(listens: impl Iterator<Item = &'a Listen>) -> Option<(i64, i64)> {
    listens.fold(None, |span, l| {
        let at = to_ms(l.started_at);
        Some(span.map_or((at, at), |(from, to): (i64, i64)| {
            (from.min(at), to.max(at))
        }))
    })
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
    fn the_poll_cursor_is_the_end_of_the_last_listen() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.poll_cursor().unwrap(), None);

        s.record_listens(&[listen("polled", 100, Some(30_000), Provenance::Poll)])
            .unwrap();
        let polled_end = Timestamp::from_second(130).unwrap();
        assert_eq!(s.poll_cursor().unwrap(), Some(polled_end));

        s.import_history(&[listen("history", 200, Some(200_000), Provenance::History)])
            .unwrap();
        let history_end = Timestamp::from_second(400).unwrap();
        assert_eq!(s.poll_cursor().unwrap(), Some(history_end));
    }

    #[test]
    fn a_basic_history_is_taken_to_end_at_the_end_of_its_minute() {
        let s = Store::open_in_memory().unwrap();
        s.import_history(&[listen("A", 60, Some(60_000), Provenance::BasicHistory)])
            .unwrap();
        let end = Timestamp::from_millisecond(120_000 + 59_999).unwrap();
        assert_eq!(s.poll_cursor().unwrap(), Some(end));
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
                replaced: 1,
                covered: 0,
            }
        );
        assert_eq!(s.plays().unwrap().len(), 3);

        let again = s.import_history(&history).unwrap();
        assert_eq!(again, ImportSummary::default());
    }

    #[test]
    fn a_full_history_replaces_a_basic_one_over_its_span() {
        let s = Store::open_in_memory().unwrap();
        let basic = [
            listen("old", 100, Some(60_000), Provenance::BasicHistory),
            listen("newer", 5_000, Some(60_000), Provenance::BasicHistory),
        ];
        s.import_history(&basic).unwrap();
        // The full export's times for "old" are a few seconds off the basic one's.
        let full = [listen("old", 130, Some(60_000), Provenance::History)];
        let summary = s.import_history(&full).unwrap();
        assert_eq!(summary.replaced, 1);
        let starts: Vec<i64> = s
            .plays()
            .unwrap()
            .iter()
            .map(|p| p.started_at.as_second())
            .collect();
        assert_eq!(starts, vec![130, 5_000]);
    }

    #[test]
    fn a_basic_history_leaves_out_what_a_full_one_covers() {
        let s = Store::open_in_memory().unwrap();
        s.import_history(&[
            listen("A", 100, Some(60_000), Provenance::History),
            listen("B", 1_000, Some(60_000), Provenance::History),
        ])
        .unwrap();
        let basic = [
            listen("A", 90, Some(60_000), Provenance::BasicHistory),
            listen("C", 2_000, Some(60_000), Provenance::BasicHistory),
        ];
        let summary = s.import_history(&basic).unwrap();
        // "A" at 90 s may really have started at 100 s, inside the full span.
        assert_eq!(summary.covered, 1);
        assert_eq!(summary.recorded, 1);
        let inside = [listen("B", 995, Some(60_000), Provenance::BasicHistory)];
        assert_eq!(s.import_history(&inside).unwrap().covered, 1);
        assert_eq!(s.plays().unwrap().len(), 3);
    }

    #[test]
    fn plays_polled_with_played_at_as_their_start_are_remeasured_once() {
        let s = Store::open_in_memory().unwrap();
        let track = |title: &str| {
            let mut t = TrackRef::new(title, ArtistRef::named("Artist"));
            t.duration_ms = Some(200_000);
            t
        };
        // The old poller stored each `played_at` (an end) as the start, and
        // the gap to the next one as `ms_played`.
        let old = |t: TrackRef, ended_s: i64, ms: Option<u32>| Listen {
            track: t,
            started_at: Timestamp::from_second(ended_s).unwrap(),
            ms_played: ms,
            ending: Ending::Unknown,
            started_by: StartedBy::Unknown,
            provenance: Provenance::Poll,
        };
        s.record_listens(&[
            old(track("first"), 1_000, Some(20_000)),
            old(track("skipped"), 1_020, Some(180_000)),
            old(track("finished"), 1_200, None),
        ])
        .unwrap();
        s.set_setting(POLL_TIMING, "").unwrap();

        s.ensure_polls_measured_from_end().unwrap();
        let measured: Vec<(i64, Option<u32>)> = s
            .plays()
            .unwrap()
            .iter()
            .map(|p| (p.started_at.as_second(), p.ms_played))
            .collect();
        assert_eq!(
            measured,
            vec![
                (800, Some(200_000)),
                (1_000, Some(20_000)),
                (1_020, Some(180_000))
            ]
        );

        // Recorded as measured, so a second open leaves them be.
        s.ensure_polls_measured_from_end().unwrap();
        assert_eq!(s.plays().unwrap()[0].started_at.as_second(), 800);
    }

    #[test]
    fn two_streams_that_agree_stand_in_for_an_unknown_length() {
        let s = Store::open_in_memory().unwrap();
        let basic = |title: &str, at_s: i64, ms: u32| {
            listen(title, at_s, Some(ms), Provenance::BasicHistory)
        };
        s.import_history(&[
            basic("Short Song", 1_000, 100_000),
            basic("Short Song", 2_000, 100_900),
            basic("Short Song", 3_000, 40_000),
            basic("Once", 4_000, 90_000),
            basic("Never Agreed", 5_000, 60_000),
            basic("Never Agreed", 6_000, 90_000),
        ])
        .unwrap();
        let plays = s.plays().unwrap();
        let lengths: Vec<Option<u32>> = plays.iter().map(|p| p.duration_ms).collect();
        assert_eq!(
            lengths,
            vec![
                Some(100_900),
                Some(100_900),
                Some(100_900),
                None,
                None,
                None
            ]
        );
        assert_eq!(plays[0].verdict(), Some(crate::model::Verdict::Completed));
        assert_eq!(plays[2].verdict(), Some(crate::model::Verdict::Partial));
        // The stored length is still unknown.
        let short = s
            .resolve_track(&TrackRef::new("Short Song", ArtistRef::named("Artist")))
            .unwrap();
        assert!(s.durations([short]).unwrap().is_empty());
    }
}
