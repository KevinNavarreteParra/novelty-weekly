//! Rediscover: artists I loved and stopped playing (spec §13, v4). Part
//! nostalgia, part novelty: each artist offers a song I showed I loved, or
//! one of theirs I never kept.
//!
//! Unlike the other pools this one relaxes "never played" (spec G1) on
//! purpose. "Never recommended" still holds, so each loved song comes back at
//! most once. An artist I start streaming again is no longer lapsed, so it
//! leaves the pool by itself.

use std::collections::{HashMap, HashSet};

use rand::seq::IndexedRandom;
use rand::{Rng, RngExt};

use crate::engine::candidate::{Candidate, Why};
use crate::engine::profile::Profile;
use crate::model::{Arm, ArtistId, Familiarity, LapsedArtist, Pool, PopularTracks, TrackId};

/// Below this much evidence an artist is someone I merely came across: a
/// service rating me a light listener, with nothing saved, isn't fondness.
pub const MIN_FONDNESS: f64 = 1.0;

/// How many lapsed artists to consider each week: the fondest, plus a few
/// drawn from further down so the same names don't lead every week.
#[derive(Debug, Clone, Copy)]
pub struct Shortlist {
    pub top: usize,
    pub sampled: usize,
}

impl Default for Shortlist {
    fn default() -> Self {
        Shortlist {
            top: 40,
            sampled: 20,
        }
    }
}

/// Relevance of an artist's other kind of song, and of its second song of
/// the chosen kind, relative to the first: fallbacks for when it can't play.
const OTHER_KIND: f64 = 0.5;
const SECOND_SONG: f64 = 0.6;

/// How much evidence says I liked an artist, in saved tracks:
///
/// `saved + listed + 2·albums + 0.2·streams + rating`, where the service's
/// rating counts 4 for heavy, 2 for moderate, 1 for lapsed and 0.5 otherwise.
pub fn fondness(l: &LapsedArtist) -> f64 {
    let rating = match l.familiarity {
        Some(Familiarity::Heavy) => 4.0,
        Some(Familiarity::Moderate) => 2.0,
        Some(Familiarity::Lapsed) => 1.0,
        Some(Familiarity::Light | Familiarity::Unknown) => 0.5,
        None => 0.0,
    };
    f64::from(l.saved)
        + f64::from(l.listed)
        + 2.0 * f64::from(l.albums)
        + 0.2 * f64::from(l.streams)
        + rating
}

/// The lapsed artists to offer this week: fond enough, not `resting` (picked
/// recently), the `top` fondest plus `sampled` drawn from the rest.
pub fn shortlist<'a>(
    lapsed: &'a [LapsedArtist],
    resting: &HashSet<ArtistId>,
    plan: Shortlist,
    rng: &mut impl Rng,
) -> Vec<&'a LapsedArtist> {
    let mut fond: Vec<(&LapsedArtist, f64)> = lapsed
        .iter()
        .filter(|l| !resting.contains(&l.artist))
        .map(|l| (l, fondness(l)))
        .filter(|(_, f)| *f >= MIN_FONDNESS)
        .collect();
    fond.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.artist.cmp(&b.0.artist)));
    let top = plan.top.min(fond.len());
    let mut chosen: Vec<&LapsedArtist> = fond[..top].iter().map(|(l, _)| *l).collect();
    chosen.extend(fond[top..].sample(rng, plan.sampled).map(|(l, _)| *l));
    chosen
}

/// Candidates from the shortlisted artists. Each artist leads with one kind of
/// song, chosen at random: a loved one (from `loved`, most loved first), or
/// one of their popular tracks I haven't heard, skipping their biggest hit.
/// The other kind follows at lower relevance in case the first can't play.
/// Relevance is `ln(1 + fondness)`.
pub fn candidates(
    shortlist: &[&LapsedArtist],
    loved: &HashMap<ArtistId, Vec<TrackId>>,
    popular: &PopularTracks,
    profile: &Profile,
    rng: &mut impl Rng,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for lapsed in shortlist {
        let artist = lapsed.artist;
        let relevance = fondness(lapsed).ln_1p();
        let loved_songs: Vec<TrackId> = loved
            .get(&artist)
            .into_iter()
            .flatten()
            .copied()
            .take(2)
            .collect();
        let unkept: Vec<TrackId> = popular
            .get(&artist)
            .into_iter()
            .flatten()
            .skip(1)
            .map(|(track, _)| *track)
            .filter(|t| !profile.heard(*t))
            .take(2)
            .collect();
        let lead_loved = match (loved_songs.is_empty(), unkept.is_empty()) {
            (true, true) => continue,
            (false, true) => true,
            (true, false) => false,
            (false, false) => rng.random_bool(0.5),
        };
        let (lead, follow) = if lead_loved {
            ((&loved_songs, true), (&unkept, false))
        } else {
            ((&unkept, false), (&loved_songs, true))
        };
        let offer = |track: TrackId, was_loved: bool, weight: f64| Candidate {
            track,
            artist,
            pool: Pool::Rediscover,
            arm: Arm::Pool(Pool::Rediscover),
            relevance: relevance * weight,
            why: Why::Rediscover {
                loved: was_loved,
                lapsed: (*lapsed).clone(),
            },
        };
        for (i, track) in lead.0.iter().enumerate() {
            out.push(offer(
                *track,
                lead.1,
                if i == 0 { 1.0 } else { SECOND_SONG },
            ));
        }
        if let Some(track) = follow.0.first() {
            out.push(offer(*track, follow.1, OTHER_KIND));
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
    use crate::model::Heard;

    fn lapsed(artist: i64, saved: u32, familiarity: Option<Familiarity>) -> LapsedArtist {
        LapsedArtist {
            artist: ArtistId(artist),
            saved,
            listed: 0,
            albums: 0,
            familiarity,
            streams: 0,
            last_stream: None,
        }
    }

    fn profile(heard: &[i64]) -> Profile {
        let params = ProfileParams {
            tau_days: 365.0,
            autoplay_weight: 1.0,
        };
        Profile::build(&[], None, Timestamp::UNIX_EPOCH, &params).with_heard(&Heard {
            tracks: heard.iter().map(|t| TrackId(*t)).collect(),
            artists: HashSet::new(),
        })
    }

    #[test]
    fn fondness_needs_more_than_a_light_rating() {
        assert!(fondness(&lapsed(1, 0, Some(Familiarity::Light))) < MIN_FONDNESS);
        assert!(fondness(&lapsed(1, 0, Some(Familiarity::Lapsed))) >= MIN_FONDNESS);
        assert_eq!(fondness(&lapsed(1, 3, Some(Familiarity::Moderate))), 5.0);
    }

    #[test]
    fn the_shortlist_is_the_fondest_who_are_not_resting() {
        let all = [
            lapsed(1, 9, None),
            lapsed(2, 5, None),
            lapsed(3, 7, None),
            lapsed(4, 0, Some(Familiarity::Light)),
        ];
        let resting = HashSet::from([ArtistId(3)]);
        let plan = Shortlist { top: 2, sampled: 0 };
        let chosen: Vec<ArtistId> = shortlist(&all, &resting, plan, &mut StdRng::seed_from_u64(1))
            .iter()
            .map(|l| l.artist)
            .collect();
        assert_eq!(chosen, vec![ArtistId(1), ArtistId(2)]);
    }

    #[test]
    fn each_artist_leads_with_a_loved_song_or_one_never_kept() {
        let artists = [lapsed(1, 3, None), lapsed(2, 3, None)];
        let shortlisted: Vec<&LapsedArtist> = artists.iter().collect();
        let loved = HashMap::from([(ArtistId(1), vec![TrackId(10), TrackId(11), TrackId(12)])]);
        // Artist 2's hit (20) is skipped, 21 I've heard, so 22 and 23 are offered.
        let popular: PopularTracks = HashMap::from([(
            ArtistId(2),
            vec![
                (TrackId(20), None),
                (TrackId(21), None),
                (TrackId(22), None),
                (TrackId(23), None),
            ],
        )]);
        let found = candidates(
            &shortlisted,
            &loved,
            &popular,
            &profile(&[21]),
            &mut StdRng::seed_from_u64(3),
        );
        let offered: Vec<(i64, bool, f64)> = found
            .iter()
            .map(|c| match &c.why {
                Why::Rediscover { loved, .. } => (c.track.0, *loved, c.relevance),
                _ => unreachable!(),
            })
            .collect();
        let top = 3.0_f64.ln_1p();
        assert_eq!(
            offered,
            vec![
                (10, true, top),
                (11, true, top * SECOND_SONG),
                (22, false, top),
                (23, false, top * SECOND_SONG),
            ]
        );
        assert!(found.iter().all(|c| c.pool == Pool::Rediscover));
    }

    #[test]
    fn with_both_kinds_the_lead_is_random_and_the_other_follows() {
        let artists = [lapsed(1, 3, None)];
        let shortlisted: Vec<&LapsedArtist> = artists.iter().collect();
        let loved = HashMap::from([(ArtistId(1), vec![TrackId(10)])]);
        let popular: PopularTracks =
            HashMap::from([(ArtistId(1), vec![(TrackId(19), None), (TrackId(20), None)])]);
        let mut leads = HashSet::new();
        for seed in 0..20 {
            let found = candidates(
                &shortlisted,
                &loved,
                &popular,
                &profile(&[]),
                &mut StdRng::seed_from_u64(seed),
            );
            assert_eq!(found.len(), 2);
            assert!(found[0].relevance > found[1].relevance);
            leads.insert(found[0].track);
        }
        assert_eq!(leads, HashSet::from([TrackId(10), TrackId(20)]));
    }
}
