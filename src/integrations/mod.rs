//! Adapters between outside services and the canonical [`model`](crate::model).
//!
//! Each adapter's job is translation: read a service's own format and hand back
//! model types that follow the model's rules, or take model types and write
//! them out. Recommendation logic never lives here, and a service's wire
//! format never leaks past its module.
//!
//! The traits below are the roles the app needs filled. To swap or add a
//! service, implement the role; nothing else changes.

pub mod cache;
pub mod http;
pub mod lastfm;
pub mod spotify;

use std::ops::Range;

use anyhow::Result;
use jiff::Timestamp;

use crate::model::{ArtistRef, ArtistTag, Listen, PopularTrack, SimilarArtist, TrackRef};

/// The service I listen on: where listens come from, where catalogs are
/// browsed, and where the playlist goes. Spotify fills this role.
pub trait ListeningService {
    /// Listens that started after `after`, oldest first. The newest may have
    /// `ms_played: None` until a later call (asked from just before it) can
    /// tell how long it played.
    fn recent_listens(&mut self, after: Option<Timestamp>) -> Result<Vec<Listen>>;

    /// Tracks saved to the library since `since`, with when each was saved.
    fn saved_since(&mut self, since: Timestamp) -> Result<Vec<(TrackRef, Timestamp)>>;

    /// Every track on the artist's albums and singles where they are the
    /// primary artist. Empty if the service doesn't know them.
    fn catalog(&mut self, artist: &ArtistRef) -> Result<Vec<TrackRef>>;

    /// The service's own copy of `track`, carrying the service's ID, if it has
    /// the song (see [`TrackRef::is_same_song_as`]).
    fn find(&mut self, track: &TrackRef) -> Result<Option<TrackRef>>;

    /// Replaces a playlist's contents and description, creating it first when
    /// `playlist` is `None`. Returns the playlist's ID. Every track must carry
    /// the service's ID, as [`find`](Self::find) returns them.
    fn publish(
        &mut self,
        playlist: Option<&str>,
        name: &str,
        description: &str,
        tracks: &[TrackRef],
    ) -> Result<String>;
}

/// What the app needs to know about artists it may not have heard. Last.fm
/// fills this role today; ListenBrainz or MusicBrainz could.
///
/// An artist the source doesn't know is an empty answer, not an error.
pub trait Knowledge {
    /// Recorded with everything this source reports, e.g. `"lastfm"`.
    fn source(&self) -> &'static str;

    /// Artists like this one, most similar first.
    fn similar_artists(&self, artist: &ArtistRef) -> Result<Vec<SimilarArtist>>;

    /// Tags that describe the artist's music. Not tags that describe the
    /// listener ("seen live", "favorites"): filter those out.
    fn artist_tags(&self, artist: &ArtistRef) -> Result<Vec<ArtistTag>>;

    /// The artist's most played tracks, most played first.
    fn popular_tracks(&self, artist: &ArtistRef) -> Result<Vec<PopularTrack>>;

    /// How many people listen to the artist, as a popularity measure.
    fn listeners(&self, artist: &ArtistRef) -> Result<Option<u64>>;

    /// The artists most associated with a tag, at chart positions `ranks`
    /// (0 is the top).
    fn tag_chart(&self, tag: &str, ranks: Range<usize>) -> Result<Vec<ArtistRef>>;
}
