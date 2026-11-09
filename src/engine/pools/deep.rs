//! Deep: unheard tracks by artists I already like (spec §7.2).

use std::collections::{HashMap, HashSet};

use crate::engine::candidate::{Candidate, Why};
use crate::engine::profile::Profile;
use crate::model::{Arm, ArtistId, Pool, TrackId};

/// Explore the catalogs of this many of my best-liked artists.
pub const ARTISTS: usize = 40;
/// Leave an artist alone once I've heard this share of their catalog.
pub const MAX_COVERAGE: f64 = 0.6;
/// The popularity percentile deep cuts are drawn toward: below the hits,
/// above the forgotten B-sides.
const SWEET_SPOT: f64 = 0.3;

/// One track in an artist's catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogTrack {
    pub track: TrackId,
    /// Distinct listeners, where a knowledge source counted them.
    pub listeners: Option<u64>,
}

/// The artists whose catalogs to fetch: my best-liked.
pub fn artists_to_explore(profile: &Profile) -> Vec<ArtistId> {
    profile
        .ranked_artists()
        .iter()
        .take(ARTISTS)
        .map(|(artist, _)| *artist)
        .collect()
}

/// Candidates from liked artists' catalogs, `aff(artist) × w(track)`.
///
/// Artists whose catalogs I've mostly heard are skipped. Within a catalog,
/// tracks with a known listener count are ranked by it; the artist's single
/// biggest track is left out (likely heard elsewhere), and `w` peaks at the
/// 30th percentile, falling to 0.5 at the bottom. Tracks with no count are the
/// long tail and get 0.5.
pub fn candidates(
    profile: &Profile,
    catalogs: &HashMap<ArtistId, Vec<CatalogTrack>>,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (&artist, catalog) in catalogs {
        let affinity = profile.affinity(artist);
        if affinity <= 0.0 {
            continue;
        }
        let distinct: HashSet<TrackId> = catalog.iter().map(|t| t.track).collect();
        let heard = distinct.iter().filter(|t| profile.heard(**t)).count();
        if distinct.is_empty() || heard as f64 / distinct.len() as f64 >= MAX_COVERAGE {
            continue;
        }

        let mut counted: Vec<&CatalogTrack> =
            catalog.iter().filter(|t| t.listeners.is_some()).collect();
        counted.sort_by_key(|t| std::cmp::Reverse(t.listeners));
        let biggest = counted.first().map(|t| t.track);
        let percentile: HashMap<TrackId, f64> = counted
            .iter()
            .enumerate()
            .map(|(rank, t)| (t.track, rank as f64 / (counted.len().max(2) - 1) as f64))
            .collect();

        let mut seen = HashSet::new();
        for t in catalog {
            if Some(t.track) == biggest || !seen.insert(t.track) {
                continue;
            }
            let weight = percentile.get(&t.track).map_or(0.5, |&p| {
                1.0 - 0.5 * (p - SWEET_SPOT).abs() / (1.0 - SWEET_SPOT)
            });
            out.push(Candidate {
                track: t.track,
                artist,
                pool: Pool::Deep,
                arm: Arm::Pool(Pool::Deep),
                relevance: affinity * weight,
                why: Why::DeepCut { affinity },
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::engine::profile::ProfileParams;
    use crate::model::{Ending, Play, StartedBy};

    fn profile(heard: &[(i64, i64)]) -> Profile {
        let plays: Vec<Play> = heard
            .iter()
            .map(|&(artist, track)| Play {
                track: TrackId(track),
                artist: ArtistId(artist),
                started_at: Timestamp::UNIX_EPOCH,
                ms_played: Some(200_000),
                duration_ms: Some(200_000),
                ending: Ending::Finished,
                started_by: StartedBy::Listener,
            })
            .collect();
        Profile::build(
            &plays,
            Timestamp::UNIX_EPOCH,
            &ProfileParams {
                tau_days: 365.0,
                autoplay_weight: 1.0,
            },
        )
    }

    fn track(id: i64, listeners: Option<u64>) -> CatalogTrack {
        CatalogTrack {
            track: TrackId(id),
            listeners,
        }
    }

    #[test]
    fn skips_the_biggest_hit_and_favors_the_middle() {
        let p = profile(&[(1, 10)]);
        let catalog = (0..11)
            .map(|i| track(100 + i, Some(1_000_000 / (i as u64 + 1))))
            .collect();
        let out = candidates(&p, &HashMap::from([(ArtistId(1), catalog)]));
        assert!(out.iter().all(|c| c.track != TrackId(100)));
        let relevance = |id| {
            out.iter()
                .find(|c| c.track == TrackId(id))
                .unwrap()
                .relevance
        };
        // Ranks 3 (30th percentile) beats 1 (10th) and 10 (bottom).
        assert!(relevance(103) > relevance(101));
        assert!(relevance(103) > relevance(110));
    }

    #[test]
    fn a_mostly_heard_catalog_is_left_alone() {
        let p = profile(&[(1, 1), (1, 2), (1, 3)]);
        let catalog = (1..=4).map(|i| track(i, None)).collect();
        assert!(candidates(&p, &HashMap::from([(ArtistId(1), catalog)])).is_empty());
    }

    #[test]
    fn artists_i_dont_like_are_not_explored() {
        let p = profile(&[(1, 1)]);
        let catalog = vec![track(50, None)];
        assert!(candidates(&p, &HashMap::from([(ArtistId(2), catalog)])).is_empty());
    }
}
