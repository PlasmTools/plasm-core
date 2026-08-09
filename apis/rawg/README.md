# RAWG Video Games Database — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [RAWG API](https://rawg.io/apidocs).

```bash
export RAWG_API_KEY=...   # optional for higher rate limits
cargo run -p plasm-repl -- \
  --schema apis/rawg \
  --backend https://api.rawg.io \
  --repl
```
