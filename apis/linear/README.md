# Linear — task-oriented Plasm catalog

[Linear](https://linear.app/) GraphQL at `https://api.linear.app/graphql`. This catalog is **name-centric** (issue identifiers like `ENG-42`, team keys like `ENG`, state/label names in search) — aligned with [Linear #1035](https://github.com/linear/linear/issues/1035) task-shaped agent surfaces, not a mirror of every GraphQL operation.

**Vendor schema:** run introspection — do not hand-author SDL.

```bash
export LINEAR_API_TOKEN='lin_api_…'
apis/linear/scripts/refresh_schema.sh
```

## Auth

Personal API keys use a **raw** `Authorization` header (no `Bearer` prefix).

```bash
export LINEAR_API_TOKEN='lin_api_…'
```

## Live MCP / agent smoke

NL eval cases under [`eval/cases.yaml`](eval/cases.yaml) use fixture names like `ENG` and `ENG-42` for synthesis coverage. **Do not assume those ids exist in your workspace.** Before live `plasm_run` tests:

1. `discover_capabilities` or `Issue.search` for a real team key and issue identifier.
2. Substitute discovered values in get/comment/workflow programs.
3. Treat `Entity not found` or zero-row search as workspace data mismatch unless auth fails.

## Schema overlay (deferred)

Linear workspaces can define **issue custom fields**, but the **public GraphQL schema** exposed in [Linear's SDK](https://github.com/linear/linear/blob/master/packages/sdk/src/schema.graphql) does not yet include a stable **`customFields`** (or equivalent) query for field definitions and issue value shapes. When Linear publishes that surface, add **`custom_field_query`** + **`schema_overlay:`** with `projection.mode: augment_base` on **`Issue`** (same pattern as [ClickUp](../clickup/domain.yaml)). Until then, use static `Issue` fields and `issue_update` for standard properties only.

## REPL

```bash
cargo run --bin plasm -- \
  --schema apis/linear \
  --backend https://api.linear.app \
  --repl
```

## Task expressions (examples)

**Search / filter**

```text
plasm> Issue.search(team_key=ENG, label_name=bug, q=auth)
plasm> Issue.search(assignee_name=Jane Doe, state_name=In Progress)
plasm> Project.search(q=Mobile)
plasm> Document.search(q=spec)
```

**Get by human identifier**

```text
plasm> Issue(ENG-42)
plasm> Team(ENG)
```

**Composed reads (#1035-style)**

```text
plasm> IssueContext(ENG-42)
plasm> MyWorkSnapshot
plasm> ProjectContext(<project-uuid>)
plasm> IssueNavigationLink(ENG-42)
plasm> CycleBoardSnapshot(<cycle-uuid>)
```

**Writes**

```text
plasm> Issue.create(team=Team(ENG), title=New bug from Plasm)
plasm> Issue(ENG-42).update(title=Renamed, state_name=In Progress)
plasm> Issue(ENG-42).delete
plasm> Comment.create(issue=Issue(ENG-42), body=LGTM)
plasm> Issue(ENG-42).comments
```

**Pagination:** list/search capabilities paginate via `page(pg#)` in execute sessions after the first wave.

## Federated write recipe (e.g. PokeAPI → Linear)

Cross-catalog goals that end in a Linear write should be **one coherent pipeline**, not parallel reads from each catalog. Shape (substitute session `e#`/`m#`/`p#` from the teaching table):

```text
team = Team.limit(1)                       # discover a live anchor — never hardcode workspace ids
src  = Pokemon(pikachu)                    # one upstream GET (simple-id entity, positional identity)
ticket = src => Issue.create(team=Team(ENG), title=Pokedex #025, description=<<MD
…markdown body…
MD)
ticket[identifier,title,description]        # single created-row return
```

Discipline:

- **Single return** = the created row; do not return `read_a, read_b` parallel roots.
- **Call budget:** one GET per catalog; avoid relation fanout unless you genuinely need N rows.
- A provably-singleton source (`Pokemon(pikachu)`) `=>` runs the write **once** (not a fanout).
- **Discover before id:** resolve the team key from `Team` / `Team.limit(1)` / search — eval ids like `ENG`/`ENG-42` are not your workspace.
- `issue_create` now projects `description`, so `ticket[description]` confirms the markdown body was applied.

## Coverage

See [COVERAGE.md](COVERAGE.md). Eval goals: [eval/cases.yaml](eval/cases.yaml).

```bash
cargo run -p plasm-eval -- coverage --schema apis/linear --cases apis/linear/eval/cases.yaml
```

## Tests

```bash
cargo test -p plasm-e2e --test linear_smoke
cargo test -p plasm-e2e --test linear_live -- --ignored   # needs LINEAR_API_TOKEN
```
