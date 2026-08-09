# Reddit (OAuth) — Plasm CGS

Curated read surface for the Reddit OAuth host: identity, subreddit metadata, post listings, post lookup, subreddit discovery, and post search (optional subreddit filter via query parameters).

## Auth and environment

- Obtain a user or installed-app OAuth access token with at least the `identity` and `read` scopes (see `oauth` in `domain.yaml` for scope notes).
- Set `REDDIT_ACCESS_TOKEN` to the bearer token.
- Every mapping sends a `User-Agent` header (`plasm:reddit-schema/1.0 (by plasm)`). Reddit often rejects anonymous defaults; override in `mappings.yaml` if the machine-spirit demands a richer product string.

## Run

```bash
export REDDIT_ACCESS_TOKEN=…
cargo run -p plasm-repl -- --schema apis/reddit --backend https://oauth.reddit.com
```

Eval coverage (no LLM):

```bash
cargo run -p plasm-eval -- coverage --schema apis/reddit --cases apis/reddit/eval/cases.yaml
```

## Listing decode

Reddit list endpoints return `data.children` entries shaped as `{ "kind": "…", "data": { … } }`. This catalog uses CML `response.item_inner_key: data` so rows decode from the inner `data` object (supported by `plasm-cml` / `plasm-runtime`).

## Post search and subreddits

`post_search_query` hits `search.json` on the OAuth origin. When `--subreddit` is set, the mapping sends `restrict_sr=true` and `sr=<display_name>` so results can be limited without a second capability.

## Domain choices

- **Post identity:** `Post.name` is the Reddit **fullname** (`t3_…`), which is stable for `api/info` lookups and write-side `thing_id` wiring.
- **Account identity:** `Account.id_field` is `name` (username). Listing JSON often exposes `author` as that same string; `Post.author` / `Comment.author` are optional `entity_ref` to `Account`, so rows with `"[deleted]"`, missing authors, or other non-username sentinels may omit or fail navigation to `account_get` without crashing the whole session.
- **Subreddit identity:** `Subreddit.display_name` matches the `subreddit` string on post rows (no `r/` prefix). `Post.subreddit` is an `entity_ref` for forward navigation to `subreddit_get`.
- **Feeds:** `Subreddit.posts` materializes via `post_feed_query` scoped on `subreddit` (hot/new/top/rising/controversial).
- **Comments:** `Post.comments` materializes `comment_thread_query` using the post’s `subreddit` ref and short `id` as `article`. The thread JSON is a **root array** (post listing, then comment listing); mappings decode the second listing’s `data.children` (top-level `t1` rows only in typical slices; `more` objects are skipped by kind filtering).
- **Out of scope:** modqueue, inbox, media uploads, and exhaustive path mirroring of reddit.com.
