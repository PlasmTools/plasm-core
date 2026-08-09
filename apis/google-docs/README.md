# Google Docs API v1 — Plasm CGS

Curated schema for the [Google Docs API](https://developers.google.com/docs/api/reference/rest) (documents.get, documents.create, documents.batchUpdate).

A native Google Doc’s **document id** is the same as the **Drive file id** for that document (`application/vnd.google-apps.document`). Use **`apis/google-drive`** alongside this catalog when workflows need Drive file metadata, sharing, or binary export.

`discovery.json` here is a **vendored reference** for authors, not mechanical input to emit YAML.

```bash
export GOOGLE_DOCS_ACCESS_TOKEN=ya29...
cargo run -p plasm-repl -- --schema apis/google-docs --backend https://docs.googleapis.com
```

## Auth

OAuth 2.0 bearer token with Docs (and often Drive) scopes — see `domain.yaml` `oauth` and `requirements`.

## Validation

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/google-docs/
cargo run -p plasm-eval -- coverage --schema apis/google-docs --cases apis/google-docs/eval/cases.yaml
```
