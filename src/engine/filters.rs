//! The hard filters every candidate must pass (spec §7.3). Unlike relevance,
//! these are never traded off: a track that fails one is out.

use std::collections::{HashMap, HashSet};

use super::candidate::Candidate;
use super::profile::Profile;
use crate::model::{ArtistId, ArtistTag, Pool, TrackId};

/// Shortest track worth recommending; shorter is usually an interlude or skit.
pub const MIN_DURATION_MS: u32 = 90_000;
pub const MAX_DURATION_MS: u32 = 15 * 60_000;
/// A blocked tag only rules an artist out when it describes them this strongly.
const BLOCKED_TAG_WEIGHT: f64 = 0.5;

/// Why a candidate was ruled out, for the build log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rejection {
    HeardTrack,
    AlreadyRecommended,
    HeardArtist,
    Duration,
    BlockedArtist,
    BlockedTag,
}

pub struct Filters<'a> {
    pub profile: &'a Profile,
    /// Every track in the ledger.
    pub recommended: &'a HashSet<TrackId>,
    /// Track lengths; a track with no known length passes, and is checked
    /// again once resolution to the listening service supplies one.
    pub durations: &'a HashMap<TrackId, u32>,
    pub tags: &'a HashMap<ArtistId, Vec<ArtistTag>>,
    pub blocked_artists: &'a HashSet<ArtistId>,
    /// Normalized with `model::names::tag_key`.
    pub blocked_tags: &'a HashSet<String>,
}

impl Filters<'_> {
    pub fn check(&self, c: &Candidate) -> Result<(), Rejection> {
        if self.profile.track_plays(c.track) > 0 {
            return Err(Rejection::HeardTrack);
        }
        if self.recommended.contains(&c.track) {
            return Err(Rejection::AlreadyRecommended);
        }
        if c.pool != Pool::Deep && self.profile.artist_plays(c.artist) > 0 {
            return Err(Rejection::HeardArtist);
        }
        let length = self.durations.get(&c.track);
        if length.is_some_and(|ms| !(MIN_DURATION_MS..=MAX_DURATION_MS).contains(ms)) {
            return Err(Rejection::Duration);
        }
        if self.blocked_artists.contains(&c.artist) {
            return Err(Rejection::BlockedArtist);
        }
        let blocked_tag = self
            .tags
            .get(&c.artist)
            .into_iter()
            .flatten()
            .any(|t| t.weight >= BLOCKED_TAG_WEIGHT && self.blocked_tags.contains(&t.tag));
        if blocked_tag {
            return Err(Rejection::BlockedTag);
        }
        Ok(())
    }

    /// Keeps the candidates that pass, and counts why the others didn't.
    pub fn apply(&self, candidates: Vec<Candidate>) -> (Vec<Candidate>, HashMap<Rejection, usize>) {
        let mut rejected: HashMap<Rejection, usize> = HashMap::new();
        let kept = candidates
            .into_iter()
            .filter(|c| match self.check(c) {
                Ok(()) => true,
                Err(why) => {
                    *rejected.entry(why).or_default() += 1;
                    false
                }
            })
            .collect();
        (kept, rejected)
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::engine::candidate::Why;
    use crate::engine::profile::ProfileParams;
    use crate::model::{Arm, Ending, Play, StartedBy};

    fn candidate(pool: Pool, track: i64, artist: i64) -> Candidate {
        Candidate {
            track: TrackId(track),
            artist: ArtistId(artist),
            pool,
            arm: Arm::Pool(pool),
            relevance: 1.0,
            why: Why::DeepCut { affinity: 1.0 },
        }
    }

    #[test]
    fn each_rule_rejects_what_it_should() {
        // I've heard track 10 by artist 1.
        let plays = [Play {
            track: TrackId(10),
            artist: ArtistId(1),
            started_at: Timestamp::UNIX_EPOCH,
            ms_played: Some(200_000),
            duration_ms: Some(200_000),
            ending: Ending::Finished,
            started_by: StartedBy::Listener,
        }];
        let profile = Profile::build(
            &plays,
            Timestamp::UNIX_EPOCH,
            &ProfileParams {
                tau_days: 365.0,
                autoplay_weight: 1.0,
            },
        );
        let recommended = HashSet::from([TrackId(11)]);
        let durations = HashMap::from([(TrackId(12), 45_000)]);
        let tags = HashMap::from([(ArtistId(3), vec![ArtistTag::new("Christmas", 1.0)])]);
        let blocked_artists = HashSet::from([ArtistId(4)]);
        let blocked_tags = HashSet::from(["christmas".to_owned()]);
        let filters = Filters {
            profile: &profile,
            recommended: &recommended,
            durations: &durations,
            tags: &tags,
            blocked_artists: &blocked_artists,
            blocked_tags: &blocked_tags,
        };

        let check = |pool, track, artist| filters.check(&candidate(pool, track, artist));
        assert_eq!(check(Pool::Deep, 10, 1), Err(Rejection::HeardTrack));
        assert_eq!(check(Pool::Deep, 11, 1), Err(Rejection::AlreadyRecommended));
        assert_eq!(check(Pool::Deep, 13, 1), Ok(()));
        assert_eq!(check(Pool::Near, 13, 1), Err(Rejection::HeardArtist));
        assert_eq!(check(Pool::Near, 12, 2), Err(Rejection::Duration));
        assert_eq!(check(Pool::Far, 14, 3), Err(Rejection::BlockedTag));
        assert_eq!(check(Pool::Far, 15, 4), Err(Rejection::BlockedArtist));
        assert_eq!(check(Pool::Far, 16, 5), Ok(()));
    }
}
