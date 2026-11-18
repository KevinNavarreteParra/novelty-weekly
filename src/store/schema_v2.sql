-- Version 2: what I know apart from plays (model::Heard), from a data export.
-- Each table is replaced whole by an import that has it.

-- Tracks in playlists I made. The app's own playlists are never recorded.
CREATE TABLE playlist_tracks (
    playlist TEXT NOT NULL,           -- the playlist's name
    track_id INTEGER NOT NULL REFERENCES tracks (track_id),
    added_on TEXT,                    -- ISO date, e.g. '2020-06-03'
    PRIMARY KEY (playlist, track_id)
);
CREATE INDEX playlist_tracks_by_track ON playlist_tracks (track_id);

-- Albums in my library. Identity is the artist plus the title's key, like a
-- track's, so a deluxe edition is the album.
CREATE TABLE saved_albums (
    artist_id INTEGER NOT NULL REFERENCES artists (artist_id),
    title     TEXT NOT NULL,
    title_key TEXT NOT NULL,          -- model::names::title_key
    PRIMARY KEY (artist_id, title_key)
);

-- How much I listen to an artist, as a listening service rates it.
CREATE TABLE artist_familiarity (
    artist_id INTEGER NOT NULL REFERENCES artists (artist_id),
    level     TEXT NOT NULL,          -- model::Familiarity
    source    TEXT NOT NULL,          -- e.g. 'spotify'
    PRIMARY KEY (artist_id, source)
);
