# GitHub Pages documentation (`doc-site/`)

Static site for **plasm-core** — built by `.github/workflows/docs.yml` and deployed with **`actions/deploy-pages`** (artifact upload, not a legacy `gh-pages` branch checkout).

## Publish target

- **Repository:** whichever repo runs the workflow (e.g. [`ryan-s-roberts/plasm-core`](https://github.com/ryan-s-roberts/plasm-core) or an org fork such as [`PlasmTools/plasm-core`](https://github.com/PlasmTools/plasm-core)).
- **Public URL:** typically `https://<owner>.github.io/<repo>/` — keep `site_url` / `repo_url` in `mkdocs.yml` aligned with that owner/repo.

### GitHub Pages must use “GitHub Actions”

If **`deploy-pages`** fails with **`HttpError: Not Found`** / **Creating Pages deployment failed**:

1. Open **Settings → Pages** for the **same repository** that runs the workflow.
2. Under **Build and deployment**, set **Source** to **GitHub Actions** (not “Deploy from a branch”).
3. Re-run the workflow (or push to `main`). GitHub creates the **`github-pages`** environment when this source is selected.

Org-owned repos: confirm **Pages** is enabled under organization policy and that you have permission to change Pages settings.

- **Versioning:** docs track **`main`**; tag **`docs-vYYYY.MM.dd`** or release tags when you need a frozen snapshot (optional **mike** integration — not enabled by default).

## Doc inclusion policy

Sources under `docs/` are **allowlisted**. Maintainer workflow:

1. Edit canonical markdown under the **private monorepo** `docs/` and `.cursor/skills/plasm-forge/` as needed.
2. Run **`python scripts/sync_allowlisted_docs.py`** from `doc-site/` with monorepo root containing sibling `docs/` (paths adjusted in the script).
3. Commit updates under **`doc-site/docs/`** so the OSS repo stays self-contained for CI.

Excluded from sync (never published): SaaS architecture, OSS/SaaS boundary essays, private control-plane specs, Phoenix/UI UX docs — see project IA.

## Local build

```bash
cd doc-site
python -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt
mkdocs serve   # http://127.0.0.1:8000
mkdocs build --strict
```

## Essays (not in the sidebar)

Long-form pieces live under `docs/essay/` and are **omitted from `nav:`** so the technical IA stays tight. They are still built, searchable, and reachable by URL—for example:

- `/essay/plasm-typed-interaction-layer/` — companion to the [Medium launch essay](https://medium.com/@ryansroberts/plasm-a-typed-interaction-layer-for-agents-working-across-apis-38d9d90066a7).

## Navigation (MkDocs Material)

The theme uses a **nested left sidebar** for all `nav:` levels (Language → Language definition, MCP → …, etc.). Header **tabs** are intentionally disabled: with `navigation.tabs`, Material lifts every top-level entry into the header bar and the sidebar tends to look **flat / fragmented**. **`navigation.expand`** opens subsections by default; **`navigation.path`** adds breadcrumbs for nested pages.

## Visual design (SaaS-aligned)

[`docs/stylesheets/plasm-saas-theme.css`](docs/stylesheets/plasm-saas-theme.css) mirrors **OKLCH palette tokens** from the Phoenix shell (`web/assets/css/app.css` in the product monorepo): warm primary (light), violet primary (dark), subtle **plasma-style header gradients**, shared radii, and **Plus Jakarta Sans** / **JetBrains Mono**. The Material footer includes a **Plasm Cloud** social link to match hosted UX positioning.

## Editorial constraints

- Examples emphasize **HTTP** and **GraphQL** catalogs; omit experimental transport tutorials from onboarding.
- Appliance onboarding includes OAuth friction (incl. Google Workspace), PAT preference, and **[platform.plasm.tools](https://platform.plasm.tools)** for hosted OAuth management.
