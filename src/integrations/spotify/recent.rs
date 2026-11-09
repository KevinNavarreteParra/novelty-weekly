//! Turning the recently-played feed into listens (spec §5.2).
//!
//! The feed gives each track and when it was played, but not how long. The
//! gap to the next entry stands in: a track followed 40 s later by another
//! played about 40 s. The newest entry has no successor yet, so it is reported
//! with `ms_played: None` and completed by a later poll, which re-reads it.
//!
//! This assumes `played_at` is when playback *started*. Spotify doesn't say.
//! Once a history export overlaps some polled weeks, comparing the two will
//! settle it; if it is the end time, the gap belongs to the later track.

use jiff::Timestamp;

use crate::model::{Ending, Listen, Provenance, StartedBy, TrackRef};

/// A gap this much longer than the track means a pause or idle time, so the
/// gap no longer measures the listen.
const IDLE_SLACK_MS: i64 = 120_000;

/// Listens from feed entries `(track, played_at)`, oldest first.
pub fn listens_from_feed(mut entries: Vec<(TrackRef, Timestamp)>) -> Vec<Listen> {
    entries.sort_by_key(|(_, at)| *at);
    let next_starts: Vec<Option<Timestamp>> = entries
        .iter()
        .skip(1)
        .map(|(_, at)| Some(*at))
        .chain(std::iter::once(None))
        .collect();

    entries
        .into_iter()
        .zip(next_starts)
        .map(|((track, started_at), next)| {
            let (ms_played, provenance) = match next {
                None => (None, Provenance::Poll),
                Some(next) => estimate(&track, next.as_millisecond() - started_at.as_millisecond()),
            };
            Listen {
                track,
                started_at,
                ms_played,
                ending: Ending::Unknown,
                started_by: StartedBy::Unknown,
                provenance,
            }
        })
        .collect()
}

/// How long a track played, given the gap before the next one started.
fn estimate(track: &TrackRef, gap_ms: i64) -> (Option<u32>, Provenance) {
    let gap = gap_ms.max(0);
    match track.duration_ms.map(i64::from) {
        Some(duration) if gap > duration + IDLE_SLACK_MS => {
            // Paused or idle: assume it played out, and say this is a guess.
            (Some(duration as u32), Provenance::PollEstimated)
        }
        Some(duration) => (Some(gap.min(duration) as u32), Provenance::Poll),
        None => (
            Some(gap.min(u32::MAX as i64) as u32),
            Provenance::PollEstimated,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ArtistRef;

    fn entry(title: &str, duration_ms: u32, at_s: i64) -> (TrackRef, Timestamp) {
        let mut track = TrackRef::new(title, ArtistRef::named("A"));
        track.duration_ms = Some(duration_ms);
        (track, Timestamp::from_second(at_s).unwrap())
    }

    #[test]
    fn the_gap_to_the_next_track_is_the_listen_capped_at_its_length() {
        let listens = listens_from_feed(vec![
            entry("skipped", 200_000, 1_000),
            entry("finished", 180_000, 1_020),
            entry("pending", 200_000, 1_200),
        ]);
        let played: Vec<_> = listens
            .iter()
            .map(|l| (l.track.title.as_str(), l.ms_played))
            .collect();
        assert_eq!(
            played,
            vec![
                ("skipped", Some(20_000)),
                ("finished", Some(180_000)),
                ("pending", None)
            ]
        );
        assert!(listens.iter().all(|l| l.provenance == Provenance::Poll));
    }

    #[test]
    fn a_long_idle_gap_is_flagged_as_a_guess() {
        let listens = listens_from_feed(vec![entry("a", 200_000, 0), entry("b", 200_000, 3_600)]);
        assert_eq!(listens[0].ms_played, Some(200_000));
        assert_eq!(listens[0].provenance, Provenance::PollEstimated);
    }

    #[test]
    fn entries_arrive_newest_first_but_come_out_oldest_first() {
        let listens = listens_from_feed(vec![
            entry("later", 200_000, 500),
            entry("earlier", 200_000, 100),
        ]);
        assert_eq!(listens[0].track.title, "earlier");
        assert_eq!(listens[0].ms_played, Some(200_000));
    }
}
