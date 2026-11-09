//! Spotify's Extended Streaming History export (spec §5.1), read as listens.
//!
//! The export is a ZIP of `Streaming_History_Audio_*.json` files, each a JSON
//! array with one object per stream. It is read in memory and never extracted,
//! so nothing in the archive lands on disk.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

use crate::model::{
    ArtistRef, Ending, ExternalIds, Listen, Namespace, Provenance, StartedBy, TrackRef,
};

/// Largest JSON file accepted from an export. Real ones are a few MB; the cap
/// keeps a hostile archive from exhausting memory.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Every music listen in an export, oldest first. `path` is the `.zip`, or a
/// directory it was extracted into.
pub fn read_export(path: &Path) -> Result<Vec<Listen>> {
    let mut listens = if path.is_dir() {
        read_dir(path)?
    } else {
        read_zip(path)?
    };
    listens.sort_by_key(|l| l.started_at);
    Ok(listens)
}

/// The music files; video and podcast-only files are skipped.
fn is_audio_history(file_name: &str) -> bool {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    base.starts_with("Streaming_History_Audio_") && base.ends_with(".json")
}

fn read_zip(path: &Path) -> Result<Vec<Listen>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("reading {} as a zip", path.display()))?;
    let mut listens = Vec::new();
    let mut files = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name()?.into_owned();
        if !entry.is_file() || !is_audio_history(&name) {
            continue;
        }
        if entry.size() > MAX_FILE_BYTES {
            bail!("{name} is larger than {MAX_FILE_BYTES} bytes");
        }
        let mut json = String::new();
        // `take` holds even if the archive lies about the entry's size.
        entry
            .by_ref()
            .take(MAX_FILE_BYTES)
            .read_to_string(&mut json)?;
        listens.extend(parse(&json).with_context(|| format!("parsing {name}"))?);
        files += 1;
    }
    if files == 0 {
        bail!(
            "no Streaming_History_Audio_*.json files in {}",
            path.display()
        );
    }
    Ok(listens)
}

fn read_dir(dir: &Path) -> Result<Vec<Listen>> {
    let mut listens = Vec::new();
    let mut files = 0;
    for entry in fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !path.is_file() || !is_audio_history(name) {
            continue;
        }
        let json =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        listens.extend(parse(&json).with_context(|| format!("parsing {}", path.display()))?);
        files += 1;
    }
    if files == 0 {
        bail!(
            "no Streaming_History_Audio_*.json files in {}",
            dir.display()
        );
    }
    Ok(listens)
}

/// Parses one export file. Podcast and audiobook rows are dropped.
pub fn parse(json: &str) -> Result<Vec<Listen>> {
    let rows: Vec<Row> = serde_json::from_str(json)?;
    let mut listens = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(listen) = row.into_listen()? {
            listens.push(listen);
        }
    }
    Ok(listens)
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
        let listens = parse(SAMPLE).unwrap();
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
        let heroes = &parse(SAMPLE).unwrap()[0];
        let expected: Timestamp = "2024-03-01T18:18:04Z".parse().unwrap();
        assert_eq!(heroes.started_at, expected);
    }

    #[test]
    fn the_skipped_flag_overrides_the_end_reason() {
        let song = &parse(SAMPLE).unwrap()[1];
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
        let listens = read_export(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(listens.len(), 2);
        assert!(listens[0].started_at <= listens[1].started_at);
    }
}
