# X API v2 (Twitter) — Plasm CGS

Curated [Plasm](../../README.md) catalog for the [X API v2](https://docs.x.com/x-api/introduction) REST surface: **users**, **posts (tweets)**, **lists**, **recent search**, **timelines**, **mentions**, **likes**, and **writes** (create post, delete post, create/delete list). The CGS is **authored for agent workflows**, not a mechanical export of every OpenAPI operation.

```bash
export TWITTER_ACCESS_TOKEN=…   # OAuth 2 user access token or app-only bearer

cargo run -p plasm-repl -- \
  --schema apis/twitter \
  --backend https://api.x.com
```

The vendored OpenAPI (`openapi.json`) lists `servers[0].url` as **https://api.x.com**; **https://api.twitter.com** remains a supported alias at the network edge.

## Files

| File | Purpose |
|------|---------|
| [`domain.yaml`](domain.yaml) | Entities (`User`, `Tweet`, `List`), relations, capabilities, `oauth` scope catalog + requirements, `auth` bearer env |
| [`mappings.yaml`](mappings.yaml) | CML: paths under `/2/…`, `{ data, meta }` envelopes, cursor pagination |
| [`openapi.json`](openapi.json) | Machine-readable spec (see [`SOURCE.md`](SOURCE.md)) |
| [`eval/cases.yaml`](eval/cases.yaml) | NL eval goals + `plasm-eval coverage` buckets |

## OAuth 2 scopes

User-context tokens use short scope names (`tweet.read`, `users.read`, …). The `oauth` block in `domain.yaml` mirrors the **OAuth2UserToken** requirements from the OpenAPI security schemes and the [v2 authentication mapping](https://docs.x.com/fundamentals/authentication/guides/v2-authentication-mapping). Where the spec lists **several scopes on one line**, the token must satisfy **all** of them — modeled as `all_of` / nested `any_of` in `oauth.requirements`.

Default scope bundles (for control-plane hints):

- **`plasm_twitter_read_core`** — read timelines, profiles, lists, liked posts, recent search
- **`plasm_twitter_write_core`** — create/delete posts and lists, plus `offline.access` for refresh tokens

## Validate

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/twitter
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/twitter/openapi.json apis/twitter
cargo run -p plasm-eval -- coverage --schema apis/twitter --cases apis/twitter/eval/cases.yaml
```

## Coverage in this tree

**Implemented**

- **Users:** `user_get`, `user_get_me`, `user_query` (by usernames)
- **Posts:** `tweet_get`, `tweet_lookup_query` (ids), `tweet_search` (recent), `tweet_timeline_query`, `tweet_mentions_query`, `user_liked_tweets_query`, `tweet_create`, `tweet_delete`
- **Lists:** `list_get`, `list_create`, `list_delete`, `list_tweets_query`
- **Relations:** `User.tweets`, `User.mentions`, `User.liked_tweets` → `Tweet`; `List.tweets` → `Tweet`; `Tweet.author` → `User`

**Out of scope** (add via further authoring passes over `openapi.json`)

- Streams, filtered stream rules, compliance / firehose endpoints
- Full-archive search, DM, Spaces, bookmarks folders, follows graph mutations, communities, media upload, analytics tiers that require elevated products

## Refresh OpenAPI

```bash
curl -fsSL -A 'twitter-api-typescript-sdk/1.2.1' -o apis/twitter/openapi.json \
  'https://api.twitter.com/2/openapi.json'
```

Then re-run `plasm … validate` and adjust mappings if path or response shapes changed.
