# Hacker News (Firebase API)

Read-only mirror of the public [Hacker News API](https://github.com/HackerNews/API) plus the [Algolia HN search API](https://hn.algolia.com/api) (full-URL CML; same `http_backend` is Firebase for `item_get`). No authentication.

## Scope

- **item_search** / **item_search_by_date** — Algolia `search` and `search_by_date` (default `tags=story`). Rows use `objectID` as `id`; use **item_get** to load the Firebase item.
- **item_feed_query** — ordered item ids for `top`, `new`, `best`, `ask`, `show`, or `job` (ids only; use **item_get** to hydrate).
- **max_item_id_query** — current largest item id (`maxitem.json` returns a bare integer; mapped via `wrap_root_scalar`).
- **recent_updated_item_query** / **recent_updated_user_query** — live slices from `updates.json` (`items` and `profiles` arrays; profiles are usernames only).
- **item_get** / **user_get** — full JSON for an item or user. Poll stories expose **`parts`** (option ids) and the **`poll_options`** relation for chaining.

## Try it

```bash
cargo run --bin plasm-repl -- --schema apis/hackernews
```

Eval coverage (no LLM):

```bash
cargo run -p plasm-eval -- coverage --schema apis/hackernews --cases apis/hackernews/eval/cases.yaml
```
