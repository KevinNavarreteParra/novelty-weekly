//! Identifiers: the store's own, and the ones outside services use.

use std::collections::BTreeMap;

use super::stored_enum;

/// The store's ID for an artist, assigned the first time any source mentions it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtistId(pub i64);

/// The store's ID for a track: one song by one artist, however many releases
/// or services carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub i64);

stored_enum! {
    /// Which service an external ID belongs to. An integration that brings its
    /// own kind of ID adds a variant here.
    pub enum Namespace {
        /// Spotify's base-62 ID: the last segment of `spotify:track:…` or `spotify:artist:…`.
        Spotify => "spotify",
        /// International Standard Recording Code. Tracks only.
        Isrc => "isrc",
        /// A MusicBrainz artist or recording MBID.
        MusicBrainz => "mbid",
    }
}

impl Namespace {
    /// Puts a value in this namespace's canonical spelling, so the same ID
    /// always compares equal: ISRCs uppercase without hyphens, MBIDs lowercase.
    fn canonical(self, value: &str) -> String {
        let value = value.trim();
        match self {
            Namespace::Spotify => value.to_owned(),
            Namespace::Isrc => value
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_uppercase(),
            Namespace::MusicBrainz => value.to_ascii_lowercase(),
        }
    }
}

/// The external IDs one observation carries, at most one per namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalIds(BTreeMap<Namespace, String>);

impl ExternalIds {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an ID, builder-style. Empty values are ignored.
    pub fn with(mut self, namespace: Namespace, value: impl AsRef<str>) -> Self {
        self.insert(namespace, value);
        self
    }

    /// Adds or replaces an ID. Empty values are ignored.
    pub fn insert(&mut self, namespace: Namespace, value: impl AsRef<str>) {
        let value = namespace.canonical(value.as_ref());
        if !value.is_empty() {
            self.0.insert(namespace, value);
        }
    }

    pub fn get(&self, namespace: Namespace) -> Option<&str> {
        self.0.get(&namespace).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Namespace, &str)> {
        self.0.iter().map(|(ns, v)| (*ns, v.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stored_in_canonical_spelling() {
        let ids = ExternalIds::new()
            .with(Namespace::Isrc, "us-um7-11-00001")
            .with(
                Namespace::MusicBrainz,
                "B10BBBFC-CF9E-42E0-BE17-E2C3E1D2600D",
            );
        assert_eq!(ids.get(Namespace::Isrc), Some("USUM71100001"));
        assert_eq!(
            ids.get(Namespace::MusicBrainz),
            Some("b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d")
        );
    }

    #[test]
    fn empty_values_are_dropped() {
        assert!(ExternalIds::new().with(Namespace::Spotify, "  ").is_empty());
    }
}
