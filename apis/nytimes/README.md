# New York Times APIs — Plasm CGS Schema

A [Plasm](../../README.md) domain model for [NY Times developer APIs](https://developer.nytimes.com/).

```bash
export NYT_API_KEY=...
cargo run -p plasm-repl -- \
  --schema apis/nytimes \
  --backend https://api.nytimes.com \
  --repl
```

Use a valid Times developer API key for the endpoints you enable in the schema.
