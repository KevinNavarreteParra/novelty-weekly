//! The vocabulary of a recommendation: which pool it came from, which bandit
//! arm it counts toward, and which week's playlist it was in.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, anyhow};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::ids::{ArtistId, TrackId};
use super::stored_enum;

stored_enum! {
    /// The three kinds of novelty a playlist mixes (spec §7.2).
    pub enum Pool {
        /// Unheard tracks by artists I already like.
        Deep => "deep",
        /// Unheard artists close to my taste.
        Near => "near",
        /// Unheard artists in territory I barely listen to.
        Far => "far",
    }
}

/// A bandit arm: the unit the feedback loop learns a success rate for.
/// v2 learns per pool; v3 adds per-tag arms inside near and far.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Arm {
    Pool(Pool),
    Tag(String),
}

impl fmt::Display for Arm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Arm::Pool(pool) => write!(f, "pool:{pool}"),
            Arm::Tag(tag) => write!(f, "tag:{tag}"),
        }
    }
}

impl FromStr for Arm {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Arm> {
        match s.split_once(':') {
            Some(("pool", p)) => Pool::parse(p)
                .map(Arm::Pool)
                .ok_or_else(|| anyhow!("unknown pool in arm {s:?}")),
            Some(("tag", t)) if !t.is_empty() => Ok(Arm::Tag(t.to_owned())),
            _ => Err(anyhow!("not an arm: {s:?}")),
        }
    }
}

/// An ISO 8601 week, written `2026-W46`. Each build's playlist is one week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Week {
    year: i16,
    week: i8,
}

impl Week {
    /// The ISO week `at` falls in, on the wall clock of `tz`.
    pub fn containing(at: Timestamp, tz: &TimeZone) -> Week {
        let date = at.to_zoned(tz.clone()).date().iso_week_date();
        Week {
            year: date.year(),
            week: date.week(),
        }
    }
}

impl fmt::Display for Week {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-W{:02}", self.year, self.week)
    }
}

impl FromStr for Week {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Week> {
        let (year, week) = s
            .split_once("-W")
            .ok_or_else(|| anyhow!("expected a week like 2026-W46, got {s:?}"))?;
        let year: i16 = year.parse().with_context(|| format!("bad year in {s:?}"))?;
        let week: i8 = week.parse().with_context(|| format!("bad week in {s:?}"))?;
        // Validates the week number against the year (some years have 53).
        jiff::civil::ISOWeekDate::new(year, week, jiff::civil::Weekday::Monday)
            .with_context(|| format!("no such week: {s:?}"))?;
        Ok(Week { year, week })
    }
}

/// One published playlist entry, as the ledger holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    pub week: Week,
    pub published_at: Timestamp,
    pub position: u32,
    pub track: TrackId,
    pub artist: ArtistId,
    pub pool: Pool,
    pub arm: Arm,
    pub score: f64,
    pub rationale: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arms_round_trip_through_text() {
        for arm in [Arm::Pool(Pool::Far), Arm::Tag("dub techno".into())] {
            assert_eq!(arm.to_string().parse::<Arm>().unwrap(), arm);
        }
        assert!("pool:nope".parse::<Arm>().is_err());
        assert!("tag:".parse::<Arm>().is_err());
    }

    #[test]
    fn week_uses_the_local_calendar() {
        let tz = TimeZone::get("America/Los_Angeles").unwrap();
        // Monday 2026-11-09 05:00 UTC is still Sunday evening in Los Angeles.
        let at: Timestamp = "2026-11-09T05:00:00Z".parse().unwrap();
        assert_eq!(Week::containing(at, &tz).to_string(), "2026-W45");
        let later: Timestamp = "2026-11-09T14:00:00Z".parse().unwrap();
        assert_eq!(Week::containing(later, &tz).to_string(), "2026-W46");
    }

    #[test]
    fn weeks_parse_and_reject_nonsense() {
        assert_eq!("2026-W46".parse::<Week>().unwrap().to_string(), "2026-W46");
        assert_eq!("2026-W03".parse::<Week>().unwrap().to_string(), "2026-W03");
        assert!("2026-W54".parse::<Week>().is_err());
        assert!("2026-46".parse::<Week>().is_err());
    }
}
