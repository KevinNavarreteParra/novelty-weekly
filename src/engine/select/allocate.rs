//! Splitting the playlist's slots across pools (spec §7.4 step 1).
//!
//! Each pool's success rate is a Beta posterior. A Thompson draw from each
//! gives raw shares; projecting those onto `[floor, cap]` guarantees the bandit
//! can tilt toward what works but never abandon a pool.

use std::collections::BTreeMap;

use rand::Rng;
use rand_distr::{Beta, Distribution};

use crate::model::Pool;

/// A Beta(alpha, beta) belief about how often a pool's picks land.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Posterior {
    pub alpha: f64,
    pub beta: f64,
}

impl Posterior {
    /// The spec's prior: Beta(2, 2), centered on 50% and easily moved.
    pub const PRIOR: Posterior = Posterior {
        alpha: 2.0,
        beta: 2.0,
    };

    pub fn mean(self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    fn sample(self, rng: &mut impl Rng) -> f64 {
        Beta::new(self.alpha.max(1e-6), self.beta.max(1e-6))
            .expect("alpha and beta are positive")
            .sample(rng)
    }
}

/// Each pool's share of a playlist stays inside these bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShareBounds {
    pub floor: f64,
    pub cap: f64,
}

/// How many of `k` slots each pool gets this week.
pub fn allocate(
    k: usize,
    posteriors: &BTreeMap<Pool, Posterior>,
    bounds: ShareBounds,
    rng: &mut impl Rng,
) -> BTreeMap<Pool, usize> {
    let pools: Vec<Pool> = posteriors.keys().copied().collect();
    let draws: Vec<f64> = posteriors.values().map(|p| p.sample(rng)).collect();
    let total: f64 = draws.iter().sum();
    let raw: Vec<f64> = if total > 0.0 {
        draws.iter().map(|d| d / total).collect()
    } else {
        vec![1.0 / pools.len() as f64; pools.len()]
    };
    let shares = project(&raw, bounds);
    pools.into_iter().zip(round_to_total(&shares, k)).collect()
}

/// The closest shares to `raw` (shifting all by one amount) that sum to 1 with
/// each inside the bounds. Bounds that can't hold for this many pools are
/// widened just enough to be satisfiable.
pub fn project(raw: &[f64], bounds: ShareBounds) -> Vec<f64> {
    let n = raw.len() as f64;
    let floor = bounds.floor.min(1.0 / n);
    let cap = bounds.cap.max(1.0 / n);
    let filled = |shift: f64| -> f64 { raw.iter().map(|s| (s + shift).clamp(floor, cap)).sum() };

    // filled() rises with shift, from n·floor ≤ 1 at -1 to n·cap ≥ 1 at +1;
    // bisect for the shift where it reaches exactly 1.
    let (mut lo, mut hi) = (-1.0, 1.0);
    for _ in 0..100 {
        let mid = (lo + hi) / 2.0;
        if filled(mid) < 1.0 {
            lo = mid
        } else {
            hi = mid
        }
    }
    raw.iter().map(|s| (s + hi).clamp(floor, cap)).collect()
}

/// Whole slot counts summing to `k`, by largest remainder.
pub fn round_to_total(shares: &[f64], k: usize) -> Vec<usize> {
    let exact: Vec<f64> = shares.iter().map(|s| s * k as f64).collect();
    let mut counts: Vec<usize> = exact.iter().map(|x| x.floor() as usize).collect();
    let mut by_remainder: Vec<usize> = (0..shares.len()).collect();
    by_remainder
        .sort_by(|&a, &b| (exact[b] - exact[b].floor()).total_cmp(&(exact[a] - exact[a].floor())));
    let missing = k.saturating_sub(counts.iter().sum());
    for &i in by_remainder.iter().cycle().take(missing) {
        counts[i] += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use rand::rngs::StdRng;
    use rand::{RngExt, SeedableRng};

    use super::*;

    const BOUNDS: ShareBounds = ShareBounds {
        floor: 0.2,
        cap: 0.5,
    };

    fn assert_close(a: &[f64], b: &[f64]) {
        assert!(
            a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9),
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn shares_already_in_bounds_are_unchanged() {
        assert_close(&project(&[0.3, 0.3, 0.4], BOUNDS), &[0.3, 0.3, 0.4]);
    }

    #[test]
    fn a_dominant_pool_is_capped_and_the_excess_shared_equally() {
        // 0.9 caps at 0.5; the other two rise by the same 0.2.
        assert_close(&project(&[0.9, 0.08, 0.02], BOUNDS), &[0.5, 0.28, 0.22]);
    }

    #[test]
    fn a_starved_pool_is_lifted_to_the_floor() {
        assert_close(&project(&[0.5, 0.45, 0.05], BOUNDS), &[0.425, 0.375, 0.2]);
    }

    #[test]
    fn projection_always_sums_to_one_within_bounds() {
        let mut rng = StdRng::seed_from_u64(3);
        for _ in 0..500 {
            let raw: Vec<f64> = (0..3).map(|_| rng.random::<f64>()).collect();
            let total: f64 = raw.iter().sum();
            let raw: Vec<f64> = raw.iter().map(|r| r / total).collect();
            let shares = project(&raw, BOUNDS);
            assert!((shares.iter().sum::<f64>() - 1.0).abs() < 1e-9);
            assert!(shares.iter().all(|s| (0.2 - 1e-9..=0.5 + 1e-9).contains(s)));
        }
    }

    #[test]
    fn impossible_bounds_are_widened() {
        // Two pools can't each be capped at 30%.
        let tight = ShareBounds {
            floor: 0.2,
            cap: 0.3,
        };
        assert_close(&project(&[0.9, 0.1], tight), &[0.5, 0.5]);
    }

    #[test]
    fn rounding_keeps_the_total() {
        assert_eq!(round_to_total(&[1.0 / 3.0; 3], 40), vec![14, 13, 13]);
        assert_eq!(round_to_total(&[0.5, 0.3, 0.2], 40), vec![20, 12, 8]);
    }

    #[test]
    fn allocation_fills_k_and_respects_the_floor() {
        let posteriors = BTreeMap::from([
            (
                Pool::Deep,
                Posterior {
                    alpha: 50.0,
                    beta: 2.0,
                },
            ),
            (Pool::Near, Posterior::PRIOR),
            (
                Pool::Far,
                Posterior {
                    alpha: 1.0,
                    beta: 50.0,
                },
            ),
        ]);
        let mut rng = StdRng::seed_from_u64(9);
        for _ in 0..100 {
            let slots = allocate(40, &posteriors, BOUNDS, &mut rng);
            assert_eq!(slots.values().sum::<usize>(), 40);
            assert!(slots.values().all(|&n| (8..=20).contains(&n)), "{slots:?}");
        }
    }
}
