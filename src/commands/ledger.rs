//! `novelty ledger show`: past recommendations, newest week first, with how
//! each one has fared so far.

use anyhow::Result;

use super::App;
use crate::engine::feedback::{self, Outcome};
use crate::model::{Verdict, Week};

pub fn show(app: &App, week: Option<&str>) -> Result<()> {
    let store = app.open_store()?;
    let only: Option<Week> = week.map(str::parse).transpose()?;
    let ledger = store.ledger()?;
    let outcomes = feedback::outcomes(&ledger, &store.plays()?, &store.saved_tracks()?);
    let names = store.artist_names(ledger.iter().map(|r| r.artist))?;

    for week in store.ledger_weeks()? {
        if only.is_some_and(|w| w != week) {
            continue;
        }
        println!("{week}");
        for (rec, outcome) in ledger.iter().zip(&outcomes).filter(|(r, _)| r.week == week) {
            let track = store.track_ref(rec.track)?;
            println!(
                "{:>3}. {:<4} {:<10} {} – {}",
                rec.position + 1,
                rec.pool,
                fate(outcome),
                names.get(&rec.artist).map_or("?", String::as_str),
                track.title,
            );
        }
        println!();
    }
    Ok(())
}

/// One word for how a recommendation fared.
fn fate(outcome: &Outcome) -> String {
    let Some(first) = &outcome.first_play else {
        return "unplayed".into();
    };
    let mut word = match first.verdict() {
        Some(Verdict::Completed) => "completed",
        Some(Verdict::Skipped) => "skipped",
        Some(Verdict::Partial) => "partial",
        None => "playing",
    }
    .to_owned();
    if outcome.saved {
        word.push('+');
    }
    word
}
