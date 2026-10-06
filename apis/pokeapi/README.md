# PokéAPI — Plasm CGS Schema

A [Plasm](../../README.md) domain model for [PokéAPI](https://pokeapi.co/) (v2). Large entity surface: Pokémon, moves, abilities, items, locations, generations, and more.

## CLI / REPL

```bash
cargo run -p plasm-repl -- \
  --schema apis/pokeapi \
  --backend https://pokeapi.co
```

```text
# In plasm-repl (wire/id from teaching table):
Pokemon(name=pikachu)
# or session-symbolic: e1(name=pikachu)[name,id]
```

No API key is required for the public service.

## Species membership evidence

The [official Pokémon response contract](https://pokeapi.co/docs/v2/#pokemon)
defines `species` as a single `NamedAPIResource` targeting `PokemonSpecies`,
representing the species to which the Pokémon belongs. This supports the
`Pokemon.species` declaration of `cardinality: one` and
`materialize.collection_coverage: complete` at the parent GET's `species` path.
Runtime completeness still requires an observed valid path and conserved decoded
membership; hydration of species fields alone supplies no membership proof.

The `relation_render_e2e::relation_species_render_capture_rate_dry_and_live`
catalog smoke uses Hermit with the local PokeAPI OpenAPI fixture. It checks the
same whole-collection render with the complete catalog and with a cloned catalog
whose species completeness assertion is omitted. The cloned catalog's relation-only
source must report `ResultCoverage::Unknown`; its whole-collection render must
fail with `ResponseContract`, `Stop`, and `collection_incomplete` at node `line`.
Here “live” means runtime
execution against the mock, not a request to the public PokeAPI service. These
assertions have not been rerun for this change; no live-provider verification is
claimed.

## HTTP execute (`plasm-mcp --http`)

Multi-entry catalogs: **`just build-catalogs`** then **`--catalog-dir target/plasm-catalogs`** (each packed catalog corresponds to an `apis/<name>/` tree). PokéAPI’s default HTTP origin is **`http_backend`** in [`domain.yaml`](domain.yaml).

`plasm-mcp --http` and `--mcp` require a strong JWT secret for auth-framework initialization (see [AGENTS.md](../../AGENTS.md)). Example:

```bash
export PLASM_AUTH_JWT_SECRET='<long random string>'
just build-catalogs
cargo run -p plasm --bin plasm-mcp -- --catalog-dir target/plasm-catalogs --backend http://localhost:1080 \
  --http --port 3001 --mcp --mcp-port 3000
```

1. `POST /execute` with `{"entry_id":"pokeapi","entities":["Pokemon"]}` → `303` + `Location`.
2. `GET` that URL for `prompt`, `session`, `prompt_hash`.
3. `POST` the same path with a Plasm line body. For a get-by-name, use **`Pokemon(pikachu)`** (same meaning as CLI `pokemon pikachu`). Plasm does **not** use a `Get(…)` wrapper or `Entity:slug` — those shapes are parse errors or invalid; **`Entity(id)`** is the only get-by-id form. Prefer `Accept: application/json` when you want structured rows.

Expression forms are validated against the teaching table prompt for that session; if a line fails to parse, the API returns a problem+json error.
