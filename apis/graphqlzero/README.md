# GraphQLZero — curated Plasm schema

Public GraphQL API ([GraphQLZero](https://graphqlzero.almansi.me/)) with no auth. JSONPlaceholder-shaped **users, posts, comments, todos, albums, photos** with list/detail reads, **post** create/update/delete mutations, and **paginated lists** via CML `pagination` + nested `variables` (`body_merge_path`).

## Run (REPL)

```bash
cargo run -p plasm-repl -- \
  --schema apis/graphqlzero \
  --backend https://graphqlzero.almansi.me \
  --repl
```

Examples:

```text
user query --summary --limit 5
post 1
post create ( title: "Hello", body: "…" )
Post(1).post_update ( title: "…" )
```

Use **`--summary`** on any `*_query` when the entity also has a **get** capability—otherwise the runtime may hydrate every row with a follow-up GraphQL call.

**Pagination:** list capabilities support **`--limit`** and **`--all`** (merged into `variables.o.paginate.{page,limit}`). Default page size comes from the mapping’s pagination block.

`--backend` must be the site origin (no trailing slash). Paths in `mappings.yaml` use `api` → `https://graphqlzero.almansi.me/api`.

## GraphQL errors

The server may return HTTP 200 with a top-level JSON **`errors`** array. If a call fails unexpectedly, inspect the raw JSON body for `errors` (structured handling in the runtime may evolve later).

## Mutations (shared demo data)

Creates/updates/deletes affect the **public shared dataset**—use sparingly; prefer read-only flows in automation.

## Tests

Schema load + CML validation (no network):

```bash
cargo test -p plasm-e2e --test graphqlzero_smoke
```

Optional live query check (network; ignored by default):

```bash
cargo test -p plasm-e2e --test graphqlzero_live -- --ignored
```

Deterministic eval coverage (no LLM):

```bash
cargo run -p plasm-eval -- coverage --schema apis/graphqlzero --cases apis/graphqlzero/eval/cases.yaml
```

## Reference artifact

`schema.graphql` (when present) is a **documentation-only** slice of the public schema for authoring; Plasm does not load it at runtime.
