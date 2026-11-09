-- The canonical state. Two kinds of rows live here:
--   facts:     what integrations observed (artists, tracks, plays, knowledge)
--   decisions: what the app recommended (ledger)
-- Everything else (affinity, outcomes, rewards, bandit arms) is derived from
-- these on each run, so changing a formula never needs a data migration.
--
-- Times are unix milliseconds, UTC.

-- Artists. Identity is the normalized name (see model::names::name_key).
CREATE TABLE artists (
    artist_id INTEGER PRIMARY KEY,
    name      TEXT NOT NULL,
    name_key  TEXT NOT NULL UNIQUE,
    listeners INTEGER                 -- popularity, from a knowledge source
);

-- Tracks. Identity is (artist, normalized title), so remasters and live
-- versions of one song are one track.
CREATE TABLE tracks (
    track_id    INTEGER PRIMARY KEY,
    artist_id   INTEGER NOT NULL REFERENCES artists (artist_id),
    title       TEXT NOT NULL,
    title_key   TEXT NOT NULL,
    album       TEXT,
    duration_ms INTEGER,
    UNIQUE (artist_id, title_key)
);

-- External IDs (model::Namespace). Many can point at one artist or track:
-- a song's single and album releases have different Spotify IDs.
CREATE TABLE artist_ids (
    namespace TEXT NOT NULL,
    value     TEXT NOT NULL,
    artist_id INTEGER NOT NULL REFERENCES artists (artist_id),
    PRIMARY KEY (namespace, value)
);
CREATE INDEX artist_ids_by_artist ON artist_ids (artist_id);

CREATE TABLE track_ids (
    namespace TEXT NOT NULL,
    value     TEXT NOT NULL,
    track_id  INTEGER NOT NULL REFERENCES tracks (track_id),
    PRIMARY KEY (namespace, value)
);
CREATE INDEX track_ids_by_track ON track_ids (track_id);

-- Listens (model::Listen). The same listen seen twice by one source is
-- dropped by the UNIQUE key; a history import replaces polled plays over the
-- span it covers.
CREATE TABLE plays (
    play_id    INTEGER PRIMARY KEY,
    track_id   INTEGER NOT NULL REFERENCES tracks (track_id),
    started_at INTEGER NOT NULL,
    ms_played  INTEGER,               -- NULL while the poller can't tell yet
    ending     TEXT NOT NULL,         -- model::Ending
    started_by TEXT NOT NULL,         -- model::StartedBy
    provenance TEXT NOT NULL,         -- model::Provenance
    UNIQUE (track_id, started_at)
);
CREATE INDEX plays_by_time ON plays (started_at);

-- Tracks in the listener's library, and since when.
CREATE TABLE saved_tracks (
    track_id INTEGER PRIMARY KEY REFERENCES tracks (track_id),
    saved_at INTEGER NOT NULL
);

-- Knowledge, per source ('lastfm', ...). Rewritten whole for an artist each
-- time that artist is fetched.
CREATE TABLE similar_artists (
    artist_id  INTEGER NOT NULL REFERENCES artists (artist_id),
    similar_id INTEGER NOT NULL REFERENCES artists (artist_id),
    similarity REAL NOT NULL,         -- [0, 1]
    source     TEXT NOT NULL,
    PRIMARY KEY (artist_id, similar_id, source)
);

CREATE TABLE artist_tags (
    artist_id INTEGER NOT NULL REFERENCES artists (artist_id),
    tag       TEXT NOT NULL,          -- model::names::tag_key
    weight    REAL NOT NULL,          -- [0, 1]
    source    TEXT NOT NULL,
    PRIMARY KEY (artist_id, tag, source)
);
CREATE INDEX artist_tags_by_tag ON artist_tags (tag);

CREATE TABLE popular_tracks (
    artist_id INTEGER NOT NULL REFERENCES artists (artist_id),
    rank      INTEGER NOT NULL,       -- 0 is the most played
    track_id  INTEGER NOT NULL REFERENCES tracks (track_id),
    listeners INTEGER,
    source    TEXT NOT NULL,
    PRIMARY KEY (artist_id, source, rank)
);

-- Every recommendation ever published. UNIQUE (track_id) is the "never
-- recommend the same song twice" guarantee (spec G1).
CREATE TABLE ledger (
    rec_id       INTEGER PRIMARY KEY,
    week         TEXT NOT NULL,       -- ISO week, e.g. '2026-W46'
    published_at INTEGER NOT NULL,
    position     INTEGER NOT NULL,
    track_id     INTEGER NOT NULL UNIQUE REFERENCES tracks (track_id),
    artist_id    INTEGER NOT NULL REFERENCES artists (artist_id),
    pool         TEXT NOT NULL,       -- 'deep' | 'near' | 'far'
    arm          TEXT NOT NULL,       -- e.g. 'pool:far', later 'tag:dub techno'
    score        REAL NOT NULL,
    rationale    TEXT NOT NULL
);
CREATE INDEX ledger_by_week ON ledger (week);

-- Raw API responses, so re-running a build doesn't re-fetch. Misses are
-- cached too.
CREATE TABLE api_cache (
    key        TEXT PRIMARY KEY,
    body       TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    ttl_s      INTEGER NOT NULL
);

-- One row per poll or build, for alerting on repeated failures.
CREATE TABLE runs (
    run_id      INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,        -- 'poll' | 'build' | 'ingest'
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    ok          INTEGER,
    notes       TEXT
);
