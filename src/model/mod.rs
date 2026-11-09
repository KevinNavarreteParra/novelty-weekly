//! The canonical data contract: every type the app reasons about.
//!
//! It has two halves.
//!
//! **Observations** come in from integrations and carry no store IDs: a
//! [`Listen`] of a [`TrackRef`] by an [`ArtistRef`], an artist's
//! [`SimilarArtist`]s, [`ArtistTag`]s and [`PopularTrack`]s. They are identified
//! the one way every source can manage, by normalized name ([`names`]), plus
//! whatever [`ExternalIds`] the source happens to know.
//!
//! **Resolved records** come out of the store after it matched observations to
//! its own [`ArtistId`]s and [`TrackId`]s; a [`Play`] is a resolved [`Listen`].
//! The engine only ever sees resolved records.
//!
//! The model also names what the app decides: a [`Recommendation`] from a
//! [`Pool`], credited to an [`Arm`], in one [`Week`]'s playlist.
//!
//! Adding an integration means producing observations that follow the rules
//! written on each type. Nothing downstream changes.

mod catalog;
mod ids;
mod knowledge;
mod listen;
pub mod names;
mod recommendation;

pub use catalog::{ArtistRef, TrackRef};
pub use ids::{ArtistId, ExternalIds, Namespace, TrackId};
pub use knowledge::{
    ArtistTag, ArtistTags, PopularTrack, PopularTracks, SimilarArtist, SimilarityGraph,
};
pub use listen::{
    COMPLETE_AFTER_MS, Ending, Listen, Play, Provenance, SKIP_BEFORE_MS, StartedBy, Verdict, judge,
};
pub use recommendation::{Arm, Pool, Recommendation, Week};

/// Declares a fieldless enum whose variants are stored as fixed strings, with
/// `as_str`, `parse` and `Display`. Keeps each variant's stored name next to it.
macro_rules! stored_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $text:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The name stored in the database.
            pub fn as_str(self) -> &'static str {
                match self {
                    $( $name::$variant => $text, )+
                }
            }

            pub fn parse(s: &str) -> Option<$name> {
                match s {
                    $( $text => Some($name::$variant), )+
                    _ => None,
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
use stored_enum;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_names_round_trip() {
        for e in Ending::ALL {
            assert_eq!(Ending::parse(e.as_str()), Some(*e));
        }
        for s in StartedBy::ALL {
            assert_eq!(StartedBy::parse(s.as_str()), Some(*s));
        }
        for p in Provenance::ALL {
            assert_eq!(Provenance::parse(p.as_str()), Some(*p));
        }
        for n in Namespace::ALL {
            assert_eq!(Namespace::parse(n.as_str()), Some(*n));
        }
    }
}
