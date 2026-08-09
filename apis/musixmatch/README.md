# Musixmatch API — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Musixmatch API](https://developer.musixmatch.com/). Lyrics are modeled as a related entity where applicable.

```bash
export MUSIXMATCH_API_KEY=...
cargo run -p plasm-repl -- \
  --schema apis/musixmatch \
  --backend https://api.musixmatch.com \
  --repl
```
