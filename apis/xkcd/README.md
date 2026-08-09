# xkcd — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [xkcd JSON API](https://xkcd.com/json.html).

```bash
cargo run -p plasm-repl -- \
  --schema apis/xkcd \
  --backend https://xkcd.com \
  --repl
```

Public read-only; no auth.
