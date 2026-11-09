//! Last.fm's JSON, exactly as it arrives. Private to the adapter.
//!
//! Two quirks of its XML-to-JSON conversion are handled here: numbers come
//! as strings (`"match": "0.83"`), and a list with one element comes as a
//! bare object instead of an array.

use std::str::FromStr;

use serde::{Deserialize, Deserializer};

/// An API error, which Last.fm can return with any HTTP status.
#[derive(Debug, Deserialize)]
pub struct ApiError {
    pub error: u32,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct SimilarResponse {
    pub similarartists: SimilarArtists,
}

#[derive(Debug, Deserialize)]
pub struct SimilarArtists {
    #[serde(default, deserialize_with = "one_or_many")]
    pub artist: Vec<SimilarArtist>,
}

#[derive(Debug, Deserialize)]
pub struct SimilarArtist {
    pub name: String,
    #[serde(default)]
    pub mbid: Option<String>,
    #[serde(rename = "match", deserialize_with = "number")]
    pub similarity: f64,
}

#[derive(Debug, Deserialize)]
pub struct TopTagsResponse {
    pub toptags: TopTags,
}

#[derive(Debug, Deserialize)]
pub struct TopTags {
    #[serde(default, deserialize_with = "one_or_many")]
    pub tag: Vec<Tag>,
}

#[derive(Debug, Deserialize)]
pub struct Tag {
    pub name: String,
    /// 0 to 100, relative to the artist's strongest tag.
    #[serde(deserialize_with = "number")]
    pub count: f64,
}

#[derive(Debug, Deserialize)]
pub struct TopTracksResponse {
    pub toptracks: TopTracks,
}

#[derive(Debug, Deserialize)]
pub struct TopTracks {
    #[serde(default, deserialize_with = "one_or_many")]
    pub track: Vec<TopTrack>,
}

#[derive(Debug, Deserialize)]
pub struct TopTrack {
    pub name: String,
    #[serde(default)]
    pub mbid: Option<String>,
    #[serde(default, deserialize_with = "optional_number")]
    pub listeners: Option<u64>,
    pub artist: ArtistName,
}

#[derive(Debug, Deserialize)]
pub struct ArtistName {
    pub name: String,
    #[serde(default)]
    pub mbid: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct InfoResponse {
    pub artist: Info,
}

#[derive(Debug, Deserialize)]
pub struct Info {
    pub stats: Stats,
}

#[derive(Debug, Deserialize)]
pub struct Stats {
    #[serde(default, deserialize_with = "optional_number")]
    pub listeners: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct TagArtistsResponse {
    pub topartists: TagArtists,
}

#[derive(Debug, Deserialize)]
pub struct TagArtists {
    #[serde(default, deserialize_with = "one_or_many")]
    pub artist: Vec<ArtistName>,
}

fn one_or_many<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany<T> {
        Many(Vec<T>),
        One(T),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::Many(items) => items,
        OneOrMany::One(item) => vec![item],
    })
}

#[derive(Deserialize)]
#[serde(untagged)]
enum NumberOrString {
    Number(serde_json::Number),
    String(String),
}

fn number<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
{
    let text = match NumberOrString::deserialize(deserializer)? {
        NumberOrString::Number(n) => n.to_string(),
        NumberOrString::String(s) => s,
    };
    text.trim()
        .parse()
        .map_err(|_| serde::de::Error::custom(format!("not a number: {text:?}")))
}

/// Like `number`, but an empty string or a missing field is `None`.
fn optional_number<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
{
    let text = match Option::<NumberOrString>::deserialize(deserializer)? {
        None => return Ok(None),
        Some(NumberOrString::Number(n)) => n.to_string(),
        Some(NumberOrString::String(s)) => s,
    };
    Ok(text.trim().parse().ok())
}
