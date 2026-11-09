# Novelty Weekly

A self-hosted weekly Spotify playlist where **novelty is the objective**. Every track is one
I have never played and was never recommended before, drawn from three pools:

- **deep**: unheard tracks by artists I already like
- **near**: unheard artists close to my taste
- **far**: genres I barely listen to

A bandit learns from completions and skips which pools land, with a floor so it never
stops exploring. The design is in [docs/spec.md](docs/spec.md).

Status: early. The CLI surface exists; commands are being filled in.

## Layout

```text
src/
  model/         the canonical data contract: Listen, TrackRef, ArtistRef, ...
  integrations/  adapters (Spotify, Last.fm) that produce and consume model types
  store/         SQLite holding the canonical state
  engine/        recommendation logic as pure functions over model types
  commands/      one file per CLI command, wiring the three above
```

Dependencies only point inward, toward `model`. A new integration only has to produce
`model` types.

## Running it

Needs Rust (stable) and, for live use, a Spotify app's client ID and a Last.fm API key.

```sh
cargo run -- --help
```
