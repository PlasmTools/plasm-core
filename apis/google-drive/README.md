# Google Drive API v3 — Plasm CGS

Curated agent surface for the [Google Drive API](https://developers.google.com/drive/api/reference/rest/v3) (REST v3): files, permissions, comments, revisions, shared drives, changes, legacy team drives, and related operations.

`discovery.json` in this directory is a **vendored reference** for human authoring while reading the API — it is **not** used to emit YAML. This catalog intentionally omits several niche or admin-heavy methods (for example access proposals, approvals, installed apps, label CRUD, some watch variants, and ID generation); add them by hand in future passes if your agents need them.

```bash
export GOOGLE_DRIVE_ACCESS_TOKEN=ya29...
cargo run -p plasm-repl -- --schema apis/google-drive --backend https://www.googleapis.com/drive/v3
```

## Auth

OAuth 2.0 bearer token with Drive scopes appropriate to the capabilities you call. See `domain.yaml` `oauth.requirements` and `default_scope_sets`.

**Comment / reply edits:** `comments_update` and `replies_update` use **`preflight`** (`hydrate_invoke_target` with `comments_get` / `replies_get`) with env prefix **`parent`**. The PATCH JSON body merges optional **`content`**, **`anchor`**, **`quotedFileContent`** (comments) and **`content`**, **`action`** (replies) over the hydrated parent row—no raw `input` blob.

## Validation

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/google-drive/
cargo run -p plasm-eval -- coverage --schema apis/google-drive --cases apis/google-drive/eval/cases.yaml
```
