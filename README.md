# Novelty Weekly

A self-hosted weekly Spotify playlist where **novelty is the objective**. Every track is one
I have never played and was never recommended before, drawn from three pools:

- **deep**: unheard tracks by artists I already like
- **near**: unheard artists close to my taste (Last.fm similarity)
- **far**: tags I barely listen to, and random walks away from my taste

A Thompson-sampling bandit learns from completions, skips and saves which pools land, with
a 20% floor per pool so it never stops exploring. Design: [docs/spec.md](docs/spec.md).

**Status:** the whole pipeline is built and tested against fake services. It has not run
against live Spotify or Last.fm yet. `report` and `block` are still stubs.

## The data contract

The app runs on one canonical model, [`src/model/`](src/model/mod.rs), not on any
service's format. Integrations only translate:

- **Observations** come in carrying names and whatever external IDs the source knows:
  a `Listen` of a `TrackRef`, an artist's `SimilarArtist`s, `ArtistTag`s, `PopularTrack`s.
- The **store** matches them to its own IDs: an artist is its normalized name, and a track
  is that artist plus a normalized title, so remasters and live versions are one song.
- The **engine** only sees resolved records, such as `Play`.

Two traits name the roles a service can fill
([`src/integrations/mod.rs`](src/integrations/mod.rs)): `ListeningService` (Spotify) and
`Knowledge` (Last.fm). Adding a service means implementing a role.

## Layout

```text
src/
  model/         the canonical types and their rules
  integrations/  Spotify (API, login, history export) and Last.fm adapters
  store/         SQLite: schema.sql is the whole data model
  engine/        profile, pools, filters, selector, feedback: pure functions
  commands/      one module per CLI command; build/ wires the full pipeline
tests/build.rs   a whole build against fake services
deploy/          systemd user timers
```

Dependencies only point inward, toward `model`.

## Running it

Needs Rust stable, a Spotify app's client ID (redirect URI `http://127.0.0.1:8765/callback`)
and a Last.fm API key. Secrets come from the environment: copy `.env.example` to `.env`
and run commands through `prun`. State lives in `$NOVELTY_HOME`
(default `~/.local/share/novelty`); `config.example.toml` lists every setting.

```sh
cargo install --path .
prun novelty auth                         # once; --paste if the browser is elsewhere
novelty ingest-history my_spotify_data.zip
prun novelty poll                         # then every 30 min via deploy/
prun novelty build --dry-run              # preview the playlist and why
novelty ledger show
```

`scripts/check.sh` runs the local CI: fmt, clippy, tests.

## License

MIT, see [LICENSE](LICENSE).
