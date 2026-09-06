# supervisor adversarial verify — 2026-09-05

- **version:** 6  
- **schema validate:** PASS  
- **live:** PASS
- **description hygiene:** purpose-only banners (v6); AccountPassword drops identity-restatement “get by app key”

## Semantic seed

- **result:** PASS (intent-only `plasm_context` `session_mode: new`, no seeds)
- **host:** `PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1` + OpenRouter (`plasm-mcp --features semantic-auto-seed`); packed `target/plasm-catalogs-appworld`
- **intent:** `Show the supervisor profile and list all account passwords`
- **seated:** AccountPassword=e1, Supervisor=e2
- **dry from card:** PASS (`e2` → live profile row / plan review)
