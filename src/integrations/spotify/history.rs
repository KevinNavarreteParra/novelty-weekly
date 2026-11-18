//! Spotify's data exports (spec §5.1), read as model observations.
//!
//! Two packages, requested separately, that can be read from one directory:
//!
//! - **Extended streaming history**: `Streaming_History_Audio_*.json`, every
//!   stream since the account opened, with endings and track URIs.
//! - **Account data**: `StreamingHistory_music_*.json`, only the last year's
//!   streams, by name, with end times to the minute; plus the playlists I
//!   made (`Playlist*.json`), my library (`YourLibrary.json`) and how much
//!   Spotify says I listen to each artist (`Marquee.json`).
//!
//! An archive is read in memory and never extracted, so nothing in it lands
//! on disk. Files the app doesn't use are ignored.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

use crate::model::{
    AlbumRef, ArtistRef, Ending, ExternalIds, Familiarity, Listen, Namespace, Playlist, Provenance,
    StartedBy, TrackRef,
};

/// Largest JSON file accepted from an export. Real ones are a few MB; the cap
/// keeps a hostile archive from exhausting memory.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// What the app reads from an export. A part is `None` when the export has no
/// file for it, so importing it leaves what's on file alone.
#[derive(Debug, Default)]
pub struct Export {
    /// Music listens from either history, oldest first.
    pub listens: Vec<Listen>,
    pub playlists: Option<Vec<Playlist>>,
    pub saved_albums: Option<Vec<AlbumRef>>,
    /// Artists Spotify rates my listening of, by how much.
    pub familiarity: Option<Vec<(ArtistRef, Familiarity)>>,
}

/// The kinds of file an export holds that the app reads.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Part {
    ExtendedHistory,
    AccountHistory,
    Playlists,
    Library,
    Marquee,
}

impl Part {
    fn of(file_name: &str) -> Option<Part> {
        let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
        let json = |prefix: &str| base.starts_with(prefix) && base.ends_with(".json");
        if json("Streaming_History_Audio_") {
            Some(Part::ExtendedHistory)
        } else if json("StreamingHistory_music_") {
            Some(Part::AccountHistory)
        } else if json("Playlist") {
            Some(Part::Playlists)
        } else if base == "YourLibrary.json" {
            Some(Part::Library)
        } else if base == "Marquee.json" {
            Some(Part::Marquee)
        } else {
            None
        }
    }
}

impl Export {
    fn add(&mut self, part: Part, json: &str) -> Result<()> {
        match part {
            Part::ExtendedHistory => self.listens.extend(parse_extended(json)?),
            Part::AccountHistory => self.listens.extend(parse_account(json)?),
            Part::Playlists => self
                .playlists
                .get_or_insert_with(Vec::new)
                .extend(parse_playlists(json)?),
            Part::Library => self.saved_albums = Some(parse_library(json)?),
            Part::Marquee => self.familiarity = Some(parse_marquee(json)?),
        }
        Ok(())
    }
}

/// Everything the app reads from an export. `path` is the `.zip`, or a
/// directory it was extracted into.
pub fn read_export(path: &Path) -> Result<Export> {
    let mut export = Export::default();
    let files = if path.is_dir() {
        read_dir(path, &mut export)?
    } else {
        read_zip(path, &mut export)?
    };
    if files == 0 {
        bail!(
            "no Spotify export files in {} (looked for Streaming_History_Audio_*.json, \
             StreamingHistory_music_*.json, Playlist*.json, YourLibrary.json, Marquee.json)",
            path.display()
        );
    }
    export.listens.sort_by_key(|l| l.started_at);
    Ok(export)
}

fn read_zip(path: &Path, export: &mut Export) -> Result<usize> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("reading {} as a zip", path.display()))?;
    let mut files = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name()?.into_owned();
        let Some(part) = Part::of(&name).filter(|_| entry.is_file()) else {
            continue;
        };
        if entry.size() > MAX_FILE_BYTES {
            bail!("{name} is larger than {MAX_FILE_BYTES} bytes");
        }
        let mut json = String::new();
        // `take` holds even if the archive lies about the entry's size.
        entry
            .by_ref()
            .take(MAX_FILE_BYTES)
            .read_to_string(&mut json)?;
        export
            .add(part, &json)
            .with_context(|| format!("parsing {name}"))?;
        files += 1;
    }
    Ok(files)
}

fn read_dir(dir: &Path, export: &mut Export) -> Result<usize> {
    let mut files = 0;
    for entry in fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let Some(part) = Part::of(name).filter(|_| path.is_file()) else {
            continue;
        };
        if fs::metadata(&path)?.len() > MAX_FILE_BYTES {
            bail!("{} is larger than {MAX_FILE_BYTES} bytes", path.display());
        }
        let json =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        export
            .add(part, &json)
            .with_context(|| format!("parsing {}", path.display()))?;
        files += 1;
    }
    Ok(files)
}

/// Parses one extended-history file. Podcast and audiobook rows are dropped.
pub fn parse_extended(json: &str) -> Result<Vec<Listen>> {
    let rows: Vec<Row> = serde_json::from_str(json)?;
    let mut listens = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(listen) = row.into_listen()? {
            listens.push(listen);
        }
    }
    Ok(listens)
}

/// Parses one account-data history file (`StreamingHistory_music_*.json`).
///
/// Rows named "Unknown Artist" / "Unknown Track" are dropped: they are not
/// music. In a real export they were 15% of rows, every one under 30 s, most
/// likely the DJ's voice between songs.
pub fn parse_account(json: &str) -> Result<Vec<Listen>> {
    let rows: Vec<AccountRow> = serde_json::from_str(json)?;
    let mut listens = Vec::with_capacity(rows.len());
    for row in rows {
        if row.artist_name.trim().is_empty()
            || row.track_name.trim().is_empty()
            || (row.artist_name == "Unknown Artist" && row.track_name == "Unknown Track")
        {
            continue;
        }
        // UTC, to the minute: checked against polled listens of the same days.
        let ended = DateTime::strptime("%Y-%m-%d %H:%M", &row.end_time)
            .with_context(|| format!("bad endTime {:?}", row.end_time))?
            .to_zoned(TimeZone::UTC)?
            .timestamp();
        listens.push(Listen {
            track: TrackRef::new(row.track_name, ArtistRef::named(row.artist_name)),
            started_at: ended.checked_sub(SignedDuration::from_millis(row.ms_played.into()))?,
            ms_played: Some(row.ms_played),
            ending: Ending::Unknown,
            started_by: StartedBy::Unknown,
            provenance: Provenance::BasicHistory,
        });
    }
    Ok(listens)
}

/// One stream in the account data: names, the minute it ended, and how long.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountRow {
    /// `"2025-11-12 00:41"`, UTC.
    end_time: String,
    artist_name: String,
    track_name: String,
    ms_played: u32,
}

/// The playlists in one `Playlist*.json`. Episodes and local files are
/// dropped; a playlist left with no tracks is too.
pub fn parse_playlists(json: &str) -> Result<Vec<Playlist>> {
    #[derive(Deserialize)]
    struct File {
        playlists: Vec<PlaylistRow>,
    }
    #[derive(Deserialize)]
    struct PlaylistRow {
        name: String,
        #[serde(default)]
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Item {
        track: Option<ItemTrack>,
        /// `"2026-11-09"`.
        added_date: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ItemTrack {
        track_name: String,
        artist_name: String,
        album_name: Option<String>,
        /// `spotify:track:<id>`; local files have none.
        track_uri: Option<String>,
    }

    let file: File = serde_json::from_str(json)?;
    let mut playlists = Vec::new();
    for row in file.playlists {
        let mut tracks = Vec::new();
        for item in row.items {
            let Some(t) = item.track else { continue };
            let Some(id) = t
                .track_uri
                .as_deref()
                .and_then(|u| u.strip_prefix("spotify:track:"))
            else {
                continue;
            };
            let mut track = TrackRef::new(t.track_name, ArtistRef::named(t.artist_name));
            track.album = t.album_name;
            track.ids = ExternalIds::new().with(Namespace::Spotify, id);
            let added = item.added_date.and_then(|d| d.parse::<Date>().ok());
            tracks.push((track, added));
        }
        if !tracks.is_empty() {
            playlists.push(Playlist {
                name: row.name,
                tracks,
            });
        }
    }
    Ok(playlists)
}

/// The saved albums in `YourLibrary.json`. Its saved tracks are left out:
/// the API's copy carries when each was saved, and `cold-start` and every
/// build read that.
pub fn parse_library(json: &str) -> Result<Vec<AlbumRef>> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        albums: Vec<AlbumRow>,
    }
    #[derive(Deserialize)]
    struct AlbumRow {
        artist: String,
        album: String,
    }
    let file: File = serde_json::from_str(json)?;
    Ok(file
        .albums
        .into_iter()
        .map(|a| AlbumRef {
            title: a.album,
            artist: ArtistRef::named(a.artist),
        })
        .collect())
}

/// `Marquee.json`: Spotify's segment for me, per artist, as used to target
/// its artist campaigns.
pub fn parse_marquee(json: &str) -> Result<Vec<(ArtistRef, Familiarity)>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Row {
        artist_name: String,
        segment: String,
    }
    let rows: Vec<Row> = serde_json::from_str(json)?;
    Ok(rows
        .into_iter()
        .map(|r| (ArtistRef::named(r.artist_name), familiarity(&r.segment)))
        .collect())
}

/// Maps a Marquee segment. Values seen in a real export: "Super Listeners",
/// "Moderate listeners", "Light listeners", "Previously Active Listeners".
fn familiarity(segment: &str) -> Familiarity {
    match segment.to_lowercase().as_str() {
        "super listeners" => Familiarity::Heavy,
        "moderate listeners" => Familiarity::Moderate,
        "light listeners" => Familiarity::Light,
        "previously active listeners" => Familiarity::Lapsed,
        _ => Familiarity::Unknown,
    }
}

/// One stream as the export writes it. Fields not listed (IP address,
/// platform, country, podcast fields...) are ignored.
#[derive(Debug, Deserialize)]
struct Row {
    /// When the stream ended, UTC, e.g. `"2024-03-01T18:22:05Z"`.
    ts: String,
    ms_played: u32,
    master_metadata_track_name: Option<String>,
    /// Despite the name, the track's artist.
    master_metadata_album_artist_name: Option<String>,
    master_metadata_album_album_name: Option<String>,
    /// `spotify:track:<id>`; null for podcasts and audiobooks.
    spotify_track_uri: Option<String>,
    reason_start: Option<String>,
    reason_end: Option<String>,
    /// Unreliable: null or false on many `fwdbtn` endings.
    skipped: Option<bool>,
}

impl Row {
    fn into_listen(self) -> Result<Option<Listen>> {
        let (Some(uri), Some(title), Some(artist)) = (
            self.spotify_track_uri,
            self.master_metadata_track_name,
            self.master_metadata_album_artist_name,
        ) else {
            return Ok(None);
        };
        let Some(spotify_id) = uri.strip_prefix("spotify:track:") else {
            return Ok(None);
        };

        let ended: Timestamp = self
            .ts
            .parse()
            .with_context(|| format!("bad ts {:?}", self.ts))?;
        let started_at = ended.checked_sub(SignedDuration::from_millis(self.ms_played.into()))?;

        let mut track = TrackRef::new(title, ArtistRef::named(artist));
        track.album = self.master_metadata_album_album_name;
        track.ids = ExternalIds::new().with(Namespace::Spotify, spotify_id);

        Ok(Some(Listen {
            track,
            started_at,
            ms_played: Some(self.ms_played),
            ending: ending(self.reason_end.as_deref(), self.skipped),
            started_by: started_by(self.reason_start.as_deref()),
            provenance: Provenance::History,
        }))
    }
}

/// Maps `reason_end`. An explicit `skipped: true` wins over the reason.
///
/// Values seen in real exports: trackdone, endplay, fwdbtn, backbtn, logout,
/// remote (another device took over), trackerror, unexpected-exit,
/// unexpected-exit-while-paused, unknown.
fn ending(reason_end: Option<&str>, skipped: Option<bool>) -> Ending {
    if skipped == Some(true) {
        return Ending::Skipped;
    }
    match reason_end {
        Some("trackdone") => Ending::Finished,
        Some("fwdbtn" | "backbtn" | "clickrow" | "clickside" | "popup" | "uriopen") => {
            Ending::Skipped
        }
        Some("endplay" | "logout" | "remote" | "trackerror" | "unexpected-exit")
        | Some("unexpected-exit-while-paused") => Ending::Interrupted,
        _ => Ending::Unknown,
    }
}

/// Maps `reason_start`.
///
/// Values seen in real exports: trackdone, fwdbtn, clickrow, playbtn, backbtn,
/// appload, remote, trackerror, unknown. None says "autoplay": an autoplayed
/// track starts with trackdone like any other next track.
fn started_by(reason_start: Option<&str>) -> StartedBy {
    match reason_start {
        Some(
            "clickrow" | "clickside" | "playbtn" | "fwdbtn" | "backbtn" | "popup" | "uriopen"
            | "remote",
        ) => StartedBy::Listener,
        Some("trackdone" | "trackerror") => StartedBy::Sequence,
        Some("autoplay") => StartedBy::Autoplay,
        _ => StartedBy::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;

    const SAMPLE: &str = r#"[
      {"ts": "2024-03-01T18:22:05Z", "ms_played": 241000,
       "master_metadata_track_name": "Heroes - 2017 Remaster",
       "master_metadata_album_artist_name": "David Bowie",
       "master_metadata_album_album_name": "\"Heroes\"",
       "spotify_track_uri": "spotify:track:7Jh1bpe76CNTCgdgAdBw4Z",
       "episode_name": null, "reason_start": "clickrow", "reason_end": "trackdone",
       "shuffle": false, "skipped": null, "offline": false, "incognito_mode": false,
       "ip_addr": "203.0.113.7", "platform": "android"},
      {"ts": "2024-03-01T18:22:20Z", "ms_played": 15000,
       "master_metadata_track_name": "Song", "master_metadata_album_artist_name": "Artist",
       "master_metadata_album_album_name": "Album",
       "spotify_track_uri": "spotify:track:abc", "reason_start": "trackdone",
       "reason_end": "endplay", "skipped": true},
      {"ts": "2024-03-01T19:00:00Z", "ms_played": 1800000,
       "master_metadata_track_name": null, "master_metadata_album_artist_name": null,
       "master_metadata_album_album_name": null, "spotify_track_uri": null,
       "episode_name": "Some Episode", "spotify_episode_uri": "spotify:episode:xyz",
       "reason_start": "clickrow", "reason_end": "endplay", "skipped": null}
    ]"#;

    #[test]
    fn music_rows_become_listens_and_podcasts_are_dropped() {
        let listens = parse_extended(SAMPLE).unwrap();
        assert_eq!(listens.len(), 2);

        let heroes = &listens[0];
        assert_eq!(heroes.track.artist.name, "David Bowie");
        assert_eq!(heroes.track.title_key(), "heroes");
        assert_eq!(
            heroes.track.ids.get(Namespace::Spotify),
            Some("7Jh1bpe76CNTCgdgAdBw4Z")
        );
        assert_eq!(heroes.ending, Ending::Finished);
        assert_eq!(heroes.started_by, StartedBy::Listener);
        assert_eq!(heroes.provenance, Provenance::History);
    }

    #[test]
    fn start_time_is_end_time_minus_ms_played() {
        let heroes = &parse_extended(SAMPLE).unwrap()[0];
        let expected: Timestamp = "2024-03-01T18:18:04Z".parse().unwrap();
        assert_eq!(heroes.started_at, expected);
    }

    #[test]
    fn the_skipped_flag_overrides_the_end_reason() {
        let song = &parse_extended(SAMPLE).unwrap()[1];
        assert_eq!(song.ending, Ending::Skipped);
        assert_eq!(song.started_by, StartedBy::Sequence);
    }

    #[test]
    fn another_device_taking_over_is_an_interruption() {
        assert_eq!(ending(Some("remote"), Some(false)), Ending::Interrupted);
        assert_eq!(started_by(Some("remote")), StartedBy::Listener);
        assert_eq!(ending(Some("unknown"), None), Ending::Unknown);
    }

    #[test]
    fn reads_audio_files_from_a_zip_and_ignores_the_rest() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for name in [
            "Spotify Extended Streaming History/Streaming_History_Audio_2024.json",
            "Spotify Extended Streaming History/Streaming_History_Video_2024.json",
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(SAMPLE.as_bytes()).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();

        let dir = std::env::temp_dir().join(format!("novelty-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("export.zip");
        fs::write(&path, bytes).unwrap();
        let export = read_export(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(export.listens.len(), 2);
        assert!(export.listens[0].started_at <= export.listens[1].started_at);
        assert!(export.playlists.is_none() && export.familiarity.is_none());
    }

    const ACCOUNT: &str = r#"[
      {"endTime": "2025-11-12 00:41", "artistName": "Jelly Roll", "trackName": "Save Me",
       "msPlayed": 38045},
      {"endTime": "2025-11-12 00:42", "artistName": "Unknown Artist",
       "trackName": "Unknown Track", "msPlayed": 5877}
    ]"#;

    #[test]
    fn account_data_listens_start_ms_played_before_their_minute() {
        let listens = parse_account(ACCOUNT).unwrap();
        assert_eq!(listens.len(), 1, "the DJ's voice is not a listen");
        let save_me = &listens[0];
        let ended: Timestamp = "2025-11-12T00:41:00Z".parse().unwrap();
        assert_eq!(
            save_me.started_at,
            ended - SignedDuration::from_millis(38_045)
        );
        assert_eq!(save_me.ms_played, Some(38_045));
        assert_eq!(save_me.ending, Ending::Unknown);
        assert_eq!(save_me.provenance, Provenance::BasicHistory);
        assert!(save_me.track.ids.get(Namespace::Spotify).is_none());
    }

    #[test]
    fn playlists_keep_tracks_with_uris_and_the_day_each_was_added() {
        let json = r#"{"playlists": [
          {"name": "Chill", "lastModifiedDate": "2026-10-07", "collaborators": [],
           "items": [
             {"track": {"trackName": "Run Through The Jungle",
                        "artistName": "Creedence Clearwater Revival",
                        "albumName": "Cosmo's Factory",
                        "trackUri": "spotify:track:6J0AjzrYsPZrQtw6IjErhy"},
              "addedDate": "2020-06-03"},
             {"track": null, "episode": {"episodeName": "x"}, "addedDate": "2020-06-03"},
             {"track": {"trackName": "Demo", "artistName": "Me", "albumName": null,
                        "trackUri": null}, "addedDate": null}
           ]},
          {"name": "Only podcasts", "items": [{"track": null, "addedDate": null}]}
        ]}"#;
        let playlists = parse_playlists(json).unwrap();
        assert_eq!(playlists.len(), 1);
        let (track, added) = &playlists[0].tracks[0];
        assert_eq!(playlists[0].tracks.len(), 1);
        assert_eq!(
            track.ids.get(Namespace::Spotify),
            Some("6J0AjzrYsPZrQtw6IjErhy")
        );
        assert_eq!(track.album.as_deref(), Some("Cosmo's Factory"));
        assert_eq!(*added, Some(jiff::civil::date(2020, 6, 3)));
    }

    #[test]
    fn the_library_gives_saved_albums() {
        let json = r#"{"tracks": [{"artist": "A", "album": "B", "track": "C",
                                   "uri": "spotify:track:x"}],
                       "albums": [{"artist": "Lil Nas X", "album": "MONTERO",
                                   "uri": "spotify:album:6pOiDiuDQqrmo5DbG0ZubR"}],
                       "shows": []}"#;
        let albums = parse_library(json).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].artist.name, "Lil Nas X");
        assert_eq!(albums[0].title_key(), "montero");
    }

    #[test]
    fn marquee_segments_become_familiarity() {
        let json = r#"[
          {"artistName": "Kanye West", "segment": "Super Listeners"},
          {"artistName": "AC/DC", "segment": "Moderate listeners"},
          {"artistName": "LSD", "segment": "Light listeners"},
          {"artistName": "Andrew Garfield", "segment": "Previously Active Listeners"},
          {"artistName": "Someone", "segment": "Brand New Segment"}
        ]"#;
        let levels: Vec<Familiarity> = parse_marquee(json)
            .unwrap()
            .into_iter()
            .map(|(_, f)| f)
            .collect();
        assert_eq!(
            levels,
            vec![
                Familiarity::Heavy,
                Familiarity::Moderate,
                Familiarity::Light,
                Familiarity::Lapsed,
                Familiarity::Unknown
            ]
        );
    }

    #[test]
    fn reads_an_extracted_account_data_directory() {
        let dir = std::env::temp_dir().join(format!("novelty-acct-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("StreamingHistory_music_0.json"), ACCOUNT).unwrap();
        fs::write(dir.join("StreamingHistory_podcast_0.json"), "[]").unwrap();
        fs::write(dir.join("Marquee.json"), "[]").unwrap();
        fs::write(dir.join("Payments.json"), "{}").unwrap();
        let export = read_export(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(export.listens.len(), 1);
        assert_eq!(export.familiarity, Some(Vec::new()));
        assert!(export.playlists.is_none() && export.saved_albums.is_none());
    }
}
