//! Listening: a [`Listen`] is what an integration reports, a [`Play`] is the same
//! listen after the store resolved its track, and [`judge`] decides what either
//! one says about taste.

use jiff::Timestamp;

use super::catalog::TrackRef;
use super::ids::{ArtistId, TrackId};
use super::stored_enum;

/// Below this, a listen is a skip whatever the integration says.
pub const SKIP_BEFORE_MS: u32 = 30_000;
/// Playing this long counts as completing a track, even a long one.
pub const COMPLETE_AFTER_MS: u32 = 120_000;
/// Playing this share of a track counts as completing it.
pub const COMPLETE_FRACTION: f64 = 0.8;

/// One play of one track, as a listening integration reports it.
///
/// Rules for integrations:
/// - Music only. Drop podcasts, audiobooks and anything without a track.
/// - `started_at` is when playback began, in UTC. Some sources only give the
///   end; subtract `ms_played`.
/// - `ms_played` is `None` only when the source can't tell yet.
/// - Report `ending` and `started_by` from what the source actually says; use
///   `Unknown` rather than guessing. [`judge`] turns the facts into a verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct Listen {
    pub track: TrackRef,
    pub started_at: Timestamp,
    pub ms_played: Option<u32>,
    pub ending: Ending,
    pub started_by: StartedBy,
    pub provenance: Provenance,
}

stored_enum! {
    /// How playback of a track ended.
    pub enum Ending {
        /// It played to the end.
        Finished => "finished",
        /// The listener moved on: next, previous, or picked something else.
        Skipped => "skipped",
        /// Playback stopped for another reason: closed app, error, logout.
        Interrupted => "interrupted",
        Unknown => "unknown",
    }
}

stored_enum! {
    /// Who chose this track. Spec §14 asks whether autoplayed listens should
    /// count less toward taste, since Spotify picked them.
    pub enum StartedBy {
        /// The listener picked it: clicked it, or pressed play or next.
        Listener => "listener",
        /// It followed the previous track in whatever was playing.
        Sequence => "sequence",
        /// The service chose it (autoplay, radio, DJ).
        Autoplay => "autoplay",
        Unknown => "unknown",
    }
}

stored_enum! {
    /// Which kind of source produced a listen, and so how far to trust it.
    pub enum Provenance {
        /// A full listening-history export: exact `ms_played` and endings.
        History => "history",
        /// A recently-played feed whose next listen is known, so `ms_played`
        /// came from the gap between them.
        Poll => "poll",
        /// A recently-played feed where the gap was unusable (a pause, a
        /// device switch) and `ms_played` is capped at the track's length.
        PollEstimated => "poll_est",
        /// Not a real listen: a stand-in from top or saved tracks, used for a
        /// cold start before any history exists.
        TopSeed => "top_seed",
    }
}

/// What a listen says about taste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Played through, or near enough.
    Completed,
    /// Moved past it, or barely started.
    Skipped,
    /// Stopped partway for some reason that isn't a skip.
    Partial,
}

/// Decides what a listen of `ms_played` milliseconds says about taste.
///
/// Spec §6 calls a listen completed at `min(80% of the track, 2 min)` unless it
/// was skipped. One deviation: skipping during the last 20% of a track of known
/// length still counts as completing it.
pub fn judge(ms_played: u32, duration_ms: Option<u32>, ending: Ending) -> Verdict {
    if ending == Ending::Finished {
        return Verdict::Completed;
    }
    let played_most =
        duration_ms.is_some_and(|d| f64::from(ms_played) >= COMPLETE_FRACTION * f64::from(d));
    if ending == Ending::Skipped {
        return if played_most {
            Verdict::Completed
        } else {
            Verdict::Skipped
        };
    }
    if ms_played < SKIP_BEFORE_MS && !played_most {
        return Verdict::Skipped;
    }
    if played_most || ms_played >= COMPLETE_AFTER_MS {
        Verdict::Completed
    } else {
        Verdict::Partial
    }
}

/// A stored listen, as the engine reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Play {
    pub track: TrackId,
    pub artist: ArtistId,
    pub started_at: Timestamp,
    pub ms_played: Option<u32>,
    pub duration_ms: Option<u32>,
    pub ending: Ending,
    pub started_by: StartedBy,
}

impl Play {
    /// `None` until `ms_played` is known.
    pub fn verdict(&self) -> Option<Verdict> {
        self.ms_played
            .map(|ms| judge(ms, self.duration_ms, self.ending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOUR_MIN: Option<u32> = Some(240_000);

    #[test]
    fn finished_is_completed_however_short() {
        assert_eq!(judge(10_000, None, Ending::Finished), Verdict::Completed);
    }

    #[test]
    fn skipping_early_or_midway_is_a_skip() {
        assert_eq!(judge(5_000, FOUR_MIN, Ending::Skipped), Verdict::Skipped);
        assert_eq!(judge(150_000, FOUR_MIN, Ending::Skipped), Verdict::Skipped);
    }

    #[test]
    fn skipping_the_outro_still_completes() {
        assert_eq!(
            judge(200_000, FOUR_MIN, Ending::Skipped),
            Verdict::Completed
        );
    }

    #[test]
    fn under_thirty_seconds_is_a_skip_unless_that_was_most_of_the_track() {
        assert_eq!(judge(20_000, FOUR_MIN, Ending::Unknown), Verdict::Skipped);
        assert_eq!(
            judge(20_000, Some(24_000), Ending::Unknown),
            Verdict::Completed
        );
    }

    #[test]
    fn two_minutes_completes_a_long_track() {
        let ten_min = Some(600_000);
        assert_eq!(
            judge(120_000, ten_min, Ending::Interrupted),
            Verdict::Completed
        );
        assert_eq!(
            judge(119_999, ten_min, Ending::Interrupted),
            Verdict::Partial
        );
    }

    #[test]
    fn without_a_duration_the_two_minute_rule_decides() {
        assert_eq!(judge(130_000, None, Ending::Unknown), Verdict::Completed);
        assert_eq!(judge(60_000, None, Ending::Unknown), Verdict::Partial);
    }
}
