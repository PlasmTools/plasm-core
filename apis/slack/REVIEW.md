# Slack toolkit — CGS completeness and ergonomics review

This document applies the [plasm-authoring skill](../../skills/plasm-authoring/SKILL.md): **gaps are expressed as proposed entities, relations, and capabilities**, not as a flat Slack method inventory. Slack’s wire API is RPC (`family.method`); authoritative external reference is the [Slack Web API methods](https://api.slack.com/methods) index and per-method pages (scopes, token types). There is no first-party OpenAPI spec in this repo; third-party OpenAPI conversions are optional cross-checks only.

**Review date:** 2026-04-12 · **Last follow-up sweep:** 2026-04-12

---

## 1. Toolchain baseline

| Check | Command | Result |
|--------|---------|--------|
| CGS + CML validate | `cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/slack` | Pass — **11 entities**, **57 capabilities** |
| Loader smoke | `cargo test -p plasm-core test_apis_split_schemas_smoke` | Pass |
| NL eval coverage | `cargo run -p plasm-eval -- coverage --schema apis/slack --cases apis/slack/eval/cases.yaml` | **Full coverage** — no missing required buckets; all capability domains appear in ≥1 eval case |
| REPL help | `cargo run -p plasm-repl -- --schema apis/slack --help` | Pass |

**Notes**

- Validating **`domain.yaml` alone** with `plasm schema validate apis/slack/domain.yaml` fails (no `method` in template); always validate the **directory** `apis/slack` so `mappings.yaml` is loaded.
- Schema validation is **`plasm-cgs`** (`plasm-cli` crate). Local live reads use **`plasm-repl`**; the remote client binary is **`plasm`** (`plasm` crate, no `--schema`).
- **teaching table / `channel_history` vs `channel_replies`:** `Message` had two query capabilities that differ only by required `ts`. The core resolver (`required_predicate_field_names_for_scoped_match` in `query_resolve.rs`) now treats required **filter-like** params (not only scope params) as part of the match key, so `channel` alone resolves to `channel_history` and `channel` + `ts` resolves to `channel_replies`. Loader warnings for those caps are cleared when teaching lines type-check.
- **`scheduledmessage_create` teaching:** `post_at` is modeled as **`integer`** (Unix seconds), matching Slack’s `post_at` argument and allowing `$` placeholders in synthesized `ScheduledMessage.create(…)` lines (temporal types do not accept the teaching table `$` token in shadow-arg parse).

---

## 2. Current CGS snapshot

| Entity | `id_field` | Relations (outgoing) |
|--------|------------|------------------------|
| Channel | `id` | `messages`, `members`, `pins`, `bookmarks`, `scheduled_messages` (materialized via scoped queries) |
| Message | `ts` | none (optional `channel` → `Channel`, optional `user` → `User` when the API returns them) |
| User | `id` | none |
| File | `id` | none |
| UserGroup | `id` | none |
| Team | `id` | none |
| Reminder | `id` | none |
| Bot | `id` | none |
| Pin | `id` | none |
| Bookmark | `id` | none |
| ScheduledMessage | `id` | none |

OAuth extension in `domain.yaml` catalogs Slack scopes and maps **`requirements.capabilities`** and **`requirements.relations`** for gating; `auth_test` is intentionally not OAuth-gated in that block.

---

## 3. Ergonomics and skill alignment (addressed in schema)

### 3.1 Message identity and channel context

**Done:** Optional `Message.channel` → `Channel` when payloads include channel id. **`ts` stays the id field** (unique per channel in Slack; capabilities supply `channel` for scope). History/replies still require `channel` in the capability input.

### 3.2 `chat.postMessage` / `chat.update` and Block Kit

**Done:** `message_post` / `message_update` declare optional `blocks`, `attachments` (`json_text`), `unfurl_links`, `unfurl_media`, and (post) `mrkdwn`. CML merges structured fields on update; post uses `body: { type: var, name: input }`. See [README.md](README.md) §Runtime / Block Kit.

### 3.3 List vs get projections

**Done:** Explicit **`provides:`** on list vs get where list rows are strict subsets (e.g. `channel_list`, `channel_history`, `channel_replies`, `user_list`, `file_list`, `reminder_list`, `pin_list`, `usergroup_list`, `scheduledmessage_list`, `bookmark_list`).

### 3.4 teaching table / prompt teaching

**Done:** Per-capability teaching table coverage is enforced by `slack_domain_covers_all_capabilities` in `plasm-core` (same pattern as Linear). Query disambiguation for Slack `Message` history vs replies is fixed in `query_resolve.rs` (see §1).

### 3.5 `entity_ref` audit

Scoped capability parameters use `entity_ref` (`channel`, `user`, `usergroup`). On **`Message`**, optional **`channel`** and **`user`** fields mirror Slack message JSON when present so composition and predicates can navigate to `Channel` / `User` without inferring only from scope params. Other nested ids (e.g. files, reactions) remain capability-driven until modeled explicitly.

---

## 4. Gap matrix — CGS-first backlog

Rows are **domain concepts**. “Wire” lists representative Slack methods; exact names may evolve — verify against current docs.

| Domain concept | Status | Notes |
|----------------|--------|--------|
| Scheduled messages | **Shipped** | `ScheduledMessage`, `Channel.scheduled_messages`, `scheduledmessage_list` / `scheduledmessage_create` / `scheduledmessage_delete` → `chat.scheduledMessages.list`, `chat.scheduleMessage`, `chat.deleteScheduledMessage` |
| Bookmarks | **Shipped** | `Bookmark`, `Channel.bookmarks`, `bookmark_*` → `bookmarks.*` (scopes `bookmarks:read` / `bookmarks:write`) |
| Stars (saved items) | Absent | `stars.*` — would need `Star` or union targets |
| User profile (full) | Partial | Consider `users.profile.get` / `users.profile.set` |
| Open DM / MPIM | Partial | `conversations.open` |
| Ephemeral messages | Absent | `chat.postEphemeral` |
| Reaction enumeration | Partial | `reactions.get` |
| File public URLs | Partial | `files.sharedPublicURL`, etc. |
| Search files | Absent | `search.files` |
| Workflows / Calls / Lists / Canvas / Assistant / Admin | Out of scope or TBD | Large or enterprise-only surfaces |

**Covered today (non-exhaustive):** conversations CRUD and membership, history/replies, scheduled messages, search messages, chat post/update/delete, reactions add/remove, users, files, usergroups, team, reminders, pins, bookmarks, bots, auth.test, user identity.

---

## 5. Prioritized follow-ups (2026-04-12)

**Correctness / contract** — **done:** Block Kit / input merge documented; list vs get `provides:` reconciled.

**Polish / agent UX** — **done:** teaching table warnings for `channel_history` / `channel_replies` addressed via resolver + `provides:`; optional `Message.channel`; README / `plasm-cgs` / validate-directory notes.

**Expansion** — **done:** Bookmarks + scheduled messages authored (domain → mappings → validate → eval).

Remaining product backlog is §4 (stars, ephemerals, reactions list, etc.), not the original five-item checklist.

---

## 6. Commands reference (copy-paste)

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/slack
cargo test -p plasm-core test_apis_split_schemas_smoke
cargo test -p plasm-core slack_domain_covers_all_capabilities
cargo run -p plasm-eval -- coverage --schema apis/slack --cases apis/slack/eval/cases.yaml
cargo run -p plasm-repl -- --schema apis/slack --help
```
