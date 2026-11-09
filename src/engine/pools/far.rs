//! Far: unheard artists in territory I barely listen to (spec §7.2). Two
//! generators, meant to supply about half each:
//!
//! - **Rare tags**: sample tags in inverse proportion to how much of my
//!   listening they cover, then take artists from the middle of each tag's chart.
//! - **Random walks**: follow similarity links away from a seed, through
//!   artists I have never played, and propose where the walk ends.
//!
//! Relevance is deliberately flat, `1 / (1 + rank)` within each generator: the
//! point of this pool is not to be close to my taste.

use std::collections::{HashMap, HashSet};

use rand::Rng;
use rand::seq::IndexedRandom;

use crate::engine::candidate::{Candidate, Why};
use crate::engine::profile::Profile;
use crate::model::{Arm, ArtistId, ArtistTags, Pool, PopularTracks};

/// Tags always worth sampling, even when no artist I know carries them.
pub const SEED_TAGS: &[&str] = &[
    "afrobeat",
    "highlife",
    "ethio jazz",
    "tropicalia",
    "bossa nova",
    "mpb",
    "cumbia",
    "fado",
    "flamenco",
    "qawwali",
    "gamelan",
    "rebetiko",
    "city pop",
    "enka",
    "k indie",
    "hard bop",
    "modal jazz",
    "free jazz",
    "spiritual jazz",
    "baroque",
    "renaissance",
    "romantic",
    "minimalism",
    "contemporary classical",
    "dub techno",
    "footwork",
    "ambient",
    "drone",
    "musique concrete",
    "bluegrass",
    "old time",
    "zydeco",
    "dancehall",
];

/// How sharply tag sampling favors what I rarely hear.
#[derive(Debug, Clone, Copy)]
pub struct TagSampling {
    /// Exponent λ in `(share + ε)^-λ`; 0 samples uniformly.
    pub lambda: f64,
    /// Keeps a tag I never hear from taking all the probability.
    pub epsilon: f64,
}

impl Default for TagSampling {
    fn default() -> Self {
        TagSampling {
            lambda: 1.0,
            epsilon: 0.001,
        }
    }
}

/// The tags to sample from: any tag on at least `min_artists` known artists,
/// plus [`SEED_TAGS`].
pub fn tag_universe(tags: &ArtistTags, min_artists: usize) -> Vec<String> {
    let mut artists_per_tag: HashMap<&str, usize> = HashMap::new();
    for artist_tags in tags.values() {
        for tag in artist_tags {
            *artists_per_tag.entry(tag.tag.as_str()).or_default() += 1;
        }
    }
    let mut universe: Vec<String> = artists_per_tag
        .into_iter()
        .filter(|(_, n)| *n >= min_artists)
        .map(|(tag, _)| tag.to_owned())
        .chain(SEED_TAGS.iter().map(|t| (*t).to_owned()))
        .collect();
    universe.sort();
    universe.dedup();
    universe
}

/// `count` distinct tags, each drawn with probability `∝ (share + ε)^-λ`.
pub fn sample_tags(
    universe: &[String],
    shares: &HashMap<String, f64>,
    count: usize,
    sampling: TagSampling,
    rng: &mut impl Rng,
) -> Vec<String> {
    let mut remaining: Vec<(&String, f64)> = universe
        .iter()
        .map(|tag| {
            let share = shares.get(tag).copied().unwrap_or(0.0);
            (tag, (share + sampling.epsilon).powf(-sampling.lambda))
        })
        .collect();
    let mut picked = Vec::new();
    while picked.len() < count {
        let Ok(choice) = remaining.choose_weighted(rng, |(_, w)| *w) else {
            break;
        };
        let tag = choice.0.clone();
        remaining.retain(|(t, _)| **t != tag);
        picked.push(tag);
    }
    picked
}

/// Candidates from one tag's chart. `chart` is the tag's artists in chart
/// order, already past the top ranks the build skips.
pub fn from_tag_chart(
    tag: &str,
    share: f64,
    chart: &[ArtistId],
    profile: &Profile,
    popular: &PopularTracks,
    per_artist: usize,
) -> Vec<Candidate> {
    let unheard = chart.iter().filter(|a| !profile.knows(**a));
    let why = || Why::RareTag {
        tag: tag.to_owned(),
        share,
    };
    candidates_in_order(unheard.copied().map(|a| (a, why())), popular, per_artist)
}

/// Walks `hops` similarity links from `start`, each step to an artist I've
/// never played and the walk hasn't visited, chosen in proportion to
/// similarity. Returns the path, start first, or `None` if it dead-ends.
///
/// `neighbors` may fetch on demand; the walk asks only for nodes it reaches.
pub fn walk(
    start: ArtistId,
    hops: usize,
    neighbors: &mut impl FnMut(ArtistId) -> Vec<(ArtistId, f64)>,
    profile: &Profile,
    rng: &mut impl Rng,
) -> Option<Vec<ArtistId>> {
    let mut path = vec![start];
    let mut visited: HashSet<ArtistId> = HashSet::from([start]);
    for _ in 0..hops {
        let here = *path.last().expect("path starts non-empty");
        let options: Vec<(ArtistId, f64)> = neighbors(here)
            .into_iter()
            .filter(|(a, sim)| *sim > 0.0 && !visited.contains(a) && !profile.knows(*a))
            .collect();
        let &(next, _) = options.choose_weighted(rng, |(_, sim)| *sim).ok()?;
        visited.insert(next);
        path.push(next);
    }
    Some(path)
}

/// Candidates from where each walk ended.
pub fn from_walks(
    walks: &[Vec<ArtistId>],
    popular: &PopularTracks,
    per_artist: usize,
) -> Vec<Candidate> {
    let ends = walks
        .iter()
        .filter_map(|path| path.last().map(|end| (*end, Why::Walk(path.clone()))));
    candidates_in_order(ends, popular, per_artist)
}

/// Candidates with relevance `1 / (1 + rank)` by artist order, each artist
/// offering up to `per_artist` of its most played tracks.
fn candidates_in_order(
    artists: impl Iterator<Item = (ArtistId, Why)>,
    popular: &PopularTracks,
    per_artist: usize,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let fresh = artists.filter(|(a, _)| seen.insert(*a));
    for (rank, (artist, why)) in fresh.enumerate() {
        for &(track, _) in popular.get(&artist).into_iter().flatten().take(per_artist) {
            out.push(Candidate {
                track,
                artist,
                pool: Pool::Far,
                arm: Arm::Pool(Pool::Far),
                relevance: 1.0 / (1.0 + rank as f64),
                why: why.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;
    use crate::engine::profile::ProfileParams;
    use crate::model::{ArtistTag, Ending, Play, StartedBy, TrackId};

    fn profile_having_heard(artists: &[i64]) -> Profile {
        let plays: Vec<Play> = artists
            .iter()
            .map(|&a| Play {
                track: TrackId(a * 100),
                artist: ArtistId(a),
                started_at: Timestamp::UNIX_EPOCH,
                ms_played: Some(200_000),
                duration_ms: Some(200_000),
                ending: Ending::Finished,
                started_by: StartedBy::Listener,
            })
            .collect();
        Profile::build(
            &plays,
            None,
            Timestamp::UNIX_EPOCH,
            &ProfileParams {
                tau_days: 365.0,
                autoplay_weight: 1.0,
            },
        )
    }

    #[test]
    fn rarely_heard_tags_are_sampled_far_more_often() {
        let universe = vec!["indie rock".to_owned(), "gamelan".to_owned()];
        let shares = HashMap::from([("indie rock".to_owned(), 0.6)]);
        let mut rng = StdRng::seed_from_u64(5);
        let gamelan_first = (0..1000)
            .filter(|_| {
                sample_tags(&universe, &shares, 1, TagSampling::default(), &mut rng)[0] == "gamelan"
            })
            .count();
        assert!(gamelan_first > 990, "{gamelan_first}");
    }

    #[test]
    fn sampled_tags_are_distinct() {
        let universe: Vec<String> = SEED_TAGS.iter().map(|t| (*t).to_owned()).collect();
        let tags = sample_tags(
            &universe,
            &HashMap::new(),
            10,
            TagSampling::default(),
            &mut StdRng::seed_from_u64(1),
        );
        assert_eq!(tags.len(), 10);
        assert_eq!(tags.iter().collect::<HashSet<_>>().len(), 10);
    }

    #[test]
    fn the_universe_needs_enough_artists_per_tag() {
        let tags = HashMap::from([
            (
                ArtistId(1),
                vec![
                    ArtistTag::new("Shoegaze", 1.0),
                    ArtistTag::new("seen live", 1.0),
                ],
            ),
            (ArtistId(2), vec![ArtistTag::new("shoegaze", 1.0)]),
        ]);
        let universe = tag_universe(&tags, 2);
        assert!(universe.contains(&"shoegaze".to_owned()));
        assert!(!universe.contains(&"seen live".to_owned()));
        assert!(universe.contains(&"gamelan".to_owned()));
    }

    #[test]
    fn a_walk_only_steps_on_unheard_artists() {
        let profile = profile_having_heard(&[1, 2]);
        let graph: HashMap<ArtistId, Vec<(ArtistId, f64)>> = HashMap::from([
            (ArtistId(1), vec![(ArtistId(2), 0.99), (ArtistId(10), 0.1)]),
            (ArtistId(10), vec![(ArtistId(1), 0.9), (ArtistId(11), 0.5)]),
            (ArtistId(11), vec![(ArtistId(12), 0.5)]),
        ]);
        let mut neighbors = |a: ArtistId| graph.get(&a).cloned().unwrap_or_default();
        let path = walk(
            ArtistId(1),
            3,
            &mut neighbors,
            &profile,
            &mut StdRng::seed_from_u64(2),
        );
        assert_eq!(
            path,
            Some(vec![ArtistId(1), ArtistId(10), ArtistId(11), ArtistId(12)])
        );

        let dead_end = walk(
            ArtistId(1),
            4,
            &mut neighbors,
            &profile,
            &mut StdRng::seed_from_u64(2),
        );
        assert_eq!(dead_end, None);
    }

    #[test]
    fn chart_candidates_skip_heard_artists_and_rank_flatly() {
        let profile = profile_having_heard(&[1]);
        let popular = HashMap::from([
            (ArtistId(1), vec![(TrackId(1), None)]),
            (ArtistId(2), vec![(TrackId(2), None)]),
            (ArtistId(3), vec![(TrackId(3), None)]),
        ]);
        let chart = [ArtistId(1), ArtistId(2), ArtistId(3)];
        let out = from_tag_chart("fado", 0.0, &chart, &profile, &popular, 1);
        let ranked: Vec<(i64, f64)> = out.iter().map(|c| (c.artist.0, c.relevance)).collect();
        assert_eq!(ranked, vec![(2, 1.0), (3, 0.5)]);
    }
}
