//! The taste profile (spec §7.1): how much I like each artist, and what I have
//! already heard. Rebuilt from every play on each build, so it is never stale.

use std::collections::HashMap;

use jiff::Timestamp;
use rand::Rng;
use rand::seq::IndexedRandom;

use crate::model::{ArtistId, ArtistTag, Play, StartedBy, TrackId, Verdict};

/// A skip counts against an artist this much, relative to a completion.
const SKIP_PENALTY: f64 = 0.25;

const MS_PER_DAY: f64 = 86_400_000.0;

#[derive(Debug, Clone)]
pub struct ProfileParams {
    /// A listen this many days old counts 1/e as much as one today.
    pub tau_days: f64,
    /// How much a listen the service chose (autoplay, radio) counts. Spec §14
    /// suggests 0.5, since those picks are partly Spotify's taste, not mine.
    pub autoplay_weight: f64,
}

/// How many seed artists to start candidate searches from.
#[derive(Debug, Clone, Copy)]
pub struct SeedPlan {
    /// The best-liked artists, always included.
    pub top: usize,
    /// Plus this many drawn at random from the ranks just below...
    pub sampled: usize,
    /// ...down to this rank, so heavy rotation doesn't dominate the search.
    pub sample_until_rank: usize,
}

impl Default for SeedPlan {
    fn default() -> Self {
        SeedPlan {
            top: 60,
            sampled: 20,
            sample_until_rank: 300,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Profile {
    affinity: HashMap<ArtistId, f64>,
    track_plays: HashMap<TrackId, u32>,
    artist_plays: HashMap<ArtistId, u32>,
    /// Artists with positive affinity, best first.
    ranked: Vec<(ArtistId, f64)>,
}

impl Profile {
    /// Affinity is the decayed sum of completions, minus a quarter of the
    /// decayed sum of skips, floored at zero:
    ///
    /// `aff(a) = Σ w·e^(-age/τ) · (1 if completed, -0.25 if skipped, 0 otherwise)`
    ///
    /// where `w` is 1, or `autoplay_weight` for autoplayed listens. Partial and
    /// still-pending listens count as heard but say nothing about taste.
    pub fn build(plays: &[Play], now: Timestamp, params: &ProfileParams) -> Profile {
        let mut affinity: HashMap<ArtistId, f64> = HashMap::new();
        let mut track_plays: HashMap<TrackId, u32> = HashMap::new();
        let mut artist_plays: HashMap<ArtistId, u32> = HashMap::new();

        for play in plays {
            *track_plays.entry(play.track).or_default() += 1;
            *artist_plays.entry(play.artist).or_default() += 1;

            let direction = match play.verdict() {
                Some(Verdict::Completed) => 1.0,
                Some(Verdict::Skipped) => -SKIP_PENALTY,
                Some(Verdict::Partial) | None => continue,
            };
            let chooser = if play.started_by == StartedBy::Autoplay {
                params.autoplay_weight
            } else {
                1.0
            };
            let age_days = (now.as_millisecond() - play.started_at.as_millisecond()).max(0) as f64
                / MS_PER_DAY;
            let decay = (-age_days / params.tau_days).exp();
            *affinity.entry(play.artist).or_default() += direction * chooser * decay;
        }

        affinity.retain(|_, a| *a > 0.0);
        let mut ranked: Vec<(ArtistId, f64)> = affinity.iter().map(|(a, s)| (*a, *s)).collect();
        ranked.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));

        Profile {
            affinity,
            track_plays,
            artist_plays,
            ranked,
        }
    }

    pub fn affinity(&self, artist: ArtistId) -> f64 {
        self.affinity.get(&artist).copied().unwrap_or(0.0)
    }

    /// Lifetime listens of a track, any version.
    pub fn track_plays(&self, track: TrackId) -> u32 {
        self.track_plays.get(&track).copied().unwrap_or(0)
    }

    /// Lifetime listens of an artist.
    pub fn artist_plays(&self, artist: ArtistId) -> u32 {
        self.artist_plays.get(&artist).copied().unwrap_or(0)
    }

    /// Artists I like, best first.
    pub fn ranked_artists(&self) -> &[(ArtistId, f64)] {
        &self.ranked
    }

    /// The artists candidate searches start from: the `top` best-liked, plus
    /// `sampled` drawn uniformly from the ranks below them.
    pub fn seeds(&self, plan: SeedPlan, rng: &mut impl Rng) -> Vec<ArtistId> {
        let top = plan.top.min(self.ranked.len());
        let tail_end = plan.sample_until_rank.min(self.ranked.len()).max(top);
        let mut seeds: Vec<ArtistId> = self.ranked[..top].iter().map(|(a, _)| *a).collect();
        seeds.extend(
            self.ranked[top..tail_end]
                .sample(rng, plan.sampled)
                .map(|(a, _)| *a),
        );
        seeds
    }
}

/// How much of my listening each tag accounts for (spec §7.1):
///
/// `share(t) = Σ_a aff(a) · w(a, t) / Σ_a aff(a)`
///
/// with each artist's tag weights normalized to sum to 1, so a heavily tagged
/// artist doesn't count more than a sparsely tagged one. Artists without tags
/// still count in the denominator. Shares sum to at most 1.
pub fn tag_shares(
    profile: &Profile,
    tags: &HashMap<ArtistId, Vec<ArtistTag>>,
) -> HashMap<String, f64> {
    let total: f64 = profile.ranked.iter().map(|(_, a)| a).sum();
    let mut shares: HashMap<String, f64> = HashMap::new();
    if total <= 0.0 {
        return shares;
    }
    for (artist, aff) in &profile.ranked {
        let Some(artist_tags) = tags.get(artist) else {
            continue;
        };
        let weight_sum: f64 = artist_tags.iter().map(|t| t.weight).sum();
        if weight_sum <= 0.0 {
            continue;
        }
        for tag in artist_tags {
            *shares.entry(tag.tag.clone()).or_default() += aff * tag.weight / weight_sum / total;
        }
    }
    shares
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;
    use crate::model::Ending;

    const DAY_MS: i64 = 86_400_000;

    fn now() -> Timestamp {
        Timestamp::from_millisecond(1_000 * DAY_MS).unwrap()
    }

    fn play(artist: i64, track: i64, days_ago: i64, ending: Ending, started_by: StartedBy) -> Play {
        Play {
            track: TrackId(track),
            artist: ArtistId(artist),
            started_at: Timestamp::from_millisecond((1_000 - days_ago) * DAY_MS).unwrap(),
            ms_played: Some(if ending == Ending::Skipped {
                5_000
            } else {
                200_000
            }),
            duration_ms: Some(200_000),
            ending,
            started_by,
        }
    }

    fn params() -> ProfileParams {
        ProfileParams {
            tau_days: 365.0,
            autoplay_weight: 0.5,
        }
    }

    #[test]
    fn completions_count_skips_count_against_and_old_listens_fade() {
        let plays = [
            play(1, 10, 0, Ending::Finished, StartedBy::Listener),
            play(1, 11, 0, Ending::Skipped, StartedBy::Listener),
            play(2, 20, 365, Ending::Finished, StartedBy::Listener),
        ];
        let p = Profile::build(&plays, now(), &params());
        assert!((p.affinity(ArtistId(1)) - 0.75).abs() < 1e-9);
        assert!((p.affinity(ArtistId(2)) - (-1.0f64).exp()).abs() < 1e-9);
        assert_eq!(p.ranked_artists()[0].0, ArtistId(1));
    }

    #[test]
    fn autoplay_counts_less() {
        let plays = [play(1, 10, 0, Ending::Finished, StartedBy::Autoplay)];
        let p = Profile::build(&plays, now(), &params());
        assert!((p.affinity(ArtistId(1)) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_skipped_only_artist_has_no_affinity_but_is_still_heard() {
        let plays = [play(3, 30, 0, Ending::Skipped, StartedBy::Listener)];
        let p = Profile::build(&plays, now(), &params());
        assert_eq!(p.affinity(ArtistId(3)), 0.0);
        assert!(p.ranked_artists().is_empty());
        assert_eq!(p.artist_plays(ArtistId(3)), 1);
        assert_eq!(p.track_plays(TrackId(30)), 1);
    }

    #[test]
    fn seeds_are_the_top_plus_a_sample_of_the_next_ranks() {
        let plays: Vec<Play> = (0..100)
            .flat_map(|a| {
                (0..(100 - a))
                    .map(move |i| play(a, a * 1000 + i, 0, Ending::Finished, StartedBy::Listener))
            })
            .collect();
        let p = Profile::build(&plays, now(), &params());
        let plan = SeedPlan {
            top: 10,
            sampled: 5,
            sample_until_rank: 30,
        };
        let seeds = p.seeds(plan, &mut StdRng::seed_from_u64(1));
        assert_eq!(seeds.len(), 15);
        let top: Vec<ArtistId> = (0..10).map(ArtistId).collect();
        assert_eq!(&seeds[..10], &top[..]);
        assert!(seeds[10..].iter().all(|a| (10..30).contains(&a.0)));
    }

    #[test]
    fn tag_shares_weight_by_affinity_and_normalize_per_artist() {
        let plays = [
            play(1, 10, 0, Ending::Finished, StartedBy::Listener),
            play(1, 11, 0, Ending::Finished, StartedBy::Listener),
            play(1, 12, 0, Ending::Finished, StartedBy::Listener),
            play(2, 20, 0, Ending::Finished, StartedBy::Listener),
        ];
        let p = Profile::build(&plays, now(), &params());
        let tags = HashMap::from([
            (
                ArtistId(1),
                vec![ArtistTag::new("jazz", 1.0), ArtistTag::new("bebop", 1.0)],
            ),
            (ArtistId(2), vec![ArtistTag::new("jazz", 0.4)]),
        ]);
        let shares = tag_shares(&p, &tags);
        // Artist 1 holds 3/4 of affinity, split evenly; artist 2 holds 1/4.
        assert!((shares["jazz"] - (0.75 * 0.5 + 0.25)).abs() < 1e-9);
        assert!((shares["bebop"] - 0.375).abs() < 1e-9);
    }
}
