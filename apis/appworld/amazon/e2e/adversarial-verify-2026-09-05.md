# amazon adversarial verify — 2026-09-05

- **version:** 8  
- **schema validate:** PASS  
- **live:** login + primary list PASS
- **description hygiene:** purpose-only entity banners (v8); Product no longer inventories filter fields

## Semantic seed

- **result:** PASS (intent-only `plasm_context` `session_mode: new`, no seeds)
- **host:** `PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1` + OpenRouter (`plasm-mcp --features semantic-auto-seed`); packed `target/plasm-catalogs-appworld`
- **intent:** `Login to Amazon and search products for headphones`
- **seated:** AuthSession=e1, Product=e2, AccountPassword=e3, Supervisor=e4
- **auth co-seed:** yes; supervisor AccountPassword co-seed: yes
- **dry from card:** PASS (`e2{access_token="tok", query="headphones"}` → dry_verdict `review`)
