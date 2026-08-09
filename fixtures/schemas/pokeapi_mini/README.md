# PokéAPI (mini) — Plasm CGS Schema

A minimal [Plasm](../../README.md) domain slice for [PokéAPI](https://pokeapi.co/) (berries + pagination), used by Hermit e2e and fast smoke tests. This tree lives under **`fixtures/schemas/`** (not **`apis/`**); for the full product surface, see [`../../apis/pokeapi/`](../../apis/pokeapi/).

```bash
cargo run -p plasm-repl -- \
  --schema fixtures/schemas/pokeapi_mini \
  --backend https://pokeapi.co \
  --repl
```

No authentication is required for the public API. **`just build-plugins`** only packs **`apis/<name>/`**; this slice is not a default multi-entry plugin artifact.
