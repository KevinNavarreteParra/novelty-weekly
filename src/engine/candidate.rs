//! A candidate: one track a pool proposes, with why it proposed it.

use crate::model::{Arm, ArtistId, Pool, TrackId};

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub track: TrackId,
    pub artist: ArtistId,
    pub pool: Pool,
    pub arm: Arm,
    /// Higher is better. Only compared against candidates from the same pool.
    pub relevance: f64,
    pub why: Why,
}

/// The evidence behind a candidate, kept as data so `--dry-run` and the ledger
/// can name the artists involved.
#[derive(Debug, Clone, PartialEq)]
pub enum Why {
    /// An unheard track by an artist I like this much.
    DeepCut { affinity: f64 },
    /// Close to these seed artists, strongest link first.
    SimilarTo(Vec<(ArtistId, f64)>),
    /// Drawn from a tag that makes up this share of my listening.
    RareTag { tag: String, share: f64 },
    /// Reached by walking similarity links from a seed, seed first.
    Walk(Vec<ArtistId>),
}
