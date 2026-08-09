# Open Brewery DB — Plasm CGS Schema

A [Plasm](../../README.md) domain model for [Open Brewery DB](https://www.openbrewerydb.org/).

```bash
cargo run -p plasm-repl -- \
  --schema apis/openbrewerydb \
  --backend https://api.openbrewerydb.org \
  --repl
```

Public read-only; no auth.
