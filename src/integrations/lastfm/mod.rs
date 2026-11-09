//! Last.fm as the app's [`Knowledge`] source (spec §5.4): similar artists,
//! tags, top tracks, listener counts and tag charts. Read-only methods that
//! need only an API key.

mod wire;

use std::ops::Range;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use log::warn;
use serde::de::DeserializeOwned;

use super::Knowledge;
use super::cache::{Cache, DAY};
use super::http::Http;
use crate::model::{
    ArtistRef, ArtistTag, ExternalIds, Namespace, PopularTrack, SimilarArtist, TrackRef,
};

const API: &str = "https://ws.audioscrobbler.com/2.0/";
/// Spec §5.4 asks for at most 4 requests a second.
const REQUESTS_PER_SECOND: f64 = 4.0;
const CHART_PAGE: usize = 50;

/// Last.fm error codes the adapter treats specially.
const ERROR_NOT_FOUND: u32 = 6;
const ERROR_RATE_LIMITED: u32 = 29;

/// Folksonomy tags that describe the listener, not the music.
const NOISE_TAGS: &[&str] = &[
    "seen live",
    "favorites",
    "favourites",
    "favorite",
    "favourite",
    "my favorite",
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

pub struct LastFm {
    http: Http,
    cache: Cache,
    api_key: String,
}

impl LastFm {
    pub fn new(api_key: String, cache: Cache, user_agent: &str) -> LastFm {
        LastFm {
            http: Http::new("last.fm", user_agent, REQUESTS_PER_SECOND),
            cache,
            api_key,
        }
    }

    /// Calls `method`, through the cache. `Ok(None)` when Last.fm doesn't
    /// know the artist or tag.
    fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &[(&str, String)],
        ttl: Duration,
    ) -> Result<Option<T>> {
        let key = cache_key(method, params);
        let body = self
            .cache
            .get_or_fetch(&key, ttl, || self.fetch(method, params))?;
        if let Ok(error) = serde_json::from_str::<wire::ApiError>(&body) {
            if error.error == ERROR_NOT_FOUND {
                return Ok(None);
            }
            bail!("last.fm {method}: error {}: {}", error.error, error.message);
        }
        let parsed =
            serde_json::from_str(&body).with_context(|| format!("parsing last.fm {method}"))?;
        Ok(Some(parsed))
    }

    /// One live request. A rate-limit error is retried a few times; a
    /// not-found error is returned as a body so it gets cached like any answer.
    fn fetch(&self, method: &str, params: &[(&str, String)]) -> Result<String> {
        let mut query: Vec<(&str, &str)> = vec![
            ("method", method),
            ("api_key", &self.api_key),
            ("format", "json"),
        ];
        query.extend(params.iter().map(|(k, v)| (*k, v.as_str())));

        for attempt in 1..=3 {
            let response = self.http.get(API, &query, None)?;
            match serde_json::from_str::<wire::ApiError>(&response.body) {
                Ok(e) if e.error == ERROR_RATE_LIMITED && attempt < 3 => {
                    warn!("last.fm rate limit; backing off");
                    std::thread::sleep(Duration::from_secs(5 * attempt));
                }
                Ok(e) if e.error == ERROR_NOT_FOUND => return Ok(response.body),
                Ok(e) => bail!("last.fm {method}: error {}: {}", e.error, e.message),
                Err(_) if response.is_success() => return Ok(response.body),
                Err(_) => bail!("last.fm {method}: HTTP {}", response.status),
            }
        }
        bail!("last.fm {method}: still rate limited")
    }
}

impl Knowledge for LastFm {
    fn source(&self) -> &'static str {
        "lastfm"
    }

    fn similar_artists(&self, artist: &ArtistRef) -> Result<Vec<SimilarArtist>> {
        let mut params = artist_params(artist);
        params.push(("limit", "100".into()));
        let found: Option<wire::SimilarResponse> =
            self.call("artist.getSimilar", &params, 60 * DAY)?;
        Ok(found
            .map(|r| r.similarartists.artist)
            .unwrap_or_default()
            .into_iter()
            .map(|a| SimilarArtist::new(artist_ref(a.name, a.mbid), a.similarity))
            .collect())
    }

    fn artist_tags(&self, artist: &ArtistRef) -> Result<Vec<ArtistTag>> {
        let found: Option<wire::TopTagsResponse> =
            self.call("artist.getTopTags", &artist_params(artist), 60 * DAY)?;
        let artist_key = artist.key();
        Ok(found
            .map(|r| r.toptags.tag)
            .unwrap_or_default()
            .into_iter()
            .map(|t| ArtistTag::new(&t.name, t.count / 100.0))
            .filter(|t| t.weight > 0.0 && !is_noise(&t.tag, &artist_key))
            .collect())
    }

    fn popular_tracks(&self, artist: &ArtistRef) -> Result<Vec<PopularTrack>> {
        let mut params = artist_params(artist);
        params.push(("limit", "50".into()));
        let found: Option<wire::TopTracksResponse> =
            self.call("artist.getTopTracks", &params, 30 * DAY)?;
        Ok(found
            .map(|r| r.toptracks.track)
            .unwrap_or_default()
            .into_iter()
            .map(|t| {
                let mut track = TrackRef::new(t.name, artist_ref(t.artist.name, t.artist.mbid));
                if let Some(mbid) = t.mbid {
                    track.ids.insert(Namespace::MusicBrainz, mbid);
                }
                PopularTrack {
                    track,
                    listeners: t.listeners,
                }
            })
            .collect())
    }

    fn listeners(&self, artist: &ArtistRef) -> Result<Option<u64>> {
        let found: Option<wire::InfoResponse> =
            self.call("artist.getInfo", &artist_params(artist), 30 * DAY)?;
        Ok(found.and_then(|r| r.artist.stats.listeners))
    }

    fn tag_chart(&self, tag: &str, ranks: Range<usize>) -> Result<Vec<ArtistRef>> {
        let mut chart = Vec::new();
        let first_page = ranks.start / CHART_PAGE + 1;
        let last_page = ranks.end.saturating_sub(1) / CHART_PAGE + 1;
        for page in first_page..=last_page {
            let params = [
                ("tag", tag.to_owned()),
                ("page", page.to_string()),
                ("limit", CHART_PAGE.to_string()),
            ];
            let found: Option<wire::TagArtistsResponse> =
                self.call("tag.getTopArtists", &params, 30 * DAY)?;
            let artists = found.map(|r| r.topartists.artist).unwrap_or_default();
            let page_was_full = artists.len() == CHART_PAGE;
            chart.extend(artists.into_iter().map(|a| artist_ref(a.name, a.mbid)));
            if !page_was_full {
                break;
            }
        }
        let start = (ranks.start % CHART_PAGE).min(chart.len());
        chart.truncate(ranks.len() + start);
        Ok(chart.split_off(start))
    }
}

/// Look an artist up by MBID when known (unambiguous), else by name with
/// Last.fm's spelling autocorrection.
fn artist_params(artist: &ArtistRef) -> Vec<(&'static str, String)> {
    match artist.ids.get(Namespace::MusicBrainz) {
        Some(mbid) => vec![("mbid", mbid.to_owned())],
        None => vec![("artist", artist.name.clone()), ("autocorrect", "1".into())],
    }
}

fn artist_ref(name: String, mbid: Option<String>) -> ArtistRef {
    let mut ids = ExternalIds::new();
    if let Some(mbid) = mbid {
        ids.insert(Namespace::MusicBrainz, mbid);
    }
    ArtistRef::named(name).with_ids(ids)
}

fn is_noise(tag: &str, artist_key: &str) -> bool {
    NOISE_TAGS.contains(&tag) || crate::model::names::name_key(tag) == artist_key
}

/// The method and its parameters, sorted; never the API key.
fn cache_key(method: &str, params: &[(&str, String)]) -> String {
    let mut parts: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
    parts.sort();
    format!("lastfm:{method}?{}", parts.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_similar_artists_with_string_numbers() {
        let body = r#"{"similarartists":{"artist":[
            {"name":"Beach House","mbid":"d5cc67b8-1cc4-453b-96e8-44487acdebea","match":"1","url":"u"},
            {"name":"Low","mbid":"","match":"0.524"}
        ],"@attr":{"artist":"Mazzy Star"}}}"#;
        let parsed: wire::SimilarResponse = serde_json::from_str(body).unwrap();
        let artists = parsed.similarartists.artist;
        assert_eq!(artists.len(), 2);
        assert_eq!(artists[1].similarity, 0.524);
    }

    #[test]
    fn a_single_result_parses_as_a_list() {
        let body = r#"{"toptags":{"tag":{"name":"dream pop","count":100}}}"#;
        let parsed: wire::TopTagsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.toptags.tag.len(), 1);
    }

    #[test]
    fn missing_or_empty_listener_counts_are_none() {
        let body = r#"{"toptracks":{"track":[
            {"name":"Fade Into You","listeners":"1234567","artist":{"name":"Mazzy Star"}},
            {"name":"Rare","listeners":"","artist":{"name":"Mazzy Star"}},
            {"name":"Rarer","artist":{"name":"Mazzy Star"}}
        ]}}"#;
        let parsed: wire::TopTracksResponse = serde_json::from_str(body).unwrap();
        let listeners: Vec<Option<u64>> =
            parsed.toptracks.track.iter().map(|t| t.listeners).collect();
        assert_eq!(listeners, vec![Some(1_234_567), None, None]);
    }

    #[test]
    fn noise_tags_and_the_artists_own_name_are_dropped() {
        assert!(is_noise("seen live", "mazzy star"));
        assert!(is_noise("mazzy star", "mazzy star"));
        assert!(!is_noise("dream pop", "mazzy star"));
    }

    #[test]
    fn cache_keys_ignore_parameter_order() {
        let a = cache_key("m", &[("b", "2".into()), ("a", "1".into())]);
        let b = cache_key("m", &[("a", "1".into()), ("b", "2".into())]);
        assert_eq!(a, b);
        assert_eq!(a, "lastfm:m?a=1&b=2");
    }

    #[test]
    fn empty_mbids_are_not_recorded() {
        let artist = artist_ref("Low".into(), Some(String::new()));
        assert!(artist.ids.is_empty());
    }
}
