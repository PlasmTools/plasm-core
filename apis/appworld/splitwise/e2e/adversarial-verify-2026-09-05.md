# splitwise adversarial verify — 2026-09-05

- **version:** 12  
- **schema validate:** PASS  
- **live:** PASS
- **description hygiene:** purpose-only banners (v12)

## Semantic seed

- **result:** PASS (intent-only `plasm_context` `session_mode: new`, no seeds)
- **host:** `PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1` + OpenRouter (`plasm-mcp --features semantic-auto-seed`); packed `target/plasm-catalogs-appworld`
- **intent:** `Login to Splitwise and list my groups and recent expenses`
- **seated:** AuthSession=e1, Expense=e2, Group=e3, AccountPassword=e4, Supervisor=e5
- **auth co-seed:** yes; supervisor AccountPassword co-seed: yes
- **dry from card:** PASS (`e3{access_token="tok"}` → dry_verdict `review`)
