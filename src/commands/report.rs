//! `novelty report`: how picks fare and whether my listening is broadening
//! (spec §10), as text.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Result, bail};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};

use super::App;
use crate::cli::{ReportArgs, ReportFormat};
use crate::engine::feedback;
use crate::engine::report::{Listening, PickStats, Streams, TOP_ARTISTS, tag_spread};
use crate::model::{ArtistId, Pool, Recommendation, Week};
use crate::store::Store;

/// Listening rows look back this far from the end of each week.
const WINDOW_DAYS: i64 = 30;

pub fn run(app: &App, args: &ReportArgs) -> Result<()> {
    if matches!(args.format, ReportFormat::Svg) {
        bail!("`novelty report --format svg` is not implemented yet; use text");
    }
    let store = app.open_store()?;
    let tz = TimeZone::get(&app.config.build.timezone)?;
    print!(
        "{}",
        report(&store, &tz, args.weeks as usize, Timestamp::now())?
    );
    Ok(())
}

pub fn report(store: &Store, tz: &TimeZone, weeks: usize, now: Timestamp) -> Result<String> {
    let plays = store.plays()?;
    let ledger = store.ledger()?;
    let outcomes = feedback::outcomes(&ledger, &plays, &store.saved_tracks()?);
    let mut out = String::new();

    let shown: Vec<Week> = store.ledger_weeks()?.into_iter().take(weeks).collect();
    if shown.is_empty() {
        writeln!(out, "No playlists published yet.\n")?;
    } else {
        picks_section(&mut out, store, &shown, &ledger, &outcomes)?;
    }

    let streams = Streams::new(&plays, &ledger);
    let Some(first) = streams.first() else {
        writeln!(out, "No listening on file yet.")?;
        return Ok(out);
    };
    weekly_listening(&mut out, &streams, tz, weeks, now)?;
    monthly_listening(&mut out, &streams, tz, first, now)?;
    writeln!(
        out,
        "\nStreams are listens of 30 s or more. Top-{TOP_ARTISTS}: the share of streams by \
         the {TOP_ARTISTS} most-streamed\nartists. Gini: 0 when artists share streams \
         evenly, toward 1 when a few take nearly all.\n\"New\": a first stream as far back \
         as the history on file goes ({}), so the first months\ncount much that was already \
         familiar as new.",
        first.to_zoned(tz.clone()).date()
    )?;
    Ok(out)
}

fn picks_section(
    out: &mut String,
    store: &Store,
    weeks: &[Week],
    ledger: &[Recommendation],
    outcomes: &[feedback::Outcome],
) -> Result<()> {
    let artists: Vec<ArtistId> = ledger.iter().map(|r| r.artist).collect();
    let tags = store.tags(Some(&artists))?;
    let mut by_pool: BTreeMap<Pool, PickStats> = BTreeMap::new();

    writeln!(
        out,
        "Picks by week (completed, skipped and kept are of played)"
    )?;
    writeln!(
        out,
        "{:<9} {:>5}  {:>9}  {:>9}  {:>9}  {:>9}  {:<13} {:>6}",
        "week", "picks", "played", "completed", "skipped", "kept", "deep/near/far", "spread"
    )?;
    for week in weeks {
        let mut stats = PickStats::default();
        let mut split: BTreeMap<Pool, usize> = BTreeMap::new();
        let mut week_artists = Vec::new();
        for (rec, outcome) in ledger.iter().zip(outcomes).filter(|(r, _)| r.week == *week) {
            stats.add(outcome);
            by_pool.entry(rec.pool).or_default().add(outcome);
            *split.entry(rec.pool).or_default() += 1;
            week_artists.push(rec.artist);
        }
        let split = Pool::ALL
            .iter()
            .map(|p| split.get(p).copied().unwrap_or(0).to_string())
            .collect::<Vec<_>>()
            .join("/");
        let spread = tag_spread(week_artists, &tags)
            .map_or_else(|| "–".to_owned(), |bits| format!("{bits:.1} b"));
        writeln!(
            out,
            "{:<9} {}  {split:<13} {spread:>6}",
            week.to_string(),
            cells(&stats)
        )?;
    }

    writeln!(out, "\nPicks by pool, over those weeks")?;
    writeln!(
        out,
        "{:<9} {:>5}  {:>9}  {:>9}  {:>9}  {:>9}",
        "pool", "picks", "played", "completed", "skipped", "kept"
    )?;
    for (pool, stats) in &by_pool {
        writeln!(out, "{:<9} {}", pool.to_string(), cells(stats))?;
    }
    writeln!(
        out,
        "(spread: entropy of the picks' dominant tags; log2(40) = 5.3 b would be a different \
         tag for every pick)\n"
    )?;
    Ok(())
}

/// The counts of one row of picks, each with its share.
fn cells(s: &PickStats) -> String {
    format!(
        "{:>5}  {}  {}  {}  {}",
        s.picks,
        count_share(s.played, s.picks),
        count_share(s.completed, s.played),
        count_share(s.skipped, s.played),
        count_share(s.kept, s.played),
    )
}

fn count_share(n: usize, of: usize) -> String {
    if of == 0 {
        format!("{n:>4}    –")
    } else {
        format!("{n:>4} {:>3.0}%", 100.0 * n as f64 / of as f64)
    }
}

fn listening_header(out: &mut String, label: &str) -> Result<()> {
    writeln!(
        out,
        "{label:<17} {:>7} {:>7} {:>7} {:>5} {:>10} {:>11} {:>10}",
        "streams",
        "artists",
        format!("top-{TOP_ARTISTS}"),
        "gini",
        "new tracks",
        "new artists",
        "from picks"
    )?;
    Ok(())
}

fn listening_row(out: &mut String, label: &str, l: Option<Listening>) -> Result<()> {
    let Some(l) = l else {
        writeln!(out, "{label:<17} {:>7}", 0)?;
        return Ok(());
    };
    let pct = |x: f64| format!("{:.0}%", 100.0 * x);
    writeln!(
        out,
        "{label:<17} {:>7} {:>7} {:>7} {:>5.2} {:>10} {:>11} {:>10}",
        l.streams,
        l.artists,
        pct(l.top_share),
        l.gini,
        pct(l.new_tracks),
        pct(l.new_artists),
        pct(l.from_picks),
    )?;
    Ok(())
}

/// One row per recent week: the 30 days to its end (to now, for this week).
fn weekly_listening(
    out: &mut String,
    streams: &Streams,
    tz: &TimeZone,
    weeks: usize,
    now: Timestamp,
) -> Result<()> {
    writeln!(
        out,
        "Listening in the {WINDOW_DAYS} days to the end of each week"
    )?;
    listening_header(out, "week")?;
    let this_week = Week::containing(now, tz)
        .starts_at(tz)?
        .to_zoned(tz.clone());
    for k in 0..weeks.max(1) as i64 {
        let start = this_week.checked_sub(k.weeks())?;
        let end = start.checked_add(1.week())?.timestamp().min(now);
        let from = end.checked_sub(jiff::SignedDuration::from_hours(24 * WINDOW_DAYS))?;
        let week = Week::containing(start.timestamp(), tz);
        let label = if k == 0 {
            format!("{week} (so far)")
        } else {
            week.to_string()
        };
        listening_row(out, &label, streams.listening(from, end))?;
    }
    writeln!(out)?;
    Ok(())
}

/// One row per calendar month since the history begins.
fn monthly_listening(
    out: &mut String,
    streams: &Streams,
    tz: &TimeZone,
    first: Timestamp,
    now: Timestamp,
) -> Result<()> {
    writeln!(out, "Listening by month")?;
    listening_header(out, "month")?;
    let mut month = first
        .to_zoned(tz.clone())
        .date()
        .first_of_month()
        .to_zoned(tz.clone())?;
    while month.timestamp() < now {
        let next = month.checked_add(1.month())?;
        let label = month.strftime("%Y-%m").to_string();
        listening_row(
            out,
            &label,
            streams.listening(month.timestamp(), next.timestamp().min(now)),
        )?;
        month = next;
    }
    Ok(())
}
