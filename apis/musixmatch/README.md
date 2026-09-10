# Musixmatch API — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Musixmatch API](https://developer.musixmatch.com/). Lyrics are modeled as a related entity where applicable.

Track listing uses an explicit source selection: `mode="search"` accepts track,
artist, genre and lyrics-availability filters; `mode="chart"` lists a country's
chart. For example, named local-schema expressions include
`Track{mode="search", q_artist="composer"}` and
`Track{mode="chart", country="gb"}`. Hosted sessions use their taught entity symbols.

```bash
export MUSIXMATCH_API_KEY=...
cargo run -p plasm-repl -- \
  --schema apis/musixmatch \
  --backend https://api.musixmatch.com \
  --repl
```
