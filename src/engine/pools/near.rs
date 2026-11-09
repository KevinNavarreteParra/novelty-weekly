//! Near: unheard artists close to my taste (spec §7.2).
//!
//! Two stages, because the second needs data fetched for the first's winners:
//! 1. [`score_artists`] ranks unheard artists by similarity to my seeds.
//! 2. [`candidates`] discounts popular ones and proposes their top tracks.

use std::collections::HashMap;

use crate::engine::candidate::{Candidate, Why};
use crate::engine::profile::Profile;
use crate::model::{Arm, ArtistId, Pool, PopularTracks, SimilarityGraph};

/// How many contributing seeds a rationale names.
const RATIONALE_SEEDS: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct NearArtist {
    pub artist: ArtistId,
    /// `Σ_s aff(s) · sim(s, artist)` over my seeds.
    pub score: f64,
    /// The seeds that contributed most, with their similarity.
    pub via: Vec<(ArtistId, f64)>,
}

/// Every artist I've never played that a seed links to, best first:
///
/// `near(c) = Σ_s aff(s) · sim(s, c)`
pub fn score_artists(
    profile: &Profile,
    seeds: &[ArtistId],
    similar: &SimilarityGraph,
) -> Vec<NearArtist> {
    // For each candidate: (seed, aff · sim, sim) for every seed that links to it.
    let mut links: HashMap<ArtistId, Vec<(ArtistId, f64, f64)>> = HashMap::new();
    for &seed in seeds {
        let affinity = profile.affinity(seed);
        for &(other, sim) in similar.get(&seed).into_iter().flatten() {
            if !profile.knows(other) {
                links
                    .entry(other)
                    .or_default()
                    .push((seed, affinity * sim, sim));
            }
        }
    }

    let mut scored: Vec<NearArtist> = links
        .into_iter()
        .map(|(artist, mut contributions)| {
            contributions.sort_by(|a, b| b.1.total_cmp(&a.1));
            NearArtist {
                artist,
                score: contributions.iter().map(|c| c.1).sum(),
                via: contributions
                    .iter()
                    .take(RATIONALE_SEEDS)
                    .map(|&(seed, _, sim)| (seed, sim))
                    .collect(),
            }
        })
        .filter(|a| a.score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.artist.cmp(&b.artist)));
    scored
}

/// Candidates from scored artists' most played tracks.
///
/// Relevance discounts fame so the pool isn't just the most famous neighbor:
/// `near(c) / log10(listeners + 10)^γ` (an unknown count counts as 0). Each
/// artist offers up to `per_artist` tracks, each a little less relevant than
/// the last; the selector takes at most one per new artist, and the extras are
/// there in case a filter rules out the first.
pub fn candidates(
    artists: &[NearArtist],
    listeners: &HashMap<ArtistId, u64>,
    popular: &PopularTracks,
    gamma: f64,
    per_artist: usize,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for artist in artists {
        let fame = listeners.get(&artist.artist).copied().unwrap_or(0) as f64;
        let relevance = artist.score / (fame + 10.0).log10().powf(gamma);
        let tracks = popular
            .get(&artist.artist)
            .into_iter()
            .flatten()
            .take(per_artist);
        for (rank, &(track, _)) in tracks.enumerate() {
            out.push(Candidate {
                track,
                artist: artist.artist,
                pool: Pool::Near,
                arm: Arm::Pool(Pool::Near),
                relevance: relevance * 0.9f64.powi(rank as i32),
                why: Why::SimilarTo(artist.via.clone()),
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
    use crate::model::{Ending, Play, StartedBy, TrackId};

    /// I've completed one track each by artists 1 and 2, twice as often by 1.
    fn profile() -> Profile {
        let play = |artist: i64, track: i64| Play {
            track: TrackId(track),
            artist: ArtistId(artist),
            started_at: Timestamp::UNIX_EPOCH,
            ms_played: Some(200_000),
            duration_ms: Some(200_000),
            ending: Ending::Finished,
            started_by: StartedBy::Listener,
        };
        let plays = [play(1, 10), play(1, 10), play(2, 20)];
        Profile::build(
            &plays,
            Timestamp::UNIX_EPOCH,
            &ProfileParams {
                tau_days: 365.0,
                autoplay_weight: 1.0,
            },
        )
    }

    fn graph() -> SimilarityGraph {
        HashMap::from([
            (ArtistId(1), vec![(ArtistId(100), 0.5), (ArtistId(2), 0.9)]),
            (
                ArtistId(2),
                vec![(ArtistId(100), 0.5), (ArtistId(200), 0.8)],
            ),
        ])
    }

    #[test]
    fn scores_sum_affinity_weighted_similarity_over_seeds() {
        let scored = score_artists(&profile(), &[ArtistId(1), ArtistId(2)], &graph());
        // 100: 2·0.5 + 1·0.5 = 1.5; 200: 1·0.8 = 0.8.
        assert_eq!(scored.len(), 2);
        assert_eq!(scored[0].artist, ArtistId(100));
        assert!((scored[0].score - 1.5).abs() < 1e-9);
        assert_eq!(scored[0].via[0], (ArtistId(1), 0.5));
        assert!((scored[1].score - 0.8).abs() < 1e-9);
    }

    #[test]
    fn artists_i_have_played_are_never_near_candidates() {
        let scored = score_artists(&profile(), &[ArtistId(1), ArtistId(2)], &graph());
        assert!(scored.iter().all(|a| a.artist != ArtistId(2)));
    }

    #[test]
    fn fame_is_discounted() {
        let artists = vec![
            NearArtist {
                artist: ArtistId(100),
                score: 1.0,
                via: vec![],
            },
            NearArtist {
                artist: ArtistId(200),
                score: 1.0,
                via: vec![],
            },
        ];
        let listeners = HashMap::from([(ArtistId(100), 5_000_000), (ArtistId(200), 5_000)]);
        let popular = HashMap::from([
            (ArtistId(100), vec![(TrackId(1), None)]),
            (ArtistId(200), vec![(TrackId(2), None)]),
        ]);
        let out = candidates(&artists, &listeners, &popular, 0.5, 3);
        let famous = out.iter().find(|c| c.artist == ArtistId(100)).unwrap();
        let obscure = out.iter().find(|c| c.artist == ArtistId(200)).unwrap();
        assert!(obscure.relevance > famous.relevance);
    }
}
