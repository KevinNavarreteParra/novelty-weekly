//! How past recommendations fared, and what the bandit believes about each arm
//! as a result (spec §9).
//!
//! Nothing here accumulates. Each build recomputes every outcome from the
//! ledger and the plays, then every arm's posterior from the outcomes. So when
//! a history import replaces rough polled plays with exact ones, the beliefs
//! correct themselves, and retuning the decay applies to all past weeks.

use std::collections::{BTreeMap, HashMap};

use jiff::Timestamp;

use super::select::allocate::Posterior;
use crate::model::{
    Arm, COMPLETE_AFTER_MS, Play, Pool, Recommendation, SKIP_BEFORE_MS, TrackId, Verdict,
};

const MS_PER_DAY: i64 = 86_400_000;
/// Replays within this many days of the first listen earn a bonus.
const REPLAY_WINDOW_DAYS: i64 = 30;
const SAVED_BONUS: f64 = 0.5;
const REPLAY_BONUS: f64 = 0.25;
const MAX_REPLAY_BONUS: f64 = 1.0;

/// What happened to one recommendation after it was published.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// The first listen since publishing, if any.
    pub first_play: Option<Play>,
    /// Further listens within 30 days of the first.
    pub replays: u32,
    /// Saved to the library since publishing.
    pub saved: bool,
}

/// One outcome per recommendation, in the same order.
///
/// `saved` maps a track to when it was saved to the library.
pub fn outcomes(
    recs: &[Recommendation],
    plays: &[Play],
    saved: &HashMap<TrackId, Timestamp>,
) -> Vec<Outcome> {
    let mut plays_by_track: HashMap<TrackId, Vec<&Play>> = HashMap::new();
    for play in plays {
        plays_by_track.entry(play.track).or_default().push(play);
    }
    recs.iter()
        .map(|rec| {
            let mut after: Vec<&Play> = plays_by_track
                .get(&rec.track)
                .into_iter()
                .flatten()
                .copied()
                .filter(|p| p.started_at >= rec.published_at)
                .collect();
            after.sort_by_key(|p| p.started_at);
            let first_play = after.first().map(|p| (*p).clone());
            let replays = first_play.as_ref().map_or(0, |first| {
                let window_end =
                    first.started_at.as_millisecond() + REPLAY_WINDOW_DAYS * MS_PER_DAY;
                after[1..]
                    .iter()
                    .filter(|p| p.started_at.as_millisecond() <= window_end)
                    .count() as u32
            });
            Outcome {
                first_play,
                replays,
                saved: saved
                    .get(&rec.track)
                    .is_some_and(|at| *at >= rec.published_at),
            }
        })
        .collect()
}

/// What an outcome teaches the bandit. `base` is in `[0, 1]`; `bonus` only
/// ever adds evidence of success.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reward {
    pub base: f64,
    pub bonus: f64,
}

/// Spec §9.1: a completed first listen is 1, a skip in the first 30 s is 0,
/// anything between is the share played. Saving adds 0.5 and each replay 0.25
/// (replays at most 1.0).
///
/// `None` while the track is unplayed or its first listen is still pending:
/// not getting to a track is not disliking it.
pub fn reward(outcome: &Outcome) -> Option<Reward> {
    let first = outcome.first_play.as_ref()?;
    let ms = first.ms_played?;
    let base = match first.verdict()? {
        Verdict::Completed => 1.0,
        Verdict::Skipped if ms < SKIP_BEFORE_MS => 0.0,
        Verdict::Skipped | Verdict::Partial => {
            let length = first.duration_ms.unwrap_or(COMPLETE_AFTER_MS);
            (f64::from(ms) / f64::from(length.max(1))).clamp(0.0, 1.0)
        }
    };
    let replay_bonus = (REPLAY_BONUS * f64::from(outcome.replays)).min(MAX_REPLAY_BONUS);
    let saved_bonus = if outcome.saved { SAVED_BONUS } else { 0.0 };
    Some(Reward {
        base,
        bonus: replay_bonus + saved_bonus,
    })
}

/// Each arm's posterior, from every rewarded recommendation.
///
/// A reward adds `base + bonus` to alpha and `1 - base` to beta, discounted by
/// `rho` per week since it was published, so the bandit forgets on a ~10-week
/// horizon at 0.9 (spec §9.2). The prior is not discounted: it is the floor
/// every arm's belief rests on.
pub fn posteriors(
    rewarded: &[(&Recommendation, Reward)],
    now: Timestamp,
    rho: f64,
) -> BTreeMap<Arm, Posterior> {
    let mut arms: BTreeMap<Arm, Posterior> = BTreeMap::new();
    for (rec, reward) in rewarded {
        let weeks_ago =
            (now.as_millisecond() - rec.published_at.as_millisecond()).max(0) / (7 * MS_PER_DAY);
        let weight = rho.powi(weeks_ago as i32);
        let arm = arms.entry(rec.arm.clone()).or_insert(Posterior::PRIOR);
        arm.alpha += weight * (reward.base + reward.bonus);
        arm.beta += weight * (1.0 - reward.base);
    }
    arms
}

/// The posterior for each pool's arm, the prior where a pool has no data.
pub fn pool_posteriors(arms: &BTreeMap<Arm, Posterior>) -> BTreeMap<Pool, Posterior> {
    Pool::ALL
        .iter()
        .map(|pool| {
            let belief = arms
                .get(&Arm::Pool(*pool))
                .copied()
                .unwrap_or(Posterior::PRIOR);
            (*pool, belief)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArtistId, Ending, StartedBy, Week};

    const DAY: i64 = MS_PER_DAY;

    fn at(day: i64) -> Timestamp {
        Timestamp::from_millisecond(day * DAY).unwrap()
    }

    fn rec(track: i64, pool: Pool, published_day: i64) -> Recommendation {
        Recommendation {
            week: "2026-W46".parse::<Week>().unwrap(),
            published_at: at(published_day),
            position: 0,
            track: TrackId(track),
            artist: ArtistId(1),
            pool,
            arm: Arm::Pool(pool),
            score: 1.0,
            rationale: String::new(),
        }
    }

    fn play(track: i64, day: i64, ms_played: Option<u32>, ending: Ending) -> Play {
        Play {
            track: TrackId(track),
            artist: ArtistId(1),
            started_at: at(day),
            ms_played,
            duration_ms: Some(200_000),
            ending,
            started_by: StartedBy::Listener,
        }
    }

    fn outcome_of(plays: &[Play], saved: bool) -> Outcome {
        let saves = if saved {
            HashMap::from([(TrackId(1), at(12))])
        } else {
            HashMap::new()
        };
        outcomes(&[rec(1, Pool::Near, 10)], plays, &saves).remove(0)
    }

    #[test]
    fn only_listens_after_publishing_count() {
        let o = outcome_of(&[play(1, 5, Some(200_000), Ending::Finished)], false);
        assert_eq!(o.first_play, None);
        assert_eq!(reward(&o), None);
    }

    #[test]
    fn replays_count_within_thirty_days_of_the_first_listen() {
        let plays = [
            play(1, 11, Some(200_000), Ending::Finished),
            play(1, 20, Some(200_000), Ending::Finished),
            play(1, 40, Some(200_000), Ending::Finished),
            play(1, 50, Some(200_000), Ending::Finished),
        ];
        assert_eq!(outcome_of(&plays, false).replays, 2);
    }

    #[test]
    fn rewards_follow_the_spec() {
        let completed = outcome_of(&[play(1, 11, Some(200_000), Ending::Finished)], true);
        assert_eq!(
            reward(&completed),
            Some(Reward {
                base: 1.0,
                bonus: 0.5
            })
        );

        let early_skip = outcome_of(&[play(1, 11, Some(10_000), Ending::Skipped)], false);
        assert_eq!(reward(&early_skip).unwrap().base, 0.0);

        let half = outcome_of(&[play(1, 11, Some(100_000), Ending::Skipped)], false);
        assert_eq!(reward(&half).unwrap().base, 0.5);

        let pending = outcome_of(&[play(1, 11, None, Ending::Unknown)], false);
        assert_eq!(reward(&pending), None);
    }

    #[test]
    fn replay_bonus_is_capped() {
        let mut o = outcome_of(&[play(1, 11, Some(200_000), Ending::Finished)], false);
        o.replays = 9;
        assert_eq!(reward(&o).unwrap().bonus, 1.0);
    }

    #[test]
    fn older_evidence_counts_less() {
        let fresh = rec(1, Pool::Far, 70);
        let old = rec(2, Pool::Far, 0);
        let hit = Reward {
            base: 1.0,
            bonus: 0.0,
        };
        let arms = posteriors(&[(&fresh, hit), (&old, hit)], at(70), 0.9);
        let far = arms[&Arm::Pool(Pool::Far)];
        // Prior 2 + this week's 1 + ten-weeks-old 0.9^10.
        assert!((far.alpha - (3.0 + 0.9f64.powi(10))).abs() < 1e-9);
        assert_eq!(far.beta, 2.0);
    }

    #[test]
    fn pools_without_evidence_keep_the_prior() {
        let beliefs = pool_posteriors(&BTreeMap::new());
        assert_eq!(beliefs.len(), 3);
        assert!(beliefs.values().all(|p| *p == Posterior::PRIOR));
    }
}
