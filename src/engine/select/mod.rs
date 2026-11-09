//! Choosing the playlist from every pool's candidates (spec §7.4).
//!
//! 1. [`allocate`] splits the `k` slots across pools by Thompson sampling.
//! 2. Each pool fills its slots greedily by MMR: the most relevant candidate
//!    that is least like what the pool already picked.
//! 3. Global caps apply throughout: one track per new artist, two per known
//!    artist, and no dominant tag over a quarter of the playlist.
//! 4. Slots a pool can't fill pass to the others (backfill).
//! 5. [`order::interleave`] arranges the result.

pub mod allocate;
pub mod order;

use std::collections::{BTreeMap, HashMap, HashSet};

use rand::Rng;

use self::allocate::{Posterior, ShareBounds};
use super::candidate::Candidate;
use super::profile::Profile;
use crate::model::{ArtistId, ArtistTag, ArtistTags, Pool, TrackId};

#[derive(Debug, Clone)]
pub struct SelectParams {
    /// Playlist size.
    pub k: usize,
    pub bounds: ShareBounds,
    /// MMR weight on relevance; the rest goes to being unlike earlier picks.
    pub mmr_lambda: f64,
    /// How deep into a pool's ranking MMR may reach, as a multiple of its slots.
    pub mmr_window: usize,
    /// No dominant tag may cover more than this share of the playlist.
    pub max_tag_share: f64,
    pub per_new_artist: u32,
    pub per_known_artist: u32,
}

impl SelectParams {
    /// The spec's fixed choices, with the tunable ones supplied.
    pub fn new(k: usize, bounds: ShareBounds, mmr_lambda: f64) -> Self {
        SelectParams {
            k,
            bounds,
            mmr_lambda,
            mmr_window: 4,
            max_tag_share: 0.25,
            per_new_artist: 1,
            per_known_artist: 2,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Selection {
    /// In playlist order.
    pub picks: Vec<Candidate>,
    /// Slots each pool was given before backfill.
    pub planned: BTreeMap<Pool, usize>,
    /// Slots a pool couldn't fill. Other pools covered them where they could.
    pub shortfall: BTreeMap<Pool, usize>,
}

/// `tags` gives each candidate artist's tags: as a vector for MMR, and the
/// strongest as its "dominant" tag for the tag cap.
pub fn select(
    candidates: Vec<Candidate>,
    posteriors: &BTreeMap<Pool, Posterior>,
    tags: &ArtistTags,
    profile: &Profile,
    params: &SelectParams,
    rng: &mut impl Rng,
) -> Selection {
    let planned = allocate::allocate(params.k, posteriors, params.bounds, rng);
    let mut ranked = rank_by_pool(candidates);
    let mut state = Picked::new(tags, profile, params);
    let mut picks: BTreeMap<Pool, Vec<Candidate>> = BTreeMap::new();

    for (&pool, &slots) in &planned {
        let pool_picks = picks.entry(pool).or_default();
        for _ in 0..slots {
            match state.best_next(ranked.get_mut(&pool), pool_picks, slots) {
                Some(pick) => pool_picks.push(pick),
                None => break,
            }
        }
    }

    let shortfall: BTreeMap<Pool, usize> = planned
        .iter()
        .map(|(pool, &slots)| (*pool, slots - picks.get(pool).map_or(0, Vec::len)))
        .filter(|(_, missing)| *missing > 0)
        .collect();

    // Backfill: hand missing slots round-robin to pools with candidates left.
    let mut missing: usize = shortfall.values().sum();
    while missing > 0 {
        let mut progressed = false;
        for &pool in planned.keys() {
            if missing == 0 {
                break;
            }
            let pool_picks = picks.entry(pool).or_default();
            let slots = planned[&pool].max(1);
            if let Some(pick) = state.best_next(ranked.get_mut(&pool), pool_picks, slots) {
                pool_picks.push(pick);
                missing -= 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    Selection {
        picks: order::interleave(picks),
        planned,
        shortfall,
    }
}

/// Each pool's candidates, most relevant first, with relevance rescaled to
/// `[0, 1]` within the pool so MMR's trade-off means the same in every pool.
fn rank_by_pool(candidates: Vec<Candidate>) -> BTreeMap<Pool, Vec<Candidate>> {
    let mut by_pool: BTreeMap<Pool, Vec<Candidate>> = BTreeMap::new();
    for c in candidates {
        by_pool.entry(c.pool).or_default().push(c);
    }
    for list in by_pool.values_mut() {
        list.sort_by(|a, b| b.relevance.total_cmp(&a.relevance));
        let top = list.first().map_or(0.0, |c| c.relevance);
        let n = list.len();
        for (rank, c) in list.iter_mut().enumerate() {
            c.relevance = if top > 0.0 {
                (c.relevance / top).max(0.0)
            } else {
                1.0 - rank as f64 / n as f64
            };
        }
    }
    by_pool
}

/// What has been picked so far, across all pools, for the global caps.
struct Picked<'a> {
    tags: &'a ArtistTags,
    profile: &'a Profile,
    params: &'a SelectParams,
    tracks: HashSet<TrackId>,
    per_artist: HashMap<ArtistId, u32>,
    per_dominant_tag: HashMap<&'a str, usize>,
}

impl<'a> Picked<'a> {
    fn new(tags: &'a ArtistTags, profile: &'a Profile, params: &'a SelectParams) -> Self {
        Picked {
            tags,
            profile,
            params,
            tracks: HashSet::new(),
            per_artist: HashMap::new(),
            per_dominant_tag: HashMap::new(),
        }
    }

    /// Removes and returns the pool's best next pick by MMR, among the
    /// `mmr_window × slots` most relevant candidates the caps still allow.
    fn best_next(
        &mut self,
        ranked: Option<&mut Vec<Candidate>>,
        pool_picks: &[Candidate],
        slots: usize,
    ) -> Option<Candidate> {
        let ranked = ranked?;
        // Caps only tighten as picks accumulate, so a candidate they rule out
        // now is out for good.
        ranked.retain(|c| self.allows(c));
        let window = (self.params.mmr_window * slots).min(ranked.len());
        let lambda = self.params.mmr_lambda;
        let best = (0..window).max_by(|&a, &b| {
            let score = |i: usize| {
                let c = &ranked[i];
                let redundancy = pool_picks
                    .iter()
                    .map(|p| self.tag_cosine(c.artist, p.artist))
                    .fold(0.0, f64::max);
                lambda * c.relevance - (1.0 - lambda) * redundancy
            };
            // On a tie, the earlier (more relevant) candidate wins.
            score(a).total_cmp(&score(b)).then(b.cmp(&a))
        })?;
        let pick = ranked.remove(best);
        self.record(&pick);
        Some(pick)
    }

    fn allows(&self, c: &Candidate) -> bool {
        if self.tracks.contains(&c.track) {
            return false;
        }
        let artist_limit = if self.profile.artist_plays(c.artist) > 0 {
            self.params.per_known_artist
        } else {
            self.params.per_new_artist
        };
        if self.per_artist.get(&c.artist).copied().unwrap_or(0) >= artist_limit {
            return false;
        }
        match self.dominant_tag(c.artist) {
            Some(tag) => self.per_dominant_tag.get(tag).copied().unwrap_or(0) < self.tag_limit(),
            None => true,
        }
    }

    fn record(&mut self, pick: &Candidate) {
        self.tracks.insert(pick.track);
        *self.per_artist.entry(pick.artist).or_default() += 1;
        if let Some(tag) = self.dominant_tag(pick.artist) {
            *self.per_dominant_tag.entry(tag).or_default() += 1;
        }
    }

    fn tag_limit(&self) -> usize {
        ((self.params.max_tag_share * self.params.k as f64).floor() as usize).max(1)
    }

    fn dominant_tag(&self, artist: ArtistId) -> Option<&'a str> {
        self.tags
            .get(&artist)?
            .iter()
            .max_by(|a, b| a.weight.total_cmp(&b.weight))
            .map(|t| t.tag.as_str())
    }

    /// Cosine similarity of two artists' tag vectors; 0 if either has none.
    fn tag_cosine(&self, a: ArtistId, b: ArtistId) -> f64 {
        let (Some(ta), Some(tb)) = (self.tags.get(&a), self.tags.get(&b)) else {
            return 0.0;
        };
        let dot: f64 = ta
            .iter()
            .filter_map(|x| {
                tb.iter()
                    .find(|y| y.tag == x.tag)
                    .map(|y| x.weight * y.weight)
            })
            .sum();
        let norm = |v: &[ArtistTag]| v.iter().map(|t| t.weight * t.weight).sum::<f64>().sqrt();
        let denominator = norm(ta) * norm(tb);
        if denominator > 0.0 {
            dot / denominator
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;
    use crate::engine::candidate::Why;
    use crate::model::Arm;

    fn candidate(pool: Pool, track: i64, artist: i64, relevance: f64) -> Candidate {
        Candidate {
            track: TrackId(track),
            artist: ArtistId(artist),
            pool,
            arm: Arm::Pool(pool),
            relevance,
            why: Why::DeepCut { affinity: 1.0 },
        }
    }

    fn posteriors() -> BTreeMap<Pool, Posterior> {
        Pool::ALL.iter().map(|p| (*p, Posterior::PRIOR)).collect()
    }

    fn params(k: usize) -> SelectParams {
        SelectParams::new(
            k,
            ShareBounds {
                floor: 0.2,
                cap: 0.5,
            },
            0.6,
        )
    }

    /// 50 candidates per pool, each by its own new artist.
    fn plenty() -> Vec<Candidate> {
        Pool::ALL
            .iter()
            .enumerate()
            .flat_map(|(p, pool)| {
                (0..50).map(move |i| {
                    let id = p as i64 * 1000 + i;
                    candidate(*pool, id, id, 1.0 - i as f64 / 100.0)
                })
            })
            .collect()
    }

    #[test]
    fn fills_k_with_every_pool_at_its_floor_or_above() {
        let sel = select(
            plenty(),
            &posteriors(),
            &ArtistTags::new(),
            &Profile::default(),
            &params(40),
            &mut StdRng::seed_from_u64(1),
        );
        assert_eq!(sel.picks.len(), 40);
        assert!(sel.shortfall.is_empty());
        for pool in Pool::ALL {
            let n = sel.picks.iter().filter(|c| c.pool == *pool).count();
            assert!(n >= 8, "{pool} got {n}");
        }
    }

    #[test]
    fn a_new_artist_appears_at_most_once() {
        let candidates: Vec<Candidate> =
            (0..10).map(|i| candidate(Pool::Near, i, 7, 1.0)).collect();
        let sel = select(
            candidates,
            &posteriors(),
            &ArtistTags::new(),
            &Profile::default(),
            &params(10),
            &mut StdRng::seed_from_u64(1),
        );
        assert_eq!(sel.picks.len(), 1);
    }

    #[test]
    fn a_dominant_tag_is_capped_at_a_quarter() {
        let candidates: Vec<Candidate> = (0..40).map(|i| candidate(Pool::Far, i, i, 1.0)).collect();
        let tags: ArtistTags = (0..40)
            .map(|i| (ArtistId(i), vec![ArtistTag::new("shoegaze", 1.0)]))
            .collect();
        let sel = select(
            candidates,
            &posteriors(),
            &tags,
            &Profile::default(),
            &params(40),
            &mut StdRng::seed_from_u64(1),
        );
        assert_eq!(sel.picks.len(), 10);
    }

    #[test]
    fn an_empty_pool_is_backfilled_by_the_others() {
        let candidates: Vec<Candidate> = plenty()
            .into_iter()
            .filter(|c| c.pool != Pool::Far)
            .collect();
        let sel = select(
            candidates,
            &posteriors(),
            &ArtistTags::new(),
            &Profile::default(),
            &params(40),
            &mut StdRng::seed_from_u64(1),
        );
        assert_eq!(sel.picks.len(), 40);
        assert!(sel.shortfall[&Pool::Far] >= 8);
        assert!(sel.picks.iter().all(|c| c.pool != Pool::Far));
    }

    #[test]
    fn mmr_prefers_variety_over_a_slightly_more_relevant_lookalike() {
        let candidates = vec![
            candidate(Pool::Near, 1, 1, 1.0),
            candidate(Pool::Near, 2, 2, 0.95),
            candidate(Pool::Near, 3, 3, 0.9),
        ];
        let tags: ArtistTags = HashMap::from([
            (ArtistId(1), vec![ArtistTag::new("techno", 1.0)]),
            (ArtistId(2), vec![ArtistTag::new("techno", 1.0)]),
            (ArtistId(3), vec![ArtistTag::new("folk", 1.0)]),
        ]);
        let only_near = BTreeMap::from([(Pool::Near, Posterior::PRIOR)]);
        let sel = select(
            candidates,
            &only_near,
            &tags,
            &Profile::default(),
            &SelectParams::new(
                2,
                ShareBounds {
                    floor: 0.0,
                    cap: 1.0,
                },
                0.6,
            ),
            &mut StdRng::seed_from_u64(1),
        );
        let tracks: HashSet<i64> = sel.picks.iter().map(|c| c.track.0).collect();
        assert_eq!(tracks, HashSet::from([1, 3]));
    }
}
