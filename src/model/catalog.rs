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

    /// Whether `self`, found by searching a service, is the song `wanted`
    /// asked for: the same artist, and titles at least [`SAME_TITLE`] alike
    /// once version markers are stripped. Used to accept search results.
    pub fn is_same_song_as(&self, wanted: &TrackRef) -> bool {
        self.artist.key() == wanted.artist.key()
            && strsim::normalized_levenshtein(&self.title_key(), &wanted.title_key()) >= SAME_TITLE
    }
}

/// Title similarity (normalized Levenshtein, 0 to 1) above which two titles by
/// the same artist name the same song (spec §7.5).
pub const SAME_TITLE: f64 = 0.85;

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str) -> TrackRef {
        TrackRef::new(title, ArtistRef::named(artist))
    }

    #[test]
    fn same_song_tolerates_versions_and_small_spelling_differences() {
        let wanted = track("Dont Look Back in Anger", "Oasis");
        assert!(track("Don't Look Back In Anger - Remastered", "Oasis").is_same_song_as(&wanted));
        assert!(!track("Don't Look Back In Anger", "Some Cover Band").is_same_song_as(&wanted));
        assert!(!track("Wonderwall", "Oasis").is_same_song_as(&wanted));
    }
}
