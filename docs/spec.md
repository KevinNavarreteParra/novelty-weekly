# Novelty Weekly — a self-hosted Spotify discovery engine

Status: draft spec, not started
Owner: Kevin
Last updated: 2026-11-09

---

## 1. Problem

Spotify DJ and the "made for you" mixes converge on a small set of familiar tracks. Steering by deliberately playing different songs makes it worse: those plays feed the same engagement-optimized model, and the new songs get absorbed into the loop.

Goal: a weekly playlist where **novelty is the objective function**, not a side effect — new tracks from known artists, new artists near my taste, and genuinely unfamiliar genres — with a feedback loop that learns what lands *without* collapsing back onto the familiar.

## 2. Goals and non-goals

### Goals
- G1. Every recommended track is one I have never played (per lifetime history) and never been recommended before. *(Holds for v0–v3. v4 relaxes it deliberately — see §13.)*
- G2. Each playlist spans three kinds of novelty: deep cuts, near-neighbor artists, far jumps.
- G3. Learn from completions/skips which kinds of novelty work, while guaranteeing a floor of exploration.
- G4. Fully automated: weekly build + continuous listen polling, unattended on the home server.
- G5. Small, auditable dependency footprint.

### Non-goals
- Replacing Spotify as a player.
- Multi-user / public app (Spotify Development Mode caps at 5 users anyway).
- Audio-content analysis (Spotify audio features are gone; out of scope for v0–v2).
- Real-time / per-song steering like DJ.

## 3. Constraints (Spotify Web API, as of Nov 2026)

Development Mode rules (Feb 2026 change, applied to existing apps from 2026-03-09):
- App owner must have **Spotify Premium**; app stops working if it lapses.
- Max **5 authorized users** per Client ID (fine — it's just me).
- Refresh tokens **expire 6 months** after original authorization → need a re-auth reminder.
- Dev-mode quota is shared per developer account; 429s may carry `reason: QUOTA_EXCEEDED`.

Removed / unusable — do **not** design around these:
- `GET /recommendations`, audio features, related artists (restricted for new apps since Nov 2024).
- `GET /artists/{id}/top-tracks`, batch `GET /tracks` / `GET /artists` / `GET /albums`.
- `popularity` on tracks/albums/artists; `followers` on artists.
- `/playlists/{id}/tracks` (renamed to `/items`).

Still available (per Feb 2026 changelog):
- `GET /me/player/recently-played` (max 50 items per call — must poll).
- `GET /me/top/{type}`, `GET /me/tracks`.
- `GET /artists/{id}/albums`, `GET /albums/{id}/tracks`, single `GET /tracks/{id}`.
- `GET /search` — `limit` max **10**, default 5. Supports `isrc:` filter.
- `POST /me/playlists`, `POST|PUT|GET|DELETE /playlists/{id}/items`.
- `external_ids` (ISRC) on tracks — removal was reverted. Key for cross-referencing MusicBrainz.

Unverified (check before relying on): artist `genres` field — not listed as removed, not confirmed present. Design does **not** depend on it; tags come from Last.fm/MusicBrainz.

**Design consequence:** Spotify is I/O only (read listens, resolve tracks, write playlist). All recommendation intelligence lives outside it.

## 4. Architecture

```
 Extended history (one-off zip) ─┐
 Recently-played poller (30 min) ─┼──► SQLite store ──► weekly build:
 Last.fm + MusicBrainz (cached) ──┘        ▲            profile → pools → selector → writer
                                           │                                          │
                                           └────── outcomes (skips/completions) ◄─────┘
```

Two scheduled entry points:
- `novelty poll` — every 30 min: pull recently played, append to `plays`, update `outcomes`.
- `novelty build` — weekly (Mon ~06:00 local): rebuild profile, generate candidates, select, publish.

## 5. Data sources

### 5.1 Spotify Extended Streaming History (one-off)
- Request: spotify.com → Account → Privacy → Download your data → tick **only** Extended streaming history. Takes up to ~30 days.
- Format: ZIP of `Streaming_History_Audio_*.json`. Relevant fields:
  - `ts` (UTC, stream **end** time), `ms_played`
  - `spotify_track_uri`, `master_metadata_track_name`, `master_metadata_album_artist_name`, `master_metadata_album_album_name`
  - `reason_start`, `reason_end` (e.g. `trackdone`, `fwdbtn`, `endplay`)
  - `skipped` (unreliable — sometimes null when it was a skip), `shuffle`, `offline`, `incognito_mode`
- Ignore podcast rows (`episode_name` non-null / track URI null).
- Derive `skipped_inferred = skipped OR reason_end = 'fwdbtn' OR ms_played < 30_000`.
- Start time = `ts - ms_played`.
- Treat the archive as untrusted input: extract into its own empty directory, parse with `python -I`.

### 5.2 Spotify recently-played (continuous)
- `GET /me/player/recently-played?limit=50&after={last_cursor_ms}`.
- Returns `played_at` (start-ish timestamp) and full track object (incl. `duration_ms`, `external_ids.isrc`), but **no `ms_played` and no skip flag**.
- Infer completion from gap to next play: `ms_played_est = min(duration_ms, next.played_at - this.played_at)`. Last item of a batch stays `pending` until the next poll.
- Gap inference fails across pauses / device switches → cap at `duration_ms`, flag `source='poll_est'`.
- Missed polls >~2.5 h of continuous listening lose data. Acceptable; log gaps.

### 5.3 Spotify top items (cold start only)
- Until the history export arrives: `GET /me/top/artists?time_range=long_term&limit=50` (+ `medium_term`) and `GET /me/tracks` as a pseudo-history with synthetic weights.

### 5.4 Last.fm API (primary similarity + tags)
- Needs an API key (free). No OAuth needed for these read methods.
- Methods:
  - `artist.getSimilar(artist|mbid, limit=100)` → `match ∈ [0,1]`
  - `artist.getTopTracks(artist|mbid, limit=50)` → playcount, listeners
  - `artist.getTopTags(artist|mbid)` → tag + count (0–100)
  - `artist.getInfo` → listeners (popularity proxy, replaces Spotify `popularity`)
  - `tag.getTopArtists(tag, page, limit)` → seeds for far jumps
  - `tag.getSimilar` (spotty; optional)
- Self-imposed rate limit: ≤ 4 req/s, exponential backoff on error 29.
- Cache responses in SQLite with TTL (similarity/tags: 60 days; top tracks: 30 days).

### 5.5 MusicBrainz (IDs, discographies, ISRC bridge)
- `GET /ws/2/isrc/{isrc}?inc=artist-credits` → recording MBID(s).
- `GET /ws/2/artist/{mbid}?inc=tags+genres` → curated genres (cleaner than Last.fm tags).
- Hard limit **1 req/s** and a descriptive `User-Agent` (`novelty-weekly/0.1 (contact)`). Use a generic contact, not personal email, unless I decide otherwise.
- Optional; v0 can skip MusicBrainz entirely and match on names.

## 6. Data model (SQLite)

```sql
CREATE TABLE plays (
  play_id      INTEGER PRIMARY KEY,
  started_at   INTEGER NOT NULL,          -- unix ms, UTC
  track_uri    TEXT NOT NULL,
  ms_played    INTEGER,
  completed    INTEGER,                   -- 1 if ms_played >= min(0.8*dur, 120000) and not skipped
  skipped      INTEGER,
  reason_end   TEXT,
  source       TEXT NOT NULL,             -- 'history' | 'poll' | 'poll_est' | 'top_seed'
  UNIQUE (started_at, track_uri)          -- dedupe history vs poller overlap
);

CREATE TABLE tracks (
  track_uri    TEXT PRIMARY KEY,
  isrc         TEXT,
  recording_mbid TEXT,
  title        TEXT NOT NULL,
  artist_id    TEXT NOT NULL REFERENCES artists(artist_id),
  album_uri    TEXT,
  duration_ms  INTEGER
);
CREATE INDEX tracks_isrc ON tracks(isrc);

CREATE TABLE artists (
  artist_id    TEXT PRIMARY KEY,          -- Spotify artist id
  name         TEXT NOT NULL,
  name_norm    TEXT NOT NULL,             -- casefolded, punctuation-stripped
  mbid         TEXT,
  lastfm_listeners INTEGER,
  fetched_at   INTEGER
);
CREATE INDEX artists_name_norm ON artists(name_norm);

CREATE TABLE artist_tags (
  artist_id TEXT, tag TEXT, weight REAL, source TEXT,   -- 'lastfm' | 'mb'
  PRIMARY KEY (artist_id, tag, source)
);

-- Similarity graph keyed by normalized name so unresolved Last.fm artists can live here
CREATE TABLE artist_edges (
  src_name_norm TEXT, dst_name_norm TEXT, sim REAL, source TEXT, fetched_at INTEGER,
  PRIMARY KEY (src_name_norm, dst_name_norm, source)
);

CREATE TABLE api_cache (
  key TEXT PRIMARY KEY, body BLOB, fetched_at INTEGER, ttl_s INTEGER
);

CREATE TABLE ledger (
  rec_id     INTEGER PRIMARY KEY,
  week       TEXT NOT NULL,               -- ISO week, e.g. '2026-W46'
  track_uri  TEXT NOT NULL UNIQUE,        -- UNIQUE enforces "never recommend twice"
  artist_id  TEXT NOT NULL,
  pool       TEXT NOT NULL,               -- 'deep' | 'near' | 'far'
  arm_key    TEXT NOT NULL,               -- e.g. 'pool:far' or 'tag:dub techno'
  score      REAL,
  position   INTEGER
);

CREATE TABLE outcomes (
  rec_id        INTEGER PRIMARY KEY REFERENCES ledger(rec_id),
  first_play_at INTEGER,
  ms_played     INTEGER,
  completed     INTEGER,
  skipped       INTEGER,
  replays_30d   INTEGER DEFAULT 0,
  saved         INTEGER DEFAULT 0,
  reward        REAL,                     -- computed, see §9
  status        TEXT DEFAULT 'unplayed'   -- 'unplayed' | 'pending' | 'scored'
);

CREATE TABLE arms (
  arm_key TEXT PRIMARY KEY, alpha REAL NOT NULL, beta REAL NOT NULL, updated_at INTEGER
);

CREATE TABLE runs (
  run_id INTEGER PRIMARY KEY, kind TEXT, started_at INTEGER, finished_at INTEGER,
  status TEXT, notes TEXT
);
```

## 7. Weekly build

### 7.1 Taste profile
Computed fresh each build (Polars over `plays`):

- **Artist affinity**
  `aff(a) = Σ_plays_of_a  completed_i · exp(-age_days_i / τ)`, τ = 365.
  Skips contribute a small negative: `-0.25 · skipped_i · decay`. Clamp at 0.
- **Exposure**
  `track_plays(t)` lifetime count; `artist_plays(a)` lifetime count; `recent_track_plays_90d(t)`.
- **Tag distribution**
  `share(tag) = Σ_a aff(a) · w(a, tag) / Σ_a aff(a)`, with `w` normalized per artist.
- **Seed set**: top `N_seed = 60` artists by affinity, plus 20 sampled from ranks 61–300 (prevents the profile itself from being dominated by heavy rotation).

### 7.2 Candidate pools
Each pool emits candidates `(track, artist, pool, arm_key, relevance)`.

**Deep cuts — new tracks from artists I already like**
- Artists: top 40 by affinity whose `artist_plays` covers < 60% of their catalog.
- Catalog: `GET /artists/{id}/albums?include_groups=album,single` → `GET /albums/{id}/tracks`.
- Exclude any track with `track_plays > 0`; dedupe by ISRC / normalized title (remasters, live versions).
- Relevance: `aff(artist)` × mild preference for mid-catalog popularity via Last.fm track listeners (skip the absolute top track — likely heard elsewhere; skip the long tail of interludes/skits by duration < 90 s).
- `arm_key = 'pool:deep'`.

**Near — unheard artists adjacent to my taste**
- For each seed artist `s`: `artist.getSimilar(s, limit=100)`.
- Candidate artist score: `near(c) = Σ_s aff(s) · match(s, c)`, restricted to `artist_plays(c) = 0`.
- Popularity dampening: divide by `log10(listeners + 10)^γ`, γ = 0.5 (tunable) so it doesn't only surface the most famous neighbor.
- Track pick: top 1–3 from `artist.getTopTracks(c)`.
- `arm_key = 'pool:near'` (v2: `'tag:<dominant tag of c>'`).

**Far — unfamiliar territory**
Two generators, 50/50:
1. *Inverse-share tag sampling.* Build tag universe from Last.fm tags seen anywhere in the graph cache + a curated seed list (world, jazz subgenres, classical periods, electronic microgenres…). Sample tag with probability ∝ `(share(tag) + ε)^-λ`, λ = 1, among tags with ≥ some minimal artist count. Pull `tag.getTopArtists(tag)`, skip ranks 1–20, take from 21–200.
2. *Random walk.* Start at a random seed artist, walk 3 hops on `artist_edges` choosing neighbors ∝ `match`, reject any node with `artist_plays > 0`. Endpoint is the candidate.
- Track pick: top 1–2 from `artist.getTopTracks`.
- Relevance: low by design; use `1 / (1 + rank)` within generator just for ordering.
- `arm_key = 'pool:far'` (v2: `'tag:<sampled tag>'`).

### 7.3 Hard filters (applied to all pools)
- `track_plays(track) = 0` (lifetime).
- `track_uri NOT IN ledger` (and ISRC not in ledger — catches duplicates across releases).
- Near/Far: `artist_plays(artist) = 0`.
- Track duration 90 s – 15 min.
- Must resolve to a playable Spotify URI (§7.5).
- Optional user blocklists (config): artists, tags.

### 7.4 Selector
Playlist size `K = 40`.

1. **Allocate slots across pools** via Thompson sampling with guardrails:
   - Sample `θ_p ~ Beta(α_p, β_p)` for each pool.
   - Raw share `s_p = θ_p / Σθ`.
   - Project onto `[floor, cap] = [0.20, 0.50]` and renormalize (water-filling).
   - `k_p = round(K · s_p)`, fix rounding so Σk = K.
   - Priors: `Beta(2, 2)` for all pools.
2. **Within pool**: rank by relevance, take top `4·k_p`, then MMR rerank to `k_p`:
   `mmr(c) = λ·rel(c) − (1−λ)·max_{c' ∈ chosen} cos(tags(c), tags(c'))`, λ = 0.6.
3. **Global diversity caps**: ≤ 1 track per new artist, ≤ 2 per known artist, no tag accounts for > 25% of the playlist (by dominant tag).
4. **Ordering**: interleave pools round-robin, starting with a deep cut (gentle onramp), never two far jumps adjacent.
5. **Backfill**: if a pool can't fill its slots after filters/resolution, redistribute to the others; log the shortfall.

### 7.5 Resolution to Spotify
- If ISRC known: `GET /search?q=isrc:{ISRC}&type=track&limit=5`.
- Else: `q=track:"{title}" artist:"{artist}"`, `limit=5`.
- Accept if normalized artist name matches and title fuzzy ratio ≥ 0.85 (strip "- Remastered 2011", "(Live)", etc.; prefer non-live unless source is live).
- Cache resolution results (including misses) for 90 days.
- Budget: ~3× K search calls per build; well within quota.

### 7.6 Writer
- One persistent playlist "Novelty Weekly" created once via `POST /me/playlists`; store its id in config.
- Each build: `PUT /playlists/{id}/items` with the new URIs (≤100 per request).
- Update playlist description with week + pool split, e.g. `2026-W46 · deep 10 / near 18 / far 12`.
- Optional: `--archive` creates a dated snapshot playlist `Novelty 2026-W46` for liked weeks.
- Insert ledger rows **only after** the PUT succeeds (transaction: write ledger in pending state → PUT → commit).

## 8. Poller

```
loop every 30 min:
  items = recently_played(after=cursor)
  upsert tracks/artists from items
  insert plays (source='poll', ms_played estimated from gaps; last item 'pending')
  finalize previous 'pending' play now that its successor is known
  for each new play whose track_uri ∈ ledger and outcome.status != 'scored':
      update outcomes (first_play_at, ms_played, completed, skipped)
  for each scored outcome < 30 days old: recompute replays_30d
  cursor = items.cursors.after
```
- `saved`: weekly diff of `GET /me/tracks` against ledger URIs (in `build`, not `poll`).

## 9. Feedback and learning

### 9.1 Reward
Per recommendation, once first played (or once the week is over):
- `completed` (≥ min(0.8·duration, 120 s) and not skipped) → 1.0
- skipped within 30 s → 0.0
- partial → `ms_played / duration`, clipped to [0, 1]
- bonus: `+0.5` if saved, `+0.25` per organic replay in 30 days (cap +1.0)
- **Unplayed → no update.** Not listening ≠ disliking (I may not get to every track).

Convert to a Bernoulli-ish update: `α += min(reward, 1)`, `β += 1 − min(reward, 1)`; bonuses add to α only.

### 9.2 Anti-convergence
- **Posterior decay** each build: `α ← 1 + ρ(α−1)`, `β ← 1 + ρ(β−1)`, ρ = 0.9 (effective memory ~10 weeks).
- **Floors** (§7.4) are hard; the bandit can tilt, never abandon a pool.
- **Tag cooldown** (v2): a tag that was dominant for > 15% of last week's playlist gets its far-pool sampling weight halved for 2 weeks.
- **Ledger uniqueness** guarantees no track repeats, ever.

### 9.3 v2: tag-level arms
- Arms keyed `tag:<tag>` for near/far candidates (dominant tag per artist), hierarchical prior from the pool arm: `Beta(α_pool/4, β_pool/4)` as prior for new tag arms.
- Within-pool relevance multiplied by sampled `θ_tag`.

## 10. Evaluation / metrics (`novelty report`)

Weekly, plus trailing 8-week trend:
- **Play-through rate**: % of recommendations played at all.
- **Completion rate** by pool and by tag.
- **Keep rate**: % saved or replayed within 30 d.
- **Spread**: Shannon entropy of dominant tags in the playlist; and entropy of my *total* listening's tag distribution over time (is my listening actually broadening?).
- **Artist concentration** of total listening: share of plays from top-20 artists, Gini over artists, 30-day rolling. This is the headline metric — it should go down.
- **Novelty of total listening**: % of weekly plays that are first-ever plays of a track / artist.
- Pool shares over time (sanity check that floors hold).

v2 report UI: single htmx page served locally over Tailscale, server-rendered SVG charts, plus per-track 👍/👎 buttons that write directly into `outcomes` (explicit feedback, weighted 2× implicit).

## 11. Interface

### 11.1 CLI
```
novelty auth                      # one-time PKCE login, stores refresh token
novelty ingest-history PATH.zip   # one-off; idempotent (UNIQUE on plays)
novelty poll                      # cron/timer every 30 min
novelty build [--dry-run] [--archive] [--k 40]
novelty report [--weeks 8] [--format text|svg]
novelty ledger show [--week 2026-W46]
novelty block artist|tag NAME     # add to blocklist
```
`--dry-run` prints the selected playlist with pool, arm, score and rationale (e.g. `near: similar to X (0.82), Y (0.61)`), writes nothing.

### 11.2 Config (`config.toml`)
```toml
[spotify]
client_id = "..."
redirect_uri = "http://127.0.0.1:8765/callback"
playlist_id = ""            # filled after first build

[lastfm]
api_key = "..."

[musicbrainz]
user_agent = "novelty-weekly/0.1 ( https://github.com/<you>/novelty-weekly )"
enabled = false

[build]
k = 40
pool_floor = 0.20
pool_cap = 0.50
mmr_lambda = 0.6
decay_rho = 0.9
affinity_tau_days = 365
popularity_gamma = 0.5
far_skip_top_ranks = 20

[blocklist]
artists = []
tags = ["christmas", "comedy", "spoken word"]
```
Secrets (refresh token) live in a separate `secrets.toml` with `0600` perms, not in config.

## 12. Stack and deployment

- Python 3.13, `uv` for env/lock.
- Dependencies: `httpx`, `polars`. Stdlib: `sqlite3`, `argparse`, `tomllib`, `zipfile`, `json`. No Spotify SDK. (Optional later: `rapidfuzz` for title matching — or hand-roll a normalized-Levenshtein.)
- Lint/type: `ruff`, `ty`.
- Host: the home Nobara box, reached over Tailscale.
- Scheduling: systemd **user** timers (`loginctl enable-linger` so they run without a session):
  - `novelty-poll.timer` — `OnCalendar=*:0/30`, `Persistent=true`
  - `novelty-build.timer` — `OnCalendar=Mon 06:00 America/Los_Angeles`, `Persistent=true`
- Alternatively a single container with supercronic; not needed for v0.
- Backups: nightly `sqlite3 novelty.db ".backup ..."` to a dated file; the ledger + outcomes are the irreplaceable part.
- Alerts: on refresh-token failure or 3 consecutive failed polls, send a notification (ntfy/email — TBD). Proactive reminder at 5.5 months after auth.

### Repo layout
```
novelty-weekly/
  pyproject.toml
  config.example.toml
  novelty/
    __init__.py
    cli.py
    config.py
    db.py              # schema, migrations (PRAGMA user_version)
    auth.py            # PKCE flow, token refresh
    http.py            # shared httpx client, rate limiter, cache wrapper
    sources/
      spotify.py
      lastfm.py
      musicbrainz.py
    ingest/
      history.py
      poll.py
    profile.py
    pools/
      deep.py
      near.py
      far.py
    select.py          # bandit, projection, MMR, caps, ordering
    resolve.py
    publish.py
    feedback.py        # rewards, arm updates, decay
    report.py
  tests/
    fixtures/          # small synthetic history + recorded API responses
  deploy/
    novelty-poll.service / .timer
    novelty-build.service / .timer
```

## 13. Roadmap

**v0 — "no repeats, new artists" (a weekend)**
- auth, poller, cold-start profile from `/me/top`, near pool only, hard filters, ledger, resolver, writer, `--dry-run`.
- Done when: a 40-track playlist of never-played artists appears Monday morning and the next week's has zero overlap.

**v1 — full novelty mix**
- history ingest, real affinity profile, deep + far pools, fixed 30/40/30 split, basic `report` (text).

**v2 — learning**
- outcomes + rewards, pool-level Thompson sampling with floors/decay, MMR + tag caps, saved-track diff.

**v3 — polish**
- tag-level arms, tag cooldown, htmx report page with SVG charts and 👍/👎, archive playlists, MusicBrainz genres.

**v4 — familiar mix + rediscover (only after novelty is solid)**

Rationale: familiar music is more enjoyable when it's mixed with enough novelty; pure novelty for its own sake isn't the end goal. And rediscovering a liked artist after years away is one of the best listening experiences there is.

- *Familiar slots (relaxes G1).* Reserve a share of K (start ~20%) for tracks I've played and completed, subject to cooldowns so it can't become the DJ loop:
  - track not played in the last 60 days, and played ≤ N times in the last year
  - an artist can only fill a familiar slot once every 4 weeks
  - the ledger stores familiar picks with their own cooldown instead of a permanent "never again"
  - weight by completion-weighted affinity, so it plays what I actually like, not what autoplay chose
- *Rediscover pool.* Artists with high lifetime affinity whose last play was > 2–3 years ago (configurable), ranked roughly by `aff_lifetime(a) × f(years_since_last_play)` with `f` rising and then flattening.
  - Mix the tracks I used to love with deep cuts from the same artist that I never played: part nostalgia, part novelty.
  - Lifetime extended history is a hard requirement here. Poller data alone can't see years back.
  - Needs its own reward rules. A rediscovered artist that gets back into regular rotation counts as a win, and it should then leave the rediscover pool.
- *Interaction with the existing pools.* "Familiar" and "rediscover" become two more arms with their own floor/cap. The novelty share (deep + near + far) keeps a hard floor (e.g. ≥ 60%) so the mix never drifts back toward mostly familiar.
- The §14 open question about "artists heard once, years ago" belongs here: treat them as rediscover candidates when they had real affinity, and as new artists for the near pool when they didn't.

## 14. Risks and open questions

- **Gap-based completion inference** from recently-played is noisy (pauses, device switches). Mitigation: conservative thresholds; explicit 👍/👎 in v3. *Open: is it worth adding a ListenBrainz scrobble as a second signal?*
- **Last.fm tag quality** is folksonomy noise ("seen live", "favorites"). Need a tag stoplist and maybe MusicBrainz genres as the canonical vocabulary.
- **Name matching** between Last.fm and Spotify for artists with common names. Mitigation: prefer MBID/ISRC paths; require top-track overlap as a tiebreak.
- **Spotify policy drift** — the API has tightened repeatedly (Nov 2024, Feb 2026, Jul 2026). Keep all Spotify calls behind `sources/spotify.py` so endpoint changes are a one-file fix.
- **Far pool too far?** Some weeks may be unlistenable. The floor keeps it ≥ 20%; tune λ (inverse-share exponent) and skip-top-ranks if completion < ~25% for 4+ weeks.
- *Open: should "artists heard only once, years ago" count as new for the near pool? (Proposal: `artist_plays ≤ 2 AND last play > 2 years ago` counts as new.)*
- *Open: K = 40 or two smaller playlists (Deep & Near / Far) so I can choose a mood?*
- *Open: include the DJ-era plays in affinity at full weight, or downweight `reason_start = 'clickrow'`-less autoplay since that's partly Spotify's choices, not mine?* (Strong candidate: weight autoplayed/DJ plays at 0.5.)

## 15. Future ideas (stubs — not scoped)

### 15.1 Offline "pocket" mode for liked songs
- Different philosophy: no discovery. Smart ordering of a fixed, already-downloaded pool, so the phone shuffle stops feeling like the same 30 songs.
- Core idea: precompute a scored queue from liked songs + history (cooldowns, artist spacing, tag variety, "haven't heard this in a while" boost), then let it run without network.
- Constraint to check first: Spotify's offline downloads are DRM'd and third-party apps can't read them. Realistic options:
  1. Push the precomputed ordering as a few rotating Spotify playlists, kept downloaded for offline use, refreshed whenever the phone is online. Thin client, no new app.
  2. If I ever keep a local music library, a tiny on-device scorer (SQLite + a small rules engine) over my own files.
- Reuses the profile, cooldowns and familiar-mix logic from v4. Mostly a packaging problem.

### 15.2 Covers as "refreshed familiarity"
- Hearing a cover of a song I love makes it feel new again. That puts it between familiar and novel, and it may be one of the best ways to mix the two.
- Pool: for songs with high affinity, find recordings of the same *work* by a different artist.
- Data: MusicBrainz work relationships (recording → "performance of" → work → other recordings), SecondHandSongs for cover lineage. Resolve to Spotify by ISRC or by search.
- Filters: different primary artist, not a karaoke/tribute-band/"made famous by" filler release (block by label/artist name patterns), not a live recording of the original artist.
- Nice side effect: a cover is often a gateway to a new artist or genre, so it can count as a near/far novelty arm as well.

### 15.3 Context/state search (e.g. "coding" → flow-state music)
- Inspired by Idagio's mood-for-classical feature, but modeled, not hand-curated playlists. Query by *what I'm doing* rather than genre.
- Possible signals, ordered by how cheap they are:
  1. My own behavior: label sessions (manually, or by time of day/weekday/device), then learn what I *complete* during "coding" sessions vs. others.
  2. Tags/genres as proxies: instrumental, ambient, minimal, post-rock, baroque, etc.
  3. Audio content: Spotify audio features and previews are gone, so this would need my own embeddings (e.g. a CLAP-style text–audio model running on the home GPU) over audio I can access legally, or archived AcousticBrainz-style descriptors.
- Query flow: text prompt → context vector → ranked candidates from the existing pools, still under novelty/familiar constraints.
- Flow-state requirements are different from discovery: low vocal density, steady energy, few abrupt transitions. Ordering matters as much as selection.

## 16. References

- Spotify — February 2026 Web API Dev Mode migration guide: https://developer.spotify.com/documentation/web-api/tutorials/february-2026-migration-guide
- Spotify — February 2026 endpoint/field changelog: https://developer.spotify.com/documentation/web-api/references/changes/february-2026
- Spotify — Update on Developer Access and Platform Security (2026-02-06): https://developer.spotify.com/blog/2026-02-06-update-on-developer-access-and-platform-security
- Vorp Labs — Spotify API changes tracker (refresh-token expiry, quota): https://vorplabs.com/agent-tools/spotify-api-changes
- Extended Streaming History request guide: https://freeyourmusic.com/blog/spotify-extended-streaming-history
- Last.fm API: https://www.last.fm/api
- MusicBrainz API & rate limiting: https://musicbrainz.org/doc/MusicBrainz_API
- ListenBrainz Weekly Exploration (prior art): https://community.metabrainz.org/t/our-weekly-recommendations-are-now-live/646950
