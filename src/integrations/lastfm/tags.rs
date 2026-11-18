//! Which Last.fm tags describe the music. Folksonomy tags also describe the
//! listener ("seen live"), where the artist is from ("usa") and who sings
//! ("female vocalists"). None of those is a sound, and a place or a voice
//! spans every genre, so all three are dropped.
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

/// Tags that describe the singer, not the music: the far pool's "female"
/// chart put country, Bollywood and indie pop side by side.
const VOICE_TAGS: &[&str] = &[
    "female",
    "male",
    "female vocalists",
    "female vocalist",
    "male vocalists",
    "male vocalist",
    "female voices",
    "male voices",
    "female singer",
    "male singer",
    "female fronted",
    "female rapper",
    "female rappers",
    "male rapper",
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
    is_musical(tag) && name_key(tag) != artist_key
}

/// Whether a tag can describe music at all, whoever carries it.
pub fn is_musical(tag: &str) -> bool {
    !LISTENER_TAGS.contains(&tag) && !PLACE_TAGS.contains(&tag) && !VOICE_TAGS.contains(&tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_and_listener_notes_are_not_sounds() {
        for tag in [
            "united states",
            "colombian",
            "puerto rico",
            "my top songs",
            "female",
            "female vocalists",
        ] {
            assert!(!describes_music(tag, "kany garcia"), "{tag}");
        }
        for tag in ["latin pop", "west coast hip hop", "neo soul", "j pop"] {
            assert!(describes_music(tag, "kany garcia"), "{tag}");
        }
        assert!(!describes_music("kany garcia", "kany garcia"));
    }
}
