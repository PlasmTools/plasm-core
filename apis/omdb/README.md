# OMDb API — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [OMDb API](http://www.omdbapi.com/) (movie and series metadata).

```bash
export OMDB_API_KEY=...
cargo run -p plasm-repl -- \
  --schema apis/omdb \
  --backend https://www.omdbapi.com \
  --repl
```

An API key is required for requests.
