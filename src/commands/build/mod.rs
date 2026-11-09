//! `novelty build`: this week's playlist, end to end (spec §7).
//!
//! [`build`] runs these steps in order, each one a call into the engine with
//! what the step before produced:
//!
//! 1. **Profile**: taste, exposure and seeds, from every play, plus the cold
//!    start until a history export is imported.
//! 2. **Pools**: knowledge gathered for the seeds, then deep, near and far
//!    candidates. A cold start has no deep candidates: without a history,
//!    which of a liked artist's tracks I've heard is unknown.
//! 3. **Filters**: the hard rules.
//! 4. **Beliefs**: how past picks fared, as the bandit's posteriors.
//! 5. **Select and resolve**: `k` picks across the pools, each matched to a
//!    playable track; picks that can't be played are replaced.
//! 6. **Publish**: the playlist and the ledger, together. `--dry-run` stops
//!    before this.
//!
//! [`build`] is generic over the two roles, so tests run it against fakes.

mod gather;
mod present;

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Result, bail};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use log::{info, warn};
use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use rand::{Rng, RngExt, SeedableRng};

use self::gather::Gather;
use super::App;
use crate::cli::BuildArgs;
use crate::config::Config;
use crate::engine::candidate::Candidate;
use crate::engine::feedback;
use crate::engine::filters::{Filters, MAX_DURATION_MS, MIN_DURATION_MS, Rejection};
use crate::engine::pools::far::{self, TagSampling};
use crate::engine::pools::{deep, near};
use crate::engine::profile::{Profile, ProfileParams, SeedPlan, tag_shares};
use crate::engine::select::allocate::{Posterior, ShareBounds};
use crate::engine::select::{self, SelectParams};
use crate::integrations::{Knowledge, ListeningService};
use crate::model::names::tag_key;
use crate::model::{
    ArtistId, ArtistRef, ArtistTags, Pool, Recommendation, TrackId, TrackRef, Week,
};
use crate::store::{PLAYLIST_ID, Store};

/// Near: how many of the best-scored artists to fetch tracks for.
const NEAR_SHORTLIST: usize = 120;
/// Near and deep offer this many tracks per artist; far offers 2.
const TRACKS_PER_ARTIST: usize = 3;
const FAR_TRACKS_PER_ARTIST: usize = 2;
/// Tag shares are computed over this many of my best-liked artists.
const TAG_SHARE_ARTISTS: usize = 300;
/// A tag joins the far pool's universe once this many known artists carry it.
const TAG_UNIVERSE_MIN_ARTISTS: usize = 3;
/// Far: tags sampled per build, how deep into each chart, and how many of a
/// chart's unheard artists to fetch tracks for.
const FAR_TAGS: usize = 6;
const FAR_CHART_DEPTH: usize = 60;
const FAR_CHART_ARTISTS: usize = 15;
const FAR_WALKS: usize = 20;
const WALK_HOPS: usize = 3;
/// Rounds of select-then-resolve before settling for a shorter playlist. A
/// round only searches its new picks, and replacements can fail one after
/// another, so this is a backstop, not a budget.
const RESOLVE_ROUNDS: usize = 20;

pub struct BuildParams {
    pub dry_run: bool,
    pub archive: bool,
    pub timezone: TimeZone,
    pub profile: ProfileParams,
    pub select: SelectParams,
    pub decay_rho: f64,
    pub popularity_gamma: f64,
    pub far_skip_top_ranks: usize,
    pub blocked_artists: Vec<String>,
    pub blocked_tags: Vec<String>,
    pub playlist_name: String,
}

impl BuildParams {
    pub fn new(config: &Config, args: &BuildArgs) -> Result<BuildParams> {
        let b = &config.build;
        Ok(BuildParams {
            dry_run: args.dry_run,
            archive: args.archive,
            timezone: TimeZone::get(&b.timezone)?,
            profile: ProfileParams {
                tau_days: b.affinity_tau_days,
                autoplay_weight: b.autoplay_weight,
            },
            select: SelectParams::new(
                args.k.unwrap_or(b.k),
                ShareBounds {
                    floor: b.pool_floor,
                    cap: b.pool_cap,
                },
                b.mmr_lambda,
            ),
            decay_rho: b.decay_rho,
            popularity_gamma: b.popularity_gamma,
            far_skip_top_ranks: b.far_skip_top_ranks,
            blocked_artists: config.blocklist.artists.clone(),
            blocked_tags: config.blocklist.tags.clone(),
            playlist_name: config.spotify.playlist_name.clone(),
        })
    }
}

/// What a build produced.
pub struct Built {
    pub week: Week,
    /// In playlist order: the pick, the playable track, and why.
    pub picks: Vec<(Candidate, TrackRef, String)>,
    pub planned: BTreeMap<Pool, usize>,
    pub shortfall: BTreeMap<Pool, usize>,
    pub rejected: HashMap<Rejection, usize>,
    /// `None` on a dry run.
    pub playlist: Option<String>,
}

pub fn run(app: &App, args: &BuildArgs) -> Result<()> {
    let store = app.open_store()?;
    let params = BuildParams::new(&app.config, args)?;
    // The timer fires weekly; a week published early (by hand) is done.
    let week = Week::containing(Timestamp::now(), &params.timezone);
    if !params.dry_run && store.ledger_weeks()?.contains(&week) {
        println!("{week} is already published; nothing to do (preview with --dry-run)");
        return Ok(());
    }
    // Dry runs are previews, not runs worth alerting on.
    let run = if params.dry_run {
        None
    } else {
        Some(store.start_run("build")?)
    };
    let result = (|| {
        let knowledge = app.lastfm()?;
        let mut spotify = app.spotify()?;
        build(
            &store,
            &knowledge,
            &mut spotify,
            &params,
            Timestamp::now(),
            &mut rand::rng(),
        )
    })();
    let notes = match &result {
        Ok(built) => format!("{}: {} picks", built.week, built.picks.len()),
        Err(e) => format!("{e:#}"),
    };
    if let Some(run) = run {
        store.finish_run(run, result.is_ok(), &notes)?;
    }
    let built = result?;

    print!("{}", present::listing(&built.picks));
    println!(
        "\n{}",
        present::description(
            built.week,
            &built.picks.iter().map(|p| p.0.clone()).collect::<Vec<_>>()
        )
    );
    if !built.shortfall.is_empty() {
        println!(
            "short of candidates: {:?} (backfilled from other pools)",
            built.shortfall
        );
    }
    match &built.playlist {
        Some(id) => println!("published to playlist {id}"),
        None => println!("dry run: nothing written"),
    }
    Ok(())
}

pub fn build(
    store: &Store,
    knowledge: &impl Knowledge,
    service: &mut impl ListeningService,
    params: &BuildParams,
    now: Timestamp,
    rng: &mut impl Rng,
) -> Result<Built> {
    let week = Week::containing(now, &params.timezone);
    if !params.dry_run && store.ledger_weeks()?.contains(&week) {
        bail!("{week} is already published; preview with --dry-run");
    }
    let gather = Gather { store, knowledge };

    // 1. Profile.
    let plays = store.plays()?;
    let cold_start = if store.has_history()? {
        None
    } else {
        Some(store.cold_start()?)
    };
    let profile = Profile::build(&plays, cold_start.as_ref(), now, &params.profile);
    if profile.ranked_artists().is_empty() {
        bail!(
            "no taste to go on yet: run `novelty ingest-history`, or `novelty cold-start` \
             until the export arrives"
        );
    }
    let seeds = profile.seeds(SeedPlan::default(), rng);
    info!(
        "profile: {} plays{}, {} liked artists, {} seeds",
        plays.len(),
        if cold_start.is_some() {
            " and the cold start"
        } else {
            ""
        },
        profile.ranked_artists().len(),
        seeds.len()
    );

    // 2. Pools.
    let mut candidates = near_candidates(&gather, &profile, &seeds, params)?;
    if cold_start.is_none() {
        candidates.extend(deep_candidates(&gather, &profile, service)?);
    } else {
        info!("cold start: no deep cuts until a history export is imported");
    }
    candidates.extend(far_candidates(&gather, &profile, &seeds, params, rng)?);
    info!("{} candidates", candidates.len());

    // 3. Filters.
    let mut candidate_artists: Vec<ArtistId> = candidates.iter().map(|c| c.artist).collect();
    candidate_artists.sort();
    candidate_artists.dedup();
    let tags = gather.tags(&candidate_artists)?;
    let recommended = store.recommended_tracks()?;
    let durations = store.durations(candidates.iter().map(|c| c.track))?;
    let blocked_artists: HashSet<ArtistId> = params
        .blocked_artists
        .iter()
        .map(|name| store.resolve_artist(&ArtistRef::named(name.as_str())))
        .collect::<Result<_>>()?;
    let blocked_tags: HashSet<String> = params.blocked_tags.iter().map(|t| tag_key(t)).collect();
    let filters = Filters {
        profile: &profile,
        recommended: &recommended,
        durations: &durations,
        tags: &tags,
        blocked_artists: &blocked_artists,
        blocked_tags: &blocked_tags,
    };
    let (kept, rejected) = filters.apply(candidates);
    info!(
        "{} candidates pass the filters; rejected {rejected:?}",
        kept.len()
    );

    // 4. Beliefs.
    let beliefs = beliefs(
        store,
        service,
        &plays,
        now,
        params.decay_rho,
        params.dry_run,
    )?;

    // 5. Select and resolve.
    let resolver = Resolver {
        store,
        profile: &profile,
        recommended: &recommended,
    };
    let (selection, playable) = select_playable(
        kept,
        &beliefs,
        &tags,
        &profile,
        &params.select,
        &resolver,
        service,
        rng.random(),
    )?;

    let names = store.artist_names(present::mentioned_artists(&selection.picks))?;
    let picks: Vec<(Candidate, TrackRef, String)> = selection
        .picks
        .into_iter()
        .filter_map(|c| {
            let track = playable.get(&c.track)?.clone();
            let why = present::rationale(&c, &names);
            Some((c, track, why))
        })
        .collect();
    if picks.is_empty() {
        bail!("no playable candidates this week");
    }

    // 6. Publish.
    let playlist = if params.dry_run {
        None
    } else {
        Some(publish(store, service, params, week, now, &picks)?)
    };
    Ok(Built {
        week,
        picks,
        planned: selection.planned,
        shortfall: selection.shortfall,
        rejected,
        playlist,
    })
}

fn near_candidates<K: Knowledge>(
    gather: &Gather<K>,
    profile: &Profile,
    seeds: &[ArtistId],
    params: &BuildParams,
) -> Result<Vec<Candidate>> {
    let similar = gather.similar(seeds)?;
    let mut scored = near::score_artists(profile, seeds, &similar);
    scored.truncate(NEAR_SHORTLIST);
    let shortlist: Vec<ArtistId> = scored.iter().map(|a| a.artist).collect();
    let listeners = gather.listeners(&shortlist)?;
    let popular = gather.popular(&shortlist)?;
    Ok(near::candidates(
        &scored,
        &listeners,
        &popular,
        params.popularity_gamma,
        TRACKS_PER_ARTIST,
    ))
}

fn deep_candidates<K: Knowledge>(
    gather: &Gather<K>,
    profile: &Profile,
    service: &mut impl ListeningService,
) -> Result<Vec<Candidate>> {
    let artists = deep::artists_to_explore(profile);
    let catalogs = gather.catalogs(&artists, service)?;
    Ok(deep::candidates(profile, &catalogs))
}

fn far_candidates<K: Knowledge>(
    gather: &Gather<K>,
    profile: &Profile,
    seeds: &[ArtistId],
    params: &BuildParams,
    rng: &mut impl Rng,
) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();

    // Rare tags.
    let liked: Vec<ArtistId> = profile
        .ranked_artists()
        .iter()
        .take(TAG_SHARE_ARTISTS)
        .map(|(a, _)| *a)
        .collect();
    let shares = tag_shares(profile, &gather.tags(&liked)?);
    let universe = far::tag_universe(&gather.store.tags(None)?, TAG_UNIVERSE_MIN_ARTISTS);
    let skip = params.far_skip_top_ranks;
    for tag in far::sample_tags(&universe, &shares, FAR_TAGS, TagSampling::default(), rng) {
        let chart = match gather.tag_chart(&tag, skip..skip + FAR_CHART_DEPTH) {
            Ok(chart) => chart,
            Err(e) => {
                warn!("chart for tag {tag:?}: {e:#}");
                continue;
            }
        };
        let unheard: Vec<ArtistId> = chart
            .into_iter()
            .filter(|a| !profile.knows(*a))
            .take(FAR_CHART_ARTISTS)
            .collect();
        let popular = gather.popular(&unheard)?;
        let share = shares.get(&tag).copied().unwrap_or(0.0);
        candidates.extend(far::from_tag_chart(
            &tag,
            share,
            &unheard,
            profile,
            &popular,
            FAR_TRACKS_PER_ARTIST,
        ));
    }

    // Random walks.
    let mut walks = Vec::new();
    for _ in 0..FAR_WALKS {
        let Some(&start) = seeds.choose(rng) else {
            break;
        };
        let mut neighbors = |a| gather.similar_to(a);
        if let Some(path) = far::walk(start, WALK_HOPS, &mut neighbors, profile, rng) {
            walks.push(path);
        }
    }
    let ends: Vec<ArtistId> = walks.iter().filter_map(|w| w.last().copied()).collect();
    let popular = gather.popular(&ends)?;
    candidates.extend(far::from_walks(&walks, &popular, FAR_TRACKS_PER_ARTIST));
    Ok(candidates)
}

/// The bandit's posterior for each pool, from every past pick's outcome.
/// Saves made since the first playlist are synced first, since saving counts.
fn beliefs(
    store: &Store,
    service: &mut impl ListeningService,
    plays: &[crate::model::Play],
    now: Timestamp,
    rho: f64,
    dry_run: bool,
) -> Result<BTreeMap<Pool, Posterior>> {
    let ledger = store.ledger()?;
    if let Some(first) = ledger.first()
        && !dry_run
    {
        match service.saved_since(first.published_at) {
            Ok(saved) => {
                store.record_saved(&saved)?;
            }
            Err(e) => warn!("couldn't sync saved tracks: {e:#}"),
        }
    }
    let outcomes = feedback::outcomes(&ledger, plays, &store.saved_tracks()?);
    let rewarded: Vec<(&Recommendation, feedback::Reward)> = ledger
        .iter()
        .zip(&outcomes)
        .filter_map(|(rec, outcome)| feedback::reward(outcome).map(|r| (rec, r)))
        .collect();
    let arms = feedback::posteriors(&rewarded, now, rho);
    let beliefs = feedback::pool_posteriors(&arms);
    info!(
        "beliefs from {} rewarded picks: {beliefs:?}",
        rewarded.len()
    );
    Ok(beliefs)
}

/// Matches picks to playable tracks on the listening service.
struct Resolver<'a> {
    store: &'a Store,
    profile: &'a Profile,
    recommended: &'a HashSet<TrackId>,
}

/// What resolving a pick to the listening service found.
enum Resolution {
    Playable(TrackRef),
    /// The service has no match. Usually it doesn't carry the artist, or
    /// knows them by another name, so their other tracks won't match either.
    NoMatch,
    /// The service's copy is too short or long, or turns out to be a song
    /// already heard or recommended under another title.
    Unplayable,
}

impl Resolver<'_> {
    fn resolve(&self, track: TrackId, service: &mut impl ListeningService) -> Result<Resolution> {
        let wanted = self.store.track_ref(track)?;
        let reject = |resolution: Resolution, reason: String| {
            info!(
                "unplayable: {} – {}: {reason}",
                wanted.artist.name, wanted.title
            );
            Ok(resolution)
        };
        let found = if wanted.ids.get(service.namespace()).is_some() {
            wanted.clone()
        } else {
            let Some(found) = service.find(&wanted)? else {
                return reject(Resolution::NoMatch, "no match on the service".into());
            };
            let same = self.store.resolve_track(&found)?;
            if same != track {
                if self.profile.heard(same) || self.recommended.contains(&same) {
                    return reject(
                        Resolution::Unplayable,
                        format!(
                            "the match, \"{}\", is a song already heard or recommended",
                            found.title
                        ),
                    );
                }
                self.store.merge_track_into(track, same)?;
            }
            self.store.record_match(track, &found)?;
            found
        };
        match found.duration_ms {
            Some(ms) if !(MIN_DURATION_MS..=MAX_DURATION_MS).contains(&ms) => {
                reject(Resolution::Unplayable, format!("{} s long", ms / 1000))
            }
            _ => Ok(Resolution::Playable(found)),
        }
    }
}

/// Selects, resolves the picks, and re-selects without the unplayable ones
/// until every pick plays. A pick with no match on the service takes its
/// artist's other candidates out too. The selector reuses `seed` each round,
/// so a round changes only what the last round couldn't resolve.
#[allow(clippy::too_many_arguments)]
fn select_playable(
    candidates: Vec<Candidate>,
    beliefs: &BTreeMap<Pool, Posterior>,
    tags: &ArtistTags,
    profile: &Profile,
    params: &SelectParams,
    resolver: &Resolver,
    service: &mut impl ListeningService,
    seed: u64,
) -> Result<(select::Selection, HashMap<TrackId, TrackRef>)> {
    let mut playable: HashMap<TrackId, TrackRef> = HashMap::new();
    let mut unplayable: HashSet<TrackId> = HashSet::new();
    let mut unmatched_artists: HashSet<ArtistId> = HashSet::new();
    for round in 1..=RESOLVE_ROUNDS {
        let remaining: Vec<Candidate> = candidates
            .iter()
            .filter(|c| !unplayable.contains(&c.track) && !unmatched_artists.contains(&c.artist))
            .cloned()
            .collect();
        let selection = select::select(
            remaining,
            beliefs,
            tags,
            profile,
            params,
            &mut StdRng::seed_from_u64(seed),
        );
        let mut all_played = true;
        for pick in &selection.picks {
            if playable.contains_key(&pick.track) {
                continue;
            }
            match resolver.resolve(pick.track, service)? {
                Resolution::Playable(track) => {
                    playable.insert(pick.track, track);
                }
                Resolution::NoMatch => {
                    unplayable.insert(pick.track);
                    unmatched_artists.insert(pick.artist);
                    all_played = false;
                }
                Resolution::Unplayable => {
                    unplayable.insert(pick.track);
                    all_played = false;
                }
            }
        }
        if all_played || round == RESOLVE_ROUNDS {
            info!("{} picks unplayable after {round} rounds", unplayable.len());
            return Ok((selection, playable));
        }
    }
    unreachable!("the last round returns")
}

fn publish(
    store: &Store,
    service: &mut impl ListeningService,
    params: &BuildParams,
    week: Week,
    now: Timestamp,
    picks: &[(Candidate, TrackRef, String)],
) -> Result<String> {
    let recs: Vec<Recommendation> = picks
        .iter()
        .enumerate()
        .map(|(position, (c, _, why))| Recommendation {
            week,
            published_at: now,
            position: position as u32,
            track: c.track,
            artist: c.artist,
            pool: c.pool,
            arm: c.arm.clone(),
            score: c.relevance,
            rationale: why.clone(),
        })
        .collect();
    let tracks: Vec<TrackRef> = picks.iter().map(|(_, t, _)| t.clone()).collect();
    let chosen: Vec<Candidate> = picks.iter().map(|(c, _, _)| c.clone()).collect();
    let description = present::description(week, &chosen);
    let existing = store.setting(PLAYLIST_ID)?;

    let playlist = store.record_recommendations(&recs, || {
        service.publish(
            existing.as_deref(),
            &params.playlist_name,
            &description,
            &tracks,
        )
    })?;
    store.set_setting(PLAYLIST_ID, &playlist)?;
    if params.archive {
        service.publish(None, &format!("Novelty {week}"), &description, &tracks)?;
    }
    Ok(playlist)
}
