//! Playlist order (spec §7.4 step 4): pools spread evenly through the list,
//! a deep cut first as a gentle on-ramp, and no two far jumps in a row.

use std::collections::BTreeMap;

use crate::engine::candidate::Candidate;
use crate::model::Pool;

/// Merges each pool's picks (best first) into one playlist.
///
/// Item `i` of a pool with `n` picks sits at fraction `(i + 0.5) / n` of the
/// way through, so every pool is spread evenly whatever its size. Then the
/// first deep cut moves to the front, and back-to-back far picks are split up
/// wherever another pool's pick is left to put between them.
pub fn interleave(by_pool: BTreeMap<Pool, Vec<Candidate>>) -> Vec<Candidate> {
    let mut keyed: Vec<(f64, Pool, Candidate)> = Vec::new();
    for (pool, picks) in by_pool {
        let n = picks.len() as f64;
        for (i, pick) in picks.into_iter().enumerate() {
            keyed.push(((i as f64 + 0.5) / n, pool, pick));
        }
    }
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut order: Vec<Candidate> = keyed.into_iter().map(|(_, _, c)| c).collect();

    if let Some(first_deep) = order.iter().position(|c| c.pool == Pool::Deep) {
        let deep = order.remove(first_deep);
        order.insert(0, deep);
    }
    separate_far_jumps(&mut order);
    order
}

/// Wherever two far picks touch, pulls the next non-far pick forward between
/// them. Stops when no non-far picks remain later in the list.
fn separate_far_jumps(order: &mut [Candidate]) {
    for i in 1..order.len() {
        if order[i].pool == Pool::Far && order[i - 1].pool == Pool::Far {
            let Some(offset) = order[i..].iter().position(|c| c.pool != Pool::Far) else {
                return;
            };
            order[i..=i + offset].rotate_right(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::candidate::Why;
    use crate::model::{Arm, ArtistId, TrackId};

    fn picks(pool: Pool, n: i64, base: i64) -> Vec<Candidate> {
        (0..n)
            .map(|i| Candidate {
                track: TrackId(base + i),
                artist: ArtistId(base + i),
                pool,
                arm: Arm::Pool(pool),
                relevance: 1.0,
                why: Why::DeepCut { affinity: 1.0 },
            })
            .collect()
    }

    fn pools(order: &[Candidate]) -> String {
        order
            .iter()
            .map(|c| match c.pool {
                Pool::Deep => 'd',
                Pool::Near => 'n',
                Pool::Far => 'f',
            })
            .collect()
    }

    #[test]
    fn starts_deep_spreads_pools_and_never_doubles_far() {
        let order = interleave(BTreeMap::from([
            (Pool::Deep, picks(Pool::Deep, 8, 0)),
            (Pool::Near, picks(Pool::Near, 20, 100)),
            (Pool::Far, picks(Pool::Far, 12, 200)),
        ]));
        let shape = pools(&order);
        assert_eq!(order.len(), 40);
        assert!(shape.starts_with('d'), "{shape}");
        assert!(!shape.contains("ff"), "{shape}");
    }

    #[test]
    fn each_pool_keeps_its_own_order() {
        let order = interleave(BTreeMap::from([
            (Pool::Deep, picks(Pool::Deep, 3, 0)),
            (Pool::Far, picks(Pool::Far, 5, 200)),
        ]));
        let far: Vec<i64> = order
            .iter()
            .filter(|c| c.pool == Pool::Far)
            .map(|c| c.track.0)
            .collect();
        assert_eq!(far, vec![200, 201, 202, 203, 204]);
    }

    #[test]
    fn unavoidable_far_runs_are_left_at_the_end() {
        let order = interleave(BTreeMap::from([
            (Pool::Near, picks(Pool::Near, 1, 100)),
            (Pool::Far, picks(Pool::Far, 4, 200)),
        ]));
        assert_eq!(pools(&order), "fnfff");
    }
}
