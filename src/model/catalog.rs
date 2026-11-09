//! Artists and tracks as an integration reports them, before the store has
//! matched them to its own IDs.

use super::ids::ExternalIds;
use super::names::{name_key, title_key};

/// An artist as a source names it.
///
/// Identity is the normalized name plus any external IDs. The name is the only
/// identity every source shares (the Spotify history export has no artist IDs,
/// and Last.fm is name-first), so two different artists with the same name are
/// one artist here. That is a known, accepted limitation.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistRef {
    /// As displayed, e.g. "Sigur Rós".
    pub name: String,
    pub ids: ExternalIds,
}

impl ArtistRef {
    pub fn named(name: impl Into<String>) -> Self {
        ArtistRef {
            name: name.into(),
            ids: ExternalIds::new(),
        }
    }

    pub fn with_ids(mut self, ids: ExternalIds) -> Self {
        self.ids = ids;
        self
    }

    /// The key the store matches artists on.
    pub fn key(&self) -> String {
        name_key(&self.name)
    }
}

/// A recording as a source reports it.
///
/// Rules for integrations:
/// - `artist` is the primary (first-credited) artist; features don't count.
/// - Leave a field `None` rather than guess. `duration_ms` in particular feeds
///   completion and the 90 s to 15 min filter.
/// - Put every ID you know in `ids`. An ISRC lets the store spot the same
///   recording across releases and services.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackRef {
    /// As displayed, version suffixes and all: "Heroes - 2017 Remaster".
    pub title: String,
    pub artist: ArtistRef,
    pub album: Option<String>,
    pub duration_ms: Option<u32>,
    pub ids: ExternalIds,
}

impl TrackRef {
    pub fn new(title: impl Into<String>, artist: ArtistRef) -> Self {
        TrackRef {
            title: title.into(),
            artist,
            album: None,
            duration_ms: None,
            ids: ExternalIds::new(),
        }
    }

    /// The key that, with the artist, identifies the song regardless of version.
    pub fn title_key(&self) -> String {
        title_key(&self.title)
    }
}
