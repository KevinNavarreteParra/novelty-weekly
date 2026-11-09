//! Spotify Web API JSON, as of the February 2026 changes. Only fields the
//! adapter reads are listed; serde ignores the rest. Private to the adapter.

use serde::Deserialize;

use crate::model::{ArtistRef, ExternalIds, Namespace, TrackRef};

/// A paged list. `next` is the URL of the following page, if any.
#[derive(Debug, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

/// A full or simplified track object.
#[derive(Debug, Deserialize)]
pub struct Track {
    /// Null for local files.
    pub id: Option<String>,
    pub name: String,
    pub artists: Vec<Artist>,
    /// Absent on an album's own track listing.
    pub album: Option<Album>,
    pub duration_ms: u32,
    /// Absent on simplified tracks.
    #[serde(default)]
    pub external_ids: TrackExternalIds,
}

#[derive(Debug, Default, Deserialize)]
pub struct TrackExternalIds {
    pub isrc: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Artist {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct Album {
    pub id: Option<String>,
    pub name: String,
}

/// One entry of `GET /me/player/recently-played`.
#[derive(Debug, Deserialize)]
pub struct PlayHistory {
    pub track: Track,
    /// RFC 3339; taken to be when playback started (to verify; see `recent`).
    pub played_at: String,
}

/// One entry of `GET /me/tracks`.
#[derive(Debug, Deserialize)]
pub struct SavedTrack {
    pub added_at: String,
    pub track: Track,
}

#[derive(Debug, Deserialize)]
pub struct TrackSearch {
    pub tracks: Page<Track>,
}

#[derive(Debug, Deserialize)]
pub struct ArtistSearch {
    pub artists: Page<Artist>,
}

#[derive(Debug, Deserialize)]
pub struct Playlist {
    pub id: String,
}

impl Track {
    /// The model's view: primary artist, every ID Spotify gave. `None` for
    /// local files, which have no Spotify ID and can't go in a playlist.
    pub fn into_ref(self, album_name: Option<&str>) -> Option<TrackRef> {
        let id = self.id?;
        let primary = self.artists.into_iter().next()?;
        let mut artist_ids = ExternalIds::new();
        if let Some(artist_id) = primary.id {
            artist_ids.insert(Namespace::Spotify, artist_id);
        }
        let mut track = TrackRef::new(
            self.name,
            ArtistRef::named(primary.name).with_ids(artist_ids),
        );
        track.album = self.album.map(|a| a.name).or(album_name.map(str::to_owned));
        track.duration_ms = Some(self.duration_ms);
        track.ids.insert(Namespace::Spotify, id);
        if let Some(isrc) = self.external_ids.isrc {
            track.ids.insert(Namespace::Isrc, isrc);
        }
        Some(track)
    }
}
