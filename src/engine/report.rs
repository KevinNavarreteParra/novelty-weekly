//! The numbers `novelty report` prints (spec §10): how picks fare, and whether
//! my listening as a whole is broadening, which is the point of the app.
//!
//! Listening counts *streams*: listens of 30 s or more, as Spotify counts
//! them. The poller never sees shorter listens and a history export does, so
//! counting every listen would make polled weeks look unlike exported ones.

use std::collections::HashMap;

use jiff::Timestamp;

use super::feedback::Outcome;
use crate::model::{ArtistId, ArtistTags, Play, Recommendation, SKIP_BEFORE_MS, TrackId, Verdict};

/// How many of the most-streamed artists the concentration share covers.
pub const TOP_ARTISTS: usize = 20;

/// How a set of picks fared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickStats {
    pub picks: usize,
    /// Listened to at all since publishing.
    pub played: usize,
    /// First listen completed, or skipped, or neither (partial). Each is a
    /// share of `played`.
    pub completed: usize,
    pub skipped: usize,
    /// Saved, or played again within 30 days of the first listen.
    pub kept: usize,
}

impl PickStats {
    pub fn add(&mut self, outcome: &Outcome) {
        self.picks += 1;
        let Some(first) = &outcome.first_play else {
            return;
        };
        self.played += 1;
        match first.verdict() {
            Some(Verdict::Completed) => self.completed += 1,
            Some(Verdict::Skipped) => self.skipped += 1,
            Some(Verdict::Partial) | None => {}
        }
        if outcome.saved || outcome.replays > 0 {
            self.kept += 1;
        }
    }
}

/// My listening over one span of time.
#[derive(Debug, Clone, PartialEq)]
pub struct Listening {
    pub streams: usize,
    pub artists: usize,
    /// Share of streams by the [`TOP_ARTISTS`] most-streamed artists.
    pub top_share: f64,
    /// Gini coefficient of streams per artist: 0 when every artist I played
    /// got the same share, toward 1 when a few artists got nearly all of it.
    pub gini: f64,
    /// Share of streams that were the first ever of their track, or of their
    /// artist, as far back as the history on file goes.
    pub new_tracks: f64,
    pub new_artists: f64,
    /// Share of streams that were of a pick, after it was published.
    pub from_picks: f64,
}

/// Every stream on file, indexed for [`Listening`] over any span.
pub struct Streams<'a> {
    /// Oldest first.
    streams: Vec<&'a Play>,
    first_of_track: HashMap<TrackId, Timestamp>,
    first_of_artist: HashMap<ArtistId, Timestamp>,
    /// Each pick's track, and when it was first published.
    picked: HashMap<TrackId, Timestamp>,
}

impl<'a> Streams<'a> {
    pub fn new(plays: &'a [Play], recs: &[Recommendation]) -> Streams<'a> {
        let mut streams: Vec<&Play> = plays
            .iter()
            .filter(|p| p.ms_played.is_some_and(|ms| ms >= SKIP_BEFORE_MS))
            .collect();
        streams.sort_by_key(|p| p.started_at);
        let mut first_of_track = HashMap::new();
        let mut first_of_artist = HashMap::new();
        for p in &streams {
            first_of_track.entry(p.track).or_insert(p.started_at);
            first_of_artist.entry(p.artist).or_insert(p.started_at);
        }
        let mut picked: HashMap<TrackId, Timestamp> = HashMap::new();
        for rec in recs {
            let at = picked.entry(rec.track).or_insert(rec.published_at);
            *at = (*at).min(rec.published_at);
        }
        Streams {
            streams,
            first_of_track,
            first_of_artist,
            picked,
        }
    }

    /// When the first stream on file started.
    pub fn first(&self) -> Option<Timestamp> {
        self.streams.first().map(|p| p.started_at)
    }

    /// Listening from `from` up to (not including) `to`; `None` if there was none.
    pub fn listening(&self, from: Timestamp, to: Timestamp) -> Option<Listening> {
        let lo = self.streams.partition_point(|p| p.started_at < from);
        let hi = self.streams.partition_point(|p| p.started_at < to);
        let window = &self.streams[lo..hi];
        if window.is_empty() {
            return None;
        }
        let mut per_artist: HashMap<ArtistId, usize> = HashMap::new();
        let (mut new_tracks, mut new_artists, mut from_picks) = (0, 0, 0);
        for p in window {
            *per_artist.entry(p.artist).or_default() += 1;
            new_tracks += usize::from(self.first_of_track[&p.track] == p.started_at);
            new_artists += usize::from(self.first_of_artist[&p.artist] == p.started_at);
            from_picks += usize::from(
                self.picked
                    .get(&p.track)
                    .is_some_and(|at| p.started_at >= *at),
            );
        }
        let n = window.len() as f64;
        let counts: Vec<usize> = per_artist.into_values().collect();
        Some(Listening {
            streams: window.len(),
            artists: counts.len(),
            top_share: top_share(&counts, TOP_ARTISTS),
            gini: gini(&counts),
            new_tracks: new_tracks as f64 / n,
            new_artists: new_artists as f64 / n,
            from_picks: from_picks as f64 / n,
        })
    }
}

/// The share of the total held by the `top` largest counts.
pub fn top_share(counts: &[usize], top: usize) -> f64 {
    let total: usize = counts.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let mut sorted = counts.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted.iter().take(top).sum::<usize>() as f64 / total as f64
}

/// The Gini coefficient of `counts`: `Σ (2i − n − 1) · x_i / (n · Σ x)` over
/// the counts sorted ascending, `i` from 1.
pub fn gini(counts: &[usize]) -> f64 {
    let n = counts.len();
    let total: usize = counts.iter().sum();
    if n == 0 || total == 0 {
        return 0.0;
    }
    let mut sorted = counts.to_vec();
    sorted.sort_unstable();
    let weighted: f64 = sorted
        .iter()
        .enumerate()
        .map(|(i, x)| (2.0 * (i as f64 + 1.0) - n as f64 - 1.0) * *x as f64)
        .sum();
    weighted / (n as f64 * total as f64)
}

/// The spread of a playlist (spec §10): Shannon entropy, in bits, of its
/// artists' dominant tags. Artists with no tags on file are left out; `None`
/// if none have any. 0 means one tag throughout; `log2(n)` means every pick
/// a different one.
pub fn tag_spread(artists: impl IntoIterator<Item = ArtistId>, tags: &ArtistTags) -> Option<f64> {
    let mut dominant: HashMap<&str, usize> = HashMap::new();
    for artist in artists {
        let Some(top) = tags
            .get(&artist)
            .and_then(|t| t.iter().max_by(|a, b| a.weight.total_cmp(&b.weight)))
        else {
            continue;
        };
        *dominant.entry(top.tag.as_str()).or_default() += 1;
    }
    let n: usize = dominant.values().sum();
    if n == 0 {
        return None;
    }
    Some(
        dominant
            .values()
            .map(|c| {
                let p = *c as f64 / n as f64;
                -p * p.log2()
            })
            .sum(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Arm, ArtistTag, Ending, Pool, StartedBy, Week};

    fn at(s: i64) -> Timestamp {
        Timestamp::from_second(s).unwrap()
    }

    fn stream(artist: i64, track: i64, s: i64, ms: u32) -> Play {
        Play {
            track: TrackId(track),
            artist: ArtistId(artist),
            started_at: at(s),
            ms_played: Some(ms),
            duration_ms: Some(200_000),
            ending: Ending::Unknown,
            started_by: StartedBy::Unknown,
        }
    }

    #[test]
    fn gini_is_zero_when_even_and_grows_with_concentration() {
        assert_eq!(gini(&[5, 5, 5, 5]), 0.0);
        assert!((gini(&[0, 0, 0, 10]) - 0.75).abs() < 1e-9);
        assert!(gini(&[1, 1, 1, 7]) > gini(&[2, 2, 3, 3]));
    }

    #[test]
    fn top_share_takes_the_largest_counts() {
        assert_eq!(top_share(&[1, 6, 3], 1), 0.6);
        assert_eq!(top_share(&[1, 6, 3], 20), 1.0);
    }

    #[test]
    fn listening_counts_streams_new_ones_and_picks_in_its_window() {
        let plays = [
            stream(1, 10, 100, 200_000),
            stream(1, 10, 200, 200_000), // a repeat: not new
            stream(2, 20, 300, 10_000),  // under 30 s: not a stream
            stream(2, 21, 400, 200_000), // a new artist, a new track
            stream(3, 30, 500, 200_000), // a pick, played after publishing
            stream(1, 11, 900, 200_000), // outside the window
        ];
        let rec = Recommendation {
            week: "2026-W46".parse::<Week>().unwrap(),
            published_at: at(450),
            position: 0,
            track: TrackId(30),
            artist: ArtistId(3),
            pool: Pool::Near,
            arm: Arm::Pool(Pool::Near),
            score: 1.0,
            rationale: String::new(),
        };
        let streams = Streams::new(&plays, &[rec]);
        let l = streams.listening(at(150), at(600)).unwrap();
        assert_eq!(l.streams, 3);
        assert_eq!(l.artists, 3);
        assert!((l.new_tracks - 2.0 / 3.0).abs() < 1e-9);
        assert!((l.new_artists - 2.0 / 3.0).abs() < 1e-9);
        assert!((l.from_picks - 1.0 / 3.0).abs() < 1e-9);
        assert!(streams.listening(at(1_000), at(2_000)).is_none());
    }

    #[test]
    fn pick_stats_count_each_fate_once() {
        let played = |ms, saved, replays| Outcome {
            first_play: Some(stream(1, 1, 0, ms)),
            replays,
            returns: 0,
            saved,
        };
        let mut stats = PickStats::default();
        for o in [
            played(200_000, true, 0),
            played(5_000, false, 0),
            played(100_000, false, 2),
            Outcome {
                first_play: None,
                replays: 0,
                returns: 0,
                saved: false,
            },
        ] {
            stats.add(&o);
        }
        assert_eq!(
            stats,
            PickStats {
                picks: 4,
                played: 3,
                completed: 1,
                skipped: 1,
                kept: 2,
            }
        );
    }

    #[test]
    fn spread_is_the_entropy_of_dominant_tags() {
        let tags = ArtistTags::from([
            (ArtistId(1), vec![ArtistTag::new("jazz", 1.0)]),
            (ArtistId(2), vec![ArtistTag::new("jazz", 0.9)]),
            (
                ArtistId(3),
                vec![ArtistTag::new("dub", 1.0), ArtistTag::new("jazz", 0.2)],
            ),
            (ArtistId(4), vec![ArtistTag::new("dub", 1.0)]),
        ]);
        let spread = tag_spread([1, 2, 3, 4, 5].map(ArtistId), &tags).unwrap();
        assert!((spread - 1.0).abs() < 1e-9, "two tags, evenly: 1 bit");
        assert_eq!(tag_spread([ArtistId(9)], &tags), None);
    }
}
