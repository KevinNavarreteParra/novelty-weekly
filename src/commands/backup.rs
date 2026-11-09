//! `novelty backup`: a consistent copy of the database, for the nightly timer.
//! The ledger exists nowhere else; everything else could be fetched again.

use std::fs;

use anyhow::{Context, Result};
use jiff::Zoned;
use jiff::civil::Date;

use super::App;

/// Backups older than this many days are deleted.
const KEEP_DAYS: i64 = 30;

pub fn run(app: &App) -> Result<()> {
    let store = app.open_store()?;
    let dir = app.paths.home.join("backups");
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let today = Zoned::now().date();
    let path = dir.join(format!("novelty-{today}.db"));
    if path.exists() {
        fs::remove_file(&path)?;
    }
    store.back_up_to(&path)?;

    let oldest = today.checked_sub(jiff::Span::new().days(KEEP_DAYS))?;
    let mut pruned = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let date = name
            .to_str()
            .and_then(|n| n.strip_prefix("novelty-")?.strip_suffix(".db"))
            .and_then(|d| d.parse::<Date>().ok());
        if date.is_some_and(|d| d < oldest) {
            fs::remove_file(entry.path())?;
            pruned += 1;
        }
    }
    println!(
        "backed up to {}; removed {pruned} older than {KEEP_DAYS} days",
        path.display()
    );
    Ok(())
}
