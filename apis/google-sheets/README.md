# Google Sheets API v4 — Plasm CGS schema

A Plasm domain model for the [Google Sheets API](https://developers.google.com/workspace/sheets/api/reference/rest) (v4 REST). Covers spreadsheets, worksheet tabs (embedded metadata), cell value reads and writes (including batch and data-filter variants), sheet copy, and developer metadata.

Machine-readable specs vendored in this directory:

- `discovery.json` — official Google Discovery document (`sheets.googleapis.com`, v4).
- `openapi.json` — OpenAPI 3 bundle from APIs.guru (for Hermit / `plasm validate` mocks).

```bash
# Live API (set GOOGLE_SHEETS_ACCESS_TOKEN to a valid OAuth access token)
export GOOGLE_SHEETS_ACCESS_TOKEN=ya29...
cargo run -p plasm-repl -- --schema apis/google-sheets --backend https://sheets.googleapis.com
```

## Auth

The API uses OAuth 2.0. Set `GOOGLE_SHEETS_ACCESS_TOKEN` to a user access token with the scopes required for the operations you call. The CGS `oauth` block lists every Google scope string and maps each capability to the same scope list as the Discovery document for that method.

Read-only flows can use `spreadsheets.readonly` (often with `drive.readonly`). Writes require `spreadsheets` and do **not** accept readonly-only tokens for mutating capabilities—see `domain.yaml` `oauth.requirements`.

## Design notes

- **No spreadsheet list in Sheets v4**: discovering file IDs is a Drive API concern; this catalog models the Sheets resource surface only.
- **Compound keys**: `ValueRange` and `DeveloperMetadata` use string/integer key parts (Calendar-style) so prompt teaching examples can synthesize valid `Entity(spreadsheetId=$, range=$)` forms. `spreadsheetId` is modeled as a plain string id, not `entity_ref`, for that reason.
- **Path quirks**: Methods such as `values:batchGet`, `{range}:append`, and `spreadsheets:batchUpdate` append suffixes directly to path variables. CML supports an optional `suffix` on `type: var` path segments for this.
- **Structural batch update (`spreadsheet_batch_update`)**: The HTTP mapping posts a JSON body bound to CML var `input`. For real requests, pass `batch-update(input={...})` with a `BatchUpdateSpreadsheetRequest` object (at minimum a `requests` array). The domain marks `input` optional so zero-arity `batch-update()` still parses, but the wire call still needs a body for the API to accept the request.
- **Cell matrix field**: `ValueRange.values` is `field_type: json` to hold the API’s heterogeneous row/column arrays.

## Validation

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/google-sheets/
cargo run -p plasm-eval -- coverage --schema apis/google-sheets --cases apis/google-sheets/eval/cases.yaml
# Optional Hermit exercise (large spec):
# cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/google-sheets/openapi.json apis/google-sheets
```

Ranges in URLs may need URL-encoding when they contain `!` or other reserved characters; pass encoded range strings if the HTTP client does not encode path segments.
