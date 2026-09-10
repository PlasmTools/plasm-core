# AppWorld CGS adversarial verify (outside CUGA)

Date: 2026-09-05
Harness: `plasm-cgs schema validate`, Hermit/`validate --spec` where applicable, teaching dump via `plasm-mcp` HTTP execute, dry (`?mode=plan`) + live against AppWorld `:9000` (task `042a9fc_1`, `remote_apis_url`), **semantic auto-seed** (`PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1` + OpenRouter, intent-only `plasm_context`).

## Doctrine

Programming agent certifies catalogs (Plasm + teaching + **semantic seating**), not CUGA ladder. Full cutover on pagination dual-wire. List cardinality ≤1 query + ≤1 search per entity preserved. Semantic seed is a **mandatory** verify tier (see `plasm-oss/skills/plasm-catalog-adversarial-verify/SKILL.md`). Entity `description` banners are **purpose-only** (no field/param inventories) — `plasm-oss/scripts/check_catalog_description_hygiene.py --fail-on warn` clean on all 10.

## Scorecard

| API | version | schema | cardinality | teach | dry | live | semantic seed | notes |
|-----|---------|--------|-------------|-------|-----|------|---------------|-------|
| spotify | 12 | PASS | OK | PASS | PASS | PASS | PASS | LikedSong + AuthSession + supervisor co-seed; purpose-only shelf banners |
| venmo | 11 | PASS | OK | PASS | PASS* | PASS | PASS | *dry without `shelf` still needs_fix; seating OK; PaymentRequest received-only banner |
| splitwise | 12 | PASS | OK | PASS | PASS | PASS | PASS | purpose-only banners |
| file_system | 7 | PASS | OK | PASS | PASS | PASS | PASS | purpose-only banners |
| simple_note | 7 | PASS | OK | PASS | PASS | PASS | PASS | purpose-only banners |
| todoist | 6 | PASS | OK | PASS | PASS | PASS | PASS | purpose-only banners |
| amazon | 8 | PASS | OK | PASS | PASS | PASS | PASS | Product purpose-only; headphones brand-lock still OK |
| gmail | 7 | PASS | OK | PASS | PASS | PASS | PASS | mailbox required on thread query |
| phone | 9 | PASS | OK | PASS | PASS | PASS | PASS | login username is **e164** |
| supervisor | 6 | PASS | OK | PASS | PASS | PASS | PASS | AccountPassword purpose + role disambiguation |

## Semantic seed (intent-only) — retested after description hygiene

Host: `plasm-mcp --features semantic-auto-seed` + `PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1` + `OPENROUTER_API_KEY`; catalogs from `target/plasm-catalogs-appworld` (repacked after hygiene). Tools: `plasm_context` / `plasm` / `plasm_run` (no `discover_capabilities`). Bearer: `__plasm_mcp_anonymous__`.

| API | intent | seated (Active symbols) | dry from card |
|-----|--------|-------------------------|---------------|
| spotify | Login to Spotify and list my liked songs | AuthSession, LikedSong, AccountPassword, Supervisor | `LikedSong{access_token=…}` → review |
| venmo | Login to Venmo, find user Jordan, and show recent transactions | AuthSession, Transaction, User, Friend + supervisor | `Transaction{access_token=…}` → needs_fix (`shelf`) — pre-existing |
| splitwise | Login to Splitwise and list my groups and recent expenses | AuthSession, Expense, Group + supervisor | `Group{access_token=…}` → review |
| file_system | Login to File System and list files in the home directory | AuthSession, DirectoryEntry + supervisor | `DirectoryEntry{…, directory_path="/"}` → review |
| simple_note | Login to Simple Note and list my notes | AuthSession, Note + supervisor | `Note{access_token=…}` → review |
| todoist | Login to Todoist and list projects and open tasks | AuthSession, Project, Task + supervisor | `Project{access_token=…}` → review |
| gmail | Login to Gmail and show inbox email threads | AuthSession, EmailThread + supervisor | `EmailThread{…, mailbox="inbox"}` → review |
| phone | Login to Phone and list contacts | AuthSession, Contact + supervisor | `Contact{access_token=…}` → review |
| supervisor | Show the supervisor profile and list all account passwords | AccountPassword, Supervisor | `Supervisor` → PASS |
| amazon | Login to Amazon and search products for headphones | AuthSession, Product + supervisor | `Product{…, query="headphones"}` → review |

## CGS / discovery / runtime fixes in this pass

1. **Dual-wire pagination** removed on `venmo.transaction_query`, `gmail.email_thread_query`; page controls stripped from domain where pagination: owns the wire.
2. **Composable `_appworld_page_index_limit`** rolled to spotify, splitwise, simple_note, todoist, phone (manual page_index/page_limit excised).
3. **`plasm-cli` validate.rs** cut over from removed `CapabilitySchema.input_schema` to lane helpers (unblocks `plasm-cgs`).
4. **Runtime hydrate soft-fail** (`plasm-runtime` hydrate.rs): synthesized detail GET failures no longer abort the whole list query (AppWorld intermittently 409s ids that appeared in list pages).
5. **Brand-lock whole-token match** (`discovery_controlled_lexicon` / `intent_mentions_catalog_id`): stop substring false locks (`headphones` → `phone`). Unblocked amazon intent-only auto-seed.
6. **Amazon Product discovery** (v6→v8): richer `discovery.names`; entity banner purpose-only (no “by name, type, price…” inventory).
7. **AppWorld-wide description hygiene:** purpose-only entity/capability banners across all 10 catalogs; versions bumped where prose changed; `check_catalog_description_hygiene.py --fail-on warn` → 0/0. Discovery `names` left rich. Semantic seed retested — no regressions requiring discovery annotation fixes.

## Remaining gaps

- Teaching TSV still emits legacy row projection `[…]`; runtime rejects it (`| select` required). Agents must use bare rowsets or `| select` until prompt renderer cutover.
- Phone OpenAPI has Hermit route conflict (`/messages/voice/{phone_number}` vs `{voice_message_id}`) — Hermit `validate --spec` panics; live AppWorld OK.
- AppWorld song/album catalog lists can 409 on individual GETs; soft-fail keeps summaries usable.
- `plasm-repl` currently fails to build (plasm-eval IdentitySlot); verification used `plasm-mcp` HTTP / MCP instead.
- Venmo transaction dry without `shelf` still `needs_fix`/plan — seating OK; query shape remains an authoring follow-up.

## Per-API evidence

See `apis/appworld/<api>/e2e/adversarial-verify-2026-09-05.md` (each includes a **Semantic seed** section).
