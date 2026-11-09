//! Which Last.fm tags describe the music. Folksonomy tags also describe the
//! listener ("seen live") and where the artist is from ("usa"); neither is a
//! sound, and a place spans every genre, so both are dropped.
//!
//! Lists are in `model::names::tag_key` form.

use crate::model::names::name_key;

/// Tags that describe the listener, not the music.
const LISTENER_TAGS: &[&str] = &[
    "seen live",
    "want to see live",
    "favorites",
    "favourites",
    "favorite",
    "favourite",
    "my favorite",
    "my top songs",
    "guilty pleasure",
    "awesome",
    "love",
    "beautiful",
    "amazing",
    "albums i own",
    "spotify",
    "under 2000 listeners",
    "all",
    "check out",
    "good",
    "cool",
    "best",
];

/// Countries, nationalities, regions and cities.
const PLACE_TAGS: &[&str] = &[
    // North America.
    "usa",
    "us",
    "united states",
    "america",
    "american",
    "canada",
    "canadian",
    "mexico",
    "mexican",
    "california",
    "texas",
    "new york",
    "nyc",
    "los angeles",
    "chicago",
    "atlanta",
    "seattle",
    "detroit",
    "houston",
    "memphis",
    "new orleans",
    "nashville",
    "philadelphia",
    "miami",
    "toronto",
    "montreal",
    // Latin America and the Caribbean.
    "puerto rico",
    "puerto rican",
    "colombia",
    "colombian",
    "argentina",
    "argentinian",
    "argentine",
    "chile",
    "chilean",
    "peru",
    "peruvian",
    "venezuela",
    "venezuelan",
    "cuba",
    "cuban",
    "dominican",
    "dominican republic",
    "brazil",
    "brazilian",
    "brasil",
    "uruguay",
    "uruguayan",
    "ecuador",
    "ecuadorian",
    "jamaica",
    "jamaican",
    "latin america",
    // Europe.
    "uk",
    "united kingdom",
    "british",
    "england",
    "english",
    "scotland",
    "scottish",
    "wales",
    "welsh",
    "ireland",
    "irish",
    "london",
    "manchester",
    "france",
    "french",
    "paris",
    "spain",
    "spanish",
    "germany",
    "german",
    "berlin",
    "italy",
    "italian",
    "portugal",
    "portuguese",
    "netherlands",
    "dutch",
    "belgium",
    "belgian",
    "sweden",
    "swedish",
    "norway",
    "norwegian",
    "denmark",
    "danish",
    "finland",
    "finnish",
    "iceland",
    "icelandic",
    "poland",
    "polish",
    "russia",
    "russian",
    "ukraine",
    "ukrainian",
    "greece",
    "greek",
    "europe",
    "european",
    // Asia, Oceania and Africa.
    "japan",
    "japanese",
    "tokyo",
    "korea",
    "korean",
    "south korea",
    "china",
    "chinese",
    "taiwan",
    "taiwanese",
    "india",
    "indian",
    "australia",
    "australian",
    "new zealand",
    "nigeria",
    "nigerian",
    "south africa",
    "south african",
    "ghana",
    "ghanaian",
    "africa",
    "african",
];

/// Whether a tag says something about the artist's music. `artist_key` is
/// the artist's `name_key`: an artist tagged with its own name says nothing.
pub fn describes_music(tag: &str, artist_key: &str) -> bool {
    !LISTENER_TAGS.contains(&tag) && !PLACE_TAGS.contains(&tag) && name_key(tag) != artist_key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_and_listener_notes_are_not_sounds() {
        for tag in ["united states", "colombian", "puerto rico", "my top songs"] {
            assert!(!describes_music(tag, "kany garcia"), "{tag}");
        }
        for tag in ["latin pop", "west coast hip hop", "neo soul", "j pop"] {
            assert!(describes_music(tag, "kany garcia"), "{tag}");
        }
        assert!(!describes_music("kany garcia", "kany garcia"));
    }
}
