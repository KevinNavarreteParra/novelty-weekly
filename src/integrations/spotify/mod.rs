//! Spotify as the app's [`ListeningService`]: recent listens, the library, the
//! catalog, and the playlist. Every Spotify call in the app is in this module,
//! so the next API change is a one-module fix (spec §14). Also home to the
//! history-export reader.

pub mod auth;
pub mod history;
mod recent;
mod wire;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use log::{info, warn};
use serde::de::DeserializeOwned;
use serde_json::json;

use self::auth::Token;
use super::ListeningService;
use super::cache::{Cache, DAY};
use super::http::{Http, Response};
use crate::model::{ArtistRef, Listen, Namespace, TrackRef};

const API: &str = "https://api.spotify.com/v1";
/// Spotify doesn't publish its limit; this stays well under it.
const REQUESTS_PER_SECOND: f64 = 4.0;
/// Development Mode caps search results at 10.
const SEARCH_LIMIT: &str = "10";
/// Playlist writes take at most 100 URIs per request.
const URIS_PER_WRITE: usize = 100;

pub struct Spotify {
    http: Http,
    cache: Cache,
    client_id: String,
    token_path: PathBuf,
    token: Token,
    access_token: Option<String>,
}

impl Spotify {
    /// Loads the token saved by `novelty auth`. No request is made until the
    /// first call.
    pub fn connect(
        client_id: String,
        token_path: PathBuf,
        cache: Cache,
        user_agent: &str,
    ) -> Result<Spotify> {
        let token = Token::load(&token_path)?;
        let days_left = (token.expires_at().as_second() - Timestamp::now().as_second()) / 86_400;
        if days_left < 14 {
            warn!("the Spotify login expires in {days_left} days; run `novelty auth` to renew it");
        }
        Ok(Spotify {
            http: Http::new("spotify", user_agent, REQUESTS_PER_SECOND),
            cache,
            client_id,
            token_path,
            token,
            access_token: None,
        })
    }

    /// The current access token, refreshing (and saving a rotated refresh
    /// token) when there is none yet.
    fn bearer(&mut self) -> Result<String> {
        if let Some(token) = &self.access_token {
            return Ok(token.clone());
        }
        let before = self.token.refresh_token.clone();
        let access = auth::refresh(&self.http, &self.client_id, &mut self.token)?;
        if self.token.refresh_token != before {
            self.token.save(&self.token_path)?;
        }
        self.access_token = Some(access.clone());
        Ok(access)
    }

    /// A request with the bearer token, refreshing once if it has expired.
    fn authorized(&mut self, send: impl Fn(&Http, &str) -> Result<Response>) -> Result<Response> {
        // The token is fetched first: `self.bearer()` borrows self mutably,
        // which can't overlap the `&self.http` borrow in the call.
        let token = self.bearer()?;
        let response = send(&self.http, &token)?;
        if response.status != 401 {
            return Ok(response);
        }
        self.access_token = None;
        let token = self.bearer()?;
        send(&self.http, &token)
    }

    /// GETs `path`, from the cache when `ttl` is given.
    fn get<T: DeserializeOwned>(
        &mut self,
        path: &str,
        query: &[(&str, &str)],
        ttl: Option<Duration>,
    ) -> Result<T> {
        let url = if path.starts_with("https://") {
            path.to_owned()
        } else {
            format!("{API}{path}")
        };
        let key = cache_key(&url, query);
        let cached = match ttl {
            Some(_) => self.cache.get(&key)?,
            None => None,
        };
        let body = match cached {
            Some(body) => body,
            None => {
                let body = self.fetch(&url, query)?;
                if let Some(ttl) = ttl {
                    self.cache.put(&key, &body, ttl)?;
                }
                body
            }
        };
        serde_json::from_str(&body).with_context(|| format!("parsing spotify {path}"))
    }

    fn fetch(&mut self, url: &str, query: &[(&str, &str)]) -> Result<String> {
        let response = self.authorized(|http, token| http.get(url, query, Some(token)))?;
        if !response.is_success() {
            bail!("spotify GET {url}: {} {}", response.status, response.body);
        }
        Ok(response.body)
    }

    /// Every item of a paged list, following `next` links.
    fn get_all<T: DeserializeOwned>(
        &mut self,
        path: &str,
        query: &[(&str, &str)],
        ttl: Option<Duration>,
    ) -> Result<Vec<T>> {
        let mut page: wire::Page<T> = self.get(path, query, ttl)?;
        let mut items = std::mem::take(&mut page.items);
        while let Some(next) = page.next.take() {
            page = self.get(&next, &[], ttl)?;
            items.append(&mut page.items);
        }
        Ok(items)
    }

    fn send_json(&mut self, method: &str, path: &str, body: serde_json::Value) -> Result<String> {
        let url = format!("{API}{path}");
        let response = self.authorized(|http, token| http.send_json(method, &url, &body, token))?;
        if !response.is_success() {
            bail!(
                "spotify {method} {path}: {} {}",
                response.status,
                response.body
            );
        }
        Ok(response.body)
    }

    /// The artist's Spotify ID: the one on file, else an exact-name search.
    fn artist_id(&mut self, artist: &ArtistRef) -> Result<Option<String>> {
        if let Some(id) = artist.ids.get(Namespace::Spotify) {
            return Ok(Some(id.to_owned()));
        }
        let q = format!("artist:\"{}\"", artist.name.replace('"', ""));
        let found: wire::ArtistSearch = self.get(
            "/search",
            &[("q", &q), ("type", "artist"), ("limit", SEARCH_LIMIT)],
            Some(90 * DAY),
        )?;
        Ok(found
            .artists
            .items
            .into_iter()
            .find(|a| ArtistRef::named(a.name.as_str()).key() == artist.key())
            .and_then(|a| a.id))
    }
}

impl ListeningService for Spotify {
    fn recent_listens(&mut self, after: Option<Timestamp>) -> Result<Vec<Listen>> {
        let after_ms = after.map(|t| t.as_millisecond().to_string());
        let mut query = vec![("limit", "50")];
        if let Some(ms) = &after_ms {
            query.push(("after", ms));
        }
        let page: wire::Page<wire::PlayHistory> =
            self.get("/me/player/recently-played", &query, None)?;
        let mut entries = Vec::new();
        for item in page.items {
            let played_at: Timestamp = item.played_at.parse()?;
            if let Some(track) = item.track.into_ref(None) {
                entries.push((track, played_at));
            }
        }
        Ok(recent::listens_from_feed(entries))
    }

    fn saved_since(&mut self, since: Timestamp) -> Result<Vec<(TrackRef, Timestamp)>> {
        let mut saved = Vec::new();
        let mut offset = 0;
        loop {
            let offset_text = offset.to_string();
            let page: wire::Page<wire::SavedTrack> = self.get(
                "/me/tracks",
                &[("limit", "50"), ("offset", &offset_text)],
                None,
            )?;
            let count = page.items.len();
            for item in page.items {
                let added: Timestamp = item.added_at.parse()?;
                // Newest first, so the first older save ends the scan.
                if added < since {
                    return Ok(saved);
                }
                if let Some(track) = item.track.into_ref(None) {
                    saved.push((track, added));
                }
            }
            if page.next.is_none() || count == 0 {
                return Ok(saved);
            }
            offset += count;
        }
    }

    fn catalog(&mut self, artist: &ArtistRef) -> Result<Vec<TrackRef>> {
        let Some(artist_id) = self.artist_id(artist)? else {
            info!("spotify has no artist named {:?}", artist.name);
            return Ok(Vec::new());
        };
        let albums: Vec<wire::Album> = self.get_all(
            &format!("/artists/{artist_id}/albums"),
            &[("include_groups", "album,single"), ("limit", "50")],
            Some(30 * DAY),
        )?;
        let mut tracks = Vec::new();
        for album in albums {
            let Some(album_id) = album.id else { continue };
            let listing: Vec<wire::Track> = self.get_all(
                &format!("/albums/{album_id}/tracks"),
                &[("limit", "50")],
                Some(30 * DAY),
            )?;
            let by_artist = listing.into_iter().filter(|t| {
                t.artists.first().and_then(|a| a.id.as_deref()) == Some(artist_id.as_str())
            });
            tracks.extend(by_artist.filter_map(|t| t.into_ref(Some(&album.name))));
        }
        Ok(tracks)
    }

    fn find(&mut self, wanted: &TrackRef) -> Result<Option<TrackRef>> {
        let q = match wanted.ids.get(Namespace::Isrc) {
            Some(isrc) => format!("isrc:{isrc}"),
            None => format!(
                "track:\"{}\" artist:\"{}\"",
                wanted.title.replace('"', ""),
                wanted.artist.name.replace('"', "")
            ),
        };
        let found: wire::TrackSearch = self.get(
            "/search",
            &[("q", &q), ("type", "track"), ("limit", SEARCH_LIMIT)],
            Some(90 * DAY),
        )?;
        let matches: Vec<TrackRef> = found
            .tracks
            .items
            .into_iter()
            .filter_map(|t| t.into_ref(None))
            .filter(|t| t.is_same_song_as(wanted))
            .collect();
        // Prefer a studio version unless a live one was asked for.
        let wants_live = is_live(&wanted.title);
        let best = matches
            .iter()
            .position(|t| is_live(&t.title) == wants_live)
            .unwrap_or(0);
        Ok(matches.into_iter().nth(best))
    }

    fn publish(
        &mut self,
        playlist: Option<&str>,
        name: &str,
        description: &str,
        tracks: &[TrackRef],
    ) -> Result<String> {
        let id = match playlist {
            Some(id) => {
                self.send_json(
                    "PUT",
                    &format!("/playlists/{id}"),
                    json!({ "description": description }),
                )?;
                id.to_owned()
            }
            None => {
                let body = json!({ "name": name, "description": description, "public": false });
                let created: wire::Playlist =
                    serde_json::from_str(&self.send_json("POST", "/me/playlists", body)?)?;
                created.id
            }
        };
        let uris: Vec<String> = tracks
            .iter()
            .filter_map(|t| t.ids.get(Namespace::Spotify))
            .map(|id| format!("spotify:track:{id}"))
            .collect();
        if uris.len() < tracks.len() {
            bail!("{} tracks have no Spotify ID", tracks.len() - uris.len());
        }
        let mut chunks = uris.chunks(URIS_PER_WRITE);
        let first = chunks.next().unwrap_or(&[]);
        self.send_json(
            "PUT",
            &format!("/playlists/{id}/items"),
            json!({ "uris": first }),
        )?;
        for chunk in chunks {
            self.send_json(
                "POST",
                &format!("/playlists/{id}/items"),
                json!({ "uris": chunk }),
            )?;
        }
        Ok(id)
    }
}

/// Whether "live" is one of the version markers on the title, rather than a
/// word of the title itself ("Live Forever").
fn is_live(title: &str) -> bool {
    use crate::model::names::{name_key, title_key};
    let lives = |key: String| key.split(' ').filter(|w| *w == "live").count();
    lives(name_key(title)) > lives(title_key(title))
}

fn cache_key(url: &str, query: &[(&str, &str)]) -> String {
    let mut parts: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
    parts.sort();
    format!("spotify:{url}?{}", parts.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_means_a_live_version_not_a_title_word() {
        assert!(is_live("Heroes - Live at Wembley"));
        assert!(is_live("Heroes (Live)"));
        assert!(!is_live("Live Forever"));
        assert!(!is_live("Live Forever - Remastered"));
        assert!(!is_live("Heroes"));
    }

    #[test]
    fn a_full_track_maps_to_the_model_with_every_id() {
        let json = r#"{"id":"t1","name":"Song","duration_ms":201000,
            "artists":[{"id":"a1","name":"Artist"},{"id":"a2","name":"Guest"}],
            "album":{"id":"al1","name":"Album"},"external_ids":{"isrc":"usabc1234567"}}"#;
        let track: wire::Track = serde_json::from_str(json).unwrap();
        let r = track.into_ref(None).unwrap();
        assert_eq!(r.artist.name, "Artist");
        assert_eq!(r.artist.ids.get(Namespace::Spotify), Some("a1"));
        assert_eq!(r.ids.get(Namespace::Isrc), Some("USABC1234567"));
        assert_eq!(r.album.as_deref(), Some("Album"));
        assert_eq!(r.duration_ms, Some(201_000));
    }

    #[test]
    fn local_files_are_dropped() {
        let json =
            r#"{"id":null,"name":"Rip","duration_ms":1,"artists":[{"id":null,"name":"Me"}]}"#;
        let track: wire::Track = serde_json::from_str(json).unwrap();
        assert!(track.into_ref(None).is_none());
    }
}
