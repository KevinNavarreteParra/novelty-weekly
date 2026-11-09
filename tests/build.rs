//! A whole build against fake integrations: the spec's v0 "done when" (§13),
//! a playlist of never-played tracks whose next week shares nothing with it.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::Range;

use anyhow::Result;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use rand::SeedableRng;
use rand::rngs::StdRng;

use novelty::commands::build::{BuildParams, build};
use novelty::engine::profile::ProfileParams;
use novelty::engine::select::SelectParams;
use novelty::engine::select::allocate::ShareBounds;
use novelty::integrations::{Knowledge, ListeningService};
use novelty::model::{
    ArtistRef, ArtistTag, Ending, Listen, Namespace, PopularTrack, Provenance, SimilarArtist,
    StartedBy, TrackRef,
};
use novelty::store::Store;

const K: usize = 12;

/// Last.fm stand-in: artists "Liked 0-2" link to "Near 0-29", which link
/// onward to each other; every tag's chart is "<tag> artist 0-199".
struct FakeKnowledge;

impl Knowledge for FakeKnowledge {
    fn source(&self) -> &'static str {
        "fake"
    }

    fn similar_artists(&self, artist: &ArtistRef) -> Result<Vec<SimilarArtist>> {
        let near =
            |i: usize, sim: f64| SimilarArtist::new(ArtistRef::named(format!("Near {i}")), sim);
        Ok(
            match artist
                .name
                .strip_prefix("Liked ")
                .and_then(|n| n.parse::<usize>().ok())
            {
                Some(n) => (0..10)
                    .map(|i| near(n * 10 + i, 0.9 - i as f64 * 0.05))
                    .collect(),
                None => match artist
                    .name
                    .strip_prefix("Near ")
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    Some(n) => (1..4).map(|d| near((n + d * 7) % 30, 0.5)).collect(),
                    None => Vec::new(),
                },
            },
        )
    }

    fn artist_tags(&self, artist: &ArtistRef) -> Result<Vec<ArtistTag>> {
        const NEAR_TAGS: [&str; 6] = [
            "dream pop",
            "shoegaze",
            "slowcore",
            "post rock",
            "folk",
            "ambient",
        ];
        let tag = if let Some(n) = artist.name.strip_prefix("Liked ") {
            format!("indie rock {n}")
        } else if let Some(n) = artist.name.strip_prefix("Near ") {
            NEAR_TAGS[n.parse::<usize>().unwrap() % 6].to_owned()
        } else {
            // "<tag> artist <i>" carries its chart's tag.
            artist.name.split(" artist ").next().unwrap().to_owned()
        };
        let tag = tag.as_str();
        Ok(vec![
            ArtistTag::new(tag, 1.0),
            ArtistTag::new(&format!("{} tag", artist.name), 0.5),
        ])
    }

    fn popular_tracks(&self, artist: &ArtistRef) -> Result<Vec<PopularTrack>> {
        Ok((0..3)
            .map(|i| PopularTrack {
                track: TrackRef::new(format!("{} song {i}", artist.name), artist.clone()),
                listeners: Some(10_000 - i * 1_000),
            })
            .collect())
    }

    fn listeners(&self, _artist: &ArtistRef) -> Result<Option<u64>> {
        Ok(Some(50_000))
    }

    fn tag_chart(&self, tag: &str, ranks: Range<usize>) -> Result<Vec<ArtistRef>> {
        Ok(ranks
            .map(|i| ArtistRef::named(format!("{tag} artist {i}")))
            .collect())
    }
}

/// Spotify stand-in: has every song, records each publish.
#[derive(Default)]
struct FakeService {
    published: RefCell<Vec<Vec<TrackRef>>>,
}

impl ListeningService for FakeService {
    fn namespace(&self) -> Namespace {
        Namespace::Spotify
    }

    fn recent_listens(&mut self, _after: Option<Timestamp>) -> Result<Vec<Listen>> {
        Ok(Vec::new())
    }

    fn saved_since(&mut self, _since: Timestamp) -> Result<Vec<(TrackRef, Timestamp)>> {
        Ok(Vec::new())
    }

    fn catalog(&mut self, artist: &ArtistRef) -> Result<Vec<TrackRef>> {
        Ok((0..12)
            .map(|i| {
                let mut t = TrackRef::new(format!("{} track {i}", artist.name), artist.clone());
                t.duration_ms = Some(210_000);
                t.ids
                    .insert(Namespace::Spotify, format!("sp:{}:{i}", artist.name));
                t
            })
            .collect())
    }

    fn find(&mut self, wanted: &TrackRef) -> Result<Option<TrackRef>> {
        let mut found = wanted.clone();
        found.duration_ms = Some(200_000);
        found.ids.insert(
            Namespace::Spotify,
            format!("sp:{}:{}", wanted.artist.name, wanted.title),
        );
        Ok(Some(found))
    }

    fn publish(
        &mut self,
        playlist: Option<&str>,
        _name: &str,
        _description: &str,
        tracks: &[TrackRef],
    ) -> Result<String> {
        self.published.borrow_mut().push(tracks.to_vec());
        Ok(playlist.unwrap_or("playlist-1").to_owned())
    }
}

fn now() -> Timestamp {
    "2026-11-16T14:00:00Z".parse().unwrap()
}

/// I've completed tracks 0-3 by "Liked 0", "Liked 1" and "Liked 2".
fn store_with_history() -> Store {
    let store = Store::open_in_memory().unwrap();
    let mut listens = Vec::new();
    for artist in 0..3 {
        for track in 0..4 {
            for day in 0..(5 - artist) {
                listens.push(Listen {
                    track: TrackRef::new(
                        format!("Liked {artist} track {track}"),
                        ArtistRef::named(format!("Liked {artist}")),
                    ),
                    started_at: now()
                        - SignedDuration::from_hours(24 * (day + 1) as i64 + track as i64),
                    ms_played: Some(200_000),
                    ending: Ending::Finished,
                    started_by: StartedBy::Listener,
                    provenance: Provenance::History,
                });
            }
        }
    }
    store.import_history(&listens).unwrap();
    store
}

fn params(dry_run: bool) -> BuildParams {
    BuildParams {
        dry_run,
        archive: false,
        timezone: TimeZone::UTC,
        profile: ProfileParams {
            tau_days: 365.0,
            autoplay_weight: 1.0,
        },
        select: SelectParams::new(
            K,
            ShareBounds {
                floor: 0.2,
                cap: 0.5,
            },
            0.6,
        ),
        decay_rho: 0.9,
        popularity_gamma: 0.5,
        far_skip_top_ranks: 20,
        blocked_artists: Vec::new(),
        blocked_tags: Vec::new(),
        playlist_name: "Novelty Weekly".into(),
    }
}

fn spotify_ids(tracks: &[TrackRef]) -> HashSet<String> {
    tracks
        .iter()
        .map(|t| t.ids.get(Namespace::Spotify).unwrap().to_owned())
        .collect()
}

#[test]
fn a_full_playlist_of_unheard_tracks_with_no_repeats_the_next_week() {
    let store = store_with_history();
    let mut service = FakeService::default();
    let mut rng = StdRng::seed_from_u64(42);

    let first = build(
        &store,
        &FakeKnowledge,
        &mut service,
        &params(false),
        now(),
        &mut rng,
    )
    .unwrap();
    assert_eq!(first.picks.len(), K);
    assert_eq!(first.playlist.as_deref(), Some("playlist-1"));
    let heard: HashSet<String> = (0..3)
        .flat_map(|a| (0..4).map(move |t| format!("Liked {a} track {t}")))
        .collect();
    for (pick, track, why) in &first.picks {
        assert!(
            !heard.contains(&track.title),
            "recommended a heard track: {}",
            track.title
        );
        assert!(!why.is_empty());
        if pick.pool != novelty::model::Pool::Deep {
            assert!(
                !track.artist.name.starts_with("Liked"),
                "a heard artist outside the deep pool"
            );
        }
    }
    let pools: HashSet<_> = first.picks.iter().map(|(c, _, _)| c.pool).collect();
    assert_eq!(pools.len(), 3, "every pool is represented");
    assert_eq!(store.ledger().unwrap().len(), K);

    let next_week = now() + SignedDuration::from_hours(24 * 7);
    let second = build(
        &store,
        &FakeKnowledge,
        &mut service,
        &params(false),
        next_week,
        &mut rng,
    )
    .unwrap();
    let published = service.published.borrow();
    assert_eq!(published.len(), 2);
    let overlap: HashSet<_> = spotify_ids(&published[0])
        .intersection(&spotify_ids(&published[1]))
        .cloned()
        .collect();
    assert!(overlap.is_empty(), "repeated across weeks: {overlap:?}");
    assert_eq!(
        second.playlist.as_deref(),
        Some("playlist-1"),
        "the same playlist is reused"
    );
    assert_eq!(
        store.ledger().unwrap().len(),
        first.picks.len() + second.picks.len()
    );
}

#[test]
fn a_dry_run_writes_nothing() {
    let store = store_with_history();
    let mut service = FakeService::default();
    let built = build(
        &store,
        &FakeKnowledge,
        &mut service,
        &params(true),
        now(),
        &mut StdRng::seed_from_u64(1),
    )
    .unwrap();
    assert_eq!(built.picks.len(), K);
    assert!(built.playlist.is_none());
    assert!(service.published.borrow().is_empty());
    assert!(store.ledger().unwrap().is_empty());
}

#[test]
fn a_week_is_only_published_once() {
    let store = store_with_history();
    let mut service = FakeService::default();
    let mut rng = StdRng::seed_from_u64(7);
    build(
        &store,
        &FakeKnowledge,
        &mut service,
        &params(false),
        now(),
        &mut rng,
    )
    .unwrap();
    let again = build(
        &store,
        &FakeKnowledge,
        &mut service,
        &params(false),
        now(),
        &mut rng,
    );
    assert!(again.is_err());
}

#[test]
fn an_unknown_artist_from_the_service_is_skipped_not_fatal() {
    struct NoCatalog(FakeService);
    impl ListeningService for NoCatalog {
        fn namespace(&self) -> Namespace {
            self.0.namespace()
        }
        fn recent_listens(&mut self, after: Option<Timestamp>) -> Result<Vec<Listen>> {
            self.0.recent_listens(after)
        }
        fn saved_since(&mut self, since: Timestamp) -> Result<Vec<(TrackRef, Timestamp)>> {
            self.0.saved_since(since)
        }
        fn catalog(&mut self, _artist: &ArtistRef) -> Result<Vec<TrackRef>> {
            Ok(Vec::new())
        }
        fn find(&mut self, t: &TrackRef) -> Result<Option<TrackRef>> {
            self.0.find(t)
        }
        fn publish(&mut self, p: Option<&str>, n: &str, d: &str, t: &[TrackRef]) -> Result<String> {
            self.0.publish(p, n, d, t)
        }
    }
    let store = store_with_history();
    let mut service = NoCatalog(FakeService::default());
    let built = build(
        &store,
        &FakeKnowledge,
        &mut service,
        &params(true),
        now(),
        &mut StdRng::seed_from_u64(3),
    )
    .unwrap();
    // No deep candidates at all; near and far backfill the playlist.
    assert_eq!(built.picks.len(), K);
    assert!(built.shortfall.contains_key(&novelty::model::Pool::Deep));
}
