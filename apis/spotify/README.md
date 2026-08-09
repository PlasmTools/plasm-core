# Spotify Web API — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Spotify Web API](https://developer.spotify.com/documentation/web-api/). Includes disjoint projections such as track detail and audio features.

```bash
export SPOTIFY_ACCESS_TOKEN=...
cargo run -p plasm-repl -- \
  --schema apis/spotify \
  --backend https://api.spotify.com \
  --repl
```

OAuth access tokens are required for most endpoints.
