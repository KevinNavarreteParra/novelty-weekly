//! Words for a build: each pick's rationale, the playlist description, and
//! the `--dry-run` listing.

use std::collections::{BTreeMap, HashMap};

use crate::engine::candidate::{Candidate, Why};
use crate::model::{ArtistId, Pool, TrackRef, Week};

/// Every artist a set of candidates' rationales mention.
pub fn mentioned_artists(picks: &[Candidate]) -> Vec<ArtistId> {
    let mut artists: Vec<ArtistId> = Vec::new();
    for c in picks {
        artists.push(c.artist);
        match &c.why {
            Why::SimilarTo(via) => artists.extend(via.iter().map(|(a, _)| *a)),
            Why::Walk(path) => artists.extend(path.iter().copied()),
            Why::DeepCut { .. } | Why::RareTag { .. } => {}
        }
    }
    artists.sort();
    artists.dedup();
    artists
}

/// e.g. "similar to Low (0.82), Galaxie 500 (0.61)".
pub fn rationale(c: &Candidate, names: &HashMap<ArtistId, String>) -> String {
    let name = |a: &ArtistId| names.get(a).map_or("?", String::as_str);
    match &c.why {
        Why::DeepCut { affinity } => {
            format!("unheard track by an artist I like (affinity {affinity:.1})")
        }
        Why::SimilarTo(via) => {
            let links: Vec<String> = via
                .iter()
                .map(|(a, sim)| format!("{} ({sim:.2})", name(a)))
                .collect();
            format!("similar to {}", links.join(", "))
        }
        Why::RareTag { tag, share } => {
            format!(
                "from the \"{tag}\" chart ({:.1}% of my listening)",
                share * 100.0
            )
        }
        Why::Walk(path) => {
            let steps: Vec<&str> = path.iter().map(name).collect();
            format!("walked {}", steps.join(" → "))
        }
    }
}

/// The playlist description, e.g. "2026-W46 · deep 10 / near 18 / far 12".
pub fn description(week: Week, picks: &[Candidate]) -> String {
    let mut counts: BTreeMap<Pool, usize> = Pool::ALL.iter().map(|p| (*p, 0)).collect();
    for c in picks {
        *counts.entry(c.pool).or_default() += 1;
    }
    let split: Vec<String> = counts
        .iter()
        .map(|(pool, n)| format!("{pool} {n}"))
        .collect();
    format!("{week} · {}", split.join(" / "))
}

/// One line per pick: position, pool, artist and title, then why.
pub fn listing(picks: &[(Candidate, TrackRef, String)]) -> String {
    let mut out = String::new();
    for (i, (c, track, why)) in picks.iter().enumerate() {
        out.push_str(&format!(
            "{:>2}. {:<4}  {} – {}\n      {why}\n",
            i + 1,
            c.pool,
            track.artist.name,
            track.title,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Arm, TrackId};

    fn candidate(pool: Pool, why: Why) -> Candidate {
        Candidate {
            track: TrackId(1),
            artist: ArtistId(1),
            pool,
            arm: Arm::Pool(pool),
            relevance: 1.0,
            why,
        }
    }

    #[test]
    fn rationales_name_the_artists_involved() {
        let names = HashMap::from([
            (ArtistId(2), "Low".to_owned()),
            (ArtistId(3), "Galaxie 500".to_owned()),
        ]);
        let near = candidate(
            Pool::Near,
            Why::SimilarTo(vec![(ArtistId(2), 0.82), (ArtistId(3), 0.61)]),
        );
        assert_eq!(
            rationale(&near, &names),
            "similar to Low (0.82), Galaxie 500 (0.61)"
        );
        let walk = candidate(Pool::Far, Why::Walk(vec![ArtistId(2), ArtistId(3)]));
        assert_eq!(rationale(&walk, &names), "walked Low → Galaxie 500");
    }

    #[test]
    fn the_description_counts_every_pool() {
        let picks = [
            candidate(Pool::Deep, Why::DeepCut { affinity: 1.0 }),
            candidate(Pool::Far, Why::Walk(vec![])),
            candidate(Pool::Far, Why::Walk(vec![])),
        ];
        let week: Week = "2026-W46".parse().unwrap();
        assert_eq!(
            description(week, &picks),
            "2026-W46 · deep 1 / near 0 / far 2"
        );
    }
}
