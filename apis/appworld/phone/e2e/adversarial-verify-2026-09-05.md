# phone adversarial verify — 2026-09-05

- **version:** 9  
- **schema validate:** PASS  
- **live:** PASS
- **description hygiene:** purpose-only banners (v9)

## Semantic seed

- **result:** PASS (intent-only `plasm_context` `session_mode: new`, no seeds)
- **host:** `PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1` + OpenRouter (`plasm-mcp --features semantic-auto-seed`); packed `target/plasm-catalogs-appworld`
- **intent:** `Login to Phone and list contacts`
- **seated:** AuthSession=e1, Contact=e2, AccountPassword=e3, Supervisor=e4
- **auth co-seed:** yes; supervisor AccountPassword co-seed: yes
- **dry from card:** PASS (`e2{access_token="tok"}` → dry_verdict `review`)
