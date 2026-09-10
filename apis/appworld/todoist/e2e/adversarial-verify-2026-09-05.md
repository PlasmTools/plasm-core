# todoist adversarial verify — 2026-09-05

- **version:** 6  
- **schema validate:** PASS  
- **live:** PASS
- **description hygiene:** purpose-only banners (v6)

## Semantic seed

- **result:** PASS (intent-only `plasm_context` `session_mode: new`, no seeds)
- **host:** `PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1` + OpenRouter (`plasm-mcp --features semantic-auto-seed`); packed `target/plasm-catalogs-appworld`
- **intent:** `Login to Todoist and list projects and open tasks`
- **seated:** AccountPassword=e1, Supervisor=e2, AuthSession=e3, Project=e4, Task=e5
- **auth co-seed:** yes; supervisor AccountPassword co-seed: yes
- **dry from card:** PASS (`e4{access_token="tok"}` → dry_verdict `review`)
