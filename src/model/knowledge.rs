//! What a knowledge integration (Last.fm today) says about artists: who is near
//! whom, how they are described, and which of their tracks people play.

use super::catalog::{ArtistRef, TrackRef};
use super::names::tag_key;

/// An artist similar to another one.
#[derive(Debug, Clone, PartialEq)]
pub struct SimilarArtist {
    pub artist: ArtistRef,
    /// In `[0, 1]`, 1 being the closest. Sources on another scale rescale.
    pub similarity: f64,
}

impl SimilarArtist {
    /// Clamps `similarity` into `[0, 1]`.
    pub fn new(artist: ArtistRef, similarity: f64) -> Self {
        SimilarArtist {
            artist,
            similarity: unit(similarity),
        }
    }
}

/// A descriptive tag on an artist: "dub techno", "baroque", "shoegaze".
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistTag {
    /// Normalized with [`tag_key`].
    pub tag: String,
    /// In `[0, 1]`, relative to the artist's strongest tag.
    pub weight: f64,
}

impl ArtistTag {
    /// Normalizes the tag and clamps `weight` into `[0, 1]`.
    pub fn new(tag: &str, weight: f64) -> Self {
        ArtistTag {
            tag: tag_key(tag),
            weight: unit(weight),
        }
    }
}

/// One of an artist's most played tracks. Sources list these most played first.
#[derive(Debug, Clone, PartialEq)]
pub struct PopularTrack {
    pub track: TrackRef,
    /// Distinct listeners, when the source counts them. Lets a pool pass over
    /// the one hit everybody has already heard.
    pub listeners: Option<u64>,
}

fn unit(x: f64) -> f64 {
    if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) }
}
