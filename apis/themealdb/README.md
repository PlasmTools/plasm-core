# TheMealDB — Plasm CGS Schema

A [Plasm](../../README.md) domain model for [TheMealDB](https://www.themealdb.com/api.php).

```bash
cargo run -p plasm-repl -- \
  --schema apis/themealdb \
  --backend https://www.themealdb.com/api/json/v1/1
```

Paths in `mappings.yaml` are relative to this origin (`search.php`, `filter.php`, `lookup.php`). Public read-only; optional Patreon key for contributor features if your mapping uses it.

See [apis/README.md](../README.md) for packing **`apis/`** to **`--catalog-dir`** packed JSON catalogs.
