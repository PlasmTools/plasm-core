# AppWorld CGS quarantine

Simulated AppWorld apps for the Plasm × AppWorld benchmark. **Not** live vendor catalogs.

- Family root has **no** `domain.yaml` — packer skips this directory under default `--apis-root apis`.
- Pack with: `plasm-pack-catalogs --apis-root apis/appworld --output-dir target/plasm-catalogs-appworld`
- Never list these names in `deploy/saas-packaged-apis.txt` or `plasm-oss/scripts/oss-packaged-apis.txt`.
- Do not confuse with live `apis/gmail`, `apis/spotify`, etc.

| Directory (`entry_id`) | Role |
|------------------------|------|
| `supervisor` | Task principal: profile, passwords, cards, addresses, `complete_task` |
| `simple_note` | Notes |
| `todoist` | Projects / tasks |
| `gmail` | Mail |
| `amazon` | Shopping |
| `venmo` | P2P payments |
| `splitwise` | Shared expenses |
| `spotify` | Music |
| `phone` | Contacts / SMS |
| `file_system` | Local FS |

OpenAPI pins: each child has `openapi.json` from AppWorld `data/api_docs/openapi/`. Refresh via [`scripts/appworld/bootstrap.sh`](../../../scripts/appworld/bootstrap.sh).

Doctrine: [`docs/appworld-plasm.md`](../../../docs/appworld-plasm.md).
