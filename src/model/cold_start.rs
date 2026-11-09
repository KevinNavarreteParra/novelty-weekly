//! The cold start (spec §5.3). Until a listening history is imported, the
//! listening service's own rankings of what I play most, and my saved tracks,
//! stand in for it.

use std::collections::BTreeMap;

use jiff::Timestamp;

use super::stored_enum;
use super::{ArtistId, TrackId};

stored_enum! {
    /// How far back a ranking of what I play most looks.
    pub enum Span {
        /// About the last four weeks.
        Month => "month",
        /// About the last six months.
        HalfYear => "half_year",
        /// About the last year.
        Year => "year",
    }
}

/// What a cold start knows, resolved to store IDs. Rankings are top first.
#[derive(Debug, Clone, Default)]
pub struct ColdStart {
    pub top_artists: BTreeMap<Span, Vec<ArtistId>>,
    /// Each track with its artist.
    pub top_tracks: BTreeMap<Span, Vec<(TrackId, ArtistId)>>,
    /// Saved tracks, each with its artist and when it was saved.
    pub saved: Vec<(TrackId, ArtistId, Timestamp)>,
}

impl ColdStart {
    pub fn is_empty(&self) -> bool {
        self.top_artists.values().all(Vec::is_empty)
            && self.top_tracks.values().all(Vec::is_empty)
            && self.saved.is_empty()
    }
}
