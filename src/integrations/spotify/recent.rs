//! Turning the recently-played feed into listens (spec §5.2).
//!
//! The feed gives each track and when it was played, but not how long. Its
//! `played_at` is when the listen *ended*: checked against a history export
//! over the same days, where it lands within the export's minute of each end
//! and minutes after each start. So a listen played for the gap since the
//! previous one ended, and started that long before `played_at`.
//!
//! Listens shorter than about 30 s never appear in the feed at all, so quick
//! skips are invisible here; only a history export records them.

use jiff::{SignedDuration, Timestamp};

use crate::model::{Ending, Listen, StartedBy, TrackRef, played_since_previous};

/// Listens from feed entries `(track, played_at)`, oldest first.
/// `previous_end` is when the last listen before them ended, if known; it
/// measures the oldest entry.
pub fn listens_from_feed(
    mut entries: Vec<(TrackRef, Timestamp)>,
    previous_end: Option<Timestamp>,
) -> Vec<Listen> {
    entries.sort_by_key(|(_, at)| *at);
    let mut previous = previous_end;
    entries
        .into_iter()
        .map(|(track, ended_at)| {
            let gap = previous.map(|p| ended_at.as_millisecond() - p.as_millisecond());
            previous = Some(ended_at);
            let (ms_played, provenance) = played_since_previous(track.duration_ms, gap);
            let played = SignedDuration::from_millis(ms_played.unwrap_or(0).into());
            Listen {
                track,
                started_at: ended_at.checked_sub(played).unwrap_or(ended_at),
                ms_played,
                ending: Ending::Unknown,
                started_by: StartedBy::Unknown,
                provenance,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArtistRef, Provenance};

    fn entry(title: &str, duration_ms: u32, ended_s: i64) -> (TrackRef, Timestamp) {
        let mut track = TrackRef::new(title, ArtistRef::named("A"));
        track.duration_ms = Some(duration_ms);
        (track, Timestamp::from_second(ended_s).unwrap())
    }

    fn at(s: i64) -> Timestamp {
        Timestamp::from_second(s).unwrap()
    }

    #[test]
    fn each_listen_is_the_gap_since_the_previous_one_ended() {
        // "skipped" ended 20 s after the listen before it; "finished" played
        // out its 180 s.
        let listens = listens_from_feed(
            vec![
                entry("skipped", 200_000, 1_020),
                entry("finished", 180_000, 1_200),
            ],
            Some(at(1_000)),
        );
        let played: Vec<_> = listens
            .iter()
            .map(|l| (l.track.title.as_str(), l.ms_played, l.started_at))
            .collect();
        assert_eq!(
            played,
            vec![
                ("skipped", Some(20_000), at(1_000)),
                ("finished", Some(180_000), at(1_020)),
            ]
        );
        assert!(listens.iter().all(|l| l.provenance == Provenance::Poll));
    }

    #[test]
    fn the_gap_is_capped_at_the_track_length() {
        let listens = listens_from_feed(vec![entry("a", 200_000, 1_250)], Some(at(1_000)));
        assert_eq!(listens[0].ms_played, Some(200_000));
        assert_eq!(listens[0].started_at, at(1_050));
        assert_eq!(listens[0].provenance, Provenance::Poll);
    }

    #[test]
    fn a_long_idle_gap_is_flagged_as_a_guess() {
        let listens = listens_from_feed(vec![entry("a", 200_000, 3_600)], Some(at(0)));
        assert_eq!(listens[0].ms_played, Some(200_000));
        assert_eq!(listens[0].provenance, Provenance::PollEstimated);
    }

    #[test]
    fn with_no_previous_listen_the_first_is_assumed_to_play_out() {
        let listens = listens_from_feed(
            vec![
                entry("first", 200_000, 1_000),
                entry("second", 200_000, 1_030),
            ],
            None,
        );
        assert_eq!(listens[0].ms_played, Some(200_000));
        assert_eq!(listens[0].started_at, at(800));
        assert_eq!(listens[0].provenance, Provenance::PollEstimated);
        assert_eq!(listens[1].ms_played, Some(30_000));
    }

    #[test]
    fn entries_arrive_newest_first_but_come_out_oldest_first() {
        let listens = listens_from_feed(
            vec![entry("later", 200_000, 500), entry("earlier", 200_000, 100)],
            Some(at(0)),
        );
        assert_eq!(listens[0].track.title, "earlier");
        assert_eq!(listens[0].ms_played, Some(100_000));
        assert_eq!(listens[1].ms_played, Some(200_000));
    }
}
