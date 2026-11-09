//! The Last.fm adapter against the live API. Ignored by default because it
//! needs the network and a key:
//!
//! ```sh
//! prun cargo test --test lastfm_live -- --ignored
//! ```

use novelty::integrations::Knowledge;
use novelty::integrations::cache::Cache;
use novelty::integrations::http::USER_AGENT;
use novelty::integrations::lastfm::LastFm;
use novelty::model::{ArtistRef, ExternalIds, Namespace};

fn lastfm() -> LastFm {
    let key = std::env::var("LASTFM_API_KEY").expect("LASTFM_API_KEY is not set");
    LastFm::new(key, Cache::open_in_memory().unwrap(), USER_AGENT)
}

fn radiohead() -> ArtistRef {
    ArtistRef::named("Radiohead")
}

#[test]
#[ignore = "calls the live Last.fm API"]
fn similar_artists_come_back_scored() {
    let similar = lastfm().similar_artists(&radiohead()).unwrap();
    assert!(
        similar.len() >= 50,
        "only {} similar artists",
        similar.len()
    );
    assert!(
        similar
            .iter()
            .all(|s| s.similarity > 0.0 && s.similarity <= 1.0)
    );
    assert!(similar.iter().all(|s| s.artist.key() != "radiohead"));
}

#[test]
#[ignore = "calls the live Last.fm API"]
fn tags_drop_noise_and_the_artists_own_name() {
    let tags = lastfm().artist_tags(&radiohead()).unwrap();
    assert!(!tags.is_empty());
    let names: Vec<&str> = tags.iter().map(|t| t.tag.as_str()).collect();
    assert!(!names.contains(&"seen live"), "{names:?}");
    assert!(!names.contains(&"radiohead"), "{names:?}");
    assert!(
        names
            .iter()
            .any(|t| t.contains("rock") || t.contains("alternative")),
        "{names:?}"
    );
}

#[test]
#[ignore = "calls the live Last.fm API"]
fn popular_tracks_and_listeners() {
    let lastfm = lastfm();
    let tracks = lastfm.popular_tracks(&radiohead()).unwrap();
    assert!(tracks.len() >= 10);
    assert!(tracks.iter().all(|t| t.track.artist.key() == "radiohead"));
    assert!(tracks.iter().any(|t| t.listeners.unwrap_or(0) > 100_000));
    let listeners = lastfm.listeners(&radiohead()).unwrap().unwrap();
    assert!(listeners > 1_000_000, "{listeners}");
}

#[test]
#[ignore = "calls the live Last.fm API"]
fn a_musicbrainz_id_finds_the_same_artist() {
    let by_mbid = ArtistRef::named("whatever").with_ids(ExternalIds::new().with(
        Namespace::MusicBrainz,
        "a74b1b7f-71a5-4011-9441-d0b5e4122711",
    ));
    let lastfm = lastfm();
    let listeners = lastfm.listeners(&by_mbid).unwrap().unwrap();
    assert!(listeners > 1_000_000, "{listeners}");
}

#[test]
#[ignore = "calls the live Last.fm API"]
fn tag_charts_page_past_the_head() {
    let chart = lastfm().tag_chart("shoegaze", 20..90).unwrap();
    assert_eq!(chart.len(), 70);
    let mut keys: Vec<String> = chart.iter().map(|a| a.key()).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 70, "a chart repeated an artist");
}

#[test]
#[ignore = "calls the live Last.fm API"]
fn an_unknown_artist_is_empty_not_an_error() {
    let nobody = ArtistRef::named("zzqx no such artist 41871");
    let lastfm = lastfm();
    assert!(lastfm.similar_artists(&nobody).unwrap().is_empty());
    assert!(lastfm.artist_tags(&nobody).unwrap().is_empty());
    assert_eq!(lastfm.listeners(&nobody).unwrap(), None);
}
