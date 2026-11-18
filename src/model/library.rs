//! What I already know apart from my plays: my playlists, my saved albums, and
//! how much the listening service says I listen to each artist. None of it is
//! taste on its own; all of it says "not new to me".

use std::collections::HashSet;

use jiff::civil::Date;

use super::catalog::{ArtistRef, TrackRef};
use super::ids::{ArtistId, TrackId};
use super::names::title_key;
use super::stored_enum;

/// One of my playlists, as a source lists it.
///
/// Rules for integrations: only playlists I made, never the app's own (the
/// weekly playlist and its snapshots hold recommendations, not songs I know).
#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    pub name: String,
    /// Each track with the day it was added, if the source says.
    pub tracks: Vec<(TrackRef, Option<Date>)>,
}

/// An album as a source names it. Identity is the artist plus the title's
/// key, so a deluxe edition is the album.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRef {
    pub title: String,
    pub artist: ArtistRef,
}

impl AlbumRef {
    pub fn title_key(&self) -> String {
        title_key(&self.title)
    }
}

stored_enum! {
    /// How much I listen to an artist, as a listening service rates it.
    /// Spotify's data export lists hundreds of artists this way, from years
    /// before any history it exports.
    pub enum Familiarity {
        /// Among the artist's heaviest listeners.
        Heavy => "heavy",
        Moderate => "moderate",
        Light => "light",
        /// Used to listen, and stopped: a candidate for rediscovery (spec §13, v4).
        Lapsed => "lapsed",
        /// Listed, under a rating the integration doesn't recognise.
        Unknown => "unknown",
    }
}

/// Tracks and artists I know without a play on file, resolved to store IDs:
/// saved or ranked tracks, my playlists, artists of saved albums, and artists
/// the service rates my listening of. Builds treat them as heard, whatever
/// the history covers.
#[derive(Debug, Clone, Default)]
pub struct Heard {
    pub tracks: HashSet<TrackId>,
    pub artists: HashSet<ArtistId>,
}
