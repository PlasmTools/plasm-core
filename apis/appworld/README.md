# AppWorld CGS quarantine

Simulated AppWorld apps for the Plasm × AppWorld benchmark. **Not** live vendor catalogs.

- Family root has **no** `domain.yaml` — packer skips this directory under default `--apis-root apis`.
- Pack with: `plasm-pack-catalogs --apis-root apis/appworld --output-dir target/plasm-catalogs-appworld`
- Never list these names in `deploy/saas-packaged-apis.txt` or `plasm-oss/scripts/oss-packaged-apis.txt`.
- Do not confuse with live `apis/gmail`, `apis/spotify`, etc.

| Directory (`entry_id`) | Role |
|------------------------|------|
| `supervisor` | Task principal: profile, passwords, cards, addresses, `complete_task` |
| `simple_note` | Notes |
| `todoist` | Projects / tasks |
| `gmail` | Mail |
| `amazon` | Shopping |
| `venmo` | P2P payments |
| `splitwise` | Shared expenses |
| `spotify` | Music |
| `phone` | Contacts / SMS |
| `file_system` | Local FS |

OpenAPI pins: each child has `openapi.json` from AppWorld `data/api_docs/openapi/`. Refresh via [`scripts/appworld/bootstrap.sh`](../../../scripts/appworld/bootstrap.sh).

Doctrine: [`docs/appworld-plasm.md`](../../../docs/appworld-plasm.md).

## Pagination audit (AppWorld OpenAPI)

Simulated AppWorld APIs use **`page_index` + `page_limit`** on many list GET routes (default **`page_limit=5`**). Plasm only auto-fetches all pages when **`mappings.yaml` declares a composable `pagination:` block** — not when those params appear only as optional manual `query:` fields.

| Catalog | OpenAPI paginated GETs | `pagination:` blocks (2026-09) | Notes |
|---------|------------------------|--------------------------------|-------|
| `venmo` | 9 | **5 list queries fixed** | `user_search`, `friend_query`, `transaction_query`, `payment_request_*_query` |
| `gmail` | 6 | **7** (inbox/outbox/archived/spam/snoozed + drafts + users) | composable `pagination:`; dual-wire forbidden |
| `amazon` | 9 | **5** (`product`/`order`/`seller`/`product_type`/`product_review` query) | same |
| `spotify` | 17 | 0 | manual `page_index`/`page_limit` — **warns at pack** |
| `splitwise` | 9 | 0 | same |
| `phone` | 6 | 0 | same |
| `todoist` | 5 | 0 | no manual page vars yet |
| `simple_note` | 1 | 0 | manual page vars |
| `supervisor` | 0 | — | small lists, no OpenAPI paging |
| `file_system` | 0 | — | no OpenAPI paging |

Authoring: [plasm-authoring/reference.md — AppWorld pagination](../../skills/plasm-authoring/reference.md#appworld-simulated-apis-apisappworld). Compiler: `paginated_list_missing_cml_pagination_warnings` (warn) + `forbid_pagination_dual_wire` (fail-closed) in `plasm-compile` during `validate_cgs_capability_templates`.

## Enum / value-domain audit (vs OpenAPI, 2026-09)

OpenAPI `enum` on **inputs** (query/body) and **projected entity fields** must be `values:` rows with `type: enum` (or `array` of enum) and wired via `value_ref`. Account signup / verify / password-reset surfaces are intentionally omitted (doctrine).

Closed vocabularies that OpenAPI types as plain `string` but AppWorld fixes at runtime (Spotify genres, phone contact relationships) are also annotated as `type: enum` so semantic discovery and teaching do not treat them as free text.

| Catalog | Annotated | Surfaces that carry them |
|---------|-----------|--------------------------|
| `venmo` | `status`, `direction`, notification `type`, `transfer_type` | payment requests, transactions, `notification_query`, `bank_transfer_query`, `social_feed_query` |
| `amazon` | `relative_size`, return `deliverer_name` (`UPS`\|`USPS`\|`FedEx`), prime `duration` | product/cart/prime/returns; sellers, product types, cart promos, receipts, Q&A |
| `file_system` | `entry_type` | directory listing; `profile_get` |
| `phone` | alarm weekdays, `pagination_order`, contact `relationship` labels | alarms, message windows, contacts, `contact_relationship_query` |
| `todoist` | `color`, `priority`, `duration_unit`, notification `type` | projects/tasks; notifications; profile; project invites |
| `splitwise` | balance `direction`, notification `type`, activity `record_type` | balances/notifications/activity; `user_search`; `profile_get` |
| `supervisor` | profile `sex`, task `status`, address `name`, card `card_name` | profile / complete_task / address+card lists |
| `simple_note` | `append`/`prepend` | note append/prepend |
| `spotify` | premium `duration`, catalog `genre` | premium; songs/albums/artists; `genre_query`; `profile_get` |
| `gmail` | — (no OpenAPI enums) | `user_query`, `category_sizes_get`, `profile_get` |

Doctrine skips only account lifecycle (signup / verify / password-reset / delete account). Remaining OpenAPI surface for eval intents is mapped.
