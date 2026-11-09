//! Normalized keys for names, so the same artist, song or tag compares equal no
//! matter which service spelled it.
//!
//! These keys are identity: the store matches artists on [`name_key`] and tracks
//! on (artist, [`title_key`]). Changing a rule here changes what counts as "the
//! same", so bump [`KEY_RULES`] with it: the store then rebuilds its keys.

/// The version of the rules below. The store rebuilds every key when it was
/// keyed under an older version.
pub const KEY_RULES: u32 = 2;

/// Words that mark a title segment as a version of the song rather than part of
/// its name: "Song - Remastered 2011", "Song (Live)", "Song [Radio Edit]".
///
/// "remix" is deliberately absent: a remix by someone else is a new listen.
const VERSION_WORDS: &[&str] = &[
    "remaster",
    "remastered",
    "live",
    "edit",
    "version",
    "mono",
    "stereo",
    "acoustic",
    "demo",
    "deluxe",
    "bonus",
    "explicit",
    "instrumental",
];

/// Leading words of a bracketed credit: "Song (feat. X)", "Song [with Y]".
const CREDIT_WORDS: &[&str] = &["feat", "ft", "featuring", "with"];

/// Apostrophes join a word rather than split it: "Don't" keys as "dont",
/// matching sources that drop the apostrophe.
const APOSTROPHES: [char; 4] = ['\'', '\u{2019}', '\u{2018}', '`'];

/// Lowercase, letters and digits only, single spaces; "&" reads as "and" and
/// "$" as "s", so "Travi$ Scott" is "Travis Scott".
///
/// `"The  Beatles!"` and `"the beatles"` both become `"the beatles"`. A name made
/// only of punctuation (the band "!!!") keeps its punctuation, so it never
/// collapses to an empty key.
pub fn name_key(name: &str) -> String {
    let lowered = name
        .to_lowercase()
        .replace(APOSTROPHES, "")
        .replace('&', " and ")
        .replace('$', "s");
    let key = words(&lowered).join(" ");
    if key.is_empty() {
        name.trim().to_lowercase()
    } else {
        key
    }
}

/// A title reduced to the song itself, for "have I heard this?" checks.
///
/// Drops version markers and featured-artist credits, so "Song - Remastered
/// 2011", "Song (Live)" and "Song (feat. X)" all key as `"song"`.
pub fn title_key(title: &str) -> String {
    let lowered = title.to_lowercase();
    let without_brackets = drop_bracketed_versions(&lowered);
    let main = drop_dash_version(&without_brackets);
    name_key(main)
}

/// Lowercase, with hyphens and underscores as spaces: "Hip-Hop" keys as "hip hop".
pub fn tag_key(tag: &str) -> String {
    let lowered = tag.to_lowercase().replace(['-', '_'], " ");
    lowered.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Splits on anything that isn't a letter or digit.
fn words(s: &str) -> Vec<&str> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

fn is_version_segment(segment: &str) -> bool {
    let ws = words(segment);
    ws.iter().any(|w| VERSION_WORDS.contains(w))
        || ws.first().is_some_and(|w| CREDIT_WORDS.contains(w))
}

/// Removes `(...)` and `[...]` groups that hold a version marker or a credit.
fn drop_bracketed_versions(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut rest = title;
    while let Some(open) = rest.find(['(', '[']) {
        let close_char = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(close) = rest[open..].find(close_char).map(|i| open + i) else {
            break;
        };
        out.push_str(&rest[..open]);
        let inner = &rest[open + 1..close];
        if !is_version_segment(inner) {
            out.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Cuts "Song - 2011 Remaster" at the first " - " whose tail is a version marker.
fn drop_dash_version(title: &str) -> &str {
    let mut search_from = 0;
    while let Some(i) = title[search_from..].find(" - ").map(|i| search_from + i) {
        if is_version_segment(&title[i + 3..]) {
            return &title[..i];
        }
        search_from = i + 3;
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_key_ignores_case_punctuation_and_spacing() {
        assert_eq!(name_key("The  Beatles!"), "the beatles");
        assert_eq!(name_key("AC/DC"), "ac dc");
        assert_eq!(name_key("Simon & Garfunkel"), "simon and garfunkel");
        assert_eq!(name_key("Sigur Rós"), "sigur rós");
        assert_eq!(name_key("Don’t Stop"), name_key("Dont Stop"));
    }

    #[test]
    fn a_dollar_sign_reads_as_s() {
        assert_eq!(name_key("Travi$ Scott"), name_key("Travis Scott"));
        assert_eq!(name_key("A$AP Rocky"), "asap rocky");
        assert_eq!(name_key("Joey Bada$$"), "joey badass");
    }

    #[test]
    fn name_key_never_empty_for_punctuation_names() {
        assert_eq!(name_key("!!!"), "!!!");
    }

    #[test]
    fn title_key_drops_version_suffixes() {
        for title in [
            "Heroes - 2017 Remaster",
            "Heroes - Remastered 2011",
            "Heroes (Live)",
            "Heroes [Single Version]",
            "Heroes (feat. Someone)",
            "Heroes - Live at Wembley (Remastered)",
        ] {
            assert_eq!(title_key(title), "heroes", "{title}");
        }
    }

    #[test]
    fn title_key_keeps_meaningful_parts() {
        // A remix is a different listen; parentheses that are part of the name stay.
        assert_eq!(title_key("Heroes (Moby Remix)"), "heroes moby remix");
        assert_eq!(title_key("(Don't Fear) The Reaper"), "dont fear the reaper");
        // "Live" inside the actual title is not a version marker.
        assert_eq!(title_key("Live Forever - Remastered"), "live forever");
        assert_eq!(title_key("Part One - Part Two"), "part one part two");
    }

    #[test]
    fn title_key_survives_unbalanced_brackets() {
        assert_eq!(title_key("Song (unfinished"), "song unfinished");
    }

    #[test]
    fn tag_key_merges_spelling_variants() {
        assert_eq!(tag_key("Hip-Hop"), "hip hop");
        assert_eq!(tag_key("  dub   techno "), "dub techno");
    }
}
