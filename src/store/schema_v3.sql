-- Version 3: other names an artist goes by. When two artists turn out to be
-- one act (see Store::merge_artists_sharing_songs), the merged one's name
-- keeps resolving to the artist that was kept.
CREATE TABLE artist_aliases (
    name_key  TEXT PRIMARY KEY,       -- model::names::name_key of `name`
    name      TEXT NOT NULL,
    artist_id INTEGER NOT NULL REFERENCES artists (artist_id)
);
CREATE INDEX artist_aliases_by_artist ON artist_aliases (artist_id);
