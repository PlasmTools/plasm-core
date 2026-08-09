# LinkedIn v2 (Rest.li) — Plasm CGS

This schema models a relation-oriented subset of LinkedIn APIs:

- OIDC `userinfo` (`OpenIdProfile`)
- Member profile (`Member`)
- UGC post read/create (`UgcPost`)
- Asset upload registration (`MediaUploadRegistration`)

It includes OAuth scope implications and scoped relation traversal:

- `Member.feed_posts` -> `UgcPost` via scoped query
- `Organization.feed_posts` -> `UgcPost` via scoped query

## Auth

Set a bearer token with sufficient LinkedIn product scopes:

```bash
export LINKEDIN_ACCESS_TOKEN=...
```

## Validate

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/linkedin
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/linkedin/openapi.json apis/linkedin
```

## Query examples

```bash
# OIDC userinfo
plasm --schema apis/linkedin openidprofile get

# Current member profile
plasm --schema apis/linkedin member get

# Member-authored posts (scope composed via CML format expression)
plasm --schema apis/linkedin ugcpost ugc-post-query-for-member --member 8675309
```

## Notes

- LinkedIn finder params use Rest.li list syntax (`List(...)` with encoded URNs).
- Mappings use the CML `format` expression to compose finder values from modeled IDs.
- Spec validation currently reports relation traversal warnings for entities with only required scoped query capabilities; direct capability execution validates successfully.
